//! Local production assembly for one explicitly selected security per Dataset.
use std::{collections::BTreeSet, error::Error, fs, path::Path};

use chrono::{NaiveDate, Utc};
use prajna_domain::InstrumentKind;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    Manifest, NormalizerRegistry, RawStore, SecurityEvidenceSnapshot, SecurityEvidenceStore,
    SecuritySnapshotId, SourceKind, SourceRecordInput,
    daily_raw::{
        ChunkStatus, DailyRawReport, METHOD_VERSION, RawResponseOutcome, SecurityKind,
        checked_output_path, validate_raw_root,
    },
    default_mvp6_root, publish_dataset, read_manifest, rebuild_dataset,
    session_calendar::{CalendarInput, CalendarStore, CalendarVersionId},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// All stores and versions are operator-selected; paths are never used on rebuild.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DailyPublicationSelection {
    pub raw_root: String,
    pub calendar_root: String,
    pub evidence_root: String,
    pub report: String,
    pub normalizer_id: String,
    pub normalizer_version: String,
    pub calendar_version: String,
    pub security_snapshot: String,
    pub raw_sha256s: Vec<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Recipe {
    schema_version: u32,
    normalizer_id: String,
    normalizer_version: String,
    calendar_version: String,
    security_snapshot: String,
    report_sha256: String,
    raw_sha256s: Vec<String>,
}

/// Local receipt binds the frozen production inputs to the Dataset commit marker.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DailyPublicationReceipt {
    pub recipe_sha256: String,
    pub dsv: String,
}

fn require(condition: bool, reason: &'static str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(std::io::Error::other(reason).into())
    }
}

fn registry(recipe: &Recipe) -> Result<NormalizerRegistry> {
    require(
        recipe.schema_version == 1,
        "unsupported publication recipe schema",
    )?;
    require(
        recipe.normalizer_id == "cn-stock-etf-daily" && recipe.normalizer_version == "1",
        "unsupported daily Normalizer identity or unit rule",
    )?;
    Ok(NormalizerRegistry::with_cn_stock_etf_daily_v1(
        CalendarVersionId::parse(&recipe.calendar_version)?,
        SecuritySnapshotId::parse(&recipe.security_snapshot)?,
    )?)
}

fn put(store: &RawStore, bytes: &[u8], source_id: &str) -> Result<String> {
    Ok(store.put(
        bytes,
        SourceRecordInput {
            content_type: "application/json".into(),
            source_kind: SourceKind::FileImport,
            source_id: source_id.into(),
            request: json!({}),
            observed_at: Utc::now().to_rfc3339(),
            ingested_by: "prajna-publish-daily".into(),
        },
    )?)
}

fn validate(
    report: &DailyRawReport,
    recipe: &Recipe,
    calendar: &CalendarInput,
    snapshot: &SecurityEvidenceSnapshot,
    raw: &RawStore,
) -> Result<()> {
    require(
        report.schema_version == 1
            && report.method_version == METHOD_VERSION
            && report.source == "Tencent FQKline"
            && report.availability == "unknown",
        "unsupported collection report source, version or availability",
    )?;
    require(
        report.requested_range.start <= report.requested_range.end,
        "invalid report date range",
    )?;
    require(
        snapshot.instruments.len() == 1,
        "select exactly one security per publication",
    )?;
    let security = &snapshot.instruments[0];
    let id = &security.record.instrument_id;
    let prefix = match id.venue().as_str() {
        "XSHG" => "sh",
        "XSHE" => "sz",
        _ => "",
    };
    require(
        !prefix.is_empty()
            && report.instrument_id == id.to_string()
            && report.native_symbol == format!("{prefix}{}", id.symbol())
            && matches!(
                (report.security_kind, security.record.kind),
                (SecurityKind::Stock, InstrumentKind::Equity)
                    | (SecurityKind::Etf, InstrumentKind::Etf)
            ),
        "report and security snapshot identity, venue or kind mismatch",
    )?;
    require(
        matches!(
            security.publication,
            crate::PublicationStatus::Publishable { .. }
        ),
        "selected security has unknown, conflicting or invalid execution terms",
    )?;
    require(
        calendar.venues.iter().any(|venue| {
            venue.venue_id == *id.venue()
                && venue.coverage_start <= report.requested_range.start
                && venue.coverage_end >= report.requested_range.end
        }),
        "calendar version does not cover the requested venue/date range",
    )?;
    require(
        !recipe.raw_sha256s.is_empty(),
        "explicit accepted Raw selection is required",
    )?;
    let mut seen = BTreeSet::new();
    for hash in &recipe.raw_sha256s {
        require(seen.insert(hash), "duplicate selected Raw identity")?;
        let mut selected = None;
        for chunk in &report.chunks {
            for reference in &chunk.raw_objects {
                if reference.raw_sha256 == *hash
                    && reference.outcome == RawResponseOutcome::AcceptedResponse
                    && chunk.status == ChunkStatus::Complete
                {
                    selected = Some((chunk, reference));
                }
            }
        }
        let (chunk, reference) = selected.ok_or("Raw is not an accepted nonempty report chunk")?;
        require(
            chunk.requested_range.start >= report.requested_range.start
                && chunk.requested_range.end <= report.requested_range.end
                && chunk.requested_range.start <= chunk.requested_range.end,
            "chunk date range is inconsistent with the report",
        )?;
        require(
            raw.sources(hash)?.iter().any(|source| {
                source.source_kind == SourceKind::VendorApi
                    && source.source_id == "tencent_fqkline_daily"
                    && source.request["method_version"] == METHOD_VERSION
                    && source.request["instrument_id"] == report.instrument_id
                    && source.request["native_symbol"] == report.native_symbol
                    && source.request["security_kind"]
                        == serde_json::to_value(report.security_kind).unwrap()
                    && source.request["requested_range"]
                        == serde_json::to_value(chunk.requested_range).unwrap()
                    && source.request["chunk_index"] == chunk.index
                    && source.request["source"] == reference.source
                    && source.request["response_outcome"] == "accepted_response"
            }),
            "selected Raw lacks the exact accepted report source record",
        )?;
        let body: Value = serde_json::from_slice(&raw.get(hash)?)?;
        let data = body["data"]
            .as_object()
            .ok_or("missing daily response data")?;
        require(
            body["code"] == 0 && data.len() == 1 && data.contains_key(&report.native_symbol),
            "Raw response security or status mismatch",
        )?;
        let rows = data[&report.native_symbol]["day"]
            .as_array()
            .ok_or("missing day rows")?;
        require(
            !rows.is_empty(),
            "empty response cannot advance a publication",
        )?;
        let mut dates = BTreeSet::new();
        for row in rows {
            let date =
                NaiveDate::parse_from_str(row[0].as_str().ok_or("missing row date")?, "%Y-%m-%d")?;
            require(
                (chunk.requested_range.start..=chunk.requested_range.end).contains(&date),
                "response date is outside the selected request range",
            )?;
            if let crate::FieldEvidence::Known { value, .. } = &security.record.listing_date {
                require(
                    date >= *value,
                    "pre-listing row cannot be published as trading history",
                )?;
            }
            dates.insert(date);
        }
        require(
            dates.into_iter().collect::<Vec<_>>() == chunk.actual_response_dates,
            "Raw dates disagree with the selected collection report",
        )?;
    }
    Ok(())
}

/// Publish explicit #288/#328/#329 inputs, freezing report and recipe in the output lake.
/// Receipt names are root-relative. Detailed errors must remain in local diagnostics.
pub fn publish_daily(
    root: &Path,
    selection: DailyPublicationSelection,
    receipt_name: &Path,
) -> Result<Manifest> {
    let root = validate_raw_root(root, &default_mvp6_root())?;
    fs::create_dir_all(&root)?;
    let receipt_path = checked_output_path(&root, receipt_name)?;
    let existing: Option<DailyPublicationReceipt> = if receipt_path.exists() {
        Some(serde_json::from_slice(&fs::read(&receipt_path)?)?)
    } else {
        None
    };
    let parent = receipt_path.parent().ok_or("invalid receipt path")?;
    fs::create_dir_all(parent)?;
    let mut receipt_file = tempfile::NamedTempFile::new_in(parent)?;
    let source = RawStore::open(selection.raw_root)?;
    let report_bytes = fs::read(selection.report)?;
    let report: DailyRawReport = serde_json::from_slice(&report_bytes)?;
    let calendar_id = CalendarVersionId::parse(&selection.calendar_version)?;
    let snapshot_id = SecuritySnapshotId::parse(&selection.security_snapshot)?;
    let calendar = CalendarStore::open(selection.calendar_root)?.get(&calendar_id)?;
    let snapshot =
        SecurityEvidenceStore::open(selection.evidence_root)?.get_snapshot(&snapshot_id)?;
    let mut recipe = Recipe {
        schema_version: 1,
        normalizer_id: selection.normalizer_id,
        normalizer_version: selection.normalizer_version,
        calendar_version: selection.calendar_version,
        security_snapshot: selection.security_snapshot,
        report_sha256: String::new(),
        raw_sha256s: selection.raw_sha256s,
    };
    recipe.raw_sha256s.sort();
    let registry = registry(&recipe)?;
    validate(&report, &recipe, &calendar, &snapshot, &source)?;
    // Reject symlink destinations before writing through the existing Raw/publisher APIs.
    for relative in ["raw", "raw-sources", "normalized", "manifests"] {
        check_tree(&checked_output_path(&root, Path::new(relative))?)?;
    }
    let target = RawStore::open(&root)?;
    for hash in &recipe.raw_sha256s {
        for record in source.sources(hash)? {
            target.put(
                &source.get(hash)?,
                SourceRecordInput {
                    content_type: record.content_type,
                    source_kind: record.source_kind,
                    source_id: record.source_id,
                    request: record.request,
                    observed_at: record.observed_at,
                    ingested_by: record.ingested_by,
                },
            )?;
        }
    }
    require(
        put(
            &target,
            &serde_json::to_vec(&calendar)?,
            "pinned-session-calendar",
        )? == recipe.calendar_version,
        "calendar bytes/version mismatch",
    )?;
    require(
        put(
            &target,
            &serde_json::to_vec(&snapshot)?,
            "pinned-security-snapshot",
        )? == recipe.security_snapshot,
        "security snapshot bytes/version mismatch",
    )?;
    recipe.report_sha256 = put(&target, &report_bytes, "pinned-daily-report")?;
    let recipe_sha256 = put(
        &target,
        &serde_json::to_vec(&recipe)?,
        "daily-publication-recipe",
    )?;
    if let Some(existing) = &existing {
        require(
            existing.recipe_sha256 == recipe_sha256,
            "receipt already names different fixed inputs",
        )?;
    }
    let mut inputs = recipe.raw_sha256s.clone();
    inputs.extend([recipe.calendar_version, recipe.security_snapshot]);
    let manifest = publish_dataset(
        &root,
        &target,
        &registry,
        &recipe.normalizer_id,
        &recipe.normalizer_version,
        &inputs,
        Utc::now().to_rfc3339(),
    )
    .map_err(|error| format!("{error:?}"))?;
    if let Some(existing) = existing {
        require(
            existing.dsv == manifest.dsv,
            "receipt Dataset identity mismatch",
        )?;
        return Ok(manifest);
    }
    serde_json::to_writer(
        &mut receipt_file,
        &DailyPublicationReceipt {
            recipe_sha256,
            dsv: manifest.dsv.clone(),
        },
    )?;
    receipt_file.as_file().sync_all()?;
    receipt_file.persist_noclobber(receipt_path)?;
    Ok(manifest)
}

fn check_tree(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            require(
                !metadata.file_type().is_symlink(),
                "publication destination contains a symlink",
            )?;
            if metadata.is_dir() {
                for entry in fs::read_dir(path)? {
                    check_tree(&entry?.path())?;
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

/// Rebuild against the committed DSV using only frozen local inputs, never source stores.
pub fn rebuild_daily(root: &Path, receipt_name: &Path) -> Result<()> {
    let root = validate_raw_root(root, &default_mvp6_root())?;
    let receipt: DailyPublicationReceipt =
        serde_json::from_slice(&fs::read(checked_output_path(&root, receipt_name)?)?)?;
    let raw = RawStore::open(&root)?;
    let recipe: Recipe = serde_json::from_slice(&raw.get(&receipt.recipe_sha256)?)?;
    let registry = registry(&recipe)?;
    let report: DailyRawReport = serde_json::from_slice(&raw.get(&recipe.report_sha256)?)?;
    let calendar = CalendarInput::parse_and_validate(&raw.get(&recipe.calendar_version)?)?;
    let snapshot: SecurityEvidenceSnapshot =
        serde_json::from_slice(&raw.get(&recipe.security_snapshot)?)?;
    snapshot.validate()?;
    validate(&report, &recipe, &calendar, &snapshot, &raw)?;
    let manifest = read_manifest(&root, &receipt.dsv)?;
    let mut inputs = recipe.raw_sha256s.clone();
    inputs.extend([
        recipe.calendar_version.clone(),
        recipe.security_snapshot.clone(),
    ]);
    inputs.sort();
    let mut actual = manifest
        .core
        .inputs
        .iter()
        .map(|input| input.raw_sha256.clone())
        .collect::<Vec<_>>();
    actual.sort();
    require(
        actual == inputs,
        "receipt recipe does not match Dataset inputs",
    )?;
    rebuild_dataset(&raw, &registry, &manifest)?;
    Ok(())
}
