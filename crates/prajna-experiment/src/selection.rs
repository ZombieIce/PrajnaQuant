//! Versioned selection: frozen task definitions without execution or browser defaults.
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use prajna_data::dataset_preview::PreviewError;
use prajna_data::{adjusted_series, dataset_preview};
use prajna_domain::{InstrumentId, Notional, Quantity, VenueId};
use prajna_research::{PanelError, StaticUniverse, UniverseError, load_panel_for_venues};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{Experiment, ExperimentError, window};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub dsv: String,
    #[serde(default)]
    pub instrument: String,
    pub pool: Option<crate::pools::Reference>,
    pub strategy: String,
    /// Existing native parameter list/grid; omitted only for legacy Buy & Hold.
    pub parameter_space: Option<Value>,
    #[serde(default)]
    pub settings: Settings,
    /// Explicit immutable adjusted research version per Universe member.
    #[serde(default)]
    pub adjusted: BTreeMap<String, String>,
    pub corporate_actions: Option<crate::actions::Binding>,
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

/// Prepares the frozen Universe and Dataset evidence shared by freeze and plan.
struct Prepared {
    frozen_pool: Option<crate::pools::PoolVersion>,
    universe: StaticUniverse,
    instruments: Vec<Value>,
    /// Real Venues of the Universe members; a shared pool may contain XSHG+XSHE.
    venues: BTreeSet<VenueId>,
    warnings: Value,
}

fn prepare(root: &Path, selection: &Selection) -> Result<Prepared, SelectionError> {
    if !matches!(
        selection.strategy.as_str(),
        "buy_and_hold" | "ma_crossover" | "s2_rotation" | "single_etf_timing"
    ) {
        return Err(error(
            "invalid_selection",
            "strategy must declare a native research window",
        ));
    }
    let frozen_pool = selection
        .pool
        .as_ref()
        .map(|reference| {
            if !selection.instrument.is_empty() {
                return Err(error(
                    "invalid_selection",
                    "choose instrument or pool, not both",
                ));
            }
            crate::pools::resolve(root, reference).map_err(|e| error(e.code, e.message))
        })
        .transpose()?;
    let universe = if let Some(version) = &frozen_pool {
        StaticUniverse::load(root, &version.universe).map_err(universe_error)?
    } else {
        let id: InstrumentId = selection
            .instrument
            .parse()
            .map_err(|e| error("invalid_selection", e))?;
        StaticUniverse::new(
            format!("single-security@1:{}", selection.instrument),
            vec![id],
        )
        .map_err(|e| error("invalid_selection", e))?
    };
    let preview = dataset_preview::show(root, &selection.dsv).map_err(|e| error(e.code(), e))?;
    let mut instruments = Vec::new();
    let mut venues = BTreeSet::new();
    for id in universe.members() {
        let venue = id.venue().as_str();
        if !matches!(venue, "XSHG" | "XSHE") {
            return Err(error(
                "unsupported_venue",
                "selection supports XSHG/XSHE only",
            ));
        }
        venues.insert(id.venue().clone());
        let instrument = preview["instruments"]
            .as_array()
            .and_then(|items| {
                items
                    .iter()
                    .find(|item| item["instrument_id"].as_str() == Some(id.to_string().as_str()))
            })
            .ok_or_else(|| {
                error(
                    "instrument_not_in_dataset",
                    "security is absent from the fixed DSV",
                )
            })?;
        validate_instrument(instrument, venue)?;
        if frozen_pool.is_some() && instrument["kind"] != "etf" {
            return Err(error(
                "invalid_selection",
                "research pools require ETF members",
            ));
        }
        instruments.push(instrument.clone());
    }
    if selection.strategy == "single_etf_timing"
        && (universe.members().len() != 1 || instruments[0]["kind"] != "etf")
    {
        return Err(error(
            "invalid_selection",
            "single_etf_timing requires exactly one ETF",
        ));
    }
    parameters(selection, universe.members().len())?;
    universe
        .validate_against(root, &selection.dsv)
        .map_err(universe_error)?;
    Ok(Prepared {
        frozen_pool,
        universe,
        instruments,
        venues,
        warnings: preview["warnings"].clone(),
    })
}

fn adjusted_json(values: &BTreeMap<InstrumentId, String>) -> Value {
    Value::Object(
        values
            .iter()
            .map(|(instrument, version)| (instrument.to_string(), Value::String(version.clone())))
            .collect(),
    )
}

fn venue_names(venues: &BTreeSet<VenueId>) -> Vec<&str> {
    venues.iter().map(VenueId::as_str).collect()
}

/// Validates the explicit per-member adjusted research versions.
///
/// The binding is required for research-pool submissions (the T08a window path)
/// and optional for the legacy single-security Buy & Hold path. Missing, corrupt,
/// foreign-DSV or mismatched-identity versions fail closed; no latest version is
/// selected and no other version is substituted.
fn research_inputs(
    root: &Path,
    selection: &Selection,
    members: &[InstrumentId],
    required: bool,
) -> Result<BTreeMap<InstrumentId, String>, SelectionError> {
    if selection.adjusted.is_empty() {
        if required {
            return Err(error(
                "adjusted_version_missing",
                "research pool submissions require an explicit adjusted version for every member",
            ));
        }
        return Ok(BTreeMap::new());
    }
    let mut frozen = BTreeMap::new();
    for (key, version) in &selection.adjusted {
        let instrument: InstrumentId = key
            .parse()
            .map_err(|e| error("adjusted_instrument_mismatch", format!("{key}: {e}")))?;
        if !members.contains(&instrument) {
            return Err(error(
                "adjusted_instrument_mismatch",
                format!("{key} is not a member of the selected Universe"),
            ));
        }
        frozen.insert(instrument, version.clone());
    }
    for member in members {
        if !frozen.contains_key(member) {
            return Err(error(
                "adjusted_version_missing",
                format!("missing adjusted version for {member}"),
            ));
        }
    }
    for (instrument, version) in &frozen {
        read_adjusted(root, &selection.dsv, instrument, version)?;
    }
    Ok(frozen)
}

/// Reads and validates one explicit adjusted research version for the selection.
fn read_adjusted(
    root: &Path,
    dsv: &str,
    instrument: &InstrumentId,
    version: &str,
) -> Result<adjusted_series::AdjustedInput, SelectionError> {
    let research = adjusted_series::read(root, version).map_err(|failure| {
        let code = match &failure {
            PreviewError::NotFound(_) => "adjusted_version_missing",
            _ => "adjusted_series_invalid",
        };
        error(code, format!("{version} for {instrument}: {failure}"))
    })?;
    if research.raw_dsv != dsv {
        return Err(error(
            "adjusted_raw_dsv_mismatch",
            format!("{version} for {instrument} belongs to another raw DSV"),
        ));
    }
    let research_instrument: InstrumentId = research
        .instrument_id
        .parse()
        .map_err(|e| error("adjusted_instrument_mismatch", format!("{version}: {e}")))?;
    if &research_instrument != instrument {
        return Err(error(
            "adjusted_instrument_mismatch",
            format!("{version} names another security"),
        ));
    }
    if research.rows.is_empty() {
        return Err(error(
            "adjusted_series_empty",
            format!("{version} for {instrument} has no research rows"),
        ));
    }
    Ok(research)
}

fn panel_error(failure: PanelError) -> SelectionError {
    let code = match &failure {
        PanelError::Manifest(_) | PanelError::InvalidManifest(_) => "invalid_manifest",
        PanelError::Data(_) | PanelError::InvalidTableData(_) => "invalid_dataset_table",
        PanelError::InvalidVenueSelection(_) => "invalid_venue_selection",
        PanelError::InconsistentSessionAxis(_) => "inconsistent_venue_calendar",
        PanelError::Polars(_) => "dataset_read_failed",
    };
    error(code, failure)
}

/// Plans the common research window from validated in-memory selection inputs.
///
/// The precheck is read-only: it does not store the content-addressed Universe or
/// the Experiment definition, so a first single-security precheck works before any
/// freeze. Coverage comes from the bound adjusted rows on the frozen Session axis;
/// execution prices stay raw.
fn plan_window(
    root: &Path,
    selection: &Selection,
    prepared: &Prepared,
    adjusted: &BTreeMap<InstrumentId, String>,
) -> Result<window::WindowPlan, SelectionError> {
    // The shared axis is validated and loaded once for the real Venue set, never
    // per member and never by picking one Venue of a mixed pool.
    let venues = prepared.venues.iter().cloned().collect::<Vec<_>>();
    let panel = load_panel_for_venues(root, &selection.dsv, &venues).map_err(panel_error)?;
    let sessions = panel
        .sessions
        .iter()
        .map(|session| session.session_date)
        .collect::<Vec<_>>();
    let mut coverage = Vec::new();
    for member in prepared.universe.members() {
        let version = adjusted.get(member).ok_or_else(|| {
            error(
                "adjusted_version_missing",
                format!("missing adjusted version for {member}"),
            )
        })?;
        let research = read_adjusted(root, &selection.dsv, member, version)?;
        let dates = research
            .rows
            .iter()
            .map(|row| row.trade_date)
            .collect::<BTreeSet<_>>();
        coverage.push(window::Coverage {
            instrument: member.to_string(),
            present: sessions.iter().map(|date| dates.contains(date)).collect(),
        });
    }
    window::plan(
        &sessions,
        &coverage,
        &parameters(selection, prepared.universe.members().len())?,
    )
    .map_err(|message| error("invalid_selection", message))
}

fn parameter_space(selection: &Selection) -> Result<Value, SelectionError> {
    selection
        .parameter_space
        .clone()
        .or_else(|| {
            (selection.strategy == "buy_and_hold").then(|| json!({"list":[{"params_version":1}]}))
        })
        .ok_or_else(|| {
            error(
                "invalid_selection",
                "parameter_space is required for this strategy",
            )
        })
}

// Reuse the Experiment native parser and S2 validation, never accept a caller's warm-up count.
fn parameters(
    selection: &Selection,
    members: usize,
) -> Result<Vec<window::Parameter>, SelectionError> {
    let parsed = crate::engine::parse_space(&selection.strategy, &parameter_space(selection)?)
        .map_err(experiment_error)?;
    let combinations = match parsed {
        crate::ParameterSpace::Native { combinations, .. } => combinations,
        space => {
            let values = match space {
                crate::ParameterSpace::Grid(axes) => {
                    crate::expand_grid(&axes).map_err(experiment_error)?
                }
                crate::ParameterSpace::List(values) => values,
                _ => unreachable!(),
            };
            let mut combinations = Vec::new();
            for value in values {
                let factor = value.factor().map_err(experiment_error)?;
                value.strategy(factor).map_err(experiment_error)?;
                if value.short >= value.long || value.top_k as u128 > members as u128 {
                    return Err(error(
                        "invalid_selection",
                        "short must be below long and top_k must fit the Universe",
                    ));
                }
                combinations.push(value.normalized());
            }
            combinations
        }
    };
    combinations
        .into_iter()
        .map(|value| {
            Ok(window::Parameter {
                warmup_sessions: window::parameter_warmup(&selection.strategy, &value)
                    .map_err(|e| error("invalid_selection", e))?,
                label: value.to_string(),
            })
        })
        .collect()
}

fn base_definition(
    selection: &Selection,
    prepared: &Prepared,
    adjusted: &BTreeMap<InstrumentId, String>,
) -> Result<Value, SelectionError> {
    let settings = &selection.settings;
    let universe = prepared
        .universe
        .id()
        .map_err(|e| error("invalid_selection", e))?;
    let mut definition = json!({
        "experiment_version": 1, "dsv": selection.dsv, "universe": universe,
        "strategy": selection.strategy, "engine": "fast_event",
        "availability_assumption": "treat_unknown_as_ts_close", "sessions_per_year": 252,
        "unfilled_entry": "retry", "parameter_space": parameter_space(selection)?,
        "costs": {"commission_rate": settings.commission_rate,
            "buy_slippage_bps": settings.buy_slippage_bps, "sell_slippage_bps": settings.sell_slippage_bps,
            "buy_tax_rate": settings.buy_tax_rate, "sell_tax_rate": settings.sell_tax_rate},
        "fast_event": {"currency": "CNY", "initial_cash": settings.initial_cash,
            "sizing": "lot", "rebalance": "entry_only", "entry_budget": "remaining_cash",
            "minimum_commission": settings.minimum_commission,
            "portfolios": [{"id": 1, "capital": settings.initial_cash}], "lot_size_source": {"kind": "instrument"}}
    });
    crate::write_venue_scope(&mut definition, &prepared.venues);
    // ADR 0027/0028: multi-Venue Fast Event accepts only full_target. A single
    // Venue keeps the historical entry-only Buy & Hold definition and identity.
    if selection.strategy != "buy_and_hold" || prepared.venues.len() > 1 {
        definition["unfilled_entry"] = json!(if selection.strategy == "buy_and_hold" {
            "retry"
        } else {
            "skip"
        });
        definition["fast_event"]["rebalance"] = json!("full_target");
        definition["fast_event"]
            .as_object_mut()
            .expect("settings object")
            .remove("entry_budget");
    }
    if !adjusted.is_empty() {
        definition["adjusted_versions"] = adjusted_json(adjusted);
    }
    Ok(definition)
}

/// Saves only a content-addressed Universe; the task service persists the definition.
///
/// A research-pool submission must bind an adjusted version for every member. The
/// shared window is computed once, rejected when not runnable, and frozen in the
/// definition so execution cannot drift. A single-security submission without an
/// adjusted binding keeps the legacy T04b definition and identity.
pub fn freeze(root: &Path, selection: &Selection) -> Result<Value, SelectionError> {
    let prepared = prepare(root, selection)?;
    let required = prepared.frozen_pool.is_some() || selection.strategy != "buy_and_hold";
    let adjusted = research_inputs(root, selection, prepared.universe.members(), required)?;
    prepared
        .universe
        .store(root)
        .map_err(|e| error("selection_store_failed", e))?;
    let mut definition = base_definition(selection, &prepared, &adjusted)?;
    if let Some(binding) = &selection.corporate_actions {
        definition["corporate_actions"] =
            serde_json::to_value(binding).map_err(|e| error("invalid_selection", e))?;
        definition["fast_event"]["rebalance"] = json!("full_target");
        definition["fast_event"]
            .as_object_mut()
            .expect("settings")
            .remove("entry_budget");
    }
    let mut window = None;
    let experiment = if adjusted.is_empty() {
        Experiment::parse(&definition.to_string(), root).map_err(experiment_error)?
    } else {
        let planned = plan_window(root, selection, &prepared, &adjusted)?;
        if !planned.runnable {
            return Err(error(
                "evaluation_window_not_runnable",
                planned.block_message(),
            ));
        }
        definition["evaluation"] = planned
            .evaluation()
            .expect("runnable window has an evaluation object");
        let experiment =
            Experiment::parse(&definition.to_string(), root).map_err(experiment_error)?;
        window = Some(planned);
        experiment
    };
    let mut response = json!({"schema_version": 1, "data": {
        "selection_version": 1, "defaults_source": "mvp7-d7-simulation@1",
        "definition": definition, "instrument": prepared.instruments[0],
        "venues": venue_names(&prepared.venues), "engine": experiment.engine_identity(),
        "warnings": prepared.warnings,
        "limitations": ["Static Universe is retrospective, not historical PIT membership",
            "Unknown daily availability treated as close; no historical publication proof",
            "Raw-price research; corporate-action total return is not complete"]
    }});
    if let Some(planned) = window {
        response["data"]["limitations"][2] = json!(
            "Supplier-adjusted research only; raw execution and valuation; corporate-action total return is not complete"
        );
        response["data"]["adjusted"] = adjusted_json(&adjusted);
        response["data"]["window"] =
            serde_json::to_value(&planned).map_err(|e| error("invalid_selection", e))?;
    }
    if selection.corporate_actions.is_some() {
        response["data"]["limitations"][2] = json!(
            "Fixed structured corporate actions use the raw-price account ledger; completeness is reported by the execution, not guaranteed by the binding"
        );
    }
    if let Some(version) = prepared.frozen_pool {
        response["data"]["pool"] = json!({"id":selection.pool.as_ref().expect("pool reference").id,
            "version":version.version,"universe":version.universe,"members":version.members});
        response["data"]["instruments"] = json!(prepared.instruments);
    }
    Ok(response)
}

/// Read-only precheck of the shared window for one frozen selection.
///
/// Planning uses the validated in-memory Universe and bound adjusted rows; it does
/// not require (or write) a stored Universe, so a first single-security precheck
/// works before any freeze. Missing or invalid adjusted bindings are returned as
/// structured blocking reasons instead of errors so the T08b preflight page can
/// display them. Errors that make the selection itself unreadable (unknown pool,
/// absent Dataset member, unsupported evidence) stay errors; a shared-axis
/// disagreement is a structural `inconsistent_venue_calendar` error, not a plan.
pub fn plan(root: &Path, selection: &Selection) -> Result<Value, SelectionError> {
    let prepared = prepare(root, selection)?;
    let mut blocking = Vec::new();
    let adjusted = match research_inputs(root, selection, prepared.universe.members(), true) {
        Ok(adjusted) => adjusted,
        Err(failure) => {
            blocking.push(json!({"code": failure.code, "message": failure.message}));
            BTreeMap::new()
        }
    };
    let mut window = None;
    if blocking.is_empty() {
        let planned = plan_window(root, selection, &prepared, &adjusted)?;
        if !planned.runnable {
            blocking = planned
                .blocking
                .iter()
                .map(|blocking| json!({"code": blocking.code, "message": blocking.message}))
                .collect();
        }
        window = Some(serde_json::to_value(&planned).map_err(|e| error("invalid_selection", e))?);
    }
    let mut data = json!({
        "selection_plan_version": 1,
        "dsv": selection.dsv,
        "strategy": selection.strategy,
        "venues": venue_names(&prepared.venues),
        "universe": prepared.universe.id().map_err(|e| error("invalid_selection", e))?,
        "adjusted": adjusted_json(&adjusted),
        "window": window,
        "runnable": blocking.is_empty() && window.is_some(),
        "blocking": blocking,
        "instruments": prepared.instruments,
    });
    // Single-Venue plans keep the historical `venue` spelling; mixed Venue sets
    // are never collapsed into one Venue for display or identity.
    if let Some(venue) = prepared
        .venues
        .first()
        .filter(|_| prepared.venues.len() == 1)
    {
        data["venue"] = json!(venue.as_str());
    }
    if let Some(version) = &prepared.frozen_pool {
        data["pool"] = json!({"id":selection.pool.as_ref().expect("pool reference").id,
            "version":version.version,"universe":version.universe,"members":version.members});
    }
    Ok(json!({"schema_version": 1, "data": data}))
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
