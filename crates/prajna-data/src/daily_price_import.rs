//! Local ETF daily research import through the production Raw, Dataset and adjustment stores.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use chrono::NaiveDate;
use prajna_domain::{InstrumentId, InstrumentKind, Price, Quantity};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tencent_transport::{
    DailyAdjustment, HttpReply, HttpRequest, HttpTransport, ReqwestTransport, TransportFailure,
    TransportFailureKind,
};

use crate::{
    PublicationStatus, RawStore, SecurityEvidenceStore, SecuritySnapshotId,
    adjusted_series::{self, AdjustedInput, AdjustedRow},
    daily_publish::{self, DailyPublicationReceipt, DailyPublicationSelection},
    daily_raw::{
        ChunkStatus, CollectionRun, DailyRawCollector, DailyRawRequest, DailyRawTiming,
        FORWARD_METHOD_VERSION, RawResponseOutcome, STOCK_DATA_REVISION, SecurityKind, Venue,
        checked_output_path, response_rows, validate_raw_root,
    },
    pool_publish::{self, PoolSelection},
    session_calendar::{CalendarInput, CalendarSource, CalendarStore, CalendarVersionId},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    calendar_root: PathBuf,
    calendar_version: String,
    evidence_root: PathBuf,
    start: NaiveDate,
    end: NaiveDate,
    /// Fixed, single-security snapshots from the production security evidence store.
    members: BTreeMap<String, String>,
}

struct Member {
    request: DailyRawRequest,
    native: String,
    snapshot: String,
    sessions: BTreeSet<NaiveDate>,
}

pub enum Acquisition<'a> {
    Fetch,
    Replay(&'a Path),
}

struct Replay(PathBuf);

impl HttpTransport for Replay {
    fn get(&mut self, request: &HttpRequest) -> std::result::Result<HttpReply, TransportFailure> {
        // Replay is selected explicitly, and no network client exists in this branch.
        let parts = request.parameter.split(',').collect::<Vec<_>>();
        let [symbol, "day", start, end, "640", adjust] = parts.as_slice() else {
            return Err(TransportFailure::new(
                TransportFailureKind::ClientBuild,
                "invalid daily replay request",
            ));
        };
        let series = if adjust.is_empty() { "raw" } else { adjust };
        let file = format!("{symbol}-{series}-{start}-{end}.json");
        let path = checked_output_path(&self.0, Path::new(&file)).map_err(|error| {
            TransportFailure::new(TransportFailureKind::ClientBuild, error.to_string())
        })?;
        let bytes = fs::read(path).map_err(|error| {
            TransportFailure::new(
                TransportFailureKind::ClientBuild,
                format!("{file}: {error}"),
            )
        })?;
        Ok(HttpReply {
            source: "local-replay".into(),
            bytes,
        })
    }
}

#[derive(Default)]
struct ReplayClock(Mutex<Duration>);

impl DailyRawTiming for ReplayClock {
    fn now(&self) -> Duration {
        *self.0.lock().expect("replay clock lock")
    }

    fn sleep(&self, duration: Duration) {
        *self.0.lock().expect("replay clock lock") += duration;
    }
}

fn members(config: &Config, calendar: &CalendarInput) -> Result<BTreeMap<String, Member>> {
    if config.start > config.end || config.members.is_empty() {
        return Err("an inclusive date range and at least one ETF are required".into());
    }
    let evidence = SecurityEvidenceStore::open(&config.evidence_root)?;
    config
        .members
        .iter()
        .map(|(id, snapshot_id)| {
            let instrument: InstrumentId = id.parse()?;
            let venue = match instrument.venue().as_str() {
                "XSHG" => Venue::Xshg,
                "XSHE" => Venue::Xshe,
                _ => return Err("daily price import supports XSHG and XSHE ETFs".into()),
            };
            let snapshot = evidence.get_snapshot(&SecuritySnapshotId::parse(snapshot_id)?)?;
            let [security] = snapshot.instruments.as_slice() else {
                return Err("each member needs a single-security snapshot".into());
            };
            if security.record.instrument_id != instrument
                || security.record.kind != InstrumentKind::Etf
                || !matches!(security.publication, PublicationStatus::Publishable { .. })
            {
                return Err(
                    "member snapshot must identify the requested ETF and execution terms".into(),
                );
            }
            let venue_calendar = calendar
                .venues
                .iter()
                .find(|entry| &entry.venue_id == instrument.venue())
                .filter(|entry| {
                    entry.coverage_start <= config.start && entry.coverage_end >= config.end
                })
                .ok_or("pinned calendar does not cover the requested Venue and dates")?;
            let sessions = venue_calendar
                .sessions
                .iter()
                .filter(|entry| (config.start..=config.end).contains(&entry.session_date))
                .map(|entry| entry.session_date)
                .collect::<BTreeSet<_>>();
            if sessions.is_empty() {
                return Err("requested date range contains no trading Sessions".into());
            }
            let request = DailyRawRequest {
                symbol: instrument.symbol().to_owned(),
                venue,
                kind: SecurityKind::Etf,
                start: config.start,
                end: config.end,
            };
            let (canonical, native) = request.identity()?;
            if canonical != *id {
                return Err("member identity must be in canonical symbol.Venue format".into());
            }
            Ok((
                id.clone(),
                Member {
                    request,
                    native,
                    snapshot: snapshot_id.clone(),
                    sessions,
                },
            ))
        })
        .collect()
}

fn clip_calendar(input: &Path, config: &Config) -> Result<(CalendarInput, CalendarVersionId)> {
    if config.start > config.end {
        return Err("invalid calendar clip range".into());
    }
    let mut calendar = CalendarStore::open(&config.calendar_root)?
        .get(&CalendarVersionId::parse(&config.calendar_version)?)?;
    for venue in &mut calendar.venues {
        if venue.coverage_start > config.start || venue.coverage_end < config.end {
            return Err("pinned calendar must cover the requested range for XSHG and XSHE".into());
        }
        venue.coverage_start = config.start;
        venue.coverage_end = config.end;
        venue
            .sessions
            .retain(|session| (config.start..=config.end).contains(&session.session_date));
        venue.source.source_ref = format!(
            "{} (clipped from {})",
            venue.source.source_ref, config.calendar_version
        );
    }
    let version = CalendarStore::open(input)?.import_json(&serde_json::to_vec(&calendar)?)?;
    Ok((calendar, version))
}

struct Series {
    run: CollectionRun,
    rows: BTreeMap<NaiveDate, AdjustedRow>,
    hashes: Vec<String>,
    fallback_chunks: usize,
    observed_at: String,
}

fn decimal_text(value: &Value) -> Result<String> {
    match value {
        Value::String(text) => Ok(text.clone()),
        Value::Number(number) => Ok(number.to_string()),
        _ => Err("daily price and volume fields must be exact decimals".into()),
    }
}

fn series(
    raw: &RawStore,
    run: CollectionRun,
    member: &Member,
    adjustment: DailyAdjustment,
) -> Result<Series> {
    run.report.validate_consistency()?;
    let mut rows = BTreeMap::new();
    let mut hashes = Vec::new();
    let mut fallback_chunks = 0;
    let mut observed_at = None;
    for chunk in &run.report.chunks {
        if !matches!(chunk.status, ChunkStatus::Complete | ChunkStatus::Empty) {
            return Err(
                "source did not return a usable response for every range; see collection report"
                    .into(),
            );
        }
        let response = chunk
            .raw_objects
            .iter()
            .find(|entry| entry.outcome == RawResponseOutcome::AcceptedResponse)
            .ok_or("accepted response missing")?;
        if observed_at.is_none() {
            let request_range = serde_json::to_value(chunk.requested_range)?;
            observed_at = raw
                .sources(&response.raw_sha256)?
                .into_iter()
                .find(|record| {
                    record.request["method_version"] == run.report.method_version
                        && record.request["instrument_id"] == run.report.instrument_id
                        && record.request["a_stock_data_revision"] == STOCK_DATA_REVISION
                        && record.request["requested_range"] == request_range
                        && record.request["response_outcome"] == "accepted_response"
                })
                .map(|record| record.observed_at);
            if observed_at.is_none() {
                return Err("accepted response provenance is missing".into());
            }
        }
        let bytes = raw.get(&response.raw_sha256)?;
        let values = response_rows(&bytes, &member.native, adjustment)?;
        if adjustment == DailyAdjustment::Forward
            && serde_json::from_slice::<Value>(&bytes)?["data"][&member.native]
                .get("qfqday")
                .is_none()
        {
            fallback_chunks += 1;
        }
        hashes.push(response.raw_sha256.clone());
        for value in values {
            let cells = value
                .as_array()
                .filter(|cells| cells.len() >= 6)
                .ok_or("daily row is missing required fields")?;
            let date = NaiveDate::parse_from_str(
                cells[0].as_str().ok_or("daily row date must be a string")?,
                "%Y-%m-%d",
            )?;
            if !(chunk.requested_range.start..=chunk.requested_range.end).contains(&date)
                || !member.sessions.contains(&date)
            {
                return Err("daily row date is outside its requested Session range".into());
            }
            let open = decimal_text(&cells[1])?;
            let close = decimal_text(&cells[2])?;
            let high = decimal_text(&cells[3])?;
            let low = decimal_text(&cells[4])?;
            let (o, c, h, l) = (
                Price::parse(&open)?,
                Price::parse(&close)?,
                Price::parse(&high)?,
                Price::parse(&low)?,
            );
            if l <= Price::ZERO || l > o || l > c || h < o || h < c {
                return Err("daily OHLC must be positive and ordered".into());
            }
            if Quantity::parse(&decimal_text(&cells[5])?)? < Quantity::ZERO {
                return Err("daily volume in vendor lots must be non-negative".into());
            }
            let row = AdjustedRow {
                trade_date: date,
                open,
                high,
                low,
                close,
            };
            if rows.insert(date, row).is_some() {
                return Err("daily response contains a duplicate Session".into());
            }
        }
    }
    Ok(Series {
        run,
        rows,
        hashes,
        fallback_chunks,
        observed_at: observed_at.ok_or("source returned no request ranges")?,
    })
}

fn key(value: &impl Serialize) -> Result<String> {
    Ok(crate::sha256(&serde_json::to_vec(value)?))
}

fn persist(path: &Path, value: &impl Serialize, replace: bool) -> Result<()> {
    persist_bytes(path, &serde_json::to_vec(value)?, replace)
}

fn persist_bytes(path: &Path, bytes: &[u8], replace: bool) -> Result<()> {
    let parent = path.parent().ok_or("invalid local output path")?;
    fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    if replace {
        file.persist(path)?;
    } else {
        match file.persist_noclobber(path) {
            Ok(_) => {}
            Err(error)
                if error.error.kind() == io::ErrorKind::AlreadyExists
                    && bytes == fs::read(path)? => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn publish_member(
    lake: &Path,
    input: &Path,
    config: &Config,
    calendar_version: &CalendarVersionId,
    member: &Member,
    unadjusted: &Series,
) -> Result<DailyPublicationReceipt> {
    let mut hashes = unadjusted
        .run
        .report
        .chunks
        .iter()
        .filter(|chunk| chunk.status == ChunkStatus::Complete)
        .flat_map(|chunk| &chunk.raw_objects)
        .filter(|entry| entry.outcome == RawResponseOutcome::AcceptedResponse)
        .map(|entry| entry.raw_sha256.clone())
        .collect::<Vec<_>>();
    if hashes.is_empty() {
        return Err("no unadjusted daily Bars were returned for a requested ETF".into());
    }
    hashes.sort();
    let receipt = PathBuf::from(format!(
        "daily-price/member-receipts/{}.json",
        key(&(
            STOCK_DATA_REVISION,
            calendar_version.as_str(),
            &member.snapshot,
            &unadjusted.run.report.instrument_id,
            config.start,
            config.end,
            &unadjusted.hashes,
        ))?
    ));
    let path = checked_output_path(lake, &receipt)?;
    if path.exists() {
        let receipt = serde_json::from_slice(&fs::read(path)?)?;
        let (recipe, _) = daily_publish::verify_receipt(lake, &receipt)?;
        if recipe.raw_sha256s != hashes
            || recipe.calendar_version != calendar_version.as_str()
            || recipe.security_snapshot != member.snapshot
            || recipe.normalizer_version != "1"
            || recipe.status_snapshot.is_some()
        {
            return Err("cached member receipt does not match the selected inputs".into());
        }
        return Ok(receipt);
    }
    daily_publish::publish_daily(
        lake,
        DailyPublicationSelection {
            raw_root: input.to_string_lossy().into_owned(),
            calendar_root: input.to_string_lossy().into_owned(),
            evidence_root: config.evidence_root.to_string_lossy().into_owned(),
            report: unadjusted.run.report_path.to_string_lossy().into_owned(),
            normalizer_id: "cn-stock-etf-daily".into(),
            normalizer_version: "1".into(),
            calendar_version: calendar_version.as_str().into(),
            security_snapshot: member.snapshot.clone(),
            raw_sha256s: hashes,
            status_snapshot: None,
        },
        &receipt,
    )?;
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn import_adjustment(
    lake: &Path,
    input: &Path,
    dsv: &str,
    member: &Member,
    adjusted: &Series,
    raw_rows: &BTreeMap<NaiveDate, AdjustedRow>,
) -> Result<String> {
    let id = &adjusted.run.report.instrument_id;
    let request = AdjustedInput {
            schema_version: 1,
            raw_dsv: dsv.into(),
            instrument_id: id.into(),
            method: "forward".into(),
            method_version: FORWARD_METHOD_VERSION.into(),
            source: CalendarSource {
                source_id: "Tencent FQKline".into(),
                source_version: STOCK_DATA_REVISION.into(),
                source_ref: adjusted.hashes.join(","),
            },
            observed_at: adjusted.observed_at.clone(),
            retrospective_assumption: "Supplier qfq snapshot may incorporate later ex-date adjustments; price research only.".into(),
            close_availability_assumption: "Daily model assumes the session's adjusted close is available for the close signal; historical availability is unknown.".into(),
            start: *member.sessions.first().expect("preflight requires Sessions"),
            end: *member.sessions.last().expect("preflight requires Sessions"),
            rows: adjusted.rows.iter().filter(|(date, _)| raw_rows.contains_key(date))
                .map(|(_, row)| row.clone()).collect(),
        };
    let bytes = serde_json::to_vec(&request)?;
    let path = format!("daily-price/adjusted-inputs/{}.json", crate::sha256(&bytes));
    persist(
        &checked_output_path(input, Path::new(&path))?,
        &request,
        false,
    )?;
    Ok(adjusted_series::import(lake, &bytes)?)
}

fn save_replay(
    root: &Path,
    raw: &RawStore,
    collected: &BTreeMap<&String, (Series, Series)>,
) -> Result<String> {
    let mut files = BTreeMap::new();
    for (unadjusted, adjusted) in collected.values() {
        for (name, series) in [("raw", unadjusted), ("qfq", adjusted)] {
            let report = &series.run.report;
            for (chunk, hash) in report.chunks.iter().zip(&series.hashes) {
                let file = format!(
                    "{}-{name}-{}-{}.json",
                    report.native_symbol, chunk.requested_range.start, chunk.requested_range.end,
                );
                files.insert(file, hash);
            }
        }
    }
    let relative = format!("daily-price/replays/{}", key(&files)?);
    for (file, hash) in files {
        let path = checked_output_path(root, Path::new(&relative).join(file).as_path())?;
        persist_bytes(&path, &raw.get(hash)?, false)?;
    }
    Ok(relative)
}

fn gap_summary(mut missing: impl Iterator<Item = NaiveDate>) -> Value {
    let sample = missing.by_ref().take(12).collect::<Vec<_>>();
    let count = sample.len() + missing.count();
    json!({"count": count, "first_dates": sample})
}

fn run<T: HttpTransport>(
    lake: &Path,
    input: &Path,
    config: Config,
    output: &Path,
    mut collector: DailyRawCollector<T>,
    acquisition: &str,
) -> Result<()> {
    let (calendar, calendar_version) = clip_calendar(input, &config)?;
    let members = members(&config, &calendar)?;
    let raw = RawStore::open(input)?;
    let mut collected = BTreeMap::new();
    let mut reports = BTreeMap::new();
    let mut coverage = BTreeMap::new();
    for (id, member) in &members {
        let raw_run =
            collector.collect_research(member.request.clone(), DailyAdjustment::Unadjusted)?;
        let unadjusted = series(&raw, raw_run, member, DailyAdjustment::Unadjusted)?;
        let qfq_run =
            collector.collect_research(member.request.clone(), DailyAdjustment::Forward)?;
        let adjusted = series(&raw, qfq_run, member, DailyAdjustment::Forward)?;
        reports.insert(
            id,
            json!({
                "raw_report": unadjusted.run.report_path.strip_prefix(input)?,
                "qfq_report": adjusted.run.report_path.strip_prefix(input)?,
                "raw_hashes": unadjusted.hashes,
                "qfq_hashes": adjusted.hashes,
            }),
        );
        coverage.insert(id, json!({
            "expected_sessions": member.sessions.len(),
            "raw_rows": unadjusted.rows.len(),
            "raw_missing": gap_summary(member.sessions.iter().copied().filter(|date| !unadjusted.rows.contains_key(date))),
            "qfq_rows": adjusted.rows.len(),
            "qfq_usable_rows": adjusted.rows.keys().filter(|date| unadjusted.rows.contains_key(date)).count(),
            "qfq_missing": gap_summary(member.sessions.iter().copied().filter(|date| !adjusted.rows.contains_key(date))),
            "qfq_without_raw": gap_summary(adjusted.rows.keys().copied().filter(|date| !unadjusted.rows.contains_key(date))),
            "qfq_day_fallback_chunks": adjusted.fallback_chunks,
        }));
        collected.insert(id, (unadjusted, adjusted));
    }
    let selection = PoolSelection {
        schema_version: 1,
        members: members
            .iter()
            .map(|(id, member)| {
                let receipt = publish_member(
                    lake,
                    input,
                    &config,
                    &calendar_version,
                    member,
                    &collected[id].0,
                )?;
                Ok((id.clone(), receipt))
            })
            .collect::<Result<_>>()?,
    };
    let pool_path = format!("daily-price/pool-receipts/{}.json", key(&selection)?);
    let published = pool_publish::publish(lake, &selection, Path::new(&pool_path))?;
    let mut adjustments = BTreeMap::new();
    for (id, member) in &members {
        let (unadjusted, adjusted) = &collected[id];
        adjustments.insert(
            id,
            import_adjustment(
                lake,
                input,
                &published.dsv,
                member,
                adjusted,
                &unadjusted.rows,
            )?,
        );
    }
    let replay_dir = save_replay(input, &raw, &collected)?;
    let batch = json!({
        "schema_version": 1,
        "kind": "daily-price-import/1",
        "a_stock_data_revision": STOCK_DATA_REVISION,
        "method": "tencent_kline(day, unadjusted and qfq, 700-calendar-day windows, count=640)",
        "acquisition": acquisition,
        "replay_dir": replay_dir,
        "start": config.start,
        "end": config.end,
        "calendar_version": calendar_version.as_str(),
        "parent_calendar_version": config.calendar_version,
        "security_snapshots": config.members,
        "units": {"prices": "CNY_per_fund_unit", "raw_volume": "vendor_lots", "vendor_lot_fund_units": 100, "dataset_volume": "fund_units"},
        "dsv": published.dsv,
        "pool_receipt": published,
        "adjustments": adjustments,
        "reports": reports,
        "coverage": coverage,
    });
    let batch_path = format!("daily-price/batches/{}.json", key(&batch)?);
    persist(
        &checked_output_path(input, Path::new(&batch_path))?,
        &batch,
        false,
    )?;
    persist(output, &batch, true)
}

pub fn import_daily_price(
    lake: &Path,
    input: &Path,
    config: &Path,
    receipt: &Path,
    acquisition: Acquisition<'_>,
) -> Result<()> {
    let config: Config = serde_json::from_slice(&fs::read(config)?)?;
    let protected = crate::default_mvp6_root();
    let lake = validate_raw_root(lake, &protected)?;
    let input = validate_raw_root(input, &protected)?;
    fs::create_dir_all(&lake)?;
    fs::create_dir_all(&input)?;
    let lock_path = checked_output_path(&lake, Path::new("daily-price-import.lock"))?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)?;
    fs2::FileExt::lock_exclusive(&lock)?;
    let output = checked_output_path(&input, receipt)?;
    if receipt.components().count() != 1 || output.extension().is_none_or(|ext| ext != "json") {
        return Err("receipt must be a JSON file name at the input root".into());
    }
    if output.exists()
        && serde_json::from_slice::<Value>(&fs::read(&output)?)?["kind"] != "daily-price-import/1"
    {
        return Err("receipt would replace a file from outside this command".into());
    }
    match acquisition {
        Acquisition::Fetch => run(
            &lake,
            &input,
            config,
            &output,
            DailyRawCollector::new(ReqwestTransport::new(), &input)?,
            "network",
        ),
        Acquisition::Replay(relative) => {
            let replay = checked_output_path(&input, relative)?;
            if !replay.is_dir() {
                return Err(
                    io::Error::new(io::ErrorKind::NotFound, "replay directory is missing").into(),
                );
            }
            run(
                &lake,
                &input,
                config,
                &output,
                DailyRawCollector::new_with_timing(
                    Replay(replay),
                    &input,
                    Arc::new(ReplayClock::default()),
                )?,
                "local-replay",
            )
        }
    }
}
