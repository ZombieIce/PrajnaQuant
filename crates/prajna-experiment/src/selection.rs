//! Versioned single-security selection; no execution or browser-derived defaults.
use std::path::Path;

use prajna_data::dataset_preview;
use prajna_domain::{InstrumentId, Notional, Quantity};
use prajna_research::StaticUniverse;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::Experiment;

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
    let universe = StaticUniverse::new(
        format!("single-security@1:{}", selection.instrument),
        vec![id.clone()],
    )
    .map_err(|e| error("invalid_selection", e))?;
    universe
        .validate_against(root, &selection.dsv)
        .map_err(|e| error("invalid_selection", e))?;
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
    Experiment::parse(&definition.to_string(), root).map_err(|e| error("invalid_definition", e))?;
    Ok(json!({"schema_version": 1, "data": {
        "selection_version": 1, "defaults_source": "mvp7-d7-simulation@1",
        "definition": definition, "instrument": instrument,
        "warnings": preview["warnings"],
        "limitations": ["Static Universe is retrospective, not historical PIT membership",
            "Unknown daily availability treated as close; no historical publication proof",
            "Raw-price research; corporate-action total return is not complete"]
    }}))
}
