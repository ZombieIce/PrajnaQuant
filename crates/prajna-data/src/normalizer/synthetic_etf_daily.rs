use std::collections::{BTreeMap, BTreeSet, HashSet};

use chrono::NaiveDate;
use prajna_domain::{
    Bar, BarData, Currency, InstrumentId, InstrumentKind, InstrumentSpec, InstrumentSpecData,
    InstrumentSpecs, Notional, Price, Quantity, Session, TimestampNs, VenueId,
};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::{
    NormalizationIssue, NormalizationOutput, NormalizedTables, bars_to_record_batch,
    instruments_to_record_batch, sessions_to_record_batch,
};

use super::{Normalizer, RawInput};

const NORMALIZER_ID: &str = "synthetic-etf-daily";
const CONFIG_V1: &str = r#"{"bar_spec":"1d@session","high":"max(open,close)","instrument_kind":"Etf","low":"min(open,close)","session_close":"15:00:00+08:00","session_open":"09:30:00+08:00","timezone":"Asia/Shanghai","volume":"shares","volume_multiplier":1}"#;
const CONFIG_V2: &str = r#"{"bar_spec":"1d@session","high":"max(open,close)","instrument_kind":"Etf","low":"min(open,close)","session_close":"15:00:00+08:00","session_open":"09:30:00+08:00","timezone":"Asia/Shanghai","volume":"lots","volume_multiplier":100}"#;

pub(super) struct SyntheticEtfDaily {
    version: &'static str,
    volume_multiplier: i128,
    config: &'static str,
}

impl SyntheticEtfDaily {
    pub(super) const fn v1() -> Self {
        Self {
            version: "1",
            volume_multiplier: 1,
            config: CONFIG_V1,
        }
    }

    pub(super) const fn v2() -> Self {
        Self {
            version: "2",
            volume_multiplier: 100,
            config: CONFIG_V2,
        }
    }
}

impl Normalizer for SyntheticEtfDaily {
    fn id(&self) -> &str {
        NORMALIZER_ID
    }

    fn version(&self) -> &str {
        self.version
    }

    fn config_sha256(&self) -> String {
        format!("sha256:{:x}", Sha256::digest(self.config.as_bytes()))
    }

    fn normalize(
        &self,
        raw_inputs: &[RawInput],
    ) -> Result<NormalizationOutput, Vec<NormalizationIssue>> {
        normalize_fixture(raw_inputs, self.volume_multiplier)
    }
}

fn normalize_fixture(
    raw_inputs: &[RawInput],
    volume_multiplier: i128,
) -> Result<NormalizationOutput, Vec<NormalizationIssue>> {
    if raw_inputs.len() != 1 {
        return Err(vec![issue(
            "invalid_input_count",
            "$",
            "synthetic-etf-daily requires exactly one fixture Raw object",
        )]);
    }
    let raw = &raw_inputs[0];
    if !raw.has_valid_identity() {
        return Err(vec![issue(
            "raw_hash_mismatch",
            "$",
            "raw_sha256 does not match the supplied bytes",
        )]);
    }
    let root: Value = serde_json::from_slice(&raw.bytes).map_err(|error| {
        vec![issue(
            "invalid_json",
            "$",
            format!("fixture JSON cannot be parsed: {error}"),
        )]
    })?;
    let Some(root) = root.as_object() else {
        return Err(vec![issue(
            "invalid_document",
            "$",
            "fixture must be a JSON object",
        )]);
    };

    let mut issues = Vec::new();
    let calendar = match string_array(root.get("calendar"), "calendar", &mut issues) {
        Some(values) => values
            .into_iter()
            .enumerate()
            .filter_map(
                |(index, text)| match NaiveDate::parse_from_str(text, "%Y-%m-%d") {
                    Ok(date) => Some((index, date)),
                    Err(_) => {
                        issues.push(issue(
                            "invalid_date",
                            format!("calendar[{index}]"),
                            "calendar dates must use YYYY-MM-DD",
                        ));
                        None
                    }
                },
            )
            .collect::<Vec<_>>(),
        None => Vec::new(),
    };
    let mut seen_dates = HashSet::new();
    for (index, date) in &calendar {
        if !seen_dates.insert(*date) {
            issues.push(issue(
                "duplicate_primary_key",
                format!("calendar[{index}]"),
                format!("duplicate session date {date}"),
            ));
        }
    }
    if !calendar.windows(2).all(|pair| pair[0].1 < pair[1].1) {
        issues.push(issue(
            "invalid_calendar_order",
            "calendar",
            "calendar sessions must be in strictly increasing date order",
        ));
    }

    let defaults = object_field(root, "bar_defaults", "$", &mut issues);
    let (open, default_volume, amount) = if let Some(defaults) = defaults {
        (
            decimal_field::<Price>(defaults, "open", "bar_defaults", &mut issues),
            decimal_field::<Quantity>(defaults, "volume", "bar_defaults", &mut issues),
            decimal_field::<Notional>(defaults, "amount", "bar_defaults", &mut issues),
        )
    } else {
        (None, None, None)
    };
    let high_rule = defaults
        .and_then(|value| value.get("high_rule"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let low_rule = defaults
        .and_then(|value| value.get("low_rule"))
        .and_then(Value::as_str)
        .unwrap_or("");

    let account = object_field(root, "account", "$", &mut issues);
    let lot_size = account
        .and_then(|value| decimal_field::<Quantity>(value, "lot_size", "account", &mut issues));
    let instruments = array_field(root.get("instruments"), "instruments", &mut issues)
        .map_or(&[][..], |values| values.as_slice());
    let missing_bars = parse_missing_bars(root.get("missing_bars"), &mut issues);

    if calendar.is_empty() {
        issues.push(issue(
            "empty_calendar",
            "calendar",
            "calendar must not be empty",
        ));
    }
    if instruments.is_empty() {
        issues.push(issue(
            "empty_instruments",
            "instruments",
            "at least one instrument is required",
        ));
    }
    if !matches!(high_rule, "max(open, close)" | "min(open, close)") {
        issues.push(issue(
            "invalid_high_rule",
            "bar_defaults.high_rule",
            "unsupported high_rule",
        ));
    }
    if !matches!(low_rule, "min(open, close)" | "max(open, close)") {
        issues.push(issue(
            "invalid_low_rule",
            "bar_defaults.low_rule",
            "unsupported low_rule",
        ));
    }

    let venue = VenueId::new("SYNTH").expect("static venue is valid");
    let mut spec_registry = InstrumentSpecs::default();
    let mut specs = Vec::new();
    let mut sessions = Vec::new();
    let mut sessions_by_date = BTreeMap::new();
    for (_, date) in &calendar {
        let Some(session) = make_session(venue.clone(), *date) else {
            issues.push(issue(
                "invalid_session",
                "calendar",
                "session bounds are invalid",
            ));
            continue;
        };
        sessions_by_date.insert(*date, session.clone());
        sessions.push(session);
    }

    let mut bars = Vec::new();
    let mut coverage = serde_json::Map::new();
    let mut symbols_seen: BTreeMap<String, (String, u64, u64)> = BTreeMap::new();
    for (instrument_index, instrument_value) in instruments.iter().enumerate() {
        let path = format!("instruments[{instrument_index}]");
        let Some(instrument) = instrument_value.as_object() else {
            issues.push(issue(
                "invalid_instrument",
                &path,
                "instrument must be an object",
            ));
            continue;
        };
        let symbol = required_string(instrument, "symbol", &path, &mut issues);
        let currency_text = required_string(instrument, "currency", &path, &mut issues);
        let price_precision = required_u64(instrument, "price_precision", &path, &mut issues);
        let quantity_precision = required_u64(instrument, "quantity_precision", &path, &mut issues);
        let closes = decimal_array(
            instrument.get("closes"),
            &format!("{path}.closes"),
            &mut issues,
        );
        let (
            Some(symbol),
            Some(currency_text),
            Some(price_precision),
            Some(quantity_precision),
            Some(closes),
        ) = (
            symbol,
            currency_text,
            price_precision,
            quantity_precision,
            closes,
        )
        else {
            continue;
        };
        if closes.len() != calendar.len() {
            issues.push(issue(
                "invalid_closes_length",
                format!("{path}.closes"),
                "closes must have one value for every calendar session",
            ));
            continue;
        }
        if price_precision > 18 || quantity_precision > 18 {
            issues.push(issue(
                "invalid_precision",
                &path,
                "precision must be between 0 and 18",
            ));
            continue;
        }
        let price_increment = match increment(price_precision) {
            Ok(value) => value,
            Err(message) => {
                issues.push(issue(
                    "invalid_precision",
                    format!("{path}.price_precision"),
                    message,
                ));
                continue;
            }
        };
        let size_increment = match increment(quantity_precision) {
            Ok(value) => Quantity::parse(&value).ok(),
            Err(_) => None,
        };
        let Some(size_increment) = size_increment else {
            issues.push(issue(
                "invalid_precision",
                format!("{path}.quantity_precision"),
                "invalid quantity precision",
            ));
            continue;
        };
        let id = match InstrumentId::new(symbol.clone(), venue.clone()) {
            Ok(id) => id,
            Err(_) => {
                issues.push(issue(
                    "invalid_symbol",
                    format!("{path}.symbol"),
                    "symbol is invalid for an InstrumentId",
                ));
                continue;
            }
        };
        let currency = match Currency::new(currency_text.clone()) {
            Ok(currency) => currency,
            Err(_) => {
                issues.push(issue(
                    "invalid_currency",
                    format!("{path}.currency"),
                    "currency code is invalid",
                ));
                continue;
            }
        };
        let price_increment = match Price::parse(&price_increment) {
            Ok(value) => value,
            Err(_) => {
                issues.push(issue(
                    "invalid_precision",
                    format!("{path}.price_precision"),
                    "invalid price increment",
                ));
                continue;
            }
        };
        let spec = match InstrumentSpec::new(InstrumentSpecData {
            id: id.clone(),
            kind: InstrumentKind::Etf,
            native_symbol: symbol.clone(),
            market_segment: None,
            base_currency: None,
            quote_currency: Some(currency),
            settle_currency: None,
            is_inverse: false,
            multiplier: None,
            expiry: None,
            price_increment,
            size_increment,
            lot_size,
            session_timezone: Some("Asia/Shanghai".into()),
        }) {
            Ok(spec) => spec,
            Err(error) => {
                issues.push(issue("invalid_instrument_spec", &path, error.to_string()));
                continue;
            }
        };
        if let Some((previous_currency, previous_price_precision, previous_quantity_precision)) =
            symbols_seen.get(&symbol)
        {
            let code = if previous_currency != &currency_text
                || previous_price_precision != &price_precision
                || previous_quantity_precision != &quantity_precision
            {
                "instrument_id_reused"
            } else {
                "duplicate_primary_key"
            };
            issues.push(issue(
                code,
                format!("{path}.symbol"),
                format!("instrument ID {id} occurs more than once"),
            ));
            continue;
        }
        symbols_seen.insert(
            symbol.clone(),
            (currency_text, price_precision, quantity_precision),
        );
        if let Err(error) = spec_registry.insert(spec.clone()) {
            issues.push(issue(
                "instrument_id_reused",
                format!("{path}.symbol"),
                error.to_string(),
            ));
            continue;
        }
        specs.push(spec);

        let volume = match default_volume {
            Some(value) => match value.checked_mul(volume_multiplier) {
                Ok(value) => value,
                Err(_) => {
                    issues.push(issue(
                        "quantity_out_of_range",
                        "bar_defaults.volume",
                        "volume exceeds the fixed-point range",
                    ));
                    continue;
                }
            },
            None => continue,
        };
        if volume < Quantity::ZERO {
            issues.push(issue(
                "negative_quantity",
                "bar_defaults.volume",
                "volume must not be negative",
            ));
            continue;
        }
        if let Some(lot_size) = lot_size {
            if lot_size <= Quantity::ZERO {
                issues.push(issue(
                    "invalid_lot_size",
                    "account.lot_size",
                    "lot_size must be positive",
                ));
                continue;
            }
            if !lot_size.is_aligned_to(size_increment).unwrap_or(false) {
                issues.push(issue(
                    "misaligned_increment",
                    "account.lot_size",
                    "lot_size is not aligned to the quantity increment",
                ));
                continue;
            }
        }
        if amount.is_some_and(|value| value < Notional::ZERO) {
            issues.push(issue(
                "negative_amount",
                "bar_defaults.amount",
                "amount must not be negative",
            ));
            continue;
        }
        let mut actual_bars = 0_u64;
        let expected_sessions = calendar.len() as u64;
        let mut missing_dates = BTreeSet::new();
        for ((_, date), close) in calendar.iter().zip(closes) {
            if missing_bars.contains(&(symbol.clone(), *date)) {
                missing_dates.insert(date.to_string());
                continue;
            }
            let (Some(open), Some(amount), Some(session)) =
                (open, amount, sessions_by_date.get(date))
            else {
                continue;
            };
            let (high, low) = match (high_rule, low_rule) {
                ("max(open, close)", "min(open, close)") => (open.max(close), open.min(close)),
                ("min(open, close)", "min(open, close)") => (open.min(close), open.min(close)),
                ("max(open, close)", "max(open, close)") => (open.max(close), open.max(close)),
                ("min(open, close)", "max(open, close)") => (open.min(close), open.max(close)),
                _ => continue,
            };
            if open < Price::ZERO || high < Price::ZERO || low < Price::ZERO || close < Price::ZERO
            {
                issues.push(issue(
                    "negative_price",
                    format!("{path}.closes"),
                    format!("price for {symbol} on {date} must not be negative"),
                ));
                continue;
            }
            let bar_data = BarData {
                instrument_id: id.clone(),
                bar_spec: "1d@session".parse().expect("static bar spec is valid"),
                session_date: *date,
                ts_open: session.ts_open(),
                ts_close: session.ts_close(),
                open,
                high,
                low,
                close,
                volume,
                amount: Some(amount),
                available_at: Some(session.ts_close()),
            };
            if ![open, high, low, close]
                .into_iter()
                .all(|price| price.is_aligned_to(price_increment).unwrap_or(false))
            {
                issues.push(issue(
                    "misaligned_increment",
                    format!("{path}.closes"),
                    format!("OHLC price for {symbol} on {date} is not aligned to its increment"),
                ));
                continue;
            }
            if !volume.is_aligned_to(size_increment).unwrap_or(false) {
                issues.push(issue(
                    "misaligned_increment",
                    "bar_defaults.volume",
                    format!("volume for {symbol} is not aligned to its increment"),
                ));
                continue;
            }
            match Bar::new(bar_data, Some(session)) {
                Ok(bar) => {
                    bars.push(bar);
                    actual_bars += 1;
                }
                Err(error) => issues.push(issue(
                    "invalid_ohlc_or_bar",
                    format!("{path}.closes"),
                    format!("invalid bar for {symbol} on {date}: {error}"),
                )),
            }
        }
        coverage.insert(
            symbol,
            json!({
                "expected_sessions": expected_sessions,
                "actual_bars": actual_bars,
                "missing_dates": missing_dates,
            }),
        );
    }

    for (symbol, date) in &missing_bars {
        if !symbols_seen.contains_key(symbol) {
            issues.push(issue(
                "missing_bar_unknown_instrument",
                "missing_bars",
                format!("missing bar references unknown instrument {symbol}"),
            ));
        }
        if !seen_dates.contains(date) {
            issues.push(issue(
                "missing_bar_unknown_session",
                "missing_bars",
                format!("missing bar references unknown session {date}"),
            ));
        }
    }

    if !issues.is_empty() {
        return Err(issues);
    }
    specs.sort_by(|left, right| left.id().cmp(right.id()));
    bars.sort_by(|left, right| {
        left.instrument_id()
            .cmp(right.instrument_id())
            .then_with(|| left.ts_open().cmp(&right.ts_open()))
    });
    let batch_result = (|| -> Result<NormalizedTables, String> {
        Ok(NormalizedTables {
            instruments: instruments_to_record_batch(&specs).map_err(|error| error.to_string())?,
            sessions: sessions_to_record_batch(&sessions).map_err(|error| error.to_string())?,
            bars: bars_to_record_batch(&bars).map_err(|error| error.to_string())?,
        })
    })();
    let tables =
        batch_result.map_err(|error| vec![issue("table_conversion_failed", "$", error)])?;
    Ok(NormalizationOutput {
        tables,
        coverage: Value::Object(coverage),
        synthetic_assumptions: vec!["available_at = ts_close".into()],
        ignored_input_fields: vec![
            "dataset_version".into(),
            "seed".into(),
            "seed_semantics".into(),
            "source_identity".into(),
            "calendar_basis".into(),
            "timezone".into(),
            "bar_defaults.open_available_at".into(),
            "bar_defaults.close_available_at".into(),
            "missing_bars[].reason".into(),
            "execution_status_default".into(),
            "execution_status_overrides".into(),
            "time_model".into(),
            "result_model_boundary".into(),
            "strategy".into(),
            "costs".into(),
            "account.initial_cash".into(),
        ],
    })
}

fn make_session(venue: VenueId, date: NaiveDate) -> Option<Session> {
    let date_text = date.format("%Y-%m-%d");
    let open = TimestampNs::parse(&format!("{date_text}T09:30:00+08:00")).ok()?;
    let close = TimestampNs::parse(&format!("{date_text}T15:00:00+08:00")).ok()?;
    Session::new(venue, date, open, close).ok()
}

fn increment(precision: u64) -> Result<String, String> {
    if precision == 0 {
        return Ok("1".into());
    }
    Ok(format!("0.{}1", "0".repeat(precision as usize - 1)))
}

fn decimal_value<T>(value: &Value, path: &str, issues: &mut Vec<NormalizationIssue>) -> Option<T>
where
    T: ParseDecimal,
{
    let Some(number) = value.as_number() else {
        issues.push(issue(
            "invalid_decimal",
            path,
            "expected a JSON number literal",
        ));
        return None;
    };
    let literal = number.to_string();
    match T::parse_decimal(&literal) {
        Ok(number) => Some(number),
        Err(error) => {
            issues.push(issue("invalid_decimal", path, error.to_string()));
            None
        }
    }
}

fn decimal_field<T>(
    object: &Map<String, Value>,
    name: &str,
    parent: &str,
    issues: &mut Vec<NormalizationIssue>,
) -> Option<T>
where
    T: ParseDecimal,
{
    let path = format!("{parent}.{name}");
    let Some(value) = object.get(name) else {
        issues.push(issue("missing_field", path, "required field is missing"));
        return None;
    };
    decimal_value(value, &path, issues)
}

fn decimal_array(
    value: Option<&Value>,
    path: &str,
    issues: &mut Vec<NormalizationIssue>,
) -> Option<Vec<Price>> {
    let values = array_field(value, path, issues)?;
    Some(
        values
            .iter()
            .enumerate()
            .filter_map(|(index, value)| decimal_value(value, &format!("{path}[{index}]"), issues))
            .collect(),
    )
}

fn required_string(
    object: &Map<String, Value>,
    name: &str,
    parent: &str,
    issues: &mut Vec<NormalizationIssue>,
) -> Option<String> {
    let path = format!("{parent}.{name}");
    match object.get(name).and_then(Value::as_str) {
        Some(value) => Some(value.to_owned()),
        None => {
            issues.push(issue(
                "missing_or_invalid_field",
                path,
                "required string field is missing or invalid",
            ));
            None
        }
    }
}

fn required_u64(
    object: &Map<String, Value>,
    name: &str,
    parent: &str,
    issues: &mut Vec<NormalizationIssue>,
) -> Option<u64> {
    let path = format!("{parent}.{name}");
    match object.get(name).and_then(Value::as_u64) {
        Some(value) => Some(value),
        None => {
            issues.push(issue(
                "missing_or_invalid_field",
                path,
                "required non-negative integer field is missing or invalid",
            ));
            None
        }
    }
}

fn object_field<'a>(
    object: &'a Map<String, Value>,
    name: &str,
    parent: &str,
    issues: &mut Vec<NormalizationIssue>,
) -> Option<&'a Map<String, Value>> {
    let path = format!("{parent}.{name}");
    match object.get(name).and_then(Value::as_object) {
        Some(value) => Some(value),
        None => {
            issues.push(issue(
                "missing_or_invalid_field",
                path,
                "required object field is missing or invalid",
            ));
            None
        }
    }
}

fn array_field<'a>(
    value: Option<&'a Value>,
    path: &str,
    issues: &mut Vec<NormalizationIssue>,
) -> Option<&'a Vec<Value>> {
    match value.and_then(Value::as_array) {
        Some(value) => Some(value),
        None => {
            issues.push(issue(
                "missing_or_invalid_field",
                path,
                "required array field is missing or invalid",
            ));
            None
        }
    }
}

fn string_array<'a>(
    value: Option<&'a Value>,
    path: &str,
    issues: &mut Vec<NormalizationIssue>,
) -> Option<Vec<&'a str>> {
    let values = array_field(value, path, issues)?;
    Some(
        values
            .iter()
            .enumerate()
            .filter_map(|(index, value)| match value.as_str() {
                Some(value) => Some(value),
                None => {
                    issues.push(issue(
                        "invalid_date",
                        format!("{path}[{index}]"),
                        "expected a date string",
                    ));
                    None
                }
            })
            .collect(),
    )
}

fn parse_missing_bars(
    value: Option<&Value>,
    issues: &mut Vec<NormalizationIssue>,
) -> HashSet<(String, NaiveDate)> {
    let Some(values) = value.and_then(Value::as_array) else {
        issues.push(issue(
            "missing_or_invalid_field",
            "missing_bars",
            "required array field is missing or invalid",
        ));
        return HashSet::new();
    };
    values
        .iter()
        .enumerate()
        .filter_map(|(index, value)| {
            let path = format!("missing_bars[{index}]");
            let Some(object) = value.as_object() else {
                issues.push(issue(
                    "invalid_missing_bar",
                    &path,
                    "missing bar must be an object",
                ));
                return None;
            };
            let symbol = required_string(object, "symbol", &path, issues)?;
            let date_text = required_string(object, "date", &path, issues)?;
            match NaiveDate::parse_from_str(&date_text, "%Y-%m-%d") {
                Ok(date) => Some((symbol, date)),
                Err(_) => {
                    issues.push(issue(
                        "invalid_date",
                        format!("{path}.date"),
                        "date must use YYYY-MM-DD",
                    ));
                    None
                }
            }
        })
        .collect()
}

fn issue(
    code: impl Into<String>,
    path: impl Into<String>,
    message: impl Into<String>,
) -> NormalizationIssue {
    NormalizationIssue::new(code, path, message)
}

trait ParseDecimal: Sized {
    fn parse_decimal(literal: &str) -> Result<Self, prajna_domain::FixedPointError>;
}

impl ParseDecimal for Price {
    fn parse_decimal(literal: &str) -> Result<Self, prajna_domain::FixedPointError> {
        Self::parse(literal)
    }
}

impl ParseDecimal for Quantity {
    fn parse_decimal(literal: &str) -> Result<Self, prajna_domain::FixedPointError> {
        Self::parse(literal)
    }
}

impl ParseDecimal for Notional {
    fn parse_decimal(literal: &str) -> Result<Self, prajna_domain::FixedPointError> {
        Self::parse(literal)
    }
}

#[cfg(test)]
mod tests {
    use arrow_array::{Array, Decimal128Array, StringArray, TimestampNanosecondArray};
    use serde_json::json;

    use crate::{NormalizerRegistry, RawInput};

    const FIXTURE: &[u8] =
        include_bytes!("../../../../poc/poc0-benchmark/fixtures/dataset-v1.json");

    fn normalize(version: &str) -> crate::NormalizationOutput {
        NormalizerRegistry::with_builtins()
            .get("synthetic-etf-daily", version)
            .unwrap()
            .normalize(&[RawInput::from_bytes(FIXTURE)])
            .unwrap()
    }

    fn replace_once(input: &[u8], before: &[u8], after: &[u8]) -> Vec<u8> {
        let index = input
            .windows(before.len())
            .position(|part| part == before)
            .unwrap();
        let mut output = input.to_vec();
        output.splice(index..index + before.len(), after.iter().copied());
        output
    }

    #[test]
    fn v1_maps_fixture_into_exact_table_counts_and_hand_calculated_bars() {
        let output = normalize("1");
        assert_eq!(output.tables.instruments.num_rows(), 3);
        assert_eq!(output.tables.sessions.num_rows(), 10);
        assert_eq!(output.tables.bars.num_rows(), 29);

        let bars = &output.tables.bars;
        let ids = bars
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        let opens = bars
            .column(5)
            .as_any()
            .downcast_ref::<Decimal128Array>()
            .unwrap();
        let highs = bars
            .column(6)
            .as_any()
            .downcast_ref::<Decimal128Array>()
            .unwrap();
        let lows = bars
            .column(7)
            .as_any()
            .downcast_ref::<Decimal128Array>()
            .unwrap();
        let closes = bars
            .column(8)
            .as_any()
            .downcast_ref::<Decimal128Array>()
            .unwrap();
        let volume = bars
            .column(9)
            .as_any()
            .downcast_ref::<Decimal128Array>()
            .unwrap();
        let opens_at = bars
            .column(3)
            .as_any()
            .downcast_ref::<TimestampNanosecondArray>()
            .unwrap();
        let closes_at = bars
            .column(4)
            .as_any()
            .downcast_ref::<TimestampNanosecondArray>()
            .unwrap();
        let available_at = bars
            .column(11)
            .as_any()
            .downcast_ref::<TimestampNanosecondArray>()
            .unwrap();
        let index = 0;
        assert_eq!(opens.value(index), 100 * 10_i128.pow(18));
        assert_eq!(highs.value(index), 100 * 10_i128.pow(18));
        assert_eq!(lows.value(index), 100 * 10_i128.pow(18));
        assert_eq!(closes.value(index), 100 * 10_i128.pow(18));
        assert_eq!(volume.value(index), 1_000_000 * 10_i128.pow(18));
        assert_eq!(opens_at.value(index), 1_767_576_600_000_000_000_i64);
        assert_eq!(closes_at.value(index), 1_767_596_400_000_000_000_i64);
        assert_eq!(available_at.value(index), closes_at.value(index));
        assert_eq!(ids.value(1), "A.SYNTH");
        assert_eq!(highs.value(1), 101 * 10_i128.pow(18));
        assert_eq!(lows.value(1), 100 * 10_i128.pow(18));
        assert_eq!(closes.value(1), 101 * 10_i128.pow(18));
        assert_eq!(opens_at.value(1), 1_767_663_000_000_000_000_i64);
        assert_eq!(closes_at.value(1), 1_767_682_800_000_000_000_i64);
        assert_eq!(available_at.value(1), closes_at.value(1));
        assert_eq!(ids.value(2), "A.SYNTH");
        assert_eq!(highs.value(2), 102 * 10_i128.pow(18));
        assert_eq!(lows.value(2), 100 * 10_i128.pow(18));
        assert_eq!(closes.value(2), 102 * 10_i128.pow(18));
        assert_eq!(opens_at.value(2), 1_767_749_400_000_000_000_i64);
        assert_eq!(closes_at.value(2), 1_767_769_200_000_000_000_i64);
        assert_eq!(available_at.value(2), closes_at.value(2));

        let coverage = output.coverage.get("B").unwrap();
        assert_eq!(coverage.get("expected_sessions"), Some(&json!(10)));
        assert_eq!(coverage.get("actual_bars"), Some(&json!(9)));
        assert_eq!(coverage.get("missing_dates"), Some(&json!(["2026-01-16"])));
        assert!(
            output
                .synthetic_assumptions
                .contains(&"available_at = ts_close".into())
        );
        assert!(output.ignored_input_fields.contains(&"strategy".into()));
    }

    #[test]
    fn volume_revision_is_versioned_and_decimal_literals_remain_exact() {
        let v1 = normalize("1");
        let v2 = normalize("2");
        let v1_volume = v1
            .tables
            .bars
            .column(9)
            .as_any()
            .downcast_ref::<Decimal128Array>()
            .unwrap();
        let v2_volume = v2
            .tables
            .bars
            .column(9)
            .as_any()
            .downcast_ref::<Decimal128Array>()
            .unwrap();
        assert_eq!(v2_volume.value(0), v1_volume.value(0) * 100);
        assert_ne!(
            NormalizerRegistry::with_builtins()
                .get("synthetic-etf-daily", "1")
                .unwrap()
                .config_sha256(),
            NormalizerRegistry::with_builtins()
                .get("synthetic-etf-daily", "2")
                .unwrap()
                .config_sha256(),
        );
    }

    #[test]
    fn raw_decimal_literal_is_not_rounded_through_f64() {
        let bytes = replace_once(
            FIXTURE,
            b"\"symbol\": \"A\", \"currency\": \"CNY\", \"price_precision\": 2",
            b"\"symbol\": \"A\", \"currency\": \"CNY\", \"price_precision\": 18",
        );
        let bytes = replace_once(
            &bytes,
            b"\"closes\": [100.0, 101.0",
            b"\"closes\": [100.000000000000000001, 101.0",
        );
        let output = NormalizerRegistry::with_builtins()
            .get("synthetic-etf-daily", "1")
            .unwrap()
            .normalize(&[RawInput::from_bytes(bytes)])
            .unwrap();
        let closes = output
            .tables
            .bars
            .column(8)
            .as_any()
            .downcast_ref::<Decimal128Array>()
            .unwrap();
        assert_eq!(closes.value(0), 100_000_000_000_000_000_001_i128);
    }

    #[test]
    fn malformed_fixtures_return_structured_validation_errors() {
        let registry = NormalizerRegistry::with_builtins();
        let normalizer = registry.get("synthetic-etf-daily", "1").unwrap();
        let bad = FIXTURE
            .windows(b"\"high_rule\": \"max(open, close)\"".len())
            .position(|part| part == b"\"high_rule\": \"max(open, close)\"")
            .map(|index| {
                let mut bytes = FIXTURE.to_vec();
                bytes.splice(
                    index..index + b"\"high_rule\": \"max(open, close)\"".len(),
                    b"\"high_rule\": \"min(open, close)\"".iter().copied(),
                );
                bytes
            })
            .unwrap();
        let errors = normalizer
            .normalize(&[RawInput::from_bytes(bad)])
            .unwrap_err();
        assert!(!errors.is_empty());
        assert!(
            errors
                .iter()
                .all(|error| !error.code.is_empty() && !error.path.is_empty())
        );
        assert!(
            errors
                .iter()
                .any(|error| error.code == "invalid_ohlc_or_bar")
        );
    }
}
