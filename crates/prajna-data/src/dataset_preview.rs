//! Read-only preview of formally committed Dataset Versions.
//!
//! This module never normalizes, publishes, collects or downloads data. It opens
//! one explicit local lake root, verifies each manifest through the production
//! reader, and returns Dataset identity, instruments, raw unadjusted Bars,
//! frozen security-evidence lifecycle fields, Session/Bar coverage and gaps.
//! Missing and Unknown facts stay explicit; no zero Bar, default lot or implied
//! trading Session is created.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use arrow_array::RecordBatch;
use chrono::NaiveDate;
use prajna_domain::{Bar, InstrumentKind, InstrumentSpec, Session};
use serde_json::{Value, json};

use crate::{
    BarSourceRow, FieldEvidence, Manifest, PublicationStatus, RawStore, SecurityEvidenceSnapshot,
    bar_sources_from_record_batch, bars_from_record_batch, instruments_from_record_batch,
    read_dataset_table, read_manifest, sessions_from_record_batch,
};

/// Schema version of the JSON emitted by this preview entry.
pub const PREVIEW_SCHEMA_VERSION: u32 = 1;
/// Maximum number of gap dates returned per instrument in one detail response.
pub const MAX_GAP_DATES: usize = 200;

#[derive(Debug)]
pub enum PreviewError {
    Root(String),
    NotFound(String),
    InvalidInput(String),
    Manifest { dsv: String, message: String },
    Table { table: String, message: String },
    Io(String),
}

impl PreviewError {
    /// Stable machine-readable code for the local API adapter.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Root(_) => "invalid_dataset_root",
            Self::NotFound(_) => "dataset_not_found",
            Self::InvalidInput(_) => "invalid_request",
            Self::Manifest { .. } => "invalid_manifest",
            Self::Table { .. } => "invalid_dataset_table",
            Self::Io(_) => "dataset_read_failed",
        }
    }

    /// `2` is invalid input/not found; `1` is an I/O or integrity failure.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::NotFound(_) | Self::InvalidInput(_) => 2,
            Self::Root(_) | Self::Manifest { .. } | Self::Table { .. } | Self::Io(_) => 1,
        }
    }
}

impl std::fmt::Display for PreviewError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Root(message)
            | Self::NotFound(message)
            | Self::InvalidInput(message)
            | Self::Io(message) => formatter.write_str(message),
            Self::Manifest { dsv, message } => write!(formatter, "{dsv}: {message}"),
            Self::Table { table, message } => write!(formatter, "{table}: {message}"),
        }
    }
}

impl std::error::Error for PreviewError {}

/// Opens one explicit local Dataset root, rejecting the fixed MVP-6 archive.
pub fn open_dataset_root(root: &Path) -> Result<PathBuf, PreviewError> {
    open_dataset_root_with_protected(root, &crate::default_mvp6_root())
}

/// Testable form of [`open_dataset_root`] with an explicit protected archive.
pub fn open_dataset_root_with_protected(
    root: &Path,
    protected: &Path,
) -> Result<PathBuf, PreviewError> {
    let root = crate::daily_raw::validate_raw_root(root, protected)
        .map_err(|error| PreviewError::Root(error.to_string()))?;
    if !root.is_dir() {
        return Err(PreviewError::Root(
            "dataset root does not exist or is not a directory".into(),
        ));
    }
    Ok(root)
}

/// Lists every committed Dataset in the root without choosing a latest version.
pub fn catalog(root: &Path) -> Result<Value, PreviewError> {
    let root = open_dataset_root(root)?;
    let directory = root.join("manifests");
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(json!({
                "schema_version": PREVIEW_SCHEMA_VERSION,
                "datasets": [],
                "invalid_manifests": []
            }));
        }
        Err(error) => return Err(PreviewError::Io(error.to_string())),
    };
    let mut datasets = Vec::new();
    let mut invalid_manifests = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|error| PreviewError::Io(error.to_string()))?
            .path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
            invalid_manifests.push(json!({
                "manifest": path.display().to_string(),
                "error": "manifest file name is not UTF-8"
            }));
            continue;
        };
        let dsv = format!("dsv:sha256:{stem}");
        let summary = match read_manifest(&root, &dsv) {
            Ok(manifest) => summarize(&root, &manifest).map_err(|error| error.to_string()),
            Err(error) => Err(error.to_string()),
        };
        match summary {
            Ok((created_at, summary)) => datasets.push((created_at, dsv, summary)),
            Err(error) => invalid_manifests.push(json!({
                "manifest": format!("{stem}.json"),
                "error": error
            })),
        }
    }
    datasets.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    invalid_manifests.sort_by(|left, right| {
        left["manifest"]
            .as_str()
            .unwrap_or_default()
            .cmp(right["manifest"].as_str().unwrap_or_default())
    });
    Ok(json!({
        "schema_version": PREVIEW_SCHEMA_VERSION,
        "datasets": datasets.into_iter().map(|(_, _, summary)| summary).collect::<Vec<_>>(),
        "invalid_manifests": invalid_manifests
    }))
}

fn summarize(root: &Path, manifest: &Manifest) -> Result<(String, Value), PreviewError> {
    for table in ["instruments", "sessions", "bars"] {
        if !manifest
            .core
            .tables
            .iter()
            .any(|candidate| candidate.table == table)
        {
            return Err(PreviewError::Table {
                table: table.into(),
                message: "manifest does not contain this required table".into(),
            });
        }
    }
    let sessions = read_sessions(root, manifest)?;
    let first = sessions.iter().map(Session::session_date).min();
    let last = sessions.iter().map(Session::session_date).max();
    let venues = sessions
        .iter()
        .map(|session| session.venue_id().as_str().to_owned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let created_at = manifest.provenance.created_at.clone();
    Ok((
        created_at.clone(),
        json!({
            "dsv": manifest.dsv,
            "created_at": created_at,
            "normalizer_id": manifest.core.normalizer.id,
            "normalizer_version": manifest.core.normalizer.version,
            "instruments": table_rows(manifest, "instruments"),
            "sessions": table_rows(manifest, "sessions"),
            "bars": table_rows(manifest, "bars"),
            "first_session": first.map(|date| date.to_string()),
            "last_session": last.map(|date| date.to_string()),
            "venues": venues,
            "source": coverage_text(&manifest.provenance.coverage, "source"),
            "calendar_version": coverage_text(&manifest.provenance.coverage, "calendar_version"),
            "security_snapshot": coverage_text(&manifest.provenance.coverage, "security_snapshot"),
            "coverage_notice": coverage_text(&manifest.provenance.coverage, "completeness_claim")
                .unwrap_or_else(|| "coverage is limited to this Dataset's committed inputs; missing quotes are not halts".into())
        }),
    ))
}

/// Reads one committed Dataset by its immutable DSV.
pub fn show(root: &Path, dsv: &str) -> Result<Value, PreviewError> {
    let root = open_dataset_root(root)?;
    let Core {
        manifest,
        instruments,
        sessions,
        bars,
    } = read_core(&root, dsv)?;
    let mut warnings = Vec::new();
    let evidence = load_evidence(&root, &manifest, &mut warnings);
    let sources = read_sources(&root, &manifest, &mut warnings)?;
    let venue_sessions = sessions_by_venue(&sessions);
    let instruments = instruments
        .iter()
        .map(|instrument| {
            preview_instrument(
                instrument,
                &venue_sessions,
                &bars,
                &sources,
                evidence.as_ref(),
                &mut warnings,
            )
        })
        .collect::<Vec<_>>();
    let mut venues = venue_sessions
        .iter()
        .map(|(venue, sessions)| {
            json!({
                "venue": venue,
                "sessions": sessions.len(),
                "first_session": sessions[0].session_date().to_string(),
                "last_session": sessions[sessions.len() - 1].session_date().to_string(),
                "first_open": sessions[0].ts_open().to_string(),
                "last_close": sessions[sessions.len() - 1].ts_close().to_string()
            })
        })
        .collect::<Vec<_>>();
    venues.sort_by(|left, right| left["venue"].as_str().cmp(&right["venue"].as_str()));
    Ok(json!({
        "schema_version": PREVIEW_SCHEMA_VERSION,
        "dsv": manifest.dsv,
        "created_at": manifest.provenance.created_at,
        "normalizer_id": manifest.core.normalizer.id,
        "normalizer_version": manifest.core.normalizer.version,
        "coverage": manifest.provenance.coverage,
        "venues": venues,
        "instruments": instruments,
        "warnings": warnings
    }))
}

/// Pages raw unadjusted Bars for one instrument of one committed Dataset.
pub fn bars(root: &Path, request: &BarsRequest) -> Result<Value, PreviewError> {
    let root = open_dataset_root(root)?;
    if request.limit == 0 {
        return Err(PreviewError::InvalidInput("limit must be positive".into()));
    }
    if let (Some(start), Some(end)) = (request.start, request.end) {
        if start > end {
            return Err(PreviewError::InvalidInput(
                "start must not be after end".into(),
            ));
        }
    }
    let Core {
        manifest,
        instruments,
        bars,
        ..
    } = read_core(&root, &request.dsv)?;
    let instrument = instruments
        .iter()
        .find(|instrument| instrument.id().to_string() == request.instrument_id)
        .ok_or_else(|| {
            PreviewError::NotFound(format!(
                "instrument {} is not part of this Dataset",
                request.instrument_id
            ))
        })?;
    let sources = read_sources(&root, &manifest, &mut Vec::new())?;
    let all = bars
        .iter()
        .filter(|bar| bar.instrument_id() == instrument.id())
        .collect::<Vec<_>>();
    let first = all.first().map(|bar| bar.session_date());
    let last = all.last().map(|bar| bar.session_date());
    let selected = all
        .iter()
        .filter(|bar| {
            request
                .start
                .is_none_or(|start| bar.session_date() >= start)
        })
        .filter(|bar| request.end.is_none_or(|end| bar.session_date() <= end))
        .copied()
        .collect::<Vec<_>>();
    let total = selected.len();
    if request.offset > total {
        return Err(PreviewError::InvalidInput(
            "offset exceeds the matching Bar count".into(),
        ));
    }
    let items = selected
        .iter()
        .skip(request.offset)
        .take(request.limit)
        .map(|bar| bar_item(bar, sources.get(&source_key(bar))))
        .collect::<Vec<_>>();
    let next_offset =
        (request.offset + items.len() < total).then_some(request.offset + items.len());
    let mut warnings = Vec::new();
    if items.iter().any(|item| item["source"].is_null()) {
        warnings.push("some Bars in this window have no per-Bar source record");
    }
    let (price_unit, volume_unit) = units(instrument.kind());
    Ok(json!({
        "schema_version": PREVIEW_SCHEMA_VERSION,
        "dsv": manifest.dsv,
        "normalizer_id": manifest.core.normalizer.id,
        "normalizer_version": manifest.core.normalizer.version,
        "instrument": {
            "instrument_id": instrument.id().to_string(),
            "venue": instrument.id().venue().as_str(),
            "kind": kind_label(instrument.kind()),
            "native_symbol": instrument.native_symbol()
        },
        "range": {
            "requested_start": request.start.map(|date| date.to_string()),
            "requested_end": request.end.map(|date| date.to_string()),
            "first_bar": first.map(|date| date.to_string()),
            "last_bar": last.map(|date| date.to_string())
        },
        "price_adjustment": "none",
        "price_unit": price_unit,
        "volume_unit": volume_unit,
        "amount_unit": "unknown; source has no amount field",
        "total": total,
        "offset": request.offset,
        "next_offset": next_offset,
        "items": items,
        "warnings": warnings
    }))
}

#[derive(Debug, Clone)]
pub struct BarsRequest {
    pub dsv: String,
    pub instrument_id: String,
    pub start: Option<NaiveDate>,
    pub end: Option<NaiveDate>,
    pub limit: usize,
    pub offset: usize,
}

struct Core {
    manifest: Manifest,
    instruments: Vec<InstrumentSpec>,
    sessions: Vec<Session>,
    bars: Vec<Bar>,
}

fn read_core(root: &Path, dsv: &str) -> Result<Core, PreviewError> {
    let dsv = normalize_dsv(dsv)?;
    let manifest = match read_manifest(root, &dsv) {
        Ok(manifest) => manifest,
        Err(crate::ManifestError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(PreviewError::NotFound(format!(
                "Dataset Version {dsv} does not exist in this root"
            )));
        }
        Err(error) => {
            return Err(PreviewError::Manifest {
                dsv,
                message: error.to_string(),
            });
        }
    };
    let instruments = read_table(root, &manifest, "instruments", &|batch| {
        instruments_from_record_batch(batch).map_err(|error| error.to_string())
    })?;
    let sessions = read_sessions(root, &manifest)?;
    let mut bars = read_table(root, &manifest, "bars", &|batch| {
        bars_from_record_batch(batch, &sessions).map_err(|error| error.to_string())
    })?;
    bars.sort_by(|left, right| {
        (left.instrument_id().to_string(), left.session_date())
            .cmp(&(right.instrument_id().to_string(), right.session_date()))
    });
    Ok(Core {
        manifest,
        instruments,
        sessions,
        bars,
    })
}

fn read_table<T>(
    root: &Path,
    manifest: &Manifest,
    table: &str,
    convert: &dyn Fn(&RecordBatch) -> Result<Vec<T>, String>,
) -> Result<Vec<T>, PreviewError> {
    if !manifest
        .core
        .tables
        .iter()
        .any(|candidate| candidate.table == table)
    {
        return Err(PreviewError::Table {
            table: table.into(),
            message: "manifest does not contain this table".into(),
        });
    }
    let batches =
        read_dataset_table(root, manifest, table).map_err(|error| PreviewError::Table {
            table: table.into(),
            message: error.to_string(),
        })?;
    let mut rows = Vec::new();
    for batch in &batches {
        rows.extend(convert(batch).map_err(|message| PreviewError::Table {
            table: table.into(),
            message,
        })?);
    }
    Ok(rows)
}

fn read_sessions(root: &Path, manifest: &Manifest) -> Result<Vec<Session>, PreviewError> {
    let mut sessions = read_table(root, manifest, "sessions", &|batch| {
        sessions_from_record_batch(batch).map_err(|error| error.to_string())
    })?;
    sessions.sort_by(|left, right| {
        (left.venue_id(), left.session_date()).cmp(&(right.venue_id(), right.session_date()))
    });
    Ok(sessions)
}

fn read_sources(
    root: &Path,
    manifest: &Manifest,
    warnings: &mut Vec<String>,
) -> Result<BTreeMap<(String, NaiveDate), BarSourceRow>, PreviewError> {
    let mut sources = BTreeMap::new();
    if !manifest
        .core
        .tables
        .iter()
        .any(|table| table.table == "bar_sources")
    {
        return Ok(sources);
    }
    let rows = read_table(root, manifest, "bar_sources", &|batch| {
        bar_sources_from_record_batch(batch).map_err(|error| error.to_string())
    })?;
    for row in rows {
        sources.insert((row.instrument_id.to_string(), row.session_date), row);
    }
    if sources.is_empty() {
        warnings.push("bar_sources table is empty; per-Bar source is unknown".into());
    }
    Ok(sources)
}

fn load_evidence(
    root: &Path,
    manifest: &Manifest,
    warnings: &mut Vec<String>,
) -> Option<SecurityEvidenceSnapshot> {
    let Some(snapshot_id) = coverage_text(&manifest.provenance.coverage, "security_snapshot")
    else {
        warnings.push("Dataset coverage does not identify frozen security-evidence lifecycle; listing and establishment stay unknown".into());
        return None;
    };
    if !manifest
        .core
        .inputs
        .iter()
        .any(|input| input.raw_sha256 == snapshot_id)
    {
        warnings.push(format!(
            "security-evidence snapshot {snapshot_id} is not one of the Dataset's committed inputs; lifecycle stays unknown"
        ));
        return None;
    }
    let bytes = match RawStore::open(root).and_then(|store| store.get(&snapshot_id)) {
        Ok(bytes) => bytes,
        Err(error) => {
            warnings.push(format!(
                "lifecycle evidence snapshot {snapshot_id} is unavailable: {error}"
            ));
            return None;
        }
    };
    match serde_json::from_slice::<SecurityEvidenceSnapshot>(&bytes) {
        Ok(snapshot) if snapshot.validate().is_ok() => Some(snapshot),
        Ok(_) => {
            warnings.push(format!(
                "lifecycle evidence snapshot {snapshot_id} is invalid"
            ));
            None
        }
        Err(error) => {
            warnings.push(format!(
                "lifecycle evidence snapshot {snapshot_id} is invalid: {error}"
            ));
            None
        }
    }
}

fn preview_instrument(
    instrument: &InstrumentSpec,
    venue_sessions: &BTreeMap<String, Vec<Session>>,
    bars: &[Bar],
    sources: &BTreeMap<(String, NaiveDate), BarSourceRow>,
    evidence: Option<&SecurityEvidenceSnapshot>,
    warnings: &mut Vec<String>,
) -> Value {
    let id = instrument.id().to_string();
    let venue = instrument.id().venue().as_str().to_owned();
    let sessions = venue_sessions
        .get(&venue)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let instrument_bars = bars
        .iter()
        .filter(|bar| bar.instrument_id() == instrument.id())
        .collect::<Vec<_>>();
    let first = instrument_bars.first().map(|bar| bar.session_date());
    let last = instrument_bars.last().map(|bar| bar.session_date());
    let observed = instrument_bars
        .iter()
        .map(|bar| bar.session_date())
        .collect::<BTreeSet<_>>();
    let in_range = |date: NaiveDate| match (first, last) {
        (Some(first), Some(last)) => date >= first && date <= last,
        _ => false,
    };
    let gaps = sessions
        .iter()
        .map(Session::session_date)
        .filter(|date| in_range(*date) && !observed.contains(date))
        .collect::<Vec<_>>();
    let before = first.map_or(0, |first| {
        sessions
            .iter()
            .filter(|session| session.session_date() < first)
            .count()
    });
    let after = last.map_or(0, |last| {
        sessions
            .iter()
            .filter(|session| session.session_date() > last)
            .count()
    });
    let matching_sources = sources
        .iter()
        .filter(|((instrument_id, _), _)| instrument_id == &id)
        .map(|(_, source)| source)
        .collect::<Vec<_>>();
    let source_names = matching_sources
        .iter()
        .map(|source| source.source.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let bars_without_source = instrument_bars
        .iter()
        .filter(|bar| {
            !matching_sources
                .iter()
                .any(|source| source.session_date == bar.session_date())
        })
        .count();
    if !gaps.is_empty() {
        warnings.push(format!(
            "{id}: {} Sessions in the observed Bar range have no Bar; this is not inferred as a halt",
            gaps.len()
        ));
    }
    if instrument_bars.is_empty() {
        warnings.push(format!(
            "{id}: no Bars are committed; Session coverage is not tradable coverage"
        ));
    }
    if bars_without_source > 0 {
        warnings.push(format!(
            "{id}: {bars_without_source} Bars have no bar_sources record"
        ));
    }
    json!({
        "instrument_id": id,
        "venue": venue,
        "kind": kind_label(instrument.kind()),
        "native_symbol": instrument.native_symbol(),
        "base_currency": instrument.base_currency().map(|value| value.as_str()),
        "quote_currency": instrument.quote_currency().map(|value| value.as_str()),
        "session_timezone": instrument.session_timezone(),
        "lot_size": instrument.lot_size().map(|value| value.to_string()),
        "price_increment": instrument.price_increment().to_string(),
        "size_increment": instrument.size_increment().to_string(),
        "lifecycle": lifecycle_preview(evidence, instrument, warnings),
        "coverage": {
            "bars": instrument_bars.len(),
            "first_bar": first.map(|date| date.to_string()),
            "last_bar": last.map(|date| date.to_string()),
            "venue_sessions": sessions.len(),
            "sessions_in_bar_range": sessions.iter().filter(|session| in_range(session.session_date())).count(),
            "sessions_without_bar_in_observed_range": gaps.len(),
            "session_gaps": gaps.iter().take(MAX_GAP_DATES).map(|date| date.to_string()).collect::<Vec<_>>(),
            "session_gaps_truncated": gaps.len() > MAX_GAP_DATES,
            "sessions_before_first_bar": before,
            "sessions_after_last_bar": after,
            "sources": source_names,
            "bars_without_source": bars_without_source
        }
    })
}

fn lifecycle_preview(
    evidence: Option<&SecurityEvidenceSnapshot>,
    instrument: &InstrumentSpec,
    warnings: &mut Vec<String>,
) -> Value {
    let unknown = |reason: &str| {
        json!({
            "evidence_version": null,
            "publication": null,
            "established_date": {"state": "unknown", "value": null, "reason": reason, "source_id": null, "observed_at": null, "candidates": []},
            "listing_date": {"state": "unknown", "value": null, "reason": reason, "source_id": null, "observed_at": null, "candidates": []}
        })
    };
    let Some(snapshot) = evidence else {
        return unknown("no frozen security-evidence snapshot");
    };
    let Some(selected) = snapshot
        .instruments
        .iter()
        .find(|selected| selected.record.instrument_id == *instrument.id())
    else {
        warnings.push(format!(
            "{}: instrument is absent from the frozen security-evidence snapshot",
            instrument.id()
        ));
        return unknown("instrument is absent from the frozen snapshot");
    };
    json!({
        "evidence_version": selected.evidence_version.as_str(),
        "publication": match &selected.publication {
            PublicationStatus::Publishable { .. } => "publishable",
            PublicationStatus::NotPublishable { .. } => "not_publishable"
        },
        "established_date": lifecycle_field(&selected.record.established_date),
        "listing_date": lifecycle_field(&selected.record.listing_date)
    })
}

fn lifecycle_field<T: ToString>(field: &FieldEvidence<T>) -> Value {
    match field {
        FieldEvidence::Known { value, source } => json!({
            "state": "known",
            "value": value.to_string(),
            "reason": null,
            "source_id": source.source_id,
            "observed_at": source.observed_at,
            "candidates": []
        }),
        FieldEvidence::Unknown { reason, source } => json!({
            "state": "unknown",
            "value": null,
            "reason": reason,
            "source_id": source.source_id,
            "observed_at": source.observed_at,
            "candidates": []
        }),
        FieldEvidence::NotApplicable { reason, source } => json!({
            "state": "not_applicable",
            "value": null,
            "reason": reason,
            "source_id": source.source_id,
            "observed_at": source.observed_at,
            "candidates": []
        }),
        FieldEvidence::Conflict { candidates } => json!({
            "state": "conflict",
            "value": null,
            "reason": "sources disagree; no single value is covered",
            "source_id": null,
            "observed_at": null,
            "candidates": candidates.iter().map(|candidate| candidate.value.to_string()).collect::<Vec<_>>()
        }),
    }
}

fn bar_item(bar: &Bar, source: Option<&BarSourceRow>) -> Value {
    json!({
        "trade_date": bar.session_date().to_string(),
        "open": bar.open().to_string(),
        "high": bar.high().to_string(),
        "low": bar.low().to_string(),
        "close": bar.close().to_string(),
        "volume": bar.volume().to_string(),
        "amount": bar.amount().map(|value| value.to_string()),
        "available_at": bar.available_at().map(|value| value.to_string()),
        "source": source.map(|source| source.source.clone()),
        "source_raw_sha256": source.map(|source| source.source_raw_sha256.clone()),
        "reference_price": source.and_then(|source| source.reference_price).map(|value| value.to_string())
    })
}

fn source_key(bar: &Bar) -> (String, NaiveDate) {
    (bar.instrument_id().to_string(), bar.session_date())
}

fn sessions_by_venue(sessions: &[Session]) -> BTreeMap<String, Vec<Session>> {
    let mut map: BTreeMap<String, Vec<Session>> = BTreeMap::new();
    for session in sessions {
        map.entry(session.venue_id().as_str().to_owned())
            .or_default()
            .push(session.clone());
    }
    map
}

fn table_rows(manifest: &Manifest, table: &str) -> u64 {
    manifest
        .core
        .tables
        .iter()
        .find(|candidate| candidate.table == table)
        .map_or(0, |candidate| candidate.row_count)
}

fn coverage_text(coverage: &Value, key: &str) -> Option<String> {
    coverage.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn units(kind: InstrumentKind) -> (&'static str, &'static str) {
    match kind {
        InstrumentKind::Equity => ("CNY per share; raw unadjusted", "shares"),
        InstrumentKind::Etf => ("CNY per fund unit; raw unadjusted", "fund units"),
        _ => ("source unit unknown", "source unit unknown"),
    }
}

fn kind_label(kind: InstrumentKind) -> &'static str {
    match kind {
        InstrumentKind::Equity => "equity",
        InstrumentKind::Etf => "etf",
        InstrumentKind::Spot => "spot",
        InstrumentKind::Perpetual => "perpetual",
        InstrumentKind::Future => "future",
    }
}

fn normalize_dsv(input: &str) -> Result<String, PreviewError> {
    let hex = input.strip_prefix("dsv:sha256:").unwrap_or(input);
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(PreviewError::InvalidInput(
            "dataset version must be a dsv:sha256:<64 lowercase hex> identity".into(),
        ));
    }
    Ok(format!("dsv:sha256:{hex}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn rejects_uppercase_or_short_dataset_versions() {
        for value in ["", "abc", "dsv:sha256:", &"A".repeat(64)] {
            let error = normalize_dsv(value).unwrap_err();
            assert_eq!(error.code(), "invalid_request");
        }
        assert_eq!(
            normalize_dsv(&"a".repeat(64)).unwrap(),
            format!("dsv:sha256:{}", "a".repeat(64))
        );
    }

    #[test]
    fn rejects_missing_root_and_mvp6_archive_without_creating_directories() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing");
        let error =
            open_dataset_root_with_protected(&missing, &temp.path().join("archive")).unwrap_err();
        assert_eq!(error.code(), "invalid_dataset_root");
        assert!(!missing.exists());

        let archive = temp.path().join("archive");
        fs::create_dir_all(&archive).unwrap();
        let inside = archive.join("datasets");
        fs::create_dir_all(&inside).unwrap();
        let error = open_dataset_root_with_protected(&inside, &archive).unwrap_err();
        assert_eq!(error.code(), "invalid_dataset_root");
    }

    #[test]
    fn empty_manifests_directory_is_an_explicit_empty_catalog() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("lake");
        fs::create_dir_all(root.join("manifests")).unwrap();
        let catalog = catalog(&root).unwrap();
        assert!(catalog["datasets"].as_array().unwrap().is_empty());
        assert!(catalog["invalid_manifests"].as_array().unwrap().is_empty());
    }
}
