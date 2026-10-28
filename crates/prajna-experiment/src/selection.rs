//! Versioned single-security selection; no execution or browser-derived defaults.
use std::path::Path;

use prajna_data::dataset_preview;
use prajna_domain::{InstrumentId, Notional, Quantity};
use prajna_research::{StaticUniverse, UniverseError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{Experiment, ExperimentError};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub dsv: String,
    pub instrument: String,
    pub strategy: String,
    #[serde(default)]
    pub settings: Settings,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub initial_cash: String,
    pub commission_rate: f64,
    pub minimum_commission: String,
    pub buy_slippage_bps: f64,
    pub sell_slippage_bps: f64,
    pub buy_tax_rate: f64,
    pub sell_tax_rate: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            initial_cash: "1000000".into(),
            commission_rate: 0.0003,
            minimum_commission: "5".into(),
            buy_slippage_bps: 2.0,
            sell_slippage_bps: 2.0,
            buy_tax_rate: 0.0,
            sell_tax_rate: 0.0,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct SelectionError {
    pub code: &'static str,
    pub message: String,
}

fn error(code: &'static str, message: impl ToString) -> SelectionError {
    SelectionError {
        code,
        message: message.to_string(),
    }
}

/// Caller-selection errors stay 4xx; manifest/table/read failures do not.
fn universe_error(failure: UniverseError) -> SelectionError {
    let code = match &failure {
        UniverseError::MissingMembers(_) => "instrument_not_in_dataset",
        UniverseError::Manifest(_) | UniverseError::InvalidManifest(_) => "invalid_manifest",
        UniverseError::Data(_) | UniverseError::InvalidTableData(_) => "invalid_dataset_table",
        UniverseError::EmptyMembers | UniverseError::InvalidId(_) => "invalid_selection",
        UniverseError::Io(_)
        | UniverseError::Canonical(_)
        | UniverseError::Json(_)
        | UniverseError::UnsupportedVersion(_)
        | UniverseError::IntegrityMismatch { .. }
        | UniverseError::NonCanonicalContent
        | UniverseError::Conflict(_) => "dataset_read_failed",
    };
    error(code, failure)
}

fn experiment_error(failure: ExperimentError) -> SelectionError {
    match failure {
        ExperimentError::InvalidDefinition(message) => error("invalid_definition", message),
        ExperimentError::Json(json) => error("invalid_definition", json),
        ExperimentError::Manifest(manifest) => error("invalid_manifest", manifest),
        ExperimentError::Universe(universe) => universe_error(universe),
        other => error("dataset_read_failed", other),
    }
}

/// Rejects a previewed instrument whose immutable evidence cannot support a tradable selection.
fn validate_instrument(instrument: &Value, venue: &str) -> Result<(), SelectionError> {
    if !matches!(instrument["kind"].as_str(), Some("etf" | "equity"))
        || instrument["quote_currency"] != "CNY"
        || instrument["venue"] != venue
    {
        return Err(error(
            "invalid_selection",
            "unsupported security kind/currency/venue",
        ));
    }
    let lot = instrument["lot_size"]
        .as_str()
        .and_then(|s| s.parse::<Quantity>().ok());
    if lot.is_none_or(|lot| lot <= Quantity::ZERO) {
        return Err(error(
            "selection_evidence_missing",
            "positive instrument lot_size required",
        ));
    }
    for key in ["price_increment", "size_increment"] {
        if instrument[key]
            .as_str()
            .and_then(|s| s.parse::<Notional>().ok())
            .is_none_or(|value| value <= Notional::ZERO)
        {
            return Err(error(
                "selection_evidence_missing",
                format!("positive {key} required"),
            ));
        }
    }
    let lifecycle = &instrument["lifecycle"];
    let coverage = &instrument["coverage"];
    let listing = lifecycle["listing_date"]["value"].as_str();
    if lifecycle["publication"] != "publishable"
        || lifecycle["listing_date"]["state"] != "known"
        || listing.is_none()
        || coverage["bars"].as_u64().unwrap_or(0) == 0
        || coverage["venue_sessions"].as_u64().unwrap_or(0) == 0
        || coverage["first_bar"]
            .as_str()
            .is_none_or(|first| Some(first) < listing)
        || coverage["bars_without_source"].as_u64() != Some(0)
    {
        return Err(error(
            "selection_evidence_missing",
            "publishable listing, post-listing Bars and source coverage required",
        ));
    }
    Ok(())
}

/// Saves only a content-addressed Universe; the task service persists the definition.
pub fn freeze(root: &Path, selection: &Selection) -> Result<Value, SelectionError> {
    if selection.strategy != "buy_and_hold" {
        return Err(error("invalid_selection", "strategy must be buy_and_hold"));
    }
    let id: InstrumentId = selection
        .instrument
        .parse()
        .map_err(|e| error("invalid_selection", e))?;
    let venue = id.venue().as_str();
    if !matches!(venue, "XSHG" | "XSHE") {
        return Err(error(
            "unsupported_venue",
            "selection supports XSHG/XSHE only",
        ));
    }
    let preview = dataset_preview::show(root, &selection.dsv).map_err(|e| error(e.code(), e))?;
    let instrument = preview["instruments"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|item| item["instrument_id"] == selection.instrument)
        })
        .ok_or_else(|| {
            error(
                "instrument_not_in_dataset",
                "security is absent from the fixed DSV",
            )
        })?;
    validate_instrument(instrument, venue)?;
    let universe = StaticUniverse::new(
        format!("single-security@1:{}", selection.instrument),
        vec![id.clone()],
    )
    .map_err(|e| error("invalid_selection", e))?;
    universe
        .validate_against(root, &selection.dsv)
        .map_err(universe_error)?;
    universe
        .store(root)
        .map_err(|e| error("selection_store_failed", e))?;
    let settings = &selection.settings;
    let definition = json!({
        "experiment_version": 1, "dsv": selection.dsv, "universe": universe.id().map_err(|e| error("invalid_selection", e))?,
        "venue": venue, "strategy": "buy_and_hold", "engine": "fast_event",
        "availability_assumption": "treat_unknown_as_ts_close", "sessions_per_year": 252,
        "unfilled_entry": "retry", "parameter_space": {"list": [{"params_version": 1}]},
        "costs": {"commission_rate": settings.commission_rate,
            "buy_slippage_bps": settings.buy_slippage_bps, "sell_slippage_bps": settings.sell_slippage_bps,
            "buy_tax_rate": settings.buy_tax_rate, "sell_tax_rate": settings.sell_tax_rate},
        "fast_event": {"currency": "CNY", "initial_cash": settings.initial_cash,
            "sizing": "lot", "rebalance": "entry_only", "entry_budget": "remaining_cash",
            "minimum_commission": settings.minimum_commission,
            "portfolios": [{"id": 1, "capital": settings.initial_cash}], "lot_size_source": {"kind": "instrument"}}
    });
    Experiment::parse(&definition.to_string(), root).map_err(experiment_error)?;
    Ok(json!({"schema_version": 1, "data": {
        "selection_version": 1, "defaults_source": "mvp7-d7-simulation@1",
        "definition": definition, "instrument": instrument,
        "warnings": preview["warnings"],
        "limitations": ["Static Universe is retrospective, not historical PIT membership",
            "Unknown daily availability treated as close; no historical publication proof",
            "Raw-price research; corporate-action total return is not complete"]
    }}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instrument() -> Value {
        json!({
            "instrument_id": "510300.XSHG", "venue": "XSHG", "kind": "etf",
            "quote_currency": "CNY", "lot_size": "100",
            "price_increment": "0.001", "size_increment": "1",
            "lifecycle": {"publication": "publishable",
                "listing_date": {"state": "known", "value": "2020-01-01"}},
            "coverage": {"bars": 3, "venue_sessions": 3,
                "first_bar": "2026-01-05", "bars_without_source": 0}
        })
    }

    #[test]
    fn instrument_gate_rejects_each_missing_evidence_branch() {
        assert!(validate_instrument(&instrument(), "XSHG").is_ok());
        for (pointer, replacement, code) in [
            ("/kind", json!("index"), "invalid_selection"),
            ("/quote_currency", json!("USD"), "invalid_selection"),
            ("/venue", json!("XSHE"), "invalid_selection"),
            ("/lot_size", json!(null), "selection_evidence_missing"),
            ("/lot_size", json!("0"), "selection_evidence_missing"),
            ("/price_increment", json!("0"), "selection_evidence_missing"),
            ("/size_increment", json!(null), "selection_evidence_missing"),
            (
                "/lifecycle/publication",
                json!("draft"),
                "selection_evidence_missing",
            ),
            (
                "/lifecycle/listing_date/state",
                json!("unknown"),
                "selection_evidence_missing",
            ),
            (
                "/lifecycle/listing_date/value",
                json!(null),
                "selection_evidence_missing",
            ),
            ("/coverage/bars", json!(0), "selection_evidence_missing"),
            (
                "/coverage/venue_sessions",
                json!(0),
                "selection_evidence_missing",
            ),
            (
                "/coverage/first_bar",
                json!("2019-12-31"),
                "selection_evidence_missing",
            ),
            (
                "/coverage/first_bar",
                json!(null),
                "selection_evidence_missing",
            ),
            (
                "/coverage/bars_without_source",
                json!(1),
                "selection_evidence_missing",
            ),
        ] {
            let mut value = instrument();
            *value.pointer_mut(pointer).unwrap() = replacement;
            assert_eq!(
                validate_instrument(&value, "XSHG").unwrap_err().code,
                code,
                "{pointer}"
            );
        }
    }

    #[test]
    fn storage_and_read_failures_are_not_caller_input_errors() {
        assert_eq!(
            universe_error(UniverseError::MissingMembers(vec!["510300.XSHG".into()])).code,
            "instrument_not_in_dataset"
        );
        assert_eq!(
            universe_error(UniverseError::InvalidManifest("synthetic".into())).code,
            "invalid_manifest"
        );
        assert_eq!(
            universe_error(UniverseError::InvalidTableData("synthetic".into())).code,
            "invalid_dataset_table"
        );
        assert_eq!(
            universe_error(UniverseError::Io(std::io::Error::other("synthetic"))).code,
            "dataset_read_failed"
        );
        assert_eq!(
            experiment_error(ExperimentError::InvalidDefinition("synthetic".into())).code,
            "invalid_definition"
        );
        assert_eq!(
            experiment_error(ExperimentError::Universe(UniverseError::InvalidManifest(
                "synthetic".into()
            )))
            .code,
            "invalid_manifest"
        );
        assert_eq!(
            experiment_error(ExperimentError::Io(std::io::Error::other("synthetic"))).code,
            "dataset_read_failed"
        );
    }
}
