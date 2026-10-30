//! A separate signal Panel: bound adjusted closes never reach the raw ledger.
use super::*;
use polars::prelude::{NamedFrom, Series};
pub(super) fn research_panel(
    experiment: &Experiment,
    root: &Path,
    raw: &Panel,
) -> Result<Panel, ExperimentError> {
    if experiment.adjusted_versions.len() != 1 || experiment.evaluation.is_none() {
        return Err(invalid(
            "single_etf_timing requires one bound adjusted version and evaluation window",
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
    Ok(Panel {
        sessions: raw.sessions.clone(),
        instruments: raw.instruments.clone(),
        grid,
    })
}
