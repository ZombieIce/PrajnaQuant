//! Versioned XSHG/XSHE stock and ETF daily Normalizer (MVP-7 T02b-C).
//!
//! The Normalizer consumes only pinned, byte-exact Raw inputs:
//!
//! - one #328 calendar version whose Raw hash equals `calendar_version`;
//! - one #329 security-evidence snapshot whose Raw hash equals `snapshot_id`;
//! - one or more Tencent unadjusted `fqkline` daily responses.
//!
//! The pinned version ids enter `config_sha256`; all Raw hashes enter the DSV
//! inputs. Nothing is read from a mutable "latest" store, and later appended
//! source records do not change the normalized bytes. Tencent `day` rows are
//! `[date, open, close, high, low, lots]`; prices are exact decimal literals
//! and volume is lots multiplied by 100 shares/fund units. The source has no
//! amount field, so `amount` stays null. `available_at` stays Unknown. The
//! Normalizer never adjusts prices, derives corporate actions, execution
//! status, or a tradability window, and does not modify MVP-6 SZSE identities.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use prajna_domain::{
    Bar, BarData, Currency, FixedPointError, InstrumentId, InstrumentKind, InstrumentSpec,
    InstrumentSpecData, Price, Quantity, Session, VenueId,
};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::{
    BarSourceRow, NormalizationIssue, NormalizationOutput, NormalizedTables, SourceKind,
    bar_sources_to_record_batch, bars_to_record_batch, instruments_to_record_batch,
    security_evidence::{
        ExecutionTerms, FieldEvidence, PublicationStatus, SecurityEvidenceSnapshot,
        SecuritySnapshotId,
    },
    session_calendar::{CalendarInput, CalendarVersionId},
    sessions_to_record_batch,
};

use super::{Normalizer, RawInput};

pub const CN_DAILY_NORMALIZER_ID: &str = "cn-stock-etf-daily";
pub const CN_DAILY_NORMALIZER_VERSION: &str = "1";
const SOURCE_ID: &str = "tencent_fqkline_daily";
const SOURCE_METHOD_VERSION: &str = "tencent-fqkline-day/1";
const VOLUME_MULTIPLIER: i128 = 100;

/// Fixed-version XSHG/XSHE stock/ETF daily Normalizer.
///
/// A rule change (source, unit, missing-value or gate semantics) requires a new
/// `version`; this v1 is frozen to the following input selection.
pub struct CnStockEtfDaily {
    calendar_version: CalendarVersionId,
    snapshot_id: SecuritySnapshotId,
}

impl CnStockEtfDaily {
    pub fn v1(calendar_version: CalendarVersionId, snapshot_id: SecuritySnapshotId) -> Self {
        Self {
            calendar_version,
            snapshot_id,
        }
    }

    fn config(&self) -> Value {
        json!({
            "bar_spec": "1d@session",
            "calendar_version": self.calendar_version.as_str(),
            "security_snapshot": self.snapshot_id.as_str(),
            "calendar_source": "pinned_328_calendar_version",
            "instrument_terms": "pinned_329_snapshot_publishable_terms",
            "source": SOURCE_ID,
            "source_method_version": SOURCE_METHOD_VERSION,
            "timezone": "Asia/Shanghai",
            "price_units": {
                "Equity": "CNY_per_share_exact_decimal_literal",
                "Etf": "CNY_per_fund_unit_exact_decimal_literal"
            },
            "volume_units": {
                "Equity": {"source": "lots", "multiplier": VOLUME_MULTIPLIER, "output": "shares"},
                "Etf": {"source": "lots", "multiplier": VOLUME_MULTIPLIER, "output": "fund_units"}
            },
            "amount_units": "absent_from_source_null",
            "available_at": "unknown_null",
            "adjustment": "none",
            "execution_status": "not_derived",
            "corporate_actions": "not_in_scope",
            "missing_quotes": "absent_bar_no_zero_sessions_preserved",
            "identical_duplicate_facts": "deduplicated_min_raw_sha256",
            "conflicting_duplicate_facts": "rejected",
            "row_fields_beyond_six": "ignored_amount_unknown",
        })
    }
}

impl Normalizer for CnStockEtfDaily {
    fn id(&self) -> &str {
        CN_DAILY_NORMALIZER_ID
    }

    fn version(&self) -> &str {
        CN_DAILY_NORMALIZER_VERSION
    }

    fn config_sha256(&self) -> String {
        let canonical =
            crate::restricted_jcs(&self.config()).expect("static cn daily config is JCS-safe");
        format!("sha256:{:x}", Sha256::digest(canonical))
    }

    fn accepted_source_kinds(&self) -> &[SourceKind] {
        &[SourceKind::FileImport, SourceKind::VendorApi]
    }

    fn normalize(
        &self,
        raw_inputs: &[RawInput],
    ) -> Result<NormalizationOutput, Vec<NormalizationIssue>> {
        normalize(raw_inputs, self)
    }
}

#[derive(Default)]
struct Acc {
    issues: Vec<NormalizationIssue>,
    calendar: Option<CalendarInput>,
    snapshot: Option<SecurityEvidenceSnapshot>,
    daily_raws: usize,
    observations: BTreeMap<(InstrumentId, NaiveDate), Observation>,
    identical_duplicates: u64,
    ignored_extra_fields: u64,
}

#[derive(Clone, PartialEq, Eq)]
struct Observation {
    open: Price,
    high: Price,
    low: Price,
    close: Price,
    volume: Quantity,
    raw_sha256: String,
}

struct VenueCalendar {
    coverage_start: NaiveDate,
    coverage_end: NaiveDate,
    sessions: BTreeMap<NaiveDate, Session>,
}

struct SelectedInstrument {
    kind: InstrumentKind,
    terms: ExecutionTerms,
    listing_date: FieldEvidence<NaiveDate>,
}

fn issue(code: &str, path: impl Into<String>, message: impl Into<String>) -> NormalizationIssue {
    NormalizationIssue::new(code, path, message)
}

fn normalize(
    raw_inputs: &[RawInput],
    config: &CnStockEtfDaily,
) -> Result<NormalizationOutput, Vec<NormalizationIssue>> {
    let mut acc = Acc::default();
    let mut seen = BTreeSet::new();
    let mut daily: Vec<&RawInput> = Vec::new();

    for (index, raw) in raw_inputs.iter().enumerate() {
        let path = format!("inputs[{index}]");
        if !raw.has_valid_identity() {
            acc.issues.push(issue(
                "raw_hash_mismatch",
                path,
                "raw_sha256 does not match the supplied bytes",
            ));
            continue;
        }
        if !seen.insert(raw.raw_sha256.clone()) {
            acc.issues.push(issue(
                "duplicate_raw_input",
                path,
                "Raw object listed twice",
            ));
            continue;
        }
        if raw.raw_sha256 == config.calendar_version.as_str() {
            match CalendarInput::parse_and_validate(&raw.bytes) {
                Ok(calendar) => acc.calendar = Some(calendar),
                Err(error) => {
                    acc.issues
                        .push(issue("invalid_pinned_calendar", &path, error.to_string()))
                }
            }
            continue;
        }
        if raw.raw_sha256 == config.snapshot_id.as_str() {
            match read_snapshot(&raw.bytes, &config.snapshot_id) {
                Ok(snapshot) => acc.snapshot = Some(snapshot),
                Err(error) => {
                    acc.issues
                        .push(issue("invalid_pinned_security_snapshot", &path, error))
                }
            }
            continue;
        }
        let Some(object) = json_object(&raw.bytes) else {
            acc.issues.push(issue(
                "unrecognized_original",
                &path,
                "Raw object is not a pinned JSON document or a Tencent daily response",
            ));
            continue;
        };
        if calendar_shape(&object) {
            acc.issues.push(issue(
                "unexpected_calendar_version",
                &path,
                "calendar bytes do not match the pinned calendar_version",
            ));
        } else if snapshot_shape(&object) {
            acc.issues.push(issue(
                "unexpected_security_snapshot",
                &path,
                "security snapshot bytes do not match the pinned snapshot id",
            ));
        } else if daily_shape(&object) {
            daily.push(raw);
        } else {
            acc.issues.push(issue(
                "unrecognized_original",
                &path,
                "JSON object matches no pinned input or Tencent daily response",
            ));
        }
    }

    if acc.calendar.is_none() {
        acc.issues.push(issue(
            "missing_pinned_calendar",
            "inputs",
            "no Raw input matches the pinned calendar_version",
        ));
    }
    if acc.snapshot.is_none() {
        acc.issues.push(issue(
            "missing_pinned_security_snapshot",
            "inputs",
            "no Raw input matches the pinned security snapshot id",
        ));
    }
    if daily.is_empty() {
        acc.issues.push(issue(
            "missing_daily_raw",
            "inputs",
            "no Tencent daily Raw response was supplied",
        ));
    }
    if !acc.issues.is_empty() {
        return Err(acc.issues);
    }

    let calendar = acc.calendar.take().expect("checked above");
    let snapshot = acc.snapshot.take().expect("checked above");
    let venues = calendar_view(&calendar, &mut acc.issues)?;
    let instruments = selected_instruments(&snapshot, &mut acc.issues);
    if !acc.issues.is_empty() {
        return Err(acc.issues);
    }

    for raw in daily {
        acc.daily_raws += 1;
        decode_daily(raw, &venues, &instruments, &mut acc);
    }
    if acc.issues.is_empty() && acc.observations.is_empty() {
        acc.issues.push(issue(
            "empty_daily_bars",
            "inputs",
            "pinned daily Raws contain no valid bar rows",
        ));
    }
    if !acc.issues.is_empty() {
        return Err(acc.issues);
    }
    build_output(config, venues, instruments, acc)
}

fn read_snapshot(
    bytes: &[u8],
    expected: &SecuritySnapshotId,
) -> Result<SecurityEvidenceSnapshot, String> {
    let snapshot: SecurityEvidenceSnapshot =
        serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    snapshot.validate().map_err(|error| error.to_string())?;
    if snapshot.id().map_err(|error| error.to_string())? != *expected {
        return Err("embedded snapshot identity does not match the pinned id".to_owned());
    }
    Ok(snapshot)
}

fn json_object(bytes: &[u8]) -> Option<Map<String, Value>> {
    serde_json::from_slice::<Value>(bytes)
        .ok()
        .and_then(|value| value.as_object().cloned())
}

fn calendar_shape(object: &Map<String, Value>) -> bool {
    object.contains_key("venues") && object.get("schema_version").is_some_and(Value::is_u64)
}

fn snapshot_shape(object: &Map<String, Value>) -> bool {
    object.contains_key("instruments") && object.get("schema_version").is_some_and(Value::is_u64)
}

fn daily_shape(object: &Map<String, Value>) -> bool {
    object.get("code").is_some() && object.get("data").is_some_and(Value::is_object)
}

fn has_pinned_source_record(raw: &RawInput, instrument_id: &InstrumentId) -> bool {
    let expected = instrument_id.to_string();
    raw.source_records.iter().any(|record| {
        record.source_id == SOURCE_ID
            && record.request.get("method_version").and_then(Value::as_str)
                == Some(SOURCE_METHOD_VERSION)
            && record.request.get("instrument_id").and_then(Value::as_str)
                == Some(expected.as_str())
    })
}

fn calendar_view(
    calendar: &CalendarInput,
    issues: &mut Vec<NormalizationIssue>,
) -> Result<BTreeMap<VenueId, VenueCalendar>, Vec<NormalizationIssue>> {
    let mut venues = BTreeMap::new();
    for venue in &calendar.venues {
        let mut sessions = BTreeMap::new();
        for row in &venue.sessions {
            match Session::new(
                venue.venue_id.clone(),
                row.session_date,
                row.ts_open,
                row.ts_close,
            ) {
                Ok(session) => {
                    sessions.insert(row.session_date, session);
                }
                Err(error) => issues.push(issue(
                    "invalid_session",
                    venue.venue_id.to_string(),
                    error.to_string(),
                )),
            }
        }
        venues.insert(
            venue.venue_id.clone(),
            VenueCalendar {
                coverage_start: venue.coverage_start,
                coverage_end: venue.coverage_end,
                sessions,
            },
        );
    }
    if issues.is_empty() {
        Ok(venues)
    } else {
        Err(std::mem::take(issues))
    }
}

fn selected_instruments(
    snapshot: &SecurityEvidenceSnapshot,
    issues: &mut Vec<NormalizationIssue>,
) -> BTreeMap<InstrumentId, SelectedInstrument> {
    let mut selected = BTreeMap::new();
    for entry in &snapshot.instruments {
        let id = entry.record.instrument_id.clone();
        match &entry.publication {
            PublicationStatus::Publishable { terms } => {
                selected.insert(
                    id,
                    SelectedInstrument {
                        kind: entry.record.kind,
                        terms: *terms,
                        listing_date: entry.record.listing_date.clone(),
                    },
                );
            }
            PublicationStatus::NotPublishable { issues: blocked } => {
                let reasons = blocked
                    .iter()
                    .map(|item| format!("{}:{}", item.field, item.code.as_str()))
                    .collect::<Vec<_>>()
                    .join(",");
                issues.push(issue(
                    "instrument_not_publishable",
                    id.to_string(),
                    format!("pinned security evidence is not publishable ({reasons})"),
                ));
            }
        }
    }
    selected
}

fn decode_daily(
    raw: &RawInput,
    venues: &BTreeMap<VenueId, VenueCalendar>,
    instruments: &BTreeMap<InstrumentId, SelectedInstrument>,
    acc: &mut Acc,
) {
    let object = json_object(&raw.bytes).expect("classified daily response");
    if object.get("code").and_then(cell_text).as_deref() != Some("0") {
        acc.issues.push(issue(
            "invalid_daily_response",
            raw.raw_sha256.clone(),
            "Tencent response code is not zero",
        ));
        return;
    }
    let data = object["data"].as_object().expect("classified data object");
    for (native_symbol, node) in data {
        let Some((venue, symbol)) = native_key(native_symbol) else {
            acc.issues.push(issue(
                "unsupported_native_symbol",
                native_symbol,
                "daily response key must be sh or sz plus six digits",
            ));
            continue;
        };
        let Ok(instrument_id) = InstrumentId::new(symbol.clone(), venue.clone()) else {
            acc.issues.push(issue(
                "invalid_instrument_id",
                native_symbol,
                "native key does not form a valid InstrumentId",
            ));
            continue;
        };
        let Some(instrument) = instruments.get(&instrument_id) else {
            acc.issues.push(issue(
                "instrument_not_in_pinned_snapshot",
                instrument_id.to_string(),
                "daily response has no publishable pinned security evidence",
            ));
            continue;
        };
        if !has_pinned_source_record(raw, &instrument_id) {
            acc.issues.push(issue(
                "source_method_mismatch",
                instrument_id.to_string(),
                format!(
                    "daily Raw needs a {SOURCE_ID} source record with method_version {SOURCE_METHOD_VERSION} for this Instrument"
                ),
            ));
            continue;
        }
        let Some(rows) = node.get("day").and_then(Value::as_array) else {
            acc.issues.push(issue(
                "invalid_daily_response",
                format!("{native_symbol}.day"),
                "unadjusted day array is missing",
            ));
            continue;
        };
        for (index, row) in rows.iter().enumerate() {
            let row_path = format!("{native_symbol}.day[{index}]");
            let Some(cells) = row.as_array().filter(|cells| cells.len() >= 6) else {
                acc.issues.push(issue(
                    "invalid_daily_row",
                    &row_path,
                    "row needs date, open, close, high, low and lots",
                ));
                continue;
            };
            if cells.len() > 6 {
                acc.ignored_extra_fields += 1;
            }
            decode_row(
                &row_path,
                cells,
                &instrument_id,
                instrument,
                venues,
                raw,
                acc,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn decode_row(
    row_path: &str,
    cells: &[Value],
    instrument_id: &InstrumentId,
    instrument: &SelectedInstrument,
    venues: &BTreeMap<VenueId, VenueCalendar>,
    raw: &RawInput,
    acc: &mut Acc,
) {
    let Some(text) = cells[0].as_str() else {
        acc.issues.push(issue(
            "invalid_date",
            format!("{row_path}.date"),
            "date must be a YYYY-MM-DD string",
        ));
        return;
    };
    let Ok(date) = NaiveDate::parse_from_str(text, "%Y-%m-%d") else {
        acc.issues.push(issue(
            "invalid_date",
            format!("{row_path}.date"),
            "date must be a YYYY-MM-DD string",
        ));
        return;
    };
    let venue = instrument_id.venue();
    let Some(calendar) = venues.get(venue) else {
        acc.issues.push(issue(
            "missing_venue_calendar",
            venue.to_string(),
            "pinned calendar has no session list for this venue",
        ));
        return;
    };
    if !calendar.sessions.contains_key(&date) {
        let code = if (calendar.coverage_start..=calendar.coverage_end).contains(&date) {
            "bar_on_non_session_date"
        } else {
            "bar_outside_calendar_coverage"
        };
        acc.issues.push(issue(
            code,
            row_path,
            format!("{instrument_id}@{date} has no pinned {venue} session"),
        ));
        return;
    }
    let Some(open) = price_field(cells, 1, "open", row_path, acc) else {
        return;
    };
    let Some(close) = price_field(cells, 2, "close", row_path, acc) else {
        return;
    };
    let Some(high) = price_field(cells, 3, "high", row_path, acc) else {
        return;
    };
    let Some(low) = price_field(cells, 4, "low", row_path, acc) else {
        return;
    };
    let Some(lots) = quantity_field(cells, 5, "lots", row_path, acc) else {
        return;
    };
    if lots < Quantity::ZERO {
        acc.issues.push(issue(
            "negative_volume",
            format!("{row_path}.lots"),
            "volume must not be negative",
        ));
        return;
    }
    let Some(volume) = lots.checked_mul(volume_multiplier(instrument.kind)).ok() else {
        acc.issues.push(issue(
            "decimal_out_of_range",
            format!("{row_path}.lots"),
            "volume conversion exceeds the fixed-point range",
        ));
        return;
    };
    if open <= Price::ZERO
        || close <= Price::ZERO
        || high <= Price::ZERO
        || low <= Price::ZERO
        || high < open
        || high < close
        || low > open
        || low > close
        || low > high
    {
        acc.issues.push(issue(
            "invalid_ohlc",
            row_path,
            "prices must be positive and satisfy low <= open/close <= high",
        ));
        return;
    }
    let observation = Observation {
        open,
        high,
        low,
        close,
        volume,
        raw_sha256: raw.raw_sha256.clone(),
    };
    let key = (instrument_id.clone(), date);
    match acc.observations.get_mut(&key) {
        Some(existing) if same_facts(existing, &observation) => {
            acc.identical_duplicates += 1;
            if observation.raw_sha256 < existing.raw_sha256 {
                existing.raw_sha256 = observation.raw_sha256;
            }
        }
        Some(existing) => acc.issues.push(issue(
            "conflicting_duplicate_bar",
            row_path,
            format!(
                "{instrument_id}@{date} conflicts with {existing_path}",
                existing_path = existing.raw_sha256
            ),
        )),
        None => {
            acc.observations.insert(key, observation);
        }
    }
}

fn same_facts(left: &Observation, right: &Observation) -> bool {
    left.open == right.open
        && left.high == right.high
        && left.low == right.low
        && left.close == right.close
        && left.volume == right.volume
}

fn volume_multiplier(kind: InstrumentKind) -> i128 {
    match kind {
        InstrumentKind::Equity | InstrumentKind::Etf => VOLUME_MULTIPLIER,
        _ => 1,
    }
}

fn price_field(
    cells: &[Value],
    index: usize,
    label: &str,
    path: &str,
    acc: &mut Acc,
) -> Option<Price> {
    let Some(text) = cells.get(index).and_then(Value::as_str) else {
        acc.issues.push(issue(
            "invalid_decimal",
            format!("{path}.{label}"),
            format!("{label} must be a decimal string"),
        ));
        return None;
    };
    match Price::parse(text) {
        Ok(value) => Some(value),
        Err(error) => {
            acc.issues.push(issue(
                decimal_code(error),
                format!("{path}.{label}"),
                format!("{label} must be an exact scale-18 decimal"),
            ));
            None
        }
    }
}

fn quantity_field(
    cells: &[Value],
    index: usize,
    label: &str,
    path: &str,
    acc: &mut Acc,
) -> Option<Quantity> {
    let Some(text) = cells.get(index).and_then(Value::as_str) else {
        acc.issues.push(issue(
            "invalid_decimal",
            format!("{path}.{label}"),
            format!("{label} must be a decimal string"),
        ));
        return None;
    };
    match Quantity::parse(text) {
        Ok(value) => Some(value),
        Err(error) => {
            acc.issues.push(issue(
                decimal_code(error),
                format!("{path}.{label}"),
                format!("{label} must be an exact scale-18 decimal"),
            ));
            None
        }
    }
}

fn decimal_code(error: FixedPointError) -> &'static str {
    match error {
        FixedPointError::InexactScale => "inexact_decimal",
        FixedPointError::OutOfRange | FixedPointError::Overflow => "decimal_out_of_range",
        _ => "invalid_decimal",
    }
}

fn native_key(key: &str) -> Option<(VenueId, String)> {
    let (venue, symbol) = if let Some(symbol) = key.strip_prefix("sh") {
        ("XSHG", symbol)
    } else {
        let symbol = key.strip_prefix("sz")?;
        ("XSHE", symbol)
    };
    if symbol.len() != 6 || !symbol.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some((VenueId::new(venue).ok()?, symbol.to_owned()))
}

fn cell_text(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn build_output(
    config: &CnStockEtfDaily,
    venues: BTreeMap<VenueId, VenueCalendar>,
    instruments: BTreeMap<InstrumentId, SelectedInstrument>,
    acc: Acc,
) -> Result<NormalizationOutput, Vec<NormalizationIssue>> {
    let mut issues = Vec::new();
    let sessions = venues
        .values()
        .flat_map(|venue| venue.sessions.values().cloned())
        .collect::<Vec<_>>();
    let session_by_key: BTreeMap<(VenueId, NaiveDate), Session> = sessions
        .iter()
        .map(|session| {
            (
                (session.venue_id().clone(), session.session_date()),
                session.clone(),
            )
        })
        .collect();
    let currency = Currency::new("CNY").expect("static currency is valid");
    let mut specs = Vec::new();
    for (id, selected) in &instruments {
        let data = InstrumentSpecData {
            id: id.clone(),
            kind: selected.kind,
            native_symbol: id.symbol().to_owned(),
            market_segment: None,
            base_currency: None,
            quote_currency: Some(currency.clone()),
            settle_currency: None,
            is_inverse: false,
            multiplier: None,
            expiry: None,
            price_increment: selected.terms.price_increment,
            size_increment: selected.terms.size_increment,
            lot_size: Some(selected.terms.lot_size),
            session_timezone: Some("Asia/Shanghai".into()),
        };
        match InstrumentSpec::new(data) {
            Ok(spec) => specs.push(spec),
            Err(error) => issues.push(issue(
                "invalid_instrument_spec",
                id.to_string(),
                error.to_string(),
            )),
        }
    }
    let mut bars = Vec::new();
    let mut bar_sources = Vec::new();
    for ((id, date), observation) in &acc.observations {
        let Some(session) = session_by_key.get(&(id.venue().clone(), *date)) else {
            issues.push(issue(
                "missing_session_for_bar",
                format!("{id}@{date}"),
                "bar has no pinned session",
            ));
            continue;
        };
        let data = BarData {
            instrument_id: id.clone(),
            bar_spec: "1d@session".parse().expect("static bar spec is valid"),
            session_date: *date,
            ts_open: session.ts_open(),
            ts_close: session.ts_close(),
            open: observation.open,
            high: observation.high,
            low: observation.low,
            close: observation.close,
            volume: observation.volume,
            amount: None,
            available_at: None,
        };
        match Bar::new(data, Some(session)) {
            Ok(bar) => {
                bar_sources.push(BarSourceRow {
                    instrument_id: id.clone(),
                    session_date: *date,
                    source: SOURCE_ID.into(),
                    source_raw_sha256: observation.raw_sha256.clone(),
                    reference_price: None,
                });
                bars.push(bar);
            }
            Err(error) => issues.push(issue(
                "invalid_bar",
                format!("{id}@{date}"),
                error.to_string(),
            )),
        }
    }
    if !issues.is_empty() {
        return Err(issues);
    }

    let convert =
        |error: crate::DataError| vec![issue("table_conversion_failed", "$", error.to_string())];
    let bar_sources = bar_sources_to_record_batch(&bar_sources).map_err(convert)?;
    let tables = NormalizedTables {
        instruments: instruments_to_record_batch(&specs).map_err(convert)?,
        sessions: sessions_to_record_batch(&sessions).map_err(convert)?,
        bars: bars_to_record_batch(&bars).map_err(convert)?,
        execution_status: None,
        source_crosscheck: None,
        announcements: None,
        corporate_actions: None,
        bar_sources: Some(bar_sources),
        tdx_code_table: None,
    };

    let sessions_by_venue = venues
        .iter()
        .map(|(venue, calendar)| (venue.to_string(), json!(calendar.sessions.len())))
        .collect::<Map<_, _>>();
    let instruments_by_kind = ["Equity", "Etf"]
        .into_iter()
        .map(|label| {
            let count = instruments
                .values()
                .filter(|instrument| {
                    matches!(
                        (label, instrument.kind),
                        ("Equity", InstrumentKind::Equity) | ("Etf", InstrumentKind::Etf)
                    )
                })
                .count();
            (label.to_owned(), json!(count))
        })
        .collect::<Map<_, _>>();
    let known_listing = instruments
        .values()
        .filter(|instrument| matches!(instrument.listing_date, FieldEvidence::Known { .. }))
        .count();
    let session_keys = sessions
        .iter()
        .map(|session| (session.venue_id().clone(), session.session_date()))
        .collect::<BTreeSet<_>>();
    let bar_session_keys = acc
        .observations
        .keys()
        .map(|(id, date)| (id.venue().clone(), *date))
        .collect::<BTreeSet<_>>();
    let expected_instrument_sessions: usize = instruments
        .keys()
        .map(|id| {
            venues
                .get(id.venue())
                .map_or(0, |calendar| calendar.sessions.len())
        })
        .sum();
    let missing_instrument_sessions = expected_instrument_sessions.saturating_sub(bars.len());
    let coverage = json!({
        "kind": CN_DAILY_NORMALIZER_ID,
        "normalizer_version": CN_DAILY_NORMALIZER_VERSION,
        "calendar_version": config.calendar_version.as_str(),
        "security_snapshot": config.snapshot_id.as_str(),
        "source": SOURCE_ID,
        "source_method_version": SOURCE_METHOD_VERSION,
        "raw_objects": {
            "calendar": 1,
            "security_snapshot": 1,
            "tencent_daily": acc.daily_raws,
        },
        "sessions_by_venue": sessions_by_venue,
        "instruments_by_kind": instruments_by_kind,
        "instruments": instruments.len(),
        "instruments_with_known_listing_date": known_listing,
        "bars": bars.len(),
        "identical_duplicate_bars": acc.identical_duplicates,
        "ignored_row_fields_beyond_six": acc.ignored_extra_fields,
        "calendar_sessions_without_any_bar": session_keys.difference(&bar_session_keys).count(),
        "missing_instrument_sessions_unknown": missing_instrument_sessions,
        "available_at": "unknown_null",
        "amount": "absent_from_source_null",
        "units": {
            "price": "exact_decimal_literal_CNY",
            "volume": "lots_x_100",
            "amount": "absent_null"
        },
        "adjustment": "none",
        "execution_status": "not_derived",
        "completeness_claim": "none; coverage is limited to the pinned Raw set and calendar versions; missing quotes are not halts"
    });
    Ok(NormalizationOutput {
        tables,
        coverage,
        synthetic_assumptions: Vec::new(),
        ignored_input_fields: vec![
            "tencent.day[6..]: unsupported extras are ignored; amount remains unknown".into(),
        ],
    })
}
