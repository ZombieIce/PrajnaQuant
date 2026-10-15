//! Immutable MVP-6 inputs and S2 decisions; execution evidence is offline, not
//! a claim that vendor daily crosschecks were historically observable at open.
use std::collections::BTreeSet;

use prajna_data::RawStore;
use prajna_domain::{InstrumentId, TimestampNs};
use serde::{Deserialize, Serialize};

use crate::{
    Panel, PanelSession,
    etf_eligible::{EligibilityParams, EligibleSet},
    etf_market::{DerivedMarket, identity},
    etf_momentum::{EtfMomentum, TotalReturnIndex},
    pit_universe::{CatalogError, Evidence, HistoricalCatalog},
    strategy::Decision,
};

pub const SEMANTIC_VERSION: &str = "pit-s2@3";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskKind {
    VoteSuspension,
    TerminationProposal,
    LiquidationNotice,
    OfficialTermination,
    LowNavWarning,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RiskNotice {
    pub instrument_id: InstrumentId,
    pub kind: RiskKind,
    pub evidence: Evidence,
}

#[derive(Debug, Clone, Serialize)]
pub struct RiskSignal {
    pub notice: RiskNotice,
    pub available_at: TimestampNs,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RiskSignals {
    pub signals: Vec<RiskSignal>,
}
impl RiskSignals {
    pub fn build(
        catalog: &HistoricalCatalog,
        mut notices: Vec<RiskNotice>,
        raw: &RawStore,
    ) -> Result<Self, CatalogError> {
        let mut members = BTreeSet::new();
        for life in &catalog.input().lifecycles {
            let id = life.instrument_id()?;
            members.insert(id.clone());
            if let Some(end) = &life.termination {
                notices.push(RiskNotice {
                    instrument_id: id,
                    kind: RiskKind::OfficialTermination,
                    evidence: end.evidence.clone(),
                });
            }
        }
        let mut signals = Vec::new();
        for notice in notices {
            if !members.contains(&notice.instrument_id) {
                return Err(CatalogError(
                    "Risk Notice outside Catalog lifecycles".into(),
                ));
            }
            notice.evidence.validate()?;
            raw.get(&notice.evidence.raw_sha256)
                .map_err(|e| CatalogError(e.to_string()))?;
            let available_at = notice.evidence.available_at(catalog.input())?;
            signals.push(RiskSignal {
                notice,
                available_at,
            });
        }
        signals.sort_by_key(|s| (s.available_at, s.notice.instrument_id.clone()));
        signals.dedup_by(|a, b| {
            a.available_at == b.available_at
                && a.notice.instrument_id == b.notice.instrument_id
                && serde_json::to_value(&a.notice).ok() == serde_json::to_value(&b.notice).ok()
        });
        Ok(Self { signals })
    }

    pub fn active(&self, at: TimestampNs) -> BTreeSet<InstrumentId> {
        self.signals
            .iter()
            .filter(|s| s.available_at <= at && !matches!(s.notice.kind, RiskKind::LowNavWarning))
            .map(|s| s.notice.instrument_id.clone())
            .collect()
    }
    pub fn active_before_open(&self, at: TimestampNs) -> BTreeSet<InstrumentId> {
        self.signals
            .iter()
            .filter(|s| s.available_at < at && !matches!(s.notice.kind, RiskKind::LowNavWarning))
            .map(|s| s.notice.instrument_id.clone())
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PitS2Params {
    pub lookback: u32,
    pub top_k: u32,
    pub rebalance_every: u32,
}
impl PitS2Params {
    pub fn validate(&self) -> Result<(), CatalogError> {
        if self.lookback == 0 || self.top_k == 0 || self.rebalance_every == 0 {
            return Err(CatalogError("PIT S2 parameters must be positive".into()));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PitDecisionInputs {
    pub eligible: EligibleSet,
    pub risk_excluded: BTreeSet<InstrumentId>,
}

pub struct PitPlan {
    pub decisions: Vec<Decision>,
    /// All Sessions, not just regular rebalance dates, retained even in Summary.
    pub inputs: Vec<PitDecisionInputs>,
    pub version: String,
}

pub fn plan(
    catalog: &HistoricalCatalog,
    market: &DerivedMarket,
    tri: &TotalReturnIndex,
    risk: &RiskSignals,
    mut eligibility: EligibilityParams,
    params: &PitS2Params,
    sessions: &[PanelSession],
) -> Result<PitPlan, CatalogError> {
    params.validate()?;
    eligibility.momentum_sessions = params.lookback;
    validate_sessions(sessions, market)?;
    let mut decisions = Vec::new();
    let mut inputs = Vec::new();
    for (i, session) in sessions.iter().enumerate() {
        let eligible = EligibleSet::build(
            catalog,
            market,
            tri,
            session.session_date,
            eligibility.clone(),
        )?;
        let risk_excluded = risk.active(session.ts_close);
        if i % params.rebalance_every as usize == 0 {
            let factor =
                EtfMomentum::new(params.lookback).map_err(|e| CatalogError(e.to_string()))?;
            let values = factor
                .compute(market, tri, &eligible.members)
                .map_err(|e| CatalogError(e.to_string()))?;
            let col = |e: polars::error::PolarsError| CatalogError(e.to_string());
            let ids = values
                .column("instrument_id")
                .map_err(col)?
                .str()
                .map_err(col)?;
            let dates = values
                .column("session_date")
                .map_err(col)?
                .date()
                .map_err(col)?
                .physical();
            let scores = values.column("value").map_err(col)?.f64().map_err(col)?;
            let epoch = chrono::NaiveDate::from_ymd_opt(1970, 1, 1).expect("epoch");
            let day = (session.session_date - epoch).num_days() as i32;
            let mut ranked = Vec::new();
            for row in 0..values.height() {
                if dates.get(row) == Some(day) {
                    let id = ids
                        .get(row)
                        .ok_or_else(|| CatalogError("missing momentum ID".into()))?
                        .parse::<InstrumentId>()
                        .map_err(|e| CatalogError(e.to_string()))?;
                    if !risk_excluded.contains(&id)
                        && let Some(score) = scores.get(row).filter(|v| v.is_finite())
                    {
                        ranked.push((id, score, 0));
                    }
                }
            }
            ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
            for (j, row) in ranked.iter_mut().enumerate() {
                row.2 = (j + 1) as u32;
            }
            let selected = ranked.len().min(params.top_k as usize);
            let targets = ranked
                .iter()
                .take(selected)
                .map(|(id, _, _)| (id.clone(), 1.0 / selected as f64))
                .collect();
            decisions.push(Decision {
                decision_session: session.clone(),
                ranked,
                targets,
            });
        }
        inputs.push(PitDecisionInputs {
            eligible,
            risk_excluded,
        });
    }
    let version = identity("pit-plan", &(SEMANTIC_VERSION, params, &inputs, risk))?;
    Ok(PitPlan {
        decisions,
        inputs,
        version,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskExitAttempt {
    pub session_date: chrono::NaiveDate,
    pub instrument_id: InstrumentId,
    pub executed: bool,
}

/// Missing cells, HALTED and UNKNOWN are never inferred sellable from a price.
pub fn permits(
    market: &DerivedMarket,
    panel: &Panel,
    id: &InstrumentId,
    s: &PanelSession,
    buy: bool,
) -> bool {
    market
        .cells()
        .get(&(id.clone(), s.session_date))
        .is_some_and(|c| c.permits(buy))
        && panel.instruments.binary_search(id).ok().is_some_and(|i| {
            panel
                .sessions
                .binary_search_by_key(&s.session_date, |s| s.session_date)
                .ok()
                .is_some_and(|j| {
                    let row = i * panel.sessions.len() + j;
                    panel
                        .grid
                        .column("has_bar")
                        .ok()
                        .and_then(|c| c.bool().ok())
                        .and_then(|c| c.get(row))
                        == Some(true)
                        && panel
                            .grid
                            .column("open_decimal")
                            .ok()
                            .and_then(|c| c.str().ok())
                            .and_then(|c| c.get(row))
                            .and_then(|p| p.parse::<prajna_domain::Price>().ok())
                            .is_some_and(|p| p > prajna_domain::Price::ZERO)
                })
        })
}

pub fn validate_execution_panel(panel: &Panel, market: &DerivedMarket) -> Result<(), CatalogError> {
    validate_sessions(&panel.sessions, market)?;
    if market
        .cells()
        .keys()
        .any(|(id, _)| !panel.instruments.contains(id))
    {
        return Err(CatalogError(
            "execution Panel/Derived Market coverage mismatch".into(),
        ));
    }
    Ok(())
}

fn validate_sessions(
    sessions: &[PanelSession],
    market: &DerivedMarket,
) -> Result<(), CatalogError> {
    if sessions.len() != market.sessions().len()
        || sessions.iter().zip(market.sessions()).any(|(a, b)| {
            a.session_date != b.session_date()
                || a.ts_open != b.ts_open()
                || a.ts_close != b.ts_close()
        })
    {
        return Err(CatalogError(
            "PIT market/Panel Session coverage mismatch".into(),
        ));
    }
    Ok(())
}

/// Split native Dataset codes into lifecycle IDs. Never copy a reused code's
/// prices across lifecycles; inactive cells are explicit missing bars.
pub fn lifecycle_panel(panel: &Panel, catalog: &HistoricalCatalog) -> Result<Panel, CatalogError> {
    use polars::prelude::*;
    let error = |e: PolarsError| CatalogError(e.to_string());
    let mut lives = catalog
        .input()
        .lifecycles
        .iter()
        .map(|l| Ok((l.instrument_id()?, l)))
        .collect::<Result<Vec<_>, CatalogError>>()?;
    lives.sort_by(|a, b| a.0.cmp(&b.0));
    let opens = panel
        .grid
        .column("open_decimal")
        .map_err(error)?
        .str()
        .map_err(error)?;
    let closes = panel
        .grid
        .column("close_decimal")
        .map_err(error)?
        .str()
        .map_err(error)?;
    let availability = panel
        .grid
        .column("close_available_at")
        .map_err(error)?
        .datetime()
        .map_err(error)?
        .physical();
    let mut ids = Vec::new();
    let mut dates = Vec::new();
    let mut indices = Vec::new();
    let mut open = Vec::new();
    let mut close = Vec::new();
    let mut open_decimal = Vec::new();
    let mut close_decimal = Vec::new();
    let mut available = Vec::new();
    let epoch = chrono::NaiveDate::from_ymd_opt(1970, 1, 1).expect("epoch");
    for (id, life) in &lives {
        let native: InstrumentId = format!("{}.SZSE", life.native_symbol)
            .parse()
            .map_err(|e: prajna_domain::InstrumentIdError| CatalogError(e.to_string()))?;
        let source = panel.instruments.binary_search(&native).ok();
        for (j, session) in panel.sessions.iter().enumerate() {
            let belongs = catalog
                .resolve_row(&life.native_symbol, session.session_date)
                .is_ok_and(|resolved| resolved == *id);
            let row = source
                .filter(|_| belongs)
                .map(|i| i * panel.sessions.len() + j);
            let o = row.and_then(|r| opens.get(r)).map(str::to_owned);
            let c = row.and_then(|r| closes.get(r)).map(str::to_owned);
            ids.push(id.to_string());
            dates.push((session.session_date - epoch).num_days() as i32);
            indices.push(j as u32);
            open.push(o.as_ref().and_then(|v| v.parse::<f64>().ok()));
            close.push(c.as_ref().and_then(|v| v.parse::<f64>().ok()));
            open_decimal.push(o);
            close_decimal.push(c);
            available.push(row.and_then(|r| availability.get(r)));
        }
    }
    let has_bar = open_decimal
        .iter()
        .zip(&close_decimal)
        .map(|(o, c)| o.is_some() && c.is_some())
        .collect::<Vec<_>>();
    let dates = Series::new("session_date".into(), dates)
        .cast(&DataType::Date)
        .map_err(error)?;
    let available = Series::new("close_available_at".into(), available)
        .cast(&DataType::Datetime(
            TimeUnit::Nanoseconds,
            Some(TimeZone::UTC),
        ))
        .map_err(error)?;
    let grid = DataFrame::new(
        ids.len(),
        vec![
            Series::new("instrument_id".into(), ids).into(),
            dates.into(),
            Series::new("session_index".into(), indices).into(),
            Series::new("open".into(), open).into(),
            Series::new("close".into(), close).into(),
            Series::new("open_decimal".into(), open_decimal).into(),
            Series::new("close_decimal".into(), close_decimal).into(),
            Series::new("has_bar".into(), has_bar).into(),
            available.into(),
        ],
    )
    .map_err(error)?;
    Ok(Panel {
        instruments: lives.into_iter().map(|(id, _)| id).collect(),
        sessions: panel.sessions.clone(),
        grid,
    })
}
