//! A separate signal Panel: bound adjusted closes never reach the raw ledger.
use super::*;
use polars::prelude::{DataType, NamedFrom, Series, TimeUnit, TimeZone};
pub(super) fn research_panel(
    experiment: &Experiment,
    root: &Path,
    raw: &Panel,
) -> Result<Panel, ExperimentError> {
    if experiment.adjusted_versions.is_empty() || experiment.evaluation.is_none() {
        return Err(invalid(
            "research templates require bound adjusted versions and a frozen evaluation window",
        ));
    }
    let mut prices = BTreeMap::new();
    for (member, version) in &experiment.adjusted_versions {
        let input = adjusted_series::read(root, version).map_err(|e| invalid(e.to_string()))?;
        for row in input.rows {
            prices.insert(
                (member.to_string(), row.trade_date),
                row.close
                    .parse::<f64>()
                    .map_err(|e| invalid(e.to_string()))?,
            );
        }
    }
    let ids = raw
        .grid
        .column("instrument_id")
        .map_err(|e| invalid(e.to_string()))?
        .str()
        .map_err(|e| invalid(e.to_string()))?;
    let indices = raw
        .grid
        .column("session_index")
        .map_err(|e| invalid(e.to_string()))?
        .u32()
        .map_err(|e| invalid(e.to_string()))?;
    let closes = (0..raw.grid.height())
        .map(|i| {
            prices
                .get(&(
                    ids.get(i).unwrap().to_owned(),
                    raw.sessions[indices.get(i).unwrap() as usize].session_date,
                ))
                .copied()
        })
        .collect::<Vec<_>>();
    let mut grid = raw.grid.clone();
    grid.with_column(
        Series::new(
            "has_bar".into(),
            closes.iter().map(Option::is_some).collect::<Vec<_>>(),
        )
        .into(),
    )
    .map_err(|e| invalid(e.to_string()))?;
    grid.with_column(Series::new("close".into(), closes).into())
        .map_err(|e| invalid(e.to_string()))?;
    if experiment.execution_mode.is_some() {
        let times = (0..indices.len())
            .map(|row| {
                indices
                    .get(row)
                    .map(|i| raw.sessions[i as usize].ts_close.as_unix_nanos())
            })
            .collect::<Vec<_>>();
        let available = Series::new("close_available_at".into(), times)
            .cast(&DataType::Datetime(
                TimeUnit::Nanoseconds,
                Some(TimeZone::UTC),
            ))
            .map_err(|e| invalid(e.to_string()))?;
        grid.with_column(available.into())
            .map_err(|e| invalid(e.to_string()))?;
    }
    Ok(Panel {
        sessions: raw.sessions.clone(),
        instruments: raw.instruments.clone(),
        grid,
    })
}

/// Scheduled member-signal dates whose adjusted inputs are missing. The
/// template cadence and lookback count actual Sessions, never available rows.
pub(super) fn signal_gaps(
    run: &ExpandedRun,
    research: &Panel,
    ledger: &Panel,
    members: &[InstrumentId],
) -> Result<BTreeMap<String, Vec<NaiveDate>>, String> {
    use prajna_research::strategy::VectorStrategy;
    let (stride, offsets) = match &run.strategy {
        RunStrategy::Native(VectorStrategy::SingleEtfTiming(params)) => (
            params.rebalance_every as usize,
            0..=params.ma_window as usize - 1,
        ),
        RunStrategy::Native(VectorStrategy::PoolMomentumTopK(params)) => (
            params.rebalance_every as usize,
            // Momentum consumes the two endpoint closes.
            params.lookback as usize..=params.lookback as usize,
        ),
        _ => return Ok(BTreeMap::new()),
    };
    let date = |session: &prajna_research::PanelSession| session.session_date;
    let first = research
        .sessions
        .binary_search_by_key(&date(&ledger.sessions[0]), date)
        .map_err(|_| "evaluation start missing from signal Session axis")?;
    let present = research
        .grid
        .column("has_bar")
        .and_then(|col| col.bool())
        .map_err(|e| e.to_string())?;
    let mut gaps = BTreeMap::new();
    for member in members {
        let offset = research
            .instruments
            .binary_search(member)
            .map_err(|_| format!("missing signal Instrument {member}"))?
            * research.sessions.len();
        let missing = (first..first + ledger.sessions.len())
            .step_by(stride)
            .filter(|index| {
                let has = |lag| {
                    index
                        .checked_sub(lag)
                        .is_some_and(|i| present.get(offset + i) == Some(true))
                };
                !has(0) || offsets.clone().any(|lag| !has(lag))
            })
            .map(|i| research.sessions[i].session_date)
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            gaps.insert(member.to_string(), missing);
        }
    }
    Ok(gaps)
}
