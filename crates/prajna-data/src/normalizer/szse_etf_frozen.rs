//! Normalizer for frozen Shenzhen ETF vendor originals (MVP-6, ticket #174).
//!
//! Every Raw input is an unmodified vendor response, recognised by content:
//!
//! - SZSE `monthList` JSON: one complete natural month, `jyrq` + `jybz` per day.
//! - TDX `g4day/YYYYMMDD.zip`: `szYYMMDD.cod` 150-byte records whose
//!   little-endian u16 at offset 32 indexes a 512-byte `.md1` block (OHLC f64 at
//!   12/20/28/36, volume u64 shares at 56, amount f64 at 72). Primary Bars.
//! - Tencent `fqkline` JSON: unadjusted `day` rows `[date, open, close, high,
//!   low, lots, ...]`. V1/v2 comparison only; v3 fills absent TDX cells.
//! - cninfo `hisAnnouncement/query` JSON: announcement index only.
//!
//! The Dataset Version is therefore a pure function of the original bytes and
//! this Normalizer's version/config. V1/v2 scope is 159000–159999; v3 uses
//! configured inclusive code ranges (builtin: 158000–159999). V4 preserves v3
//! market data and adds a complete configured-scope TDX code-table snapshot.
//! Missing Bars are never interpreted as halts; no execution status is derived.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Cursor, Read},
};

use chrono::{DateTime, Datelike, Duration, NaiveDate};
use chrono_tz::Asia::Shanghai;
use prajna_domain::{
    Bar, BarData, Currency, InstrumentId, InstrumentKind, InstrumentSpec, InstrumentSpecData,
    Notional, Price, Quantity, Session, TimestampNs, VenueId,
};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use zip::ZipArchive;

use crate::{
    AnnouncementRow, CorporateActionKind, CorporateActionRow, NormalizationIssue,
    NormalizationOutput, NormalizedTables, SourceCrosscheckRow, SourceKind,
    announcements_to_record_batch, bars_to_record_batch, corporate_actions_to_record_batch,
    instruments_to_record_batch, sessions_to_record_batch, source_crosscheck_to_record_batch,
};

use super::{Normalizer, RawInput};

const NORMALIZER_ID: &str = "szse-etf-frozen";
const CONFIG_V1: &str = r#"{"announcement_available_at":"close_of_first_session_after_announced_date_calendar_covered","announcement_category":"title_keyword_rules_v1_else_unclassified","announcement_date":"announcementTime_ms_in_Asia/Shanghai","bar_available_at":"unknown_null","bar_spec":"1d@session","bars":"tdx_g4day_sz_cod150_md1_512","code_scope":"159000-159999","crosscheck":"tencent_fqkline_day_exact_ohlc_shares","execution_status":"not_derived","identical_duplicate_facts":"deduplicated_min_raw_sha256","inputs":"vendor_originals_only","instrument_kind":"Etf","lot_size":100,"price_increment":"0.001","session_close":"15:00:00+08:00","session_open":"09:30:00+08:00","tdx_amount":"f64@72_fixed10","tdx_no_trade":"close<=0_or_open_high_low_all_zero_keyed_no_bar","tdx_revision":"trade_vs_no_trade_conflict","tdx_price":"f64_rounded_to_0.001_tolerance_1e-6","tencent_volume":"lots*100","timezone":"Asia/Shanghai","venue":"SZSE","volume":"shares"}"#;
const CONFIG_V2: &str = r#"{"actions":"cninfo_corporateActions_structured_v1","action_dates":"ISO-8601","action_unknown_fields":"reject","announcement_available_at":"close_of_first_session_after_announced_date_calendar_covered","announcement_category":"title_keyword_rules_v1_else_unclassified","announcement_date":"announcementTime_ms_in_Asia/Shanghai","bar_available_at":"unknown_null","bar_spec":"1d@session","bars":"tdx_g4day_sz_cod150_md1_512","code_scope":"159000-159999","crosscheck":"tencent_fqkline_day_exact_ohlc_shares","execution_status":"not_derived","identical_duplicate_facts":"deduplicated_min_raw_sha256","inputs":"vendor_originals_only","instrument_kind":"Etf","lot_size":100,"price_increment":"0.001","session_close":"15:00:00+08:00","session_open":"09:30:00+08:00","tdx_amount":"f64@72_fixed10","tdx_no_trade":"close<=0_or_open_high_low_all_zero_keyed_no_bar","tdx_revision":"trade_vs_no_trade_conflict","tdx_price":"f64_rounded_to_0.001_tolerance_1e-6","tencent_volume":"lots*100","timezone":"Asia/Shanghai","venue":"SZSE","volume":"shares"}"#;

const MAX_MEMBER_BYTES: u64 = 256 * 1024 * 1024;
const UNCLASSIFIED: &str = "unclassified";
/// Ordered title rules; the first category whose keyword occurs in the title wins.
const CATEGORY_RULES: [(&str, &[&str]); 9] = [
    ("termination", &["终止上市", "基金合同终止", "终止基金合同"]),
    ("resumption", &["复牌"]),
    ("suspension", &["停牌"]),
    ("conversion", &["折算"]),
    ("dividend", &["分红", "收益分配"]),
    ("listing_notice", &["上市交易公告书"]),
    ("underlying_index", &["标的指数"]),
    ("prospectus", &["招募说明书"]),
    ("fund_contract", &["基金合同"]),
];

pub struct SzseEtfFrozen {
    version: &'static str,
    code_ranges: Vec<(u32, u32)>,
}

impl SzseEtfFrozen {
    pub(super) fn v1() -> Self {
        Self {
            version: "1",
            code_ranges: vec![(159000, 159999)],
        }
    }
    pub(super) fn v2() -> Self {
        Self {
            version: "2",
            code_ranges: vec![(159000, 159999)],
        }
    }
    /// Configure inclusive Shenzhen ETF code ranges. The canonical ranges enter
    /// config_sha256; replay requires registering the same configuration.
    pub fn v3(mut code_ranges: Vec<(u32, u32)>) -> Result<Self, String> {
        code_ranges.sort_unstable();
        if code_ranges.is_empty()
            || code_ranges
                .iter()
                .any(|&(a, b)| a < 100000 || a > b || b > 999999)
            || code_ranges.windows(2).any(|w| w[0].1 >= w[1].0)
        {
            return Err("code ranges must be nonempty, six-digit, ordered and disjoint".into());
        }
        Ok(Self {
            version: "3",
            code_ranges,
        })
    }

    /// V4 preserves the v3 market-data contract and publishes the complete
    /// configured TDX code table separately from Bars.
    pub fn v4(code_ranges: Vec<(u32, u32)>) -> Result<Self, String> {
        let mut normalizer = Self::v3(code_ranges)?;
        normalizer.version = "4";
        Ok(normalizer)
    }

    fn scoped(&self, code: &str) -> bool {
        code.len() == 6
            && code.bytes().all(|b| b.is_ascii_digit())
            && code
                .parse::<u32>()
                .is_ok_and(|n| self.code_ranges.iter().any(|&(a, b)| (a..=b).contains(&n)))
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
        if self.version == "3" || self.version == "4" {
            let mut config: Value = serde_json::from_str(CONFIG_V2).expect("static config");
            config["code_scope"] = json!(self.code_ranges);
            config["bars"] = json!("tdx_primary_tencent_missing_cell_only@1");
            config["reference_price"] = json!("tdx_f64@4_exchange_ex_rights_reference");
            config["source_table"] = json!("bar_sources@1");
            config["crosscheck"] = json!("diagnostic_only");
            config["fallback_amount"] = json!("null_missing_session_not_zero");
            config["tdx_no_trade"] = json!("tdx_key_blocks_fallback_including_zero_volume");
            if self.version == "4" {
                config["tdx_code_table"] = json!("tdx_code_table@1");
            }
            return format!(
                "sha256:{:x}",
                Sha256::digest(crate::restricted_jcs(&config).expect("static config"))
            );
        }
        let config = if self.version == "1" {
            CONFIG_V1
        } else {
            CONFIG_V2
        };
        format!("sha256:{:x}", Sha256::digest(config.as_bytes()))
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

type Key = (String, NaiveDate);

#[derive(Clone, PartialEq, Eq)]
struct Ohlcv {
    open: Price,
    high: Price,
    low: Price,
    close: Price,
    shares: Quantity,
}

#[derive(Clone, PartialEq, Eq)]
struct SelectedBar {
    ohlcv: Ohlcv,
    amount: Option<Notional>,
    reference_price: Option<Price>,
    source: &'static str,
}

#[derive(Clone, PartialEq, Eq)]
struct Announcement {
    symbol: String,
    category: String,
    date: NaiveDate,
}

#[derive(Clone)]
struct ActionDraft {
    announcement_id: String,
    action_index: u32,
    instrument_id: String,
    kind: CorporateActionKind,
    announced_date: NaiveDate,
    announced_at: TimestampNs,
    record_date: Option<NaiveDate>,
    ex_date: Option<NaiveDate>,
    pay_date: Option<NaiveDate>,
    effective_date: Option<NaiveDate>,
    cash_per_share: Option<String>,
    share_ratio: Option<String>,
    liquidation_cash_per_share: Option<String>,
    source_raw_sha256: String,
}

struct ActionSource<'a> {
    announcement_id: &'a str,
    action_index: u32,
    symbol: &'a str,
    announced_date: NaiveDate,
    announced_at: TimestampNs,
    source_raw_sha256: &'a str,
    path: &'a str,
}

/// A decoded fact and the smallest Raw hash that states it.
struct Sourced<V> {
    value: V,
    raw: String,
}

#[derive(Default)]
struct Acc {
    issues: Vec<NormalizationIssue>,
    kind_counts: BTreeMap<&'static str, u64>,
    calendar: BTreeMap<NaiveDate, Sourced<bool>>,
    // None is a keyed no-trade observation, retained until revision validation.
    primary: BTreeMap<Key, Sourced<Option<SelectedBar>>>,
    secondary: BTreeMap<Key, Sourced<Ohlcv>>,
    announcements: BTreeMap<String, Sourced<Announcement>>,
    tdx_code_tables: BTreeMap<NaiveDate, Sourced<BTreeSet<String>>>,
    corporate_actions: Vec<ActionDraft>,
    excluded: BTreeMap<&'static str, u64>,
}

impl Acc {
    fn push(&mut self, code: &str, path: impl Into<String>, message: impl Into<String>) {
        self.issues.push(issue(code, path, message));
    }

    fn count(&mut self, key: &'static str) {
        *self.excluded.entry(key).or_default() += 1;
    }
}

/// Identical facts from several Raw objects collapse to one; differing facts
/// for the same key are a vendor revision conflict and fail closed.
fn insert_fact<K: Ord + std::fmt::Debug, V: PartialEq>(
    map: &mut BTreeMap<K, Sourced<V>>,
    key: K,
    value: V,
    raw: &str,
    path: &str,
    issues: &mut Vec<NormalizationIssue>,
    duplicates: &mut u64,
) {
    match map.get_mut(&key) {
        Some(previous) if previous.value == value => {
            *duplicates += 1;
            if raw < previous.raw.as_str() {
                previous.raw = raw.to_owned();
            }
        }
        Some(_) => issues.push(issue(
            "source_revision_conflict",
            path,
            format!("{key:?} has differing values in the Raw set"),
        )),
        None => {
            map.insert(
                key,
                Sourced {
                    value,
                    raw: raw.to_owned(),
                },
            );
        }
    }
}

fn normalize(
    raw_inputs: &[RawInput],
    config: &SzseEtfFrozen,
) -> Result<NormalizationOutput, Vec<NormalizationIssue>> {
    let include_actions = config.version != "1";
    let v3 = matches!(config.version, "3" | "4");
    let venue = VenueId::new("SZSE").expect("static venue is valid");
    let mut acc = Acc::default();
    let mut duplicates = 0_u64;
    let mut seen_raw = BTreeSet::new();

    for (index, raw) in raw_inputs.iter().enumerate() {
        let path = format!("inputs[{index}]");
        if !raw.has_valid_identity() {
            acc.push(
                "raw_hash_mismatch",
                &path,
                "raw_sha256 does not match the supplied bytes",
            );
            continue;
        }
        if !seen_raw.insert(raw.raw_sha256.clone()) {
            acc.push("duplicate_raw_input", &path, "Raw object listed twice");
            continue;
        }
        if raw.bytes.starts_with(b"PK\x03\x04") {
            decode_tdx(raw, &path, &mut acc, &mut duplicates, config);
            continue;
        }
        let Ok(Value::Object(object)) = serde_json::from_slice::<Value>(&raw.bytes) else {
            acc.push(
                "unrecognized_original",
                &path,
                "Raw object is neither a TDX ZIP nor a JSON object",
            );
            continue;
        };
        if object.contains_key("announcements") {
            decode_cninfo(&object, raw, &path, &mut acc, &mut duplicates, config);
        } else if object.get("data").is_some_and(Value::is_array) {
            decode_calendar(&object, raw, &path, &mut acc, &mut duplicates);
        } else if object.contains_key("code") && object.get("data").is_some_and(Value::is_object) {
            decode_tencent(&object, raw, &path, &mut acc, &mut duplicates, config);
        } else {
            acc.push(
                "unrecognized_original",
                &path,
                "JSON object matches no supported vendor response",
            );
        }
    }

    let Acc {
        mut issues,
        kind_counts,
        calendar,
        mut primary,
        secondary,
        announcements,
        corporate_actions: action_drafts,
        tdx_code_tables,
        excluded,
    } = acc;
    for (kind, label) in [("szse_calendar", "SZSE calendar"), ("tdx_daily", "TDX")] {
        if kind_counts.get(kind).copied().unwrap_or(0) == 0 {
            issues.push(issue(
                "missing_source",
                "$",
                format!("no {label} Raw object"),
            ));
        }
    }
    for (symbol, date) in primary.keys().chain(secondary.keys()) {
        match calendar.get(date).map(|day| day.value) {
            Some(true) => {}
            Some(false) => issues.push(issue(
                "bar_outside_calendar",
                format!("{symbol}@{date}"),
                "source row falls on an SZSE non-trading day",
            )),
            None => issues.push(issue(
                "calendar_not_covered",
                format!("{symbol}@{date}"),
                "source row date is not covered by a frozen SZSE calendar month",
            )),
        }
    }
    // P6: a date-only announcement is knowable after the announced day's close,
    // so it is usable from the close of the first session strictly after it.
    // Every day up to that session must be covered, or an earlier unknown
    // session could exist.
    let mut announcement_rows = Vec::new();
    for (id, fact) in announcements {
        let Announcement {
            symbol,
            category,
            date,
        } = fact.value;
        let next = (1..)
            .map(|offset| date + Duration::days(offset))
            .map(|day| (day, calendar.get(&day).map(|known| known.value)))
            .find(|(_, open)| *open != Some(false))
            .and_then(|(day, open)| open.map(|_| day));
        let Some(available_at) = next
            .and_then(|day| make_session(venue.clone(), day))
            .map(|session| session.ts_close())
        else {
            issues.push(issue(
                "announcement_next_session_unknown",
                id.clone(),
                "frozen calendar does not establish the first session after the announced date",
            ));
            continue;
        };
        announcement_rows.push(AnnouncementRow {
            announcement_id: id,
            instrument_id: InstrumentId::new(symbol, venue.clone()).expect("validated symbol"),
            category,
            announced_date: date,
            available_at,
            source_raw_sha256: fact.raw,
        });
    }
    let mut corporate_actions = Vec::new();
    let action_availability: BTreeMap<_, _> = announcement_rows
        .iter()
        .map(|row| (row.announcement_id.as_str(), row.available_at))
        .collect();
    for draft in action_drafts {
        let Some(available_at) = action_availability
            .get(draft.announcement_id.as_str())
            .copied()
        else {
            issues.push(issue(
                "corporate_action_next_session_unknown",
                draft.announcement_id.clone(),
                "corporate action has no valid announcement availability row",
            ));
            continue;
        };
        corporate_actions.push(CorporateActionRow {
            announcement_id: draft.announcement_id,
            action_index: draft.action_index,
            instrument_id: InstrumentId::new(draft.instrument_id, venue.clone())
                .expect("validated symbol"),
            kind: draft.kind,
            announced_date: draft.announced_date,
            announced_at: draft.announced_at,
            available_at,
            record_date: draft.record_date,
            ex_date: draft.ex_date,
            pay_date: draft.pay_date,
            effective_date: draft.effective_date,
            cash_per_share: draft.cash_per_share,
            share_ratio: draft.share_ratio,
            liquidation_cash_per_share: draft.liquidation_cash_per_share,
            source_raw_sha256: draft.source_raw_sha256,
            normalization_version: "szse-corporate-action@1".into(),
        });
    }
    if !issues.is_empty() {
        return Err(issues);
    }

    // Insert only genuinely absent cells. A keyed TDX no-trade fact blocks
    // fallback even if Tencent reports a positive trade for that cell.
    if v3 {
        for (key, secondary) in &secondary {
            if secondary.value.shares > Quantity::ZERO {
                primary.entry(key.clone()).or_insert_with(|| Sourced {
                    value: Some(SelectedBar {
                        ohlcv: secondary.value.clone(),
                        amount: None,
                        reference_price: None,
                        source: "tencent_fallback",
                    }),
                    raw: secondary.raw.clone(),
                });
            }
        }
    }

    // No-trade facts participate in conflict detection but never become Bars,
    // instruments or positive primary crosscheck observations.
    let primary: BTreeMap<_, _> = primary
        .into_iter()
        .filter_map(|(key, fact)| {
            fact.value.map(|value| {
                (
                    key,
                    Sourced {
                        value,
                        raw: fact.raw,
                    },
                )
            })
        })
        .collect();

    let sessions = calendar
        .iter()
        .filter(|(_, day)| day.value)
        .map(|(date, _)| make_session(venue.clone(), *date))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| vec![issue("invalid_session", "$", "session cannot be built")])?;
    let session_dates: BTreeSet<NaiveDate> = sessions.iter().map(Session::session_date).collect();
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
    for ((symbol, date), fact) in &primary {
        let session = session_by_date[date];
        let data = BarData {
            instrument_id: InstrumentId::new(symbol.clone(), venue.clone()).expect("validated"),
            bar_spec: "1d@session".parse().expect("static bar spec is valid"),
            session_date: *date,
            ts_open: session.ts_open(),
            ts_close: session.ts_close(),
            open: fact.value.ohlcv.open,
            high: fact.value.ohlcv.high,
            low: fact.value.ohlcv.low,
            close: fact.value.ohlcv.close,
            volume: fact.value.ohlcv.shares,
            amount: fact.value.amount,
            // No vendor availability evidence: stays Unknown (ADR 0015 §3.4).
            available_at: None,
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
        let (status, fields, p_raw, s_raw) = match (
            primary.get(&key).filter(|p| p.value.source == "tdx"),
            secondary.get(&key),
        ) {
            (Some(p), Some(s)) => {
                let fields = mismatch_fields(&p.value.ohlcv, &s.value);
                let status = if fields.is_empty() {
                    "match"
                } else {
                    "mismatch"
                };
                (status, fields, Some(p.raw.clone()), Some(s.raw.clone()))
            }
            (Some(p), None) => ("primary_only", String::new(), Some(p.raw.clone()), None),
            (None, Some(s)) => ("secondary_only", String::new(), None, Some(s.raw.clone())),
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
        let dates: BTreeSet<_> = primary
            .keys()
            .filter(|(s, _)| s == *symbol)
            .map(|(_, d)| *d)
            .collect();
        if let (Some(first), Some(last)) = (dates.first(), dates.last()) {
            gap_sessions += session_dates
                .range(*first..=*last)
                .filter(|date| !dates.contains(date))
                .count() as u64;
        }
    }
    let mut category_counts: BTreeMap<String, u64> = BTreeMap::new();
    for row in &announcement_rows {
        *category_counts.entry(row.category.clone()).or_default() += 1;
    }

    let convert =
        |error: crate::DataError| vec![issue("table_conversion_failed", "$", error.to_string())];
    let tables = NormalizedTables {
        bar_sources: v3
            .then(|| {
                let rows = primary
                    .iter()
                    .map(|((symbol, date), fact)| crate::BarSourceRow {
                        instrument_id: InstrumentId::new(symbol.clone(), venue.clone())
                            .expect("validated"),
                        session_date: *date,
                        source: fact.value.source.into(),
                        source_raw_sha256: fact.raw.clone(),
                        reference_price: fact.value.reference_price,
                    })
                    .collect::<Vec<_>>();
                crate::bar_sources_to_record_batch(&rows).map_err(convert)
            })
            .transpose()?,
        instruments: instruments_to_record_batch(&specs).map_err(convert)?,
        sessions: sessions_to_record_batch(&sessions).map_err(convert)?,
        bars: bars_to_record_batch(&bars).map_err(convert)?,
        execution_status: None,
        source_crosscheck: Some(source_crosscheck_to_record_batch(&crosscheck).map_err(convert)?),
        announcements: Some(announcements_to_record_batch(&announcement_rows).map_err(convert)?),
        corporate_actions: include_actions
            .then(|| corporate_actions_to_record_batch(&corporate_actions).map_err(convert))
            .transpose()?,
        tdx_code_table: (config.version == "4")
            .then(|| {
                let rows = tdx_code_tables
                    .iter()
                    .map(|(session_date, table)| crate::TdxCodeTableRow {
                        session_date: *session_date,
                        native_symbols: table.value.iter().cloned().collect(),
                        code_ranges: config.code_ranges.clone(),
                        source_raw_sha256: table.raw.clone(),
                    })
                    .collect::<Vec<_>>();
                crate::tdx_code_table_to_record_batch(&rows).map_err(convert)
            })
            .transpose()?,
    };
    let mut coverage = json!({
        "kind": "szse-etf-frozen",
        "raw_objects_by_kind": kind_counts,
        "calendar_days": calendar.len(),
        "sessions": sessions.len(),
        "instruments": specs.len(),
        "bars": bars.len(),
        "primary_gap_sessions_unknown": gap_sessions,
        "crosscheck_by_status": crosscheck_counts,
        "announcements": announcement_rows.len(),
        "announcements_by_category": category_counts,
        "corporate_actions": corporate_actions.len(),
        "excluded_records": excluded,
        "identical_duplicate_facts": duplicates,
        "execution_status": "not_derived",
        "completeness_claim": "none; coverage is limited to the frozen Raw set",
    });
    if config.version == "4" {
        coverage["tdx_code_table_sessions"] = json!(tdx_code_tables.len());
    }
    Ok(NormalizationOutput {
        tables,
        coverage,
        synthetic_assumptions: Vec::new(),
        ignored_input_fields: Vec::new(),
    })
}

fn decode_calendar(
    object: &Map<String, Value>,
    raw: &RawInput,
    path: &str,
    acc: &mut Acc,
    duplicates: &mut u64,
) {
    *acc.kind_counts.entry("szse_calendar").or_default() += 1;
    let rows = object["data"].as_array().expect("checked by caller");
    let mut days = BTreeMap::new();
    for (i, row) in rows.iter().enumerate() {
        let row_path = format!("{path}.data[{i}]");
        let date = row
            .get("jyrq")
            .and_then(Value::as_str)
            .and_then(|text| NaiveDate::parse_from_str(text, "%Y-%m-%d").ok());
        let open = row
            .get("jybz")
            .and_then(cell_text)
            .and_then(|flag| match flag.as_str() {
                "0" => Some(false),
                "1" => Some(true),
                _ => None,
            });
        match (date, open) {
            (Some(date), Some(open)) if days.insert(date, open).is_none() => {}
            (Some(_), Some(_)) => {
                acc.push("invalid_calendar", &row_path, "duplicate jyrq");
                return;
            }
            _ => {
                acc.push(
                    "invalid_calendar",
                    &row_path,
                    "row needs jyrq YYYY-MM-DD and jybz 0/1",
                );
                return;
            }
        }
    }
    let (Some(first), Some(last)) = (days.keys().next(), days.keys().next_back()) else {
        acc.push("invalid_calendar", path, "calendar month has no days");
        return;
    };
    let complete = first.day() == 1
        && (first.year(), first.month()) == (last.year(), last.month())
        && last.succ_opt().is_some_and(|next| next.day() == 1)
        && days.len() == last.day() as usize;
    if !complete {
        acc.push(
            "invalid_calendar",
            path,
            "calendar must cover exactly one complete natural month",
        );
        return;
    }
    for (date, open) in days {
        insert_fact(
            &mut acc.calendar,
            date,
            open,
            &raw.raw_sha256,
            path,
            &mut acc.issues,
            duplicates,
        );
    }
}

fn decode_tdx(
    raw: &RawInput,
    path: &str,
    acc: &mut Acc,
    duplicates: &mut u64,
    config: &SzseEtfFrozen,
) {
    *acc.kind_counts.entry("tdx_daily").or_default() += 1;
    let invalid = |acc: &mut Acc, message: String| acc.push("invalid_tdx_package", path, message);
    let v3 = matches!(config.version, "3" | "4");
    let mut archive = match ZipArchive::new(Cursor::new(raw.bytes.as_slice())) {
        Ok(archive) => archive,
        Err(error) => return invalid(acc, format!("cannot open ZIP: {error}")),
    };
    let day_keys: Vec<String> = archive
        .file_names()
        .filter_map(|name| name.strip_prefix("sz")?.strip_suffix(".cod"))
        .filter(|key| key.len() == 6 && key.bytes().all(|b| b.is_ascii_digit()))
        .map(str::to_owned)
        .collect();
    let [day_key] = day_keys.as_slice() else {
        return invalid(acc, "package must hold exactly one szYYMMDD.cod".into());
    };
    let Ok(date) = NaiveDate::parse_from_str(&format!("20{day_key}"), "%Y%m%d") else {
        return invalid(acc, format!("member date {day_key} is not a date"));
    };
    let mut member = |name: String| -> Result<Vec<u8>, String> {
        let file = archive
            .by_name(&name)
            .map_err(|error| format!("{name}: {error}"))?;
        if file.size() > MAX_MEMBER_BYTES {
            return Err(format!("{name} exceeds the member size limit"));
        }
        let mut bytes = Vec::new();
        file.take(MAX_MEMBER_BYTES)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("{name}: {error}"))?;
        Ok(bytes)
    };
    let (cod, md1) = match (
        member(format!("sz{day_key}.cod")),
        member(format!("sz{day_key}.md1")),
    ) {
        (Ok(cod), Ok(md1)) => (cod, md1),
        (Err(error), _) | (_, Err(error)) => return invalid(acc, error),
    };
    if cod.len() % 150 != 0 || md1.len() % 512 != 0 || cod.len() / 150 != md1.len() / 512 {
        return invalid(acc, "code table and price blocks are misaligned".into());
    }
    let mut codes = BTreeSet::new();
    let mut scoped_codes = BTreeSet::new();
    let mut sequences = BTreeSet::new();
    for (i, record) in cod.chunks_exact(150).enumerate() {
        let record_path = format!("{path}.sz{day_key}.cod[{i}]");
        let code = trim_nul_space(&record[0..6]);
        let sequence = u16::from_le_bytes([record[32], record[33]]);
        let Some(code) = std::str::from_utf8(code)
            .ok()
            .filter(|code| code.len() == 6 && code.bytes().all(|b| b.is_ascii_digit()))
        else {
            acc.push(
                "invalid_tdx_package",
                &record_path,
                "code is not six ASCII digits",
            );
            continue;
        };
        if !codes.insert(code.to_owned()) || !sequences.insert(sequence) {
            acc.push(
                "invalid_tdx_package",
                &record_path,
                "duplicate code or price block sequence",
            );
            continue;
        }
        if !config.scoped(code) {
            acc.count("tdx_out_of_scope_records");
            continue;
        }
        scoped_codes.insert(code.to_owned());
        let offset = usize::from(sequence) * 512;
        let Some(block) = md1.get(offset..offset + 512) else {
            acc.push(
                "invalid_tdx_package",
                &record_path,
                "price block sequence is out of range",
            );
            continue;
        };
        let read = |at: usize| f64::from_le_bytes(block[at..at + 8].try_into().expect("8 bytes"));
        let [open, high, low, close, amount] = [12, 20, 28, 36, 72].map(read);
        if ![open, high, low, close, amount]
            .iter()
            .all(|value| value.is_finite())
        {
            acc.push("invalid_price", &record_path, "non-finite value");
            continue;
        }
        // No trade: either no close, or a carried close with an empty intraday range.
        if close <= 0.0
            || [open, high, low].iter().all(|value| *value == 0.0)
            || (v3 && u64::from_le_bytes(block[56..64].try_into().expect("8 bytes")) == 0)
        {
            acc.count("tdx_no_trade_records");
            insert_fact(
                &mut acc.primary,
                (code.to_owned(), date),
                None,
                &raw.raw_sha256,
                &record_path,
                &mut acc.issues,
                duplicates,
            );
            continue;
        }
        let reference_price = if v3 {
            match f64_price(read(4)).filter(|p| *p > Price::ZERO) {
                Some(price) => Some(price),
                None => {
                    acc.push(
                        "invalid_reference_price",
                        &record_path,
                        "TDX reference price must be positive and on tick",
                    );
                    continue;
                }
            }
        } else {
            None
        };
        let shares = u64::from_le_bytes(block[56..64].try_into().expect("8 bytes"));
        let prices = [open, high, low, close].map(f64_price);
        let shares = Quantity::parse(&shares.to_string()).ok();
        let Some(ohlcv) = build_ohlcv(prices, shares, &record_path, &mut acc.issues) else {
            continue;
        };
        let amount = match Notional::parse(&format!("{amount:.10}")) {
            Ok(value) if value >= Notional::ZERO => value,
            _ => {
                acc.push(
                    "invalid_amount",
                    &record_path,
                    "amount must be non-negative",
                );
                continue;
            }
        };
        insert_fact(
            &mut acc.primary,
            (code.to_owned(), date),
            Some(SelectedBar {
                ohlcv,
                amount: Some(amount),
                reference_price,
                source: "tdx",
            }),
            &raw.raw_sha256,
            &record_path,
            &mut acc.issues,
            duplicates,
        );
    }
    if config.version == "4" {
        insert_fact(
            &mut acc.tdx_code_tables,
            date,
            scoped_codes,
            &raw.raw_sha256,
            path,
            &mut acc.issues,
            duplicates,
        );
    }
}

fn decode_tencent(
    object: &Map<String, Value>,
    raw: &RawInput,
    path: &str,
    acc: &mut Acc,
    duplicates: &mut u64,
    config: &SzseEtfFrozen,
) {
    *acc.kind_counts.entry("tencent_kline").or_default() += 1;
    if object.get("code").and_then(cell_text).as_deref() != Some("0") {
        acc.push("invalid_tencent", path, "response code is not 0");
        return;
    }
    let data = object["data"].as_object().expect("checked by caller");
    for (key, node) in data {
        let node_path = format!("{path}.data.{key}");
        let Some(symbol) = key
            .strip_prefix("sz")
            .filter(|code| config.scoped(code))
            .map(str::to_owned)
        else {
            acc.push(
                "invalid_symbol",
                &node_path,
                "Tencent key must be a configured sz ETF code",
            );
            continue;
        };
        // Only the unadjusted array is a comparable price source.
        let Some(rows) = node.get("day").and_then(Value::as_array) else {
            acc.push(
                "invalid_tencent",
                &node_path,
                "unadjusted day array is missing",
            );
            continue;
        };
        for (i, row) in rows.iter().enumerate() {
            let row_path = format!("{node_path}.day[{i}]");
            let fields: Option<Vec<String>> = row.as_array().and_then(|cells| {
                (cells.len() >= 6)
                    .then(|| cells[..6].iter().map(cell_text).collect())
                    .flatten()
            });
            let Some(fields) = fields else {
                acc.push(
                    "invalid_tencent",
                    &row_path,
                    "row needs date, open, close, high, low and lots",
                );
                continue;
            };
            let Ok(date) = NaiveDate::parse_from_str(&fields[0], "%Y-%m-%d") else {
                acc.push("invalid_date", &row_path, "dates must use YYYY-MM-DD");
                continue;
            };
            let price = |text: &String| Price::parse(text).ok();
            let prices = [
                price(&fields[1]),
                price(&fields[3]),
                price(&fields[4]),
                price(&fields[2]),
            ];
            let shares = Quantity::parse(&fields[5])
                .and_then(|lots| lots.checked_mul(100))
                .ok();
            let Some(ohlcv) = build_ohlcv(prices, shares, &row_path, &mut acc.issues) else {
                continue;
            };
            insert_fact(
                &mut acc.secondary,
                (symbol.clone(), date),
                ohlcv,
                &raw.raw_sha256,
                &row_path,
                &mut acc.issues,
                duplicates,
            );
        }
    }
}

fn decode_cninfo(
    object: &Map<String, Value>,
    raw: &RawInput,
    path: &str,
    acc: &mut Acc,
    duplicates: &mut u64,
    config: &SzseEtfFrozen,
) {
    let include_actions = config.version != "1";
    *acc.kind_counts.entry("cninfo_announcements").or_default() += 1;
    let items = match &object["announcements"] {
        Value::Null => return,
        Value::Array(items) => items,
        _ => {
            acc.push(
                "invalid_cninfo",
                path,
                "announcements must be an array or null",
            );
            return;
        }
    };
    for (i, item) in items.iter().enumerate() {
        let item_path = format!("{path}.announcements[{i}]");
        let text = |field: &str| item.get(field).and_then(cell_text);
        let (Some(id), Some(code), Some(title), Some(millis)) = (
            text("announcementId").filter(|id| !id.is_empty()),
            text("secCode"),
            text("announcementTitle"),
            item.get("announcementTime").and_then(Value::as_i64),
        ) else {
            acc.push(
                "invalid_cninfo",
                &item_path,
                "item needs announcementId, secCode, announcementTitle and announcementTime",
            );
            continue;
        };
        if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
            acc.push("invalid_symbol", &item_path, "secCode is not six digits");
            continue;
        }
        if !config.scoped(&code) {
            acc.count("announcements_out_of_scope");
            continue;
        }
        let Some(announced_time) = DateTime::from_timestamp_millis(millis) else {
            acc.push(
                "invalid_date",
                &item_path,
                "announcementTime is out of range",
            );
            continue;
        };
        let Some(announced_at) = announced_time
            .timestamp_nanos_opt()
            .map(TimestampNs::from_unix_nanos)
        else {
            acc.push(
                "invalid_date",
                &item_path,
                "announcementTime cannot be represented at nanosecond precision",
            );
            continue;
        };
        let date = announced_time.with_timezone(&Shanghai).date_naive();
        let title = title.replace("<em>", "").replace("</em>", "");
        let category = CATEGORY_RULES
            .iter()
            .find(|(_, keywords)| keywords.iter().any(|keyword| title.contains(keyword)))
            .map_or(UNCLASSIFIED, |(category, _)| category);
        insert_fact(
            &mut acc.announcements,
            id.clone(),
            Announcement {
                symbol: code.clone(),
                category: category.to_owned(),
                date,
            },
            &raw.raw_sha256,
            &item_path,
            &mut acc.issues,
            duplicates,
        );
        if include_actions {
            let actions = match item.get("corporateActions") {
                Some(Value::Array(actions)) => actions.as_slice(),
                Some(Value::Null) | None => &[],
                _ => {
                    acc.push(
                        "invalid_corporate_action",
                        &item_path,
                        "corporateActions must be an array or null",
                    );
                    continue;
                }
            };
            if matches!(category, "dividend" | "conversion" | "termination") && actions.is_empty() {
                acc.push(
                    "unparsed_corporate_action",
                    &item_path,
                    "action announcement has no normalized corporateActions facts",
                );
                continue;
            }
            for (action_index, action) in actions.iter().enumerate() {
                let action_path = format!("{item_path}.corporateActions[{action_index}]");
                match parse_action(
                    action,
                    ActionSource {
                        announcement_id: &id,
                        action_index: action_index as u32,
                        symbol: &code,
                        announced_date: date,
                        announced_at,
                        source_raw_sha256: &raw.raw_sha256,
                        path: &action_path,
                    },
                ) {
                    Ok(draft) => acc.corporate_actions.push(draft),
                    Err((code, message)) => acc.push(&code, action_path, message),
                }
            }
        }
    }
}

fn parse_action(value: &Value, source: ActionSource<'_>) -> Result<ActionDraft, (String, String)> {
    let obj = value.as_object().ok_or_else(|| {
        (
            "invalid_corporate_action".into(),
            "action must be an object".into(),
        )
    })?;
    const ALLOWED: &[&str] = &[
        "type",
        "recordDate",
        "exDate",
        "payDate",
        "effectiveDate",
        "cashPerShare",
        "shareRatio",
        "liquidationCashPerShare",
    ];
    if obj.keys().any(|key| !ALLOWED.contains(&key.as_str())) {
        return Err((
            "unknown_corporate_action_field".into(),
            "action contains an unsupported field".into(),
        ));
    }
    let date = |field: &str| -> Result<Option<NaiveDate>, (String, String)> {
        obj.get(field)
            .and_then(Value::as_str)
            .map(|text| {
                text.parse().map_err(|_| {
                    (
                        "invalid_corporate_action_date".into(),
                        format!("{field} must be YYYY-MM-DD"),
                    )
                })
            })
            .transpose()
    };
    let decimal = |field: &str| -> Result<Option<String>, (String, String)> {
        let Some(value) = obj.get(field) else {
            return Ok(None);
        };
        let text = value.as_str().ok_or_else(|| {
            (
                "invalid_corporate_action_decimal".into(),
                format!("{field} must be a decimal string"),
            )
        })?;
        let parsed = prajna_domain::Price::parse(text).map_err(|_| {
            (
                "invalid_corporate_action_decimal".into(),
                format!("{field} must be positive and finite"),
            )
        })?;
        Ok(Some(parsed.to_string()))
    };
    let kind = match obj.get("type").and_then(Value::as_str) {
        Some("dividend") => CorporateActionKind::Dividend,
        Some("share_conversion") => CorporateActionKind::ShareConversion,
        Some("liquidation") => CorporateActionKind::Liquidation,
        _ => {
            return Err((
                "invalid_corporate_action_type".into(),
                "type must be dividend, share_conversion or liquidation".into(),
            ));
        }
    };
    let record_date = date("recordDate")?;
    let ex_date = date("exDate")?;
    let pay_date = date("payDate")?;
    let effective_date = date("effectiveDate")?;
    let cash_per_share = decimal("cashPerShare")?;
    let share_ratio = decimal("shareRatio")?;
    let liquidation_cash_per_share = decimal("liquidationCashPerShare")?;
    let valid = match kind {
        CorporateActionKind::Dividend => {
            record_date.is_some()
                && ex_date.is_some()
                && pay_date.is_some()
                && cash_per_share.is_some()
        }
        CorporateActionKind::ShareConversion => effective_date.is_some() && share_ratio.is_some(),
        CorporateActionKind::Liquidation => {
            effective_date.is_some() && liquidation_cash_per_share.is_some()
        }
    };
    if !valid {
        return Err((
            "incomplete_corporate_action".into(),
            format!("required dates or amounts are missing in {}", source.path),
        ));
    }
    Ok(ActionDraft {
        announcement_id: source.announcement_id.to_owned(),
        action_index: source.action_index,
        instrument_id: source.symbol.to_owned(),
        kind,
        announced_date: source.announced_date,
        announced_at: source.announced_at,
        record_date,
        ex_date,
        pay_date,
        effective_date,
        cash_per_share,
        share_ratio,
        liquidation_cash_per_share,
        source_raw_sha256: source.source_raw_sha256.to_owned(),
    })
}

fn cell_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// Converts a TDX float price to the 0.001 grid; off-grid values are rejected.
fn f64_price(value: f64) -> Option<Price> {
    let mils = (value * 1000.0).round();
    if (value * 1000.0 - mils).abs() > 1e-6 || !(1.0..=1e15).contains(&mils) {
        return None;
    }
    let mils = mils as i64;
    Price::parse(&format!("{}.{:03}", mils / 1000, mils % 1000)).ok()
}

fn build_ohlcv(
    prices: [Option<Price>; 4],
    shares: Option<Quantity>,
    path: &str,
    issues: &mut Vec<NormalizationIssue>,
) -> Option<Ohlcv> {
    let tick = Price::parse("0.001").expect("static increment is valid");
    let valid = |price: &Option<Price>| {
        price.filter(|value| *value > Price::ZERO && value.is_aligned_to(tick).unwrap_or(false))
    };
    let [Some(open), Some(high), Some(low), Some(close)] = prices.each_ref().map(valid) else {
        issues.push(issue(
            "invalid_price",
            path,
            "prices must be positive and aligned to 0.001",
        ));
        return None;
    };
    if high < open.max(close) || low > open.min(close) {
        issues.push(issue(
            "invalid_ohlc",
            path,
            "high/low do not bound open and close",
        ));
        return None;
    }
    let whole = Quantity::parse("1").expect("static quantity is valid");
    let Some(shares) = shares
        .filter(|value| *value >= Quantity::ZERO && value.is_aligned_to(whole).unwrap_or(false))
    else {
        issues.push(issue(
            "invalid_volume",
            path,
            "volume must be a non-negative whole number of shares",
        ));
        return None;
    };
    Some(Ohlcv {
        open,
        high,
        low,
        close,
        shares,
    })
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

fn trim_nul_space(bytes: &[u8]) -> &[u8] {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    bytes[..end].trim_ascii()
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
