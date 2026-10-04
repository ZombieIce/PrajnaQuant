use prajna_research::vector::VectorResult;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSummary {
    pub total_return: Option<f64>,
    pub annualized_return: Option<f64>,
    pub n_returns: usize,
    pub mean_return: Option<f64>,
    pub std_return: Option<f64>,
    pub annualized_volatility: Option<f64>,
    pub sharpe: Option<f64>,
    pub max_drawdown: Option<f64>,
    pub drawdown_peak_session: Option<String>,
    pub drawdown_trough_session: Option<String>,
    pub total_turnover: f64,
    pub total_cost: f64,
    pub executed_count: usize,
    pub deferred_count: usize,
    pub skipped_buy_count: usize,
    pub session_count: usize,
    pub pending_at_end: bool,
    pub insufficient_sessions: bool,
    pub zero_volatility: bool,
    pub assumptions: Vec<String>,
    pub availability_assumption: String,
}

/// Summarize Vector output using the Session-open NAV interval defined by ADR 0017.
/// The first Session establishes NAV=1.0; returns therefore start at Session 2.
pub fn summarize(result: &VectorResult, sessions_per_year: u64) -> RunSummary {
    assert!(sessions_per_year > 0, "sessions_per_year must be positive");

    let session_count = result.sessions.len();
    let n_returns = session_count.saturating_sub(1);
    let returns = result
        .sessions
        .iter()
        .skip(1)
        .map(|session| session.net_return)
        .collect::<Vec<_>>();
    let mean_return =
        (!returns.is_empty()).then(|| returns.iter().sum::<f64>() / returns.len() as f64);
    let insufficient_sessions = returns.len() < 2;
    let std_return = (!insufficient_sessions).then(|| sample_std(&returns));
    let annualized_volatility = std_return.map(|std| std * (sessions_per_year as f64).sqrt());
    let zero_volatility = std_return == Some(0.0);
    let sharpe = match (mean_return, std_return) {
        (Some(mean), Some(std)) if std != 0.0 => {
            Some(mean / std * (sessions_per_year as f64).sqrt())
        }
        _ => None,
    };

    let nav_end = result.sessions.last().map(|session| session.nav);
    let total_return = nav_end.map(|nav| nav - 1.0);
    let annualized_return = nav_end
        .filter(|_| n_returns > 0)
        .map(|nav| nav.powf(sessions_per_year as f64 / n_returns as f64) - 1.0);
    let (max_drawdown, drawdown_peak_session, drawdown_trough_session) = drawdown(&result.sessions);

    RunSummary {
        total_return,
        annualized_return,
        n_returns,
        mean_return,
        std_return,
        annualized_volatility,
        sharpe,
        max_drawdown,
        drawdown_peak_session,
        drawdown_trough_session,
        total_turnover: result.sessions.iter().map(|session| session.turnover).sum(),
        total_cost: result.sessions.iter().map(|session| session.cost).sum(),
        executed_count: result
            .executions
            .iter()
            .filter(|execution| {
                execution.kind == prajna_research::vector::VectorExecutionKind::Executed
            })
            .count(),
        deferred_count: result
            .executions
            .iter()
            .filter(|execution| {
                execution.kind == prajna_research::vector::VectorExecutionKind::Deferred
            })
            .count(),
        skipped_buy_count: result
            .executions
            .iter()
            .map(|execution| execution.skipped_buys.len())
            .sum(),
        session_count,
        pending_at_end: result.pending_at_end.is_some(),
        insufficient_sessions,
        zero_volatility,
        assumptions: result.assumptions.clone(),
        availability_assumption: result.availability_assumption.clone(),
    }
}

fn sample_std(values: &[f64]) -> f64 {
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let sum_squared_deviations = values
        .iter()
        .map(|value| (value - mean).powi(2))
        .sum::<f64>();
    (sum_squared_deviations / (values.len() - 1) as f64).sqrt()
}

fn drawdown(
    sessions: &[prajna_research::vector::VectorSession],
) -> (Option<f64>, Option<String>, Option<String>) {
    let Some(first_session) = sessions.first() else {
        return (None, None, None);
    };

    let mut peak_nav = 1.0;
    let mut peak_session = first_session.session_date.clone();
    let mut max_drawdown = 0.0_f64;
    let mut drawdown_peak_session = None;
    let mut drawdown_trough_session = None;

    for session in sessions {
        if session.nav > peak_nav {
            peak_nav = session.nav;
            peak_session.clone_from(&session.session_date);
        }
        let current_drawdown = session.nav / peak_nav - 1.0;
        if current_drawdown < max_drawdown {
            max_drawdown = current_drawdown;
            drawdown_peak_session = Some(peak_session.clone());
            drawdown_trough_session = Some(session.session_date.clone());
        }
    }

    if max_drawdown == 0.0 {
        (Some(0.0), None, None)
    } else {
        (
            Some(max_drawdown),
            drawdown_peak_session,
            drawdown_trough_session,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prajna_research::vector::{
        VectorCosts, VectorExecution, VectorExecutionKind, VectorPending, VectorSession,
    };
    use std::collections::{BTreeMap, BTreeSet};

    fn result(nav_and_returns: &[(f64, f64)]) -> VectorResult {
        VectorResult {
            sessions: nav_and_returns
                .iter()
                .enumerate()
                .map(|(index, (nav, net_return))| VectorSession {
                    session_date: format!("2026-01-{:02}", index + 1),
                    target_weights: BTreeMap::new(),
                    weights_after_execution: BTreeMap::new(),
                    turnover: 0.0,
                    cost: 0.0,
                    gross_return: *net_return,
                    net_return: *net_return,
                    nav: *nav,
                    valuation_carried: Vec::new(),
                })
                .collect(),
            decisions: Vec::new(),
            executions: Vec::new(),
            pending_at_end: None,
            assumptions: vec!["synthetic assumptions".into()],
            costs: VectorCosts {
                commission_rate: 0.0,
                buy_slippage_bps: 0.0,
                sell_slippage_bps: 0.0,
                buy_tax_rate: 0.0,
                sell_tax_rate: 0.0,
            },
            availability_assumption: "none".into(),
        }
    }

    #[test]
    fn never_declining_run_has_zero_drawdown_without_peak_or_trough() {
        let summary = summarize(&result(&[(1.0, 0.0), (1.1, 0.1), (1.2, 0.1)]), 252);
        assert_eq!(summary.max_drawdown, Some(0.0));
        assert_eq!(summary.drawdown_peak_session, None);
        assert_eq!(summary.drawdown_trough_session, None);
        assert!(!summary.insufficient_sessions);
    }

    #[test]
    fn one_return_keeps_mean_but_nulls_sample_metrics_for_insufficient_sessions() {
        let summary = summarize(&result(&[(1.0, 0.0), (1.1, 0.1)]), 1);
        assert_eq!(summary.n_returns, 1);
        assert_eq!(summary.mean_return, Some(0.1));
        assert_eq!(summary.std_return, None);
        assert_eq!(summary.annualized_volatility, None);
        assert_eq!(summary.sharpe, None);
        assert!(summary.insufficient_sessions);
        assert!(!summary.zero_volatility);
    }

    #[test]
    fn zero_volatility_nulls_sharpe_with_an_explicit_reason() {
        let summary = summarize(&result(&[(1.0, 0.0), (1.1, 0.1), (1.21, 0.1)]), 252);
        assert_eq!(summary.std_return, Some(0.0));
        assert_eq!(summary.annualized_volatility, Some(0.0));
        assert_eq!(summary.sharpe, None);
        assert!(!summary.insufficient_sessions);
        assert!(summary.zero_volatility);
    }

    #[test]
    fn all_cash_run_has_zero_totals_and_null_sharpe_for_zero_volatility() {
        let mut vector = result(&[(1.0, 0.0), (1.0, 0.0), (1.0, 0.0)]);
        vector.executions.push(VectorExecution {
            session_date: "2026-01-02".into(),
            kind: VectorExecutionKind::Executed,
            blocked: BTreeSet::new(),
            skipped_buys: BTreeSet::new(),
        });
        vector.pending_at_end = Some(VectorPending {
            decision_session: "2026-01-03".into(),
            targets: BTreeMap::new(),
        });
        let summary = summarize(&vector, 252);
        assert_eq!(summary.total_return, Some(0.0));
        assert_eq!(summary.total_turnover, 0.0);
        assert_eq!(summary.total_cost, 0.0);
        assert_eq!(summary.executed_count, 1);
        assert!(summary.pending_at_end);
        assert_eq!(summary.sharpe, None);
        assert!(summary.zero_volatility);
    }
}
