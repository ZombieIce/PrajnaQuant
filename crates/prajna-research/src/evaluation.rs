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

/// Version of the cross-sectional statistics contract. Changing the tie policy,
/// minimum sample, field meaning or summary math requires a new version.
pub const STATISTICS_VERSION: &str = "cross_section_ic@1";
/// ADR 0004 minimum `max(quantiles, 3)`; this contract has no grouping, so it is 3.
pub const MINIMUM_CROSS_SECTION: usize = 3;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StatisticRow {
    pub date: NaiveDate,
    pub expected_count: usize,
    pub paired_count: usize,
    pub pearson_ic: Option<f64>,
    pub rank_ic: Option<f64>,
    /// `evaluated`, `insufficient_cross_section`, `zero_variance_factor`,
    /// `zero_variance_label` or `non_finite_statistic`.
    pub status: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StatisticsSummary {
    pub evaluated_cross_sections: usize,
    pub unavailable_cross_sections: usize,
    pub pearson_ic_mean: Option<f64>,
    pub rank_ic_mean: Option<f64>,
    pub rank_ic_stddev: Option<f64>,
    pub rank_ic_ir: Option<f64>,
    pub rank_ic_positive_rate: Option<f64>,
    /// Only null summary fields appear here; no value is silently zero-filled.
    pub reasons: BTreeMap<String, &'static str>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HorizonStatistics {
    pub version: &'static str,
    pub horizon: usize,
    pub method: &'static str,
    pub rank_method: &'static str,
    pub minimum_cross_section: usize,
    pub expected_cross_sections: usize,
    pub expected_observations: usize,
    pub factor_available_count: usize,
    pub label_available_count: usize,
    pub paired_count: usize,
    pub missing_pair_count: usize,
    pub missing_ratio: f64,
    pub status_counts: BTreeMap<&'static str, usize>,
    pub series: Vec<StatisticRow>,
    pub summary: StatisticsSummary,
}

/// Cross-sectional Pearson IC and Spearman Rank IC over the same complete
/// member × Session × horizon label grid as [`labels`]. Factor values are
/// evaluation-only inputs; missing values are not filled and non-finite values
/// are rejected. Ranks use average positions for ties.
pub fn statistics(
    sessions: &[NaiveDate],
    members: &[String],
    factors: &BTreeMap<(String, NaiveDate), f64>,
    labels: &[Label],
) -> Result<Vec<HorizonStatistics>, &'static str> {
    if sessions.is_empty() || sessions.windows(2).any(|w| w[0] >= w[1]) {
        return Err("statistics requires a nonempty strictly ordered Session calendar");
    }
    let unique = members.iter().map(String::as_str).collect::<BTreeSet<_>>();
    if members.is_empty() || unique.len() != members.len() {
        return Err("statistics requires unique nonempty members");
    }
    if factors.iter().any(|((instrument, date), value)| {
        !unique.contains(instrument.as_str())
            || sessions.binary_search(date).is_err()
            || !value.is_finite()
    }) {
        return Err("statistics factor must be finite and on the member Session grid");
    }
    if labels.len() != members.len() * sessions.len() * HORIZONS.len() {
        return Err("statistics labels must cover the member Session horizon grid exactly");
    }
    let mut label_values = BTreeMap::new();
    for label in labels {
        if !unique.contains(label.instrument.as_str())
            || sessions.binary_search(&label.date).is_err()
            || !HORIZONS.contains(&label.horizon)
            || !label.value.is_none_or(f64::is_finite)
        {
            return Err("statistics label must be finite and on the member Session horizon grid");
        }
        if label_values
            .insert(
                (label.instrument.as_str(), label.date, label.horizon),
                label.value,
            )
            .is_some()
        {
            return Err("statistics labels must not repeat a member Session horizon");
        }
    }
    let factor_values = factors
        .iter()
        .map(|((instrument, date), value)| ((instrument.as_str(), *date), *value))
        .collect::<BTreeMap<_, _>>();
    let expected_observations = sessions.len() * members.len();
    let mut results = Vec::with_capacity(HORIZONS.len());
    for horizon in HORIZONS {
        let mut series = Vec::with_capacity(sessions.len());
        let mut status_counts = BTreeMap::<&'static str, usize>::new();
        let mut factor_available_count = 0;
        let mut label_available_count = 0;
        let mut paired_count = 0;
        for date in sessions {
            let mut xs = Vec::with_capacity(members.len());
            let mut ys = Vec::with_capacity(members.len());
            for member in members {
                let factor = factor_values.get(&(member.as_str(), *date)).copied();
                let label = label_values
                    .get(&(member.as_str(), *date, horizon))
                    .copied()
                    .flatten();
                factor_available_count += usize::from(factor.is_some());
                label_available_count += usize::from(label.is_some());
                if let (Some(factor), Some(label)) = (factor, label) {
                    xs.push(factor);
                    ys.push(label);
                }
            }
            let paired = xs.len();
            let (pearson_ic, rank_ic, status) = cross_section(&xs, &ys);
            *status_counts.entry(status).or_default() += 1;
            paired_count += paired;
            series.push(StatisticRow {
                date: *date,
                expected_count: members.len(),
                paired_count: paired,
                pearson_ic,
                rank_ic,
                status,
            });
        }
        results.push(HorizonStatistics {
            version: STATISTICS_VERSION,
            horizon,
            method: "pearson_ic_and_spearman_rank_ic",
            rank_method: "average_rank_for_ties",
            minimum_cross_section: MINIMUM_CROSS_SECTION,
            expected_cross_sections: sessions.len(),
            expected_observations,
            factor_available_count,
            label_available_count,
            paired_count,
            missing_pair_count: expected_observations - paired_count,
            missing_ratio: (expected_observations - paired_count) as f64
                / expected_observations as f64,
            status_counts,
            summary: summary(&series),
            series,
        });
    }
    Ok(results)
}

fn cross_section(xs: &[f64], ys: &[f64]) -> (Option<f64>, Option<f64>, &'static str) {
    if xs.len() < MINIMUM_CROSS_SECTION {
        return (None, None, "insufficient_cross_section");
    }
    if distinct_count(xs) == 1 {
        return (None, None, "zero_variance_factor");
    }
    if distinct_count(ys) == 1 {
        return (None, None, "zero_variance_label");
    }
    let pearson_value = pearson(xs, ys);
    let rank_value = pearson(&ranks(xs), &ranks(ys));
    let pearson_ic = pearson_value.is_finite().then_some(pearson_value);
    let rank_ic = rank_value.is_finite().then_some(rank_value);
    let status = if pearson_ic.is_some() && rank_ic.is_some() {
        "evaluated"
    } else {
        "non_finite_statistic"
    };
    (pearson_ic, rank_ic, status)
}

fn summary(series: &[StatisticRow]) -> StatisticsSummary {
    let pearson = series
        .iter()
        .filter_map(|row| row.pearson_ic)
        .collect::<Vec<_>>();
    let rank = series
        .iter()
        .filter_map(|row| row.rank_ic)
        .collect::<Vec<_>>();
    let mut reasons = BTreeMap::new();
    let pearson_mean = mean(&pearson, "pearson_ic_mean", &mut reasons);
    let rank_mean = mean(&rank, "rank_ic_mean", &mut reasons);
    let rank_short_reason = if rank.is_empty() {
        Some("no_evaluated_cross_sections")
    } else if rank.len() < 2 {
        Some("insufficient_series")
    } else {
        None
    };
    let rank_stddev = if let Some(reason) = rank_short_reason {
        reasons.insert("rank_ic_stddev".into(), reason);
        None
    } else {
        Some(stddev(&rank, rank_mean.unwrap_or_default()))
    };
    let rank_ir = if let Some(reason) = rank_short_reason {
        reasons.insert("rank_ic_ir".into(), reason);
        None
    } else if rank_stddev == Some(0.0) {
        reasons.insert("rank_ic_ir".into(), "zero_dispersion");
        None
    } else {
        Some(rank_mean.unwrap_or_default() / rank_stddev.unwrap_or_default())
    };
    let rank_positive_rate = if rank.is_empty() {
        reasons.insert(
            "rank_ic_positive_rate".into(),
            "no_evaluated_cross_sections",
        );
        None
    } else {
        let positive = rank.iter().filter(|value| **value > 0.0).count();
        Some(positive as f64 / rank.len() as f64)
    };
    let evaluated_cross_sections = series
        .iter()
        .filter(|row| row.status == "evaluated")
        .count();
    StatisticsSummary {
        evaluated_cross_sections,
        unavailable_cross_sections: series.len() - evaluated_cross_sections,
        pearson_ic_mean: pearson_mean,
        rank_ic_mean: rank_mean,
        rank_ic_stddev: rank_stddev,
        rank_ic_ir: rank_ir,
        rank_ic_positive_rate: rank_positive_rate,
        reasons,
    }
}

fn mean(values: &[f64], name: &str, reasons: &mut BTreeMap<String, &'static str>) -> Option<f64> {
    if values.is_empty() {
        reasons.insert(name.into(), "no_evaluated_cross_sections");
        None
    } else {
        Some(values.iter().sum::<f64>() / values.len() as f64)
    }
}

fn stddev(values: &[f64], mean: f64) -> f64 {
    (values
        .iter()
        .map(|value| (value - mean).powi(2))
        .sum::<f64>()
        / (values.len() - 1) as f64)
        .sqrt()
}

/// Counts distinct finite values; ties are equal `f64` values, not close values.
fn distinct_count(values: &[f64]) -> usize {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted.dedup_by(|left, right| left.total_cmp(right).is_eq());
    sorted.len()
}

/// Average ranks (0-based) for tied values, so ties never depend on input order.
fn ranks(values: &[f64]) -> Vec<f64> {
    let mut order: Vec<usize> = (0..values.len()).collect();
    order.sort_by(|left, right| values[*left].total_cmp(&values[*right]));
    let mut result = vec![0.0; values.len()];
    let mut start = 0;
    while start < order.len() {
        let mut end = start + 1;
        while end < order.len() && values[order[start]].total_cmp(&values[order[end]]).is_eq() {
            end += 1;
        }
        let rank = (start + end - 1) as f64 / 2.0;
        for index in start..end {
            result[order[index]] = rank;
        }
        start = end;
    }
    result
}

fn pearson(x: &[f64], y: &[f64]) -> f64 {
    if x.len() != y.len() || x.len() < 2 {
        return f64::NAN;
    }
    let count = x.len() as f64;
    let mean_x = x.iter().sum::<f64>() / count;
    let mean_y = y.iter().sum::<f64>() / count;
    let mut covariance = 0.0;
    let mut variance_x = 0.0;
    let mut variance_y = 0.0;
    for (left, right) in x.iter().zip(y) {
        let dx = left - mean_x;
        let dy = right - mean_y;
        covariance += dx * dy;
        variance_x += dx * dx;
        variance_y += dy * dy;
    }
    if variance_x == 0.0 || variance_y == 0.0 {
        f64::NAN
    } else {
        covariance / (variance_x * variance_y).sqrt()
    }
}
