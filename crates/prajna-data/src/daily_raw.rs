//! Single-security Tencent daily Raw collection for the new Data Lake.
//!
//! This boundary stores original responses and a local coverage report only. It
//! does not normalize bars, infer an exchange calendar, or publish a Dataset.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    ffi::OsString,
    fs::{self, File},
    io::Write,
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration as StdDuration, Instant},
};

use chrono::{Duration, NaiveDate, SecondsFormat, Utc};
use prajna_domain::{InstrumentId, VenueId};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{RawStore, SourceKind, SourceRecordInput, default_mvp6_root};

pub const METHOD_VERSION: &str = "tencent-fqkline-day/1";
pub const REPORT_SCHEMA_VERSION: u32 = 1;
pub const MAX_REQUEST_CALENDAR_DAYS: i64 = 800;
pub const MIN_REQUEST_INTERVAL: StdDuration = StdDuration::from_secs(1);
const MAX_REQUEST_RETRIES: u32 = 3;
const SOURCE_ID: &str = "tencent_fqkline_daily";
const INGESTED_BY: &str = "prajna-daily-raw/1";
static NEXT_REPORT_ID: AtomicU64 = AtomicU64::new(0);
static SYSTEM_EPOCH: OnceLock<Instant> = OnceLock::new();
static SYSTEM_REQUEST_TIMES: OnceLock<Arc<Mutex<BTreeMap<String, StdDuration>>>> = OnceLock::new();

/// Clock and sleeper seam for deterministic request-pacing and retry tests.
pub trait DailyRawTiming: Send + Sync {
    fn now(&self) -> StdDuration;
    fn sleep(&self, duration: StdDuration);
}

/// Production monotonic clock and thread sleeper used by [`DailyRawCollector::new`].
#[derive(Debug, Default)]
pub struct SystemDailyRawTiming;

impl DailyRawTiming for SystemDailyRawTiming {
    fn now(&self) -> StdDuration {
        SYSTEM_EPOCH.get_or_init(Instant::now).elapsed()
    }

    fn sleep(&self, duration: StdDuration) {
        std::thread::sleep(duration);
    }
}

fn system_request_times() -> Arc<Mutex<BTreeMap<String, StdDuration>>> {
    SYSTEM_REQUEST_TIMES
        .get_or_init(|| Arc::new(Mutex::new(BTreeMap::new())))
        .clone()
}

/// The explicitly configured domestic exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Venue {
    Xshg,
    Xshe,
}

impl Venue {
    fn as_str(self) -> &'static str {
        match self {
            Self::Xshg => "XSHG",
            Self::Xshe => "XSHE",
        }
    }

    fn native_prefix(self) -> &'static str {
        match self {
            Self::Xshg => "sh",
            Self::Xshe => "sz",
        }
    }
}

/// The explicitly configured instrument kind. No kind is inferred from a code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecurityKind {
    Stock,
    Etf,
}

/// A closed date interval. Collection requests and response ranges are inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DateRange {
    pub start: NaiveDate,
    pub end: NaiveDate,
}

/// One explicitly configured stock or ETF daily history request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DailyRawRequest {
    /// Six-digit native code without the source's market prefix.
    pub symbol: String,
    pub venue: Venue,
    pub kind: SecurityKind,
    pub start: NaiveDate,
    pub end: NaiveDate,
}

impl DailyRawRequest {
    fn identity(&self) -> Result<(String, String), DailyRawError> {
        if self.symbol.len() != 6 || !self.symbol.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(DailyRawError::invalid(
                "symbol must be an explicitly configured six-digit Shanghai or Shenzhen code",
            ));
        }
        if self.start > self.end {
            return Err(DailyRawError::invalid(
                "daily Raw collection start must not follow end",
            ));
        }
        let instrument_id = InstrumentId::new(
            self.symbol.clone(),
            VenueId::new(self.venue.as_str()).map_err(|error| {
                DailyRawError::invalid(format!("invalid configured venue: {error}"))
            })?,
        )
        .map_err(|error| DailyRawError::invalid(error.to_string()))?;
        Ok((
            instrument_id.to_string(),
            format!("{}{code}", self.venue.native_prefix(), code = self.symbol),
        ))
    }

    fn chunks(&self) -> Vec<DateRange> {
        let mut ranges = Vec::new();
        let mut start = self.start;
        loop {
            let max_end = start
                .checked_add_signed(Duration::days(MAX_REQUEST_CALENDAR_DAYS - 1))
                .unwrap_or(self.end);
            let end = max_end.min(self.end);
            ranges.push(DateRange { start, end });
            if end == self.end {
                break;
            }
            start = end.succ_opt().expect("end precedes the requested end");
        }
        ranges
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollectionStatus {
    Complete,
    Partial,
    Failed,
    SourceStopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkStatus {
    Complete,
    Empty,
    Failed,
    NotAttempted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RawResponseOutcome {
    AcceptedResponse,
    RejectedCandidateResponse,
}

/// Local reference to an immutable response object; hashes are never printed by the CLI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawObjectReference {
    pub raw_sha256: String,
    pub source: String,
    pub outcome: RawResponseOutcome,
}

/// One failed or rejected source candidate, without its response body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateFailureSummary {
    pub host: String,
    pub kind: String,
    pub status_code: Option<u16>,
    pub message: String,
}

/// Per-requested-range outcome and exact dates found in the preserved response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkReport {
    pub index: u32,
    pub requested_range: DateRange,
    pub status: ChunkStatus,
    /// Number of individual candidate HTTP requests made by the shared transport.
    pub candidate_attempts: u32,
    pub candidate_failures: Vec<CandidateFailureSummary>,
    pub raw_objects: Vec<RawObjectReference>,
    /// Unique, sorted dates stated by the vendor response, including out-of-range dates.
    pub actual_response_dates: Vec<NaiveDate>,
    pub actual_response_range: Option<DateRange>,
    pub not_returned_calendar_dates: Vec<NaiveDate>,
    pub outside_requested_range_dates: Vec<NaiveDate>,
    pub duplicate_response_dates: Vec<NaiveDate>,
    pub observed_at: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionSummary {
    pub chunks_requested: u32,
    pub chunks_attempted: u32,
    pub data_chunks: u32,
    pub accepted_response_chunks: u32,
    pub empty_chunks: u32,
    pub failed_chunks: u32,
    pub not_attempted_chunks: u32,
    pub candidate_http_attempts: u32,
    pub candidate_failures: u32,
    pub raw_object_references: u32,
}

/// Persisted local report. It contains Raw references and provenance, not normalized market data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DailyRawReport {
    pub schema_version: u32,
    pub report_id: String,
    pub source: String,
    pub method_version: String,
    pub instrument_id: String,
    pub native_symbol: String,
    pub security_kind: SecurityKind,
    pub requested_range: DateRange,
    pub status: CollectionStatus,
    pub observed_at: String,
    pub observed_at_semantics: String,
    pub availability: String,
    pub field_units: BTreeMap<String, String>,
    /// Explicitly separates unknown metadata from T02-meta evidence.
    pub instrument_metadata: BTreeMap<String, String>,
    pub scope_statement: String,
    pub summary: CollectionSummary,
    pub chunks: Vec<ChunkReport>,
}

/// Report plus its local file path. The path is intentionally not serialized into the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionRun {
    pub report: DailyRawReport,
    pub report_path: PathBuf,
}

/// Invalid configuration or local persistence failure.
#[derive(Debug)]
pub struct DailyRawError(String);

impl DailyRawError {
    fn invalid(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for DailyRawError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for DailyRawError {}

struct PacedTransport<'a, T> {
    transport: &'a mut T,
    timing: &'a dyn DailyRawTiming,
    request_times: &'a Mutex<BTreeMap<String, StdDuration>>,
}

impl<T: tencent_transport::HttpTransport> tencent_transport::HttpTransport
    for PacedTransport<'_, T>
{
    fn get(
        &mut self,
        request: &tencent_transport::HttpRequest,
    ) -> Result<tencent_transport::HttpReply, tencent_transport::TransportFailure> {
        let domain = rate_limit_domain(request.host);
        let mut request_times = self
            .request_times
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(previous) = request_times.get(&domain).copied() {
            let elapsed = self.timing.now().saturating_sub(previous);
            let remaining = MIN_REQUEST_INTERVAL.saturating_sub(elapsed);
            if !remaining.is_zero() {
                self.timing.sleep(remaining);
            }
        }
        request_times.insert(domain, self.timing.now());
        drop(request_times);
        self.transport.get(request)
    }
}

fn rate_limit_domain(host: &str) -> String {
    let host = host
        .split_once("://")
        .map_or(host, |(_, authority)| authority)
        .split('/')
        .next()
        .unwrap_or(host);
    if host == "gtimg.cn" || host.ends_with(".gtimg.cn") {
        "gtimg.cn".into()
    } else if host == "qq.com" || host.ends_with(".qq.com") {
        "qq.com".into()
    } else {
        host.into()
    }
}

fn retry_delay(retry_index: u32) -> StdDuration {
    StdDuration::from_secs(1_u64 << retry_index)
}

fn is_retryable(error: &tencent_transport::FetchError) -> bool {
    matches!(
        error.kind,
        tencent_transport::FetchErrorKind::BodyRead
            | tencent_transport::FetchErrorKind::EndpointsUnavailable
    )
}

/// Collects one configured security through the shared Tencent transport and persists
/// Raw objects plus a per-invocation report beneath an explicit local Raw root.
pub struct DailyRawCollector<T> {
    transport: T,
    root: PathBuf,
    raw: RawStore,
    timing: Arc<dyn DailyRawTiming>,
    request_times: Arc<Mutex<BTreeMap<String, StdDuration>>>,
    consecutive_restrictions: usize,
    source_stopped: bool,
}

impl<T: tencent_transport::HttpTransport> DailyRawCollector<T> {
    pub fn new(transport: T, raw_root: impl AsRef<Path>) -> Result<Self, DailyRawError> {
        Self::with_timing_and_state(
            transport,
            raw_root,
            Arc::new(SystemDailyRawTiming),
            system_request_times(),
        )
    }

    /// Creates a collector with an injected clock/sleeper and isolated pacing state.
    /// Production callers should use [`DailyRawCollector::new`].
    pub fn new_with_timing(
        transport: T,
        raw_root: impl AsRef<Path>,
        timing: Arc<dyn DailyRawTiming>,
    ) -> Result<Self, DailyRawError> {
        Self::with_timing_and_state(
            transport,
            raw_root,
            timing,
            Arc::new(Mutex::new(BTreeMap::new())),
        )
    }

    fn with_timing_and_state(
        transport: T,
        raw_root: impl AsRef<Path>,
        timing: Arc<dyn DailyRawTiming>,
        request_times: Arc<Mutex<BTreeMap<String, StdDuration>>>,
    ) -> Result<Self, DailyRawError> {
        let root = canonicalize_candidate(raw_root.as_ref())?;
        let legacy_root = canonicalize_candidate(&default_mvp6_root())?;
        if root == legacy_root || root.starts_with(&legacy_root) {
            return Err(DailyRawError::invalid(
                "daily Raw root must be independent of the fixed MVP-6 local archive",
            ));
        }
        let raw = RawStore::open(&root).map_err(|error| DailyRawError(error.to_string()))?;
        Ok(Self {
            transport,
            root,
            raw,
            timing,
            request_times,
            consecutive_restrictions: 0,
            source_stopped: false,
        })
    }

    /// Collects all source-limited request ranges and persists a unique report file.
    ///
    /// Repeating this call is safe: Raw bytes are content-addressed, prior revisions
    /// remain available, and each invocation gets a separate report. A failed or empty
    /// range is recorded but never deletes old Raw or advances a completion marker.
    pub fn collect(&mut self, request: DailyRawRequest) -> Result<CollectionRun, DailyRawError> {
        let (instrument_id, native_symbol) = request.identity()?;
        let requested_range = DateRange {
            start: request.start,
            end: request.end,
        };
        let report_id = next_report_id();
        let observed_at = now();
        let mut chunks = Vec::new();

        for (index, range) in request.chunks().into_iter().enumerate() {
            if self.source_stopped {
                chunks.push(ChunkReport {
                    index: index as u32,
                    requested_range: range,
                    status: ChunkStatus::NotAttempted,
                    candidate_attempts: 0,
                    candidate_failures: Vec::new(),
                    raw_objects: Vec::new(),
                    actual_response_dates: Vec::new(),
                    actual_response_range: None,
                    not_returned_calendar_dates: calendar_dates(range),
                    outside_requested_range_dates: Vec::new(),
                    duplicate_response_dates: Vec::new(),
                    observed_at: None,
                    error: Some("source stop rule already reached".into()),
                });
                continue;
            }

            let daily_request = tencent_transport::DailyRequest {
                symbol: native_symbol.clone(),
                start: range.start,
                end: range.end,
            };
            let mut candidate_failures = Vec::new();
            let mut raw_objects = Vec::new();
            let mut storage_error = None;
            let raw = &self.raw;
            let instrument_id_for_records = &instrument_id;
            let native_symbol_for_records = &native_symbol;
            let kind_for_records = request.kind;
            let timing = Arc::clone(&self.timing);
            let request_times = Arc::clone(&self.request_times);
            let transport = &mut self.transport;
            let restriction_count = &mut self.consecutive_restrictions;
            let fetch_result = {
                let mut final_result = None;
                for retry_index in 0..=MAX_REQUEST_RETRIES {
                    let attempt_result = {
                        let mut paced_transport = PacedTransport {
                            transport,
                            timing: timing.as_ref(),
                            request_times: request_times.as_ref(),
                        };
                        let mut policy =
                            |failure: &tencent_transport::CandidateFailure,
                             response_body: Option<&[u8]>| {
                                candidate_failures.push(CandidateFailureSummary {
                                    host: failure.host.to_owned(),
                                    kind: format!("{:?}", failure.kind),
                                    status_code: failure.status_code,
                                    message: failure.message.clone(),
                                });
                                if let Some(bytes) = response_body {
                                    match store_raw(
                                        raw,
                                        RawStoreRequest {
                                            bytes,
                                            instrument_id: instrument_id_for_records,
                                            native_symbol: native_symbol_for_records,
                                            kind: kind_for_records,
                                            range,
                                            chunk_index: index as u32,
                                            source: failure.host,
                                            outcome: RawResponseOutcome::RejectedCandidateResponse,
                                            content_type: "application/octet-stream",
                                        },
                                    ) {
                                        Ok(reference) => raw_objects.push(reference),
                                        Err(error) => {
                                            storage_error = Some(error);
                                            return tencent_transport::CandidateAction::Stop;
                                        }
                                    }
                                }

                                if is_source_restriction(failure, response_body) {
                                    *restriction_count += 1;
                                } else {
                                    *restriction_count = 0;
                                }
                                if *restriction_count >= 2 {
                                    tencent_transport::CandidateAction::Stop
                                } else {
                                    tencent_transport::CandidateAction::Continue
                                }
                            };

                        tencent_transport::fetch_unadjusted_daily_with_policy(
                            &daily_request,
                            &mut paced_transport,
                            |bytes| parse_response_dates(bytes, &native_symbol).is_ok(),
                            &mut policy,
                        )
                    };
                    match &attempt_result {
                        Err(error) if retry_index < MAX_REQUEST_RETRIES && is_retryable(error) => {
                            timing.sleep(retry_delay(retry_index));
                        }
                        _ => {
                            final_result = Some(attempt_result);
                            break;
                        }
                    }
                }
                final_result.expect("one bounded transport attempt always runs")
            };
            if let Some(error) = storage_error {
                return Err(error);
            }

            let candidate_attempts =
                candidate_failures.len() as u32 + u32::from(fetch_result.is_ok());
            let mut chunk = ChunkReport {
                index: index as u32,
                requested_range: range,
                status: ChunkStatus::Failed,
                candidate_attempts,
                candidate_failures,
                raw_objects,
                actual_response_dates: Vec::new(),
                actual_response_range: None,
                not_returned_calendar_dates: calendar_dates(range),
                outside_requested_range_dates: Vec::new(),
                duplicate_response_dates: Vec::new(),
                observed_at: Some(now()),
                error: None,
            };

            match fetch_result {
                Ok(response) => {
                    self.consecutive_restrictions = 0;
                    let response_dates = parse_response_dates(&response.bytes, &native_symbol)
                        .map_err(DailyRawError::invalid)?;
                    let reference = store_raw(
                        &self.raw,
                        RawStoreRequest {
                            bytes: &response.bytes,
                            instrument_id: &instrument_id,
                            native_symbol: &native_symbol,
                            kind: request.kind,
                            range,
                            chunk_index: index as u32,
                            source: &response.source,
                            outcome: RawResponseOutcome::AcceptedResponse,
                            content_type: "application/json",
                        },
                    )?;
                    chunk.raw_objects.push(reference);
                    let (unique_dates, duplicates) = unique_and_duplicates(response_dates);
                    chunk.actual_response_range = date_range_of(&unique_dates);
                    chunk.outside_requested_range_dates = unique_dates
                        .iter()
                        .copied()
                        .filter(|date| *date < range.start || *date > range.end)
                        .collect();
                    let in_range = unique_dates
                        .iter()
                        .copied()
                        .filter(|date| *date >= range.start && *date <= range.end)
                        .collect::<BTreeSet<_>>();
                    chunk.not_returned_calendar_dates = calendar_dates(range)
                        .into_iter()
                        .filter(|date| !in_range.contains(date))
                        .collect();
                    chunk.actual_response_dates = unique_dates;
                    chunk.duplicate_response_dates = duplicates;
                    if in_range.is_empty() {
                        chunk.status = ChunkStatus::Empty;
                        chunk.error =
                            Some("valid response contained no dates in the requested range".into());
                    } else {
                        chunk.status = ChunkStatus::Complete;
                    }
                }
                Err(error) => {
                    chunk.error = Some(error.to_string());
                    if error.kind == tencent_transport::FetchErrorKind::PolicyStopped {
                        self.source_stopped = true;
                    }
                }
            }
            chunks.push(chunk);
        }

        let summary = summarize(&chunks);
        let any_data = chunks
            .iter()
            .any(|chunk| chunk.status == ChunkStatus::Complete);
        let has_failure = chunks.iter().any(|chunk| {
            matches!(
                chunk.status,
                ChunkStatus::Failed | ChunkStatus::NotAttempted
            )
        });
        let has_empty = chunks
            .iter()
            .any(|chunk| chunk.status == ChunkStatus::Empty);
        let status = if self.source_stopped {
            CollectionStatus::SourceStopped
        } else if !has_failure && !has_empty {
            CollectionStatus::Complete
        } else if any_data {
            CollectionStatus::Partial
        } else {
            CollectionStatus::Failed
        };
        let report = DailyRawReport {
            schema_version: REPORT_SCHEMA_VERSION,
            report_id,
            source: "Tencent FQKline".into(),
            method_version: METHOD_VERSION.into(),
            instrument_id,
            native_symbol,
            security_kind: request.kind,
            requested_range,
            status,
            observed_at,
            observed_at_semantics:
                "local response observation time; not vendor publication time or historical availability"
                    .into(),
            availability: "unknown".into(),
            field_units: field_units(),
            instrument_metadata: instrument_metadata(),
            scope_statement: "Complete means every requested source-limited range returned at least one valid in-range date. Calendar-day gaps are listed without a trading calendar and do not imply a missing session, suspension, or tradability. This request-scoped response is not a claim of full history."
                .into(),
            summary,
            chunks,
        };
        let report_path = persist_report(&self.root, &report)?;
        Ok(CollectionRun {
            report,
            report_path,
        })
    }
}

struct RawStoreRequest<'a> {
    bytes: &'a [u8],
    instrument_id: &'a str,
    native_symbol: &'a str,
    kind: SecurityKind,
    range: DateRange,
    chunk_index: u32,
    source: &'a str,
    outcome: RawResponseOutcome,
    content_type: &'a str,
}

fn store_raw(
    raw: &RawStore,
    request: RawStoreRequest<'_>,
) -> Result<RawObjectReference, DailyRawError> {
    let observed_at = now();
    let hash = raw
        .put(
            request.bytes,
            SourceRecordInput {
                content_type: request.content_type.into(),
                source_kind: SourceKind::VendorApi,
                source_id: SOURCE_ID.into(),
                request: json!({
                    "method_version": METHOD_VERSION,
                    "instrument_id": request.instrument_id,
                    "native_symbol": request.native_symbol,
                    "security_kind": request.kind,
                    "requested_range": request.range,
                    "chunk_index": request.chunk_index,
                    "source": request.source,
                    "response_outcome": request.outcome,
                    "available_at": "unknown",
                    "units": "unknown unless separately established by T02-meta or a versioned normalizer"
                }),
                observed_at,
                ingested_by: INGESTED_BY.into(),
            },
        )
        .map_err(|error| DailyRawError(error.to_string()))?;
    Ok(RawObjectReference {
        raw_sha256: hash,
        source: request.source.into(),
        outcome: request.outcome,
    })
}

fn parse_response_dates(bytes: &[u8], native_symbol: &str) -> Result<Vec<NaiveDate>, String> {
    let payload: Value = serde_json::from_slice(bytes)
        .map_err(|_| "Tencent response is not valid JSON".to_owned())?;
    let code = payload.get("code").map(|value| match value {
        Value::String(code) => code.clone(),
        Value::Number(code) => code.to_string(),
        _ => String::new(),
    });
    if code.as_deref() != Some("0") {
        return Err("Tencent response code is not zero".into());
    }
    let data = payload
        .get("data")
        .and_then(Value::as_object)
        .ok_or_else(|| "Tencent response has no data object".to_owned())?;
    if data.len() != 1 || !data.contains_key(native_symbol) {
        return Err("Tencent response does not contain exactly the requested symbol".into());
    }
    let rows = data[native_symbol]
        .get("day")
        .and_then(Value::as_array)
        .ok_or_else(|| "Tencent response has no unadjusted day array".to_owned())?;
    rows.iter()
        .enumerate()
        .map(|(index, row)| {
            let cells = row
                .as_array()
                .filter(|cells| cells.len() >= 6)
                .ok_or_else(|| format!("Tencent day row {index} has fewer than six fields"))?;
            let date = cells[0]
                .as_str()
                .ok_or_else(|| format!("Tencent day row {index} has no string date"))?;
            NaiveDate::parse_from_str(date, "%Y-%m-%d")
                .map_err(|_| format!("Tencent day row {index} has an invalid date"))
        })
        .collect()
}

fn unique_and_duplicates(dates: Vec<NaiveDate>) -> (Vec<NaiveDate>, Vec<NaiveDate>) {
    let mut counts = BTreeMap::new();
    for date in dates {
        *counts.entry(date).or_insert(0_u32) += 1;
    }
    let unique = counts.keys().copied().collect();
    let duplicates = counts
        .into_iter()
        .filter_map(|(date, count)| (count > 1).then_some(date))
        .collect();
    (unique, duplicates)
}

fn date_range_of(dates: &[NaiveDate]) -> Option<DateRange> {
    Some(DateRange {
        start: *dates.first()?,
        end: *dates.last()?,
    })
}

fn calendar_dates(range: DateRange) -> Vec<NaiveDate> {
    let mut dates = Vec::new();
    let mut current = range.start;
    loop {
        dates.push(current);
        if current == range.end {
            return dates;
        }
        current = current.succ_opt().expect("date precedes the range end");
    }
}

fn is_source_restriction(
    failure: &tencent_transport::CandidateFailure,
    response_body: Option<&[u8]>,
) -> bool {
    matches!(failure.status_code, Some(401 | 403 | 429 | 501))
        || response_body.is_some_and(|body| {
            let text = String::from_utf8_lossy(body).to_lowercase();
            ["captcha", "验证码", "访问验证"]
                .iter()
                .any(|marker| text.contains(marker))
        })
}

fn summarize(chunks: &[ChunkReport]) -> CollectionSummary {
    let count = |status| chunks.iter().filter(|chunk| chunk.status == status).count() as u32;
    CollectionSummary {
        chunks_requested: chunks.len() as u32,
        chunks_attempted: chunks
            .iter()
            .filter(|chunk| chunk.status != ChunkStatus::NotAttempted)
            .count() as u32,
        data_chunks: count(ChunkStatus::Complete),
        accepted_response_chunks: count(ChunkStatus::Complete) + count(ChunkStatus::Empty),
        empty_chunks: count(ChunkStatus::Empty),
        failed_chunks: count(ChunkStatus::Failed),
        not_attempted_chunks: count(ChunkStatus::NotAttempted),
        candidate_http_attempts: chunks.iter().map(|chunk| chunk.candidate_attempts).sum(),
        candidate_failures: chunks
            .iter()
            .map(|chunk| chunk.candidate_failures.len() as u32)
            .sum(),
        raw_object_references: chunks
            .iter()
            .map(|chunk| chunk.raw_objects.len() as u32)
            .sum(),
    }
}

fn field_units() -> BTreeMap<String, String> {
    [
        (
            "session_date",
            "ISO-8601 date; not checked against an exchange calendar",
        ),
        ("open", "unknown"),
        ("high", "unknown"),
        ("low", "unknown"),
        ("close", "unknown"),
        ("volume", "unknown"),
        ("amount", "unknown; not normalized or inferred"),
        ("currency", "unknown"),
        ("timezone", "unknown; no session or time-zone inference"),
    ]
    .into_iter()
    .map(|(field, unit)| (field.into(), unit.into()))
    .collect()
}

fn instrument_metadata() -> BTreeMap<String, String> {
    [
        ("established_date", "unknown; T02-meta evidence required"),
        ("listing_date", "unknown; T02-meta evidence required"),
        ("lot_size", "unknown; T02-meta evidence required"),
        ("price_increment", "unknown; T02-meta evidence required"),
        ("size_increment", "unknown; T02-meta evidence required"),
    ]
    .into_iter()
    .map(|(field, evidence)| (field.into(), evidence.into()))
    .collect()
}

fn persist_report(root: &Path, report: &DailyRawReport) -> Result<PathBuf, DailyRawError> {
    let directory = root.join("reports/daily-raw");
    fs::create_dir_all(&directory).map_err(|error| DailyRawError(error.to_string()))?;
    let path = directory.join(format!("{}.json", report.report_id));
    let mut temporary = tempfile::NamedTempFile::new_in(&directory)
        .map_err(|error| DailyRawError(error.to_string()))?;
    serde_json::to_writer_pretty(&mut temporary, report)
        .map_err(|error| DailyRawError(error.to_string()))?;
    temporary
        .write_all(b"\n")
        .map_err(|error| DailyRawError(error.to_string()))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| DailyRawError(error.to_string()))?;
    temporary
        .persist_noclobber(&path)
        .map_err(|error| DailyRawError(error.error.to_string()))?;
    File::open(&directory)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| DailyRawError(error.to_string()))?;
    Ok(path)
}

fn canonicalize_candidate(path: &Path) -> Result<PathBuf, DailyRawError> {
    if path.as_os_str().is_empty() {
        return Err(DailyRawError::invalid(
            "an explicit local Raw root is required",
        ));
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| DailyRawError(error.to_string()))?
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::RootDir | Component::Prefix(_) => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(value) => normalized.push(value),
        }
    }

    let mut existing = normalized.clone();
    let mut suffix: Vec<OsString> = Vec::new();
    while !existing.exists() {
        let name = existing
            .file_name()
            .ok_or_else(|| DailyRawError::invalid("Raw root has no existing parent"))?;
        suffix.push(name.to_os_string());
        existing = existing
            .parent()
            .ok_or_else(|| DailyRawError::invalid("Raw root has no existing parent"))?
            .to_path_buf();
    }
    let mut canonical =
        fs::canonicalize(&existing).map_err(|error| DailyRawError(error.to_string()))?;
    for component in suffix.into_iter().rev() {
        canonical.push(component);
    }
    Ok(canonical)
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn next_report_id() -> String {
    let stamp = Utc::now().format("%Y%m%dT%H%M%S%.fZ");
    let sequence = NEXT_REPORT_ID.fetch_add(1, Ordering::Relaxed);
    format!(
        "tencent-daily-raw-v1-{stamp}-{}-{sequence}",
        std::process::id()
    )
}

#[cfg(test)]
mod tests {
    use super::rate_limit_domain;

    #[test]
    fn candidate_urls_are_grouped_by_tencent_registrable_domain() {
        assert_eq!(rate_limit_domain("https://web.ifzq.gtimg.cn"), "gtimg.cn");
        assert_eq!(rate_limit_domain("https://ifzq.gtimg.cn"), "gtimg.cn");
        assert_eq!(
            rate_limit_domain("https://proxy.finance.qq.com/ifzqgtimg"),
            "qq.com"
        );
    }
}
