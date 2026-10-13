//! Per-Run Backend lifecycle and bounded process-pool scheduling.
use std::{
    fmt::Debug,
    sync::atomic::{AtomicUsize, Ordering},
};

use prajna_adapter::protocol::Response;
use prajna_adapter::{AdapterError, Backend};
use prajna_domain::{InstrumentId, Notional, Quantity};
use prajna_research::fast_event::{FastEventConfig, FastEventResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{BackendIdentity, ExperimentError};

pub trait ExperimentBackend: Backend {
    /// Called after Done. Implementations must check termination and collect diagnostics.
    fn finish(self: Box<Self>) -> Result<BackendDiagnostics, AdapterError>;
}

pub trait BackendFactory: Debug + Send + Sync {
    fn validate(&self, identity: &BackendIdentity) -> Result<(), AdapterError>;
    fn create(
        &self,
        identity: &BackendIdentity,
        config: &FastEventConfig,
        dsv: &str,
    ) -> Result<Box<dyn ExperimentBackend>, AdapterError>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendDiagnostics {
    pub evidence: String,
    pub adapter_constructions: Vec<Value>,
    pub native_events: Vec<Value>,
    pub comparisons: Vec<Value>,
}

impl BackendDiagnostics {
    /// Empty diagnostics explicitly identify fake evidence, never native Nautilus events.
    pub fn fake() -> Self {
        Self {
            evidence: "fake_backend".into(),
            adapter_constructions: vec![],
            native_events: vec![],
            comparisons: vec![],
        }
    }

    pub(super) fn compare_accounts(
        &mut self,
        execution: &FastEventResult,
        responses: &[Response],
        config: &FastEventConfig,
    ) -> Result<(), AdapterError> {
        let invalid = |message: &str| AdapterError::Invalid(message.into());
        if self.evidence == "fake_backend" {
            return Ok(());
        }
        if self.evidence != "nautilus_trader" {
            return Err(invalid("unmapped Backend evidence"));
        }
        let difference = |left: i128, right: i128| {
            left.checked_sub(right)
                .ok_or_else(|| invalid("comparison overflow"))
        };
        let decimal = |value: &Value| -> Result<Notional, AdapterError> {
            value
                .as_str()
                .ok_or_else(|| invalid("native cash must be a decimal string"))?
                .parse()
                .map_err(|e| AdapterError::Invalid(format!("native cash: {e}")))
        };
        let native_fees = responses
            .iter()
            .filter_map(|response| match response {
                Response::Fill(fill) => Some((fill.order_id.as_str(), fill.fee)),
                _ => None,
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut expected_cash_delta = 0i128;
        let mut snapshots = std::collections::BTreeMap::new();
        for (index, value) in self.comparisons.iter().enumerate() {
            if value["event"] == "account_balance" {
                let day = value["session_date"]
                    .as_str()
                    .ok_or_else(|| invalid("native account requires Session date"))?;
                if snapshots.insert(day.to_owned(), index).is_some() {
                    return Err(invalid("duplicate native account Session"));
                }
            }
        }
        if snapshots.len() != execution.sessions.len() {
            return Err(invalid("missing native account comparison for a Session"));
        }
        for session in &execution.sessions {
            let day = session.session.session_date.to_string();
            let index = snapshots
                .get(&day)
                .ok_or_else(|| invalid("native account Session does not match execution"))?;
            let value = &mut self.comparisons[*index];
            if value["currency"].as_str() != Some(config.currency.as_str()) {
                return Err(invalid("native account currency mismatch"));
            }
            for fill in execution
                .fills
                .iter()
                .filter(|fill| fill.fill.executed_at() == session.session.ts_open)
            {
                let id = fill.fill.order().id().0.to_string();
                let native = native_fees
                    .get(id.as_str())
                    .ok_or_else(|| invalid("native account comparison is missing Fill fee"))?;
                let costs = fill.fill.costs();
                let platform_fee = costs
                    .total_commission()
                    .map_err(|e| AdapterError::Invalid(e.to_string()))?
                    .mantissa()
                    .checked_add(costs.tax.mantissa())
                    .ok_or_else(|| invalid("comparison overflow"))?;
                expected_cash_delta = expected_cash_delta
                    .checked_add(difference(platform_fee, native.mantissa())?)
                    .ok_or_else(|| invalid("comparison overflow"))?;
            }
            let native_cash = decimal(&value["cash"])?;
            let platform = &session.close.account;
            let cash_delta = difference(native_cash.mantissa(), platform.cash.mantissa())?;
            let native_positions = value["positions"]
                .as_object()
                .ok_or_else(|| invalid("native account positions must be an object"))?
                .iter()
                .map(|(id, quantity)| {
                    let id: InstrumentId = id
                        .parse()
                        .map_err(|e| AdapterError::Invalid(format!("native Instrument: {e}")))?;
                    let quantity: Quantity = quantity
                        .as_str()
                        .ok_or_else(|| invalid("native quantity must be a decimal string"))?
                        .parse()
                        .map_err(|e| AdapterError::Invalid(format!("native quantity: {e}")))?;
                    if quantity < Quantity::ZERO || !config.lot_sizes.contains_key(&id) {
                        return Err(invalid("unmapped or negative native account position"));
                    }
                    Ok((id, quantity))
                })
                .collect::<Result<std::collections::BTreeMap<_, _>, AdapterError>>()?;
            let mut position_deltas = serde_json::Map::new();
            let mut same_positions = true;
            for id in platform.positions.keys().chain(native_positions.keys()) {
                let platform_quantity = platform
                    .positions
                    .get(id)
                    .map(|p| p.quantity)
                    .unwrap_or(Quantity::ZERO);
                let native_quantity = native_positions.get(id).copied().unwrap_or(Quantity::ZERO);
                let delta = difference(native_quantity.mantissa(), platform_quantity.mantissa())?;
                same_positions &= delta == 0;
                position_deltas.insert(
                    id.to_string(),
                    Quantity::from_mantissa(delta)
                        .map_err(|e| AdapterError::Invalid(e.to_string()))?
                        .to_string()
                        .into(),
                );
            }
            let text = |mantissa| {
                Notional::from_mantissa(mantissa)
                    .map(|value| Value::String(value.to_string()))
                    .map_err(|e| AdapterError::Invalid(e.to_string()))
            };
            value["platform_cash"] = platform.cash.to_string().into();
            value["platform_equity"] = platform.equity.to_string().into();
            value["platform_positions"] = serde_json::json!(platform.positions);
            value["valuation_point"] = "close".into();
            value["authority"] = "prajna-account".into();
            value["cash_delta"] = text(cash_delta)?;
            value["expected_cash_delta_from_native_fees"] = text(expected_cash_delta)?;
            value["position_deltas"] = position_deltas.into();
            value["difference_class"] = if !same_positions || cash_delta != expected_cash_delta {
                "unclassified"
            } else if cash_delta == 0 {
                "within_tolerance"
            } else {
                "backend_native"
            }
            .into();
            value["comparison_basis"] = "native cash/quantities vs platform close; fee difference reconciled from returned Fill fees".into();
        }
        Ok(())
    }
}

pub(super) fn run_pool<T: Sync, R: Send>(
    runs: &[T],
    workers: usize,
    execute: impl Fn(&T) -> R + Sync,
) -> Result<Vec<R>, ExperimentError> {
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        let handles = (0..workers.min(runs.len()))
            .map(|_| {
                scope.spawn(|| {
                    let mut completed = vec![];
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(run) = runs.get(index) else { break };
                        completed.push((index, execute(run)));
                    }
                    completed
                })
            })
            .collect::<Vec<_>>();
        let mut completed = vec![];
        for handle in handles {
            completed.extend(handle.join().map_err(|_| {
                ExperimentError::Execution("Backend process-pool controller panicked".into())
            })?);
        }
        completed.sort_by_key(|(index, _)| *index);
        Ok(completed.into_iter().map(|(_, result)| result).collect())
    })
}
