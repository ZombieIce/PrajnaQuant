//! Evaluation-only future labels. No FactorKind, Cache or Decision input accepts these rows.
use chrono::NaiveDate;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

pub const VERSION: &str = "next_open_to_forward_open@1";
pub const HORIZONS: [usize; 3] = [1, 5, 20];

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Label {
    pub instrument: String,
    pub date: NaiveDate,
    pub horizon: usize,
    pub entry_date: Option<NaiveDate>,
    pub exit_date: Option<NaiveDate>,
    pub value: Option<f64>,
    pub status: &'static str,
}

/// `sessions` is the complete, aligned exchange calendar, never observed bar dates.
/// Opens must use the same explicit adjusted-price version as the research factors.
/// Interior missing bars do not matter; required endpoints never skip to a later bar.
pub fn labels(
    sessions: &[NaiveDate],
    members: &[String],
    opens: &BTreeMap<(String, NaiveDate), f64>,
) -> Result<Vec<Label>, &'static str> {
    if sessions.is_empty() || sessions.windows(2).any(|w| w[0] >= w[1]) {
        return Err("evaluation requires a nonempty strictly ordered Session calendar");
    }
    let unique = members.iter().collect::<BTreeSet<_>>();
    if members.is_empty() || unique.len() != members.len() {
        return Err("evaluation requires unique nonempty members");
    }
    if opens.iter().any(|((id, date), value)| {
        !unique.contains(id)
            || sessions.binary_search(date).is_err()
            || !value.is_finite()
            || *value <= 0.0
    }) {
        return Err("evaluation open must be positive finite and on the member Session grid");
    }
    let mut result = Vec::new();
    for id in members {
        for (index, date) in sessions.iter().enumerate() {
            for horizon in HORIZONS {
                let entry_date = sessions.get(index + 1).copied();
                let exit_date = sessions.get(index + 1 + horizon).copied();
                let entry = entry_date.and_then(|d| opens.get(&(id.clone(), d)));
                let exit = exit_date.and_then(|d| opens.get(&(id.clone(), d)));
                let (value, status) = match (entry_date, exit_date, entry, exit) {
                    (None, _, _, _) | (_, None, _, _) => (None, "insufficient_future_sessions"),
                    (_, _, None, _) => (None, "missing_entry_open"),
                    (_, _, _, None) => (None, "missing_exit_open"),
                    (_, _, Some(entry), Some(exit)) => {
                        let value = exit / entry - 1.0;
                        if value.is_finite() {
                            (Some(value), "ok")
                        } else {
                            (None, "non_finite_return")
                        }
                    }
                };
                result.push(Label {
                    instrument: id.clone(),
                    date: *date,
                    horizon,
                    entry_date,
                    exit_date,
                    value,
                    status,
                });
            }
        }
    }
    Ok(result)
}
