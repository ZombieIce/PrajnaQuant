//! Normalizer for frozen Shenzhen ETF source extracts (MVP-6, ticket #174).
//!
//! Each Raw object is a JSON envelope `{"frozen_format_version":1,"kind":...}`
//! produced by a local extraction step from a frozen vendor file. Decoding the
//! vendor binary formats (TDX ZIP/`.cod`/`.md1`) is outside this Normalizer.
//!
//! Kinds: `szse_calendar`, `tdx_daily` (primary Bars), `tencent_kline`
//! (secondary, comparison only) and `cninfo_announcements` (index only).
//! Missing Bars are never interpreted as halts; no execution status is derived.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use prajna_domain::{
    Bar, BarData, Currency, InstrumentId, InstrumentKind, InstrumentSpec, InstrumentSpecData,
    Notional, Price, Quantity, Session, TimestampNs, VenueId,
};
use serde::Deserialize;
use serde_json::{Number, json};
use sha2::{Digest, Sha256};

use crate::{
    AnnouncementRow, NormalizationIssue, NormalizationOutput, NormalizedTables,
    SourceCrosscheckRow, SourceKind, announcements_to_record_batch, bars_to_record_batch,
    instruments_to_record_batch, sessions_to_record_batch, source_crosscheck_to_record_batch,
};

use super::{Normalizer, RawInput};

const NORMALIZER_ID: &str = "szse-etf-frozen";
const CONFIG_V1: &str = r#"{"announcement_available_at":"announced_date@15:00:00+08:00","bar_spec":"1d@session","bars":"tdx_daily","crosscheck":"tencent_kline_exact_ohlc_shares","execution_status":"not_derived","instrument_kind":"Etf","lot_size":100,"price_increment":"0.001","session_close":"15:00:00+08:00","session_open":"09:30:00+08:00","timezone":"Asia/Shanghai","venue":"SZSE","volume":"shares","tencent_volume":"lots*100"}"#;
const CATEGORIES: [&str; 9] = [
    "listing_notice",
    "fund_contract",
    "prospectus",
    "underlying_index",
    "termination",
    "dividend",
    "conversion",
    "suspension",
    "resumption",
];

pub(super) struct SzseEtfFrozen {
    version: &'static str,
}

impl SzseEtfFrozen {
    pub(super) const fn v1() -> Self {
        Self { version: "1" }
    }
}

impl Normalizer for SzseEtfFrozen {
    fn id(&self) -> &str {
        NORMALIZER_ID
    }

    fn version(&self) -> &str {
        self.version
    }

    fn config_sha256(&self) -> String {
        format!("sha256:{:x}", Sha256::digest(CONFIG_V1.as_bytes()))
    }

    fn accepted_source_kinds(&self) -> &[SourceKind] {
        &[SourceKind::FileImport, SourceKind::VendorApi]
    }

    fn normalize(
        &self,
        raw_inputs: &[RawInput],
    ) -> Result<NormalizationOutput, Vec<NormalizationIssue>> {
        normalize(raw_inputs)
    }
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Envelope {
    SzseCalendar {
        frozen_format_version: u32,
        sessions: Vec<String>,
    },
    TdxDaily {
        frozen_format_version: u32,
        bars: Vec<TdxBar>,
    },
    TencentKline {
        frozen_format_version: u32,
        rows: Vec<TencentRow>,
    },
    CninfoAnnouncements {
        frozen_format_version: u32,
        items: Vec<AnnouncementItem>,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TdxBar {
    symbol: String,
    date: String,
    open: Number,
    high: Number,
    low: Number,
    close: Number,
    volume: Number,
    amount: Number,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TencentRow {
    symbol: String,
    date: String,
    open: Number,
    high: Number,
    low: Number,
    close: Number,
    volume_lots: Number,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AnnouncementItem {
    id: String,
    symbol: String,
    category: String,
    announced_date: String,
}

#[derive(Clone, PartialEq, Eq)]
struct Ohlcv {
    open: Price,
    high: Price,
    low: Price,
    close: Price,
    shares: Quantity,
}

struct Primary {
    ohlcv: Ohlcv,
    amount: Notional,
    raw: String,
}

fn normalize(raw_inputs: &[RawInput]) -> Result<NormalizationOutput, Vec<NormalizationIssue>> {
    let mut issues = Vec::new();
    let venue = VenueId::new("SZSE").expect("static venue is valid");
    let mut kind_counts: BTreeMap<&str, u64> = BTreeMap::new();
    let mut calendar = BTreeSet::new();
    let mut primary: BTreeMap<(String, NaiveDate), Primary> = BTreeMap::new();
    let mut secondary: BTreeMap<(String, NaiveDate), (Ohlcv, String)> = BTreeMap::new();
    let mut announcements: BTreeMap<String, AnnouncementRow> = BTreeMap::new();
    let mut announcement_content: BTreeMap<String, (String, String, NaiveDate)> = BTreeMap::new();
    let mut seen_raw = BTreeSet::new();

    for (index, raw) in raw_inputs.iter().enumerate() {
        let path = format!("inputs[{index}]");
        if !raw.has_valid_identity() {
            issues.push(issue(
                "raw_hash_mismatch",
                &path,
                "raw_sha256 does not match the supplied bytes",
            ));
            continue;
        }
        if !seen_raw.insert(raw.raw_sha256.clone()) {
            issues.push(issue(
                "duplicate_raw_input",
                &path,
                "Raw object listed twice",
            ));
            continue;
        }
        let envelope: Envelope = match serde_json::from_slice(&raw.bytes) {
            Ok(envelope) => envelope,
            Err(error) => {
                issues.push(issue(
                    "invalid_envelope",
                    &path,
                    format!("frozen envelope cannot be parsed: {error}"),
                ));
                continue;
            }
        };
        match envelope {
            Envelope::SzseCalendar {
                frozen_format_version,
                sessions,
            } => {
                check_version(frozen_format_version, &path, &mut issues);
                *kind_counts.entry("szse_calendar").or_default() += 1;
                for (i, text) in sessions.iter().enumerate() {
                    if let Some(date) =
                        parse_date(text, &format!("{path}.sessions[{i}]"), &mut issues)
                        && !calendar.insert(date)
                    {
                        issues.push(issue(
                            "duplicate_primary_key",
                            format!("{path}.sessions[{i}]"),
                            format!("duplicate session date {date}"),
                        ));
                    }
                }
            }
            Envelope::TdxDaily {
                frozen_format_version,
                bars,
            } => {
                check_version(frozen_format_version, &path, &mut issues);
                *kind_counts.entry("tdx_daily").or_default() += 1;
                for (i, bar) in bars.iter().enumerate() {
                    let bar_path = format!("{path}.bars[{i}]");
                    let Some((key, ohlcv)) = parse_ohlcv(
                        &bar.symbol,
                        &bar.date,
                        [&bar.open, &bar.high, &bar.low, &bar.close],
                        &bar.volume,
                        1,
                        &bar_path,
                        &mut issues,
                    ) else {
                        continue;
                    };
                    let Some(amount) = parse_amount(&bar.amount, &bar_path, &mut issues) else {
                        continue;
                    };
                    insert_unique(
                        &mut primary,
                        key,
                        Primary {
                            ohlcv,
                            amount,
                            raw: raw.raw_sha256.clone(),
                        },
                        |a, b| a.ohlcv == b.ohlcv && a.amount == b.amount,
                        &bar_path,
                        &mut issues,
                    );
                }
            }
            Envelope::TencentKline {
                frozen_format_version,
                rows,
            } => {
                check_version(frozen_format_version, &path, &mut issues);
                *kind_counts.entry("tencent_kline").or_default() += 1;
                for (i, row) in rows.iter().enumerate() {
                    let row_path = format!("{path}.rows[{i}]");
                    let Some((key, ohlcv)) = parse_ohlcv(
                        &row.symbol,
                        &row.date,
                        [&row.open, &row.high, &row.low, &row.close],
                        &row.volume_lots,
                        100,
                        &row_path,
                        &mut issues,
                    ) else {
                        continue;
                    };
                    insert_unique(
                        &mut secondary,
                        key,
                        (ohlcv, raw.raw_sha256.clone()),
                        |a, b| a.0 == b.0,
                        &row_path,
                        &mut issues,
                    );
                }
            }
            Envelope::CninfoAnnouncements {
                frozen_format_version,
                items,
            } => {
                check_version(frozen_format_version, &path, &mut issues);
                *kind_counts.entry("cninfo_announcements").or_default() += 1;
                for (i, item) in items.iter().enumerate() {
                    let item_path = format!("{path}.items[{i}]");
                    if item.id.is_empty() {
                        issues.push(issue("invalid_announcement_id", &item_path, "empty id"));
                        continue;
                    }
                    if !CATEGORIES.contains(&item.category.as_str()) {
                        issues.push(issue(
                            "unknown_announcement_category",
                            format!("{item_path}.category"),
                            "category is not in the versioned category list",
                        ));
                        continue;
                    }
                    let Some(date) = parse_date(
                        &item.announced_date,
                        &format!("{item_path}.announced_date"),
                        &mut issues,
                    ) else {
                        continue;
                    };
                    let Some(id) = instrument_id(&item.symbol, &venue, &item_path, &mut issues)
                    else {
                        continue;
                    };
                    let content = (item.symbol.clone(), item.category.clone(), date);
                    if let Some(previous) = announcement_content.get(&item.id) {
                        let code = if previous == &content {
                            "duplicate_primary_key"
                        } else {
                            "source_revision_conflict"
                        };
                        issues.push(issue(
                            code,
                            &item_path,
                            format!("announcement {} occurs more than once", item.id),
                        ));
                        continue;
                    }
                    announcement_content.insert(item.id.clone(), content);
                    // P6: a date-only announcement is knowable after that day's close.
                    let Ok(available_at) = TimestampNs::parse(&format!("{date}T15:00:00+08:00"))
                    else {
                        issues.push(issue("invalid_date", &item_path, "unsupported date"));
                        continue;
                    };
                    announcements.insert(
                        item.id.clone(),
                        AnnouncementRow {
                            announcement_id: item.id.clone(),
                            instrument_id: id,
                            category: item.category.clone(),
                            announced_date: date,
                            available_at,
                            source_raw_sha256: raw.raw_sha256.clone(),
                        },
                    );
                }
            }
        }
    }

    if kind_counts.get("szse_calendar").copied().unwrap_or(0) == 0 {
        issues.push(issue("missing_source", "$", "no szse_calendar Raw object"));
    }
    if kind_counts.get("tdx_daily").copied().unwrap_or(0) == 0 {
        issues.push(issue("missing_source", "$", "no tdx_daily Raw object"));
    }
    for (symbol, date) in primary.keys().chain(secondary.keys()) {
        if !calendar.contains(date) {
            issues.push(issue(
                "bar_outside_calendar",
                format!("{symbol}@{date}"),
                "source row is not on a frozen SZSE session",
            ));
        }
    }
    if !issues.is_empty() {
        return Err(issues);
    }

    let sessions = calendar
        .iter()
        .map(|date| make_session(venue.clone(), *date))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| vec![issue("invalid_session", "$", "session cannot be built")])?;
    let session_by_date: BTreeMap<_, _> = sessions.iter().map(|s| (s.session_date(), s)).collect();

    let symbols: BTreeSet<&String> = primary.keys().map(|(symbol, _)| symbol).collect();
    let currency = Currency::new("CNY").expect("static currency is valid");
    let price_increment = Price::parse("0.001").expect("static increment is valid");
    let mut specs = Vec::new();
    for symbol in &symbols {
        let id = InstrumentId::new((*symbol).clone(), venue.clone()).expect("validated symbol");
        specs.push(
            InstrumentSpec::new(InstrumentSpecData {
                id,
                kind: InstrumentKind::Etf,
                native_symbol: (*symbol).clone(),
                market_segment: None,
                base_currency: None,
                quote_currency: Some(currency.clone()),
                settle_currency: None,
                is_inverse: false,
                multiplier: None,
                expiry: None,
                price_increment,
                size_increment: Quantity::parse("1").expect("static increment is valid"),
                lot_size: Some(Quantity::parse("100").expect("static lot is valid")),
                session_timezone: Some("Asia/Shanghai".into()),
            })
            .map_err(|error| vec![issue("invalid_instrument_spec", "$", error.to_string())])?,
        );
    }

    let mut bars = Vec::new();
    for ((symbol, date), value) in &primary {
        let session = session_by_date[date];
        let data = BarData {
            instrument_id: InstrumentId::new(symbol.clone(), venue.clone()).expect("validated"),
            bar_spec: "1d@session".parse().expect("static bar spec is valid"),
            session_date: *date,
            ts_open: session.ts_open(),
            ts_close: session.ts_close(),
            open: value.ohlcv.open,
            high: value.ohlcv.high,
            low: value.ohlcv.low,
            close: value.ohlcv.close,
            volume: value.ohlcv.shares,
            amount: Some(value.amount),
            available_at: Some(session.ts_close()),
        };
        bars.push(Bar::new(data, Some(session)).map_err(|error| {
            vec![issue(
                "invalid_bar",
                format!("{symbol}@{date}"),
                error.to_string(),
            )]
        })?);
    }

    let mut crosscheck = Vec::new();
    let mut crosscheck_counts: BTreeMap<&str, u64> = BTreeMap::new();
    let keys: BTreeSet<_> = primary.keys().chain(secondary.keys()).cloned().collect();
    for key in keys {
        let (status, fields, p_raw, s_raw) = match (primary.get(&key), secondary.get(&key)) {
            (Some(p), Some((s, s_raw))) => {
                let fields = mismatch_fields(&p.ohlcv, s);
                let status = if fields.is_empty() {
                    "match"
                } else {
                    "mismatch"
                };
                (status, fields, Some(p.raw.clone()), Some(s_raw.clone()))
            }
            (Some(p), None) => ("primary_only", String::new(), Some(p.raw.clone()), None),
            (None, Some((_, s_raw))) => {
                ("secondary_only", String::new(), None, Some(s_raw.clone()))
            }
            (None, None) => continue,
        };
        *crosscheck_counts.entry(status).or_default() += 1;
        crosscheck.push(SourceCrosscheckRow {
            instrument_id: InstrumentId::new(key.0.clone(), venue.clone()).expect("validated"),
            session_date: key.1,
            status: status.into(),
            mismatch_fields: fields,
            primary_raw_sha256: p_raw,
            secondary_raw_sha256: s_raw,
        });
    }

    // Sessions between an instrument's first and last primary Bar without a Bar
    // are gaps (UNKNOWN), never halts.
    let mut gap_sessions = 0_u64;
    for symbol in &symbols {
        let dates: Vec<_> = primary
            .keys()
            .filter(|(s, _)| s == *symbol)
            .map(|(_, d)| *d)
            .collect();
        if let (Some(first), Some(last)) = (dates.first(), dates.last()) {
            gap_sessions += calendar
                .range(*first..=*last)
                .filter(|date| !dates.contains(date))
                .count() as u64;
        }
    }
    let mut category_counts: BTreeMap<String, u64> = BTreeMap::new();
    for row in announcements.values() {
        *category_counts.entry(row.category.clone()).or_default() += 1;
    }
    let announcement_rows: Vec<_> = announcements.into_values().collect();

    let convert =
        |error: crate::DataError| vec![issue("table_conversion_failed", "$", error.to_string())];
    let tables = NormalizedTables {
        instruments: instruments_to_record_batch(&specs).map_err(convert)?,
        sessions: sessions_to_record_batch(&sessions).map_err(convert)?,
        bars: bars_to_record_batch(&bars).map_err(convert)?,
        execution_status: None,
        source_crosscheck: Some(source_crosscheck_to_record_batch(&crosscheck).map_err(convert)?),
        announcements: Some(announcements_to_record_batch(&announcement_rows).map_err(convert)?),
    };
    let coverage = json!({
        "kind": "szse-etf-frozen",
        "raw_objects_by_kind": kind_counts,
        "sessions": sessions.len(),
        "instruments": specs.len(),
        "bars": bars.len(),
        "primary_gap_sessions_unknown": gap_sessions,
        "crosscheck_by_status": crosscheck_counts,
        "announcements": announcement_rows.len(),
        "announcements_by_category": category_counts,
        "execution_status": "not_derived",
        "completeness_claim": "none; coverage is limited to the frozen Raw set",
    });
    Ok(NormalizationOutput {
        tables,
        coverage,
        synthetic_assumptions: Vec::new(),
        ignored_input_fields: Vec::new(),
    })
}

fn insert_unique<V>(
    map: &mut BTreeMap<(String, NaiveDate), V>,
    key: (String, NaiveDate),
    value: V,
    same: impl Fn(&V, &V) -> bool,
    path: &str,
    issues: &mut Vec<NormalizationIssue>,
) {
    if let Some(previous) = map.get(&key) {
        let code = if same(previous, &value) {
            "duplicate_primary_key"
        } else {
            "source_revision_conflict"
        };
        issues.push(issue(
            code,
            path,
            format!("{}@{} occurs more than once in the Raw set", key.0, key.1),
        ));
    } else {
        map.insert(key, value);
    }
}

fn mismatch_fields(left: &Ohlcv, right: &Ohlcv) -> String {
    let mut fields = Vec::new();
    if left.open != right.open {
        fields.push("open");
    }
    if left.high != right.high {
        fields.push("high");
    }
    if left.low != right.low {
        fields.push("low");
    }
    if left.close != right.close {
        fields.push("close");
    }
    if left.shares != right.shares {
        fields.push("volume");
    }
    fields.join(",")
}

#[allow(clippy::too_many_arguments)]
fn parse_ohlcv(
    symbol: &str,
    date: &str,
    prices: [&Number; 4],
    volume: &Number,
    volume_multiplier: i128,
    path: &str,
    issues: &mut Vec<NormalizationIssue>,
) -> Option<((String, NaiveDate), Ohlcv)> {
    let venue = VenueId::new("SZSE").expect("static venue is valid");
    instrument_id(symbol, &venue, path, issues)?;
    let date = parse_date(date, &format!("{path}.date"), issues)?;
    let tick = Price::parse("0.001").expect("static increment is valid");
    let mut parsed = Vec::new();
    for (name, number) in ["open", "high", "low", "close"].iter().zip(prices) {
        let value = match Price::parse(&number.to_string()) {
            Ok(value) => value,
            Err(error) => {
                issues.push(issue(
                    "invalid_decimal",
                    format!("{path}.{name}"),
                    error.to_string(),
                ));
                return None;
            }
        };
        if value <= Price::ZERO || !value.is_aligned_to(tick).unwrap_or(false) {
            issues.push(issue(
                "invalid_price",
                format!("{path}.{name}"),
                "price must be positive and aligned to 0.001",
            ));
            return None;
        }
        parsed.push(value);
    }
    let (open, high, low, close) = (parsed[0], parsed[1], parsed[2], parsed[3]);
    if high < open.max(close) || low > open.min(close) {
        issues.push(issue(
            "invalid_ohlc",
            path,
            "high/low do not bound open and close",
        ));
        return None;
    }
    let shares = match Quantity::parse(&volume.to_string())
        .and_then(|value| value.checked_mul(volume_multiplier))
    {
        Ok(value)
            if value >= Quantity::ZERO
                && value
                    .is_aligned_to(Quantity::parse("1").unwrap())
                    .unwrap_or(false) =>
        {
            value
        }
        _ => {
            issues.push(issue(
                "invalid_volume",
                format!("{path}.volume"),
                "volume must be a non-negative whole number of shares",
            ));
            return None;
        }
    };
    Some((
        (symbol.to_owned(), date),
        Ohlcv {
            open,
            high,
            low,
            close,
            shares,
        },
    ))
}

fn parse_amount(
    number: &Number,
    path: &str,
    issues: &mut Vec<NormalizationIssue>,
) -> Option<Notional> {
    match Notional::parse(&number.to_string()) {
        Ok(value) if value >= Notional::ZERO => Some(value),
        _ => {
            issues.push(issue(
                "invalid_amount",
                format!("{path}.amount"),
                "amount must be a non-negative decimal",
            ));
            None
        }
    }
}

fn instrument_id(
    symbol: &str,
    venue: &VenueId,
    path: &str,
    issues: &mut Vec<NormalizationIssue>,
) -> Option<InstrumentId> {
    if symbol.len() != 6 || !symbol.bytes().all(|byte| byte.is_ascii_digit()) {
        issues.push(issue(
            "invalid_symbol",
            format!("{path}.symbol"),
            "symbol must be six ASCII digits",
        ));
        return None;
    }
    InstrumentId::new(symbol, venue.clone()).ok()
}

fn parse_date(text: &str, path: &str, issues: &mut Vec<NormalizationIssue>) -> Option<NaiveDate> {
    match NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        Ok(date) => Some(date),
        Err(_) => {
            issues.push(issue("invalid_date", path, "dates must use YYYY-MM-DD"));
            None
        }
    }
}

fn check_version(version: u32, path: &str, issues: &mut Vec<NormalizationIssue>) {
    if version != 1 {
        issues.push(issue(
            "unsupported_frozen_format_version",
            path,
            "only frozen_format_version 1 is supported",
        ));
    }
}

fn make_session(venue: VenueId, date: NaiveDate) -> Option<Session> {
    let open = TimestampNs::parse(&format!("{date}T09:30:00+08:00")).ok()?;
    let close = TimestampNs::parse(&format!("{date}T15:00:00+08:00")).ok()?;
    Session::new(venue, date, open, close).ok()
}

fn issue(
    code: impl Into<String>,
    path: impl Into<String>,
    message: impl Into<String>,
) -> NormalizationIssue {
    NormalizationIssue::new(code, path, message)
}
