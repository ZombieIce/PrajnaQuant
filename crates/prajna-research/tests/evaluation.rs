use chrono::{Datelike, Duration, NaiveDate, Weekday};
use prajna_research::evaluation::{HORIZONS, Label, labels, statistics};
use std::collections::BTreeMap;

fn sessions() -> Vec<NaiveDate> {
    (0..40)
        .map(|i| NaiveDate::from_ymd_opt(2026, 1, 5).unwrap() + Duration::days(i))
        .filter(|d| !matches!(d.weekday(), Weekday::Sat | Weekday::Sun))
        .take(24)
        .collect()
}
#[test]
fn fixed_endpoints_independent_golden_and_member_isolation() {
    let days = sessions();
    let members: Vec<String> = vec!["A.XSHG".into(), "B.XSHE".into()];
    let mut opens = BTreeMap::new();
    for (i, d) in days.iter().enumerate() {
        opens.insert((members[0].clone(), *d), 100.0 + i as f64);
        opens.insert((members[1].clone(), *d), 200.0 - i as f64);
    }
    let output = labels(&days, &members, &opens).unwrap();
    assert_eq!(output.len(), 24 * 2 * 3);
    // T=first Session: entry 101/199; exit 102/198, 106/194, 121/179.
    for (h, a, b) in [
        (1, 102.0 / 101.0 - 1.0, 198.0 / 199.0 - 1.0),
        (5, 106.0 / 101.0 - 1.0, 194.0 / 199.0 - 1.0),
        (20, 121.0 / 101.0 - 1.0, 179.0 / 199.0 - 1.0),
    ] {
        for (id, expected) in [(&members[0], a), (&members[1], b)] {
            let row = output
                .iter()
                .find(|r| &r.instrument == id && r.date == days[0] && r.horizon == h)
                .unwrap();
            assert!((row.value.unwrap() - expected).abs() < 1e-14);
            assert_eq!(row.entry_date, Some(days[1]));
            assert_eq!(row.exit_date, Some(days[1 + h]));
        }
    }
    // Friday signal enters Monday and exits Tuesday; no weekend pseudo-Sessions.
    let friday = output
        .iter()
        .find(|r| r.instrument == members[0] && r.date == days[4] && r.horizon == 1)
        .unwrap();
    assert_eq!(friday.entry_date, Some(days[5]));
    assert_eq!((days[5] - days[4]).num_days(), 3);
    assert_eq!(friday.exit_date, Some(days[6]));
    // Interior missing bar cannot compress a five-Session horizon.
    opens.remove(&(members[0].clone(), days[3]));
    let missing = labels(&days, &members, &opens).unwrap();
    assert_eq!(missing[1], output[1]);
    // Missing endpoint is null even though the next bar exists; other member unchanged.
    opens.remove(&(members[0].clone(), days[6]));
    let missing = labels(&days, &members, &opens).unwrap();
    assert_eq!(missing[1].status, "missing_exit_open");
    assert_eq!(missing[1].value, None);
    assert_eq!(missing[1].exit_date, Some(days[6]));
    assert_eq!(&missing[72..], &output[72..]);
    opens.remove(&(members[0].clone(), days[1]));
    let missing = labels(&days, &members, &opens).unwrap();
    assert_eq!(missing[0].status, "missing_entry_open");
    for h in HORIZONS {
        let last = missing
            .iter()
            .find(|r| r.date == days[23] && r.horizon == h)
            .unwrap();
        assert_eq!(last.status, "insufficient_future_sessions");
        assert!(last.entry_date.is_none() && last.value.is_none());
    }
}
#[test]
fn reject_invalid_grid_prices_and_non_finite_returns_are_null() {
    let days = sessions();
    let members: Vec<String> = vec!["A.XSHG".into()];
    let mut opens = BTreeMap::new();
    for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        opens.insert((members[0].clone(), days[1]), bad);
        assert!(labels(&days, &members, &opens).is_err());
    }
    opens.clear();
    assert!(labels(&[], &members, &opens).is_err());
    assert!(labels(&[days[0], days[0]], &members, &opens).is_err());
    assert!(labels(&days, &[members[0].clone(), members[0].clone()], &opens).is_err());
    opens.insert(("other.XSHG".into(), days[1]), 1.0);
    assert!(labels(&days, &members, &opens).is_err());
    opens.clear();
    opens.insert((members[0].clone(), days[23] + Duration::days(1)), 1.0);
    assert!(labels(&days, &members, &opens).is_err());
    assert!(
        "next_open_to_forward_open"
            .parse::<prajna_research::factor::FactorKind>()
            .is_err()
    );
    assert!(
        "forward_return"
            .parse::<prajna_research::factor::FactorKind>()
            .is_err()
    );
    opens.clear();
    opens.insert((members[0].clone(), days[1]), f64::MIN_POSITIVE);
    opens.insert((members[0].clone(), days[2]), f64::MAX);
    let rows = labels(&days, &members, &opens).unwrap();
    assert_eq!(rows[0].status, "non_finite_return");
    assert_eq!(rows[0].value, None);
    assert!(
        serde_json::to_string(&rows)
            .unwrap()
            .contains("\"value\":null")
    );
}

fn day(offset: i64) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 1, 5).unwrap() + Duration::days(offset)
}
fn label_grid(
    days: &[NaiveDate],
    members: &[String],
    values: impl Fn(usize, NaiveDate, usize) -> Option<f64>,
) -> Vec<Label> {
    let mut result = Vec::new();
    for (member_index, member) in members.iter().enumerate() {
        for date in days {
            for horizon in HORIZONS {
                let value = values(member_index, *date, horizon);
                result.push(Label {
                    instrument: member.clone(),
                    date: *date,
                    horizon,
                    entry_date: None,
                    exit_date: None,
                    value,
                    status: if value.is_some() { "ok" } else { "missing" },
                });
            }
        }
    }
    result
}
fn factor_grid(
    days: &[NaiveDate],
    members: &[String],
    values: impl Fn(usize, NaiveDate) -> Option<f64>,
) -> BTreeMap<(String, NaiveDate), f64> {
    let mut result = BTreeMap::new();
    for (member_index, member) in members.iter().enumerate() {
        for date in days {
            if let Some(value) = values(member_index, *date) {
                result.insert((member.clone(), *date), value);
            }
        }
    }
    result
}

#[test]
fn cross_section_ic_hand_calculated_pearson_rank_and_average_ties() {
    let days = [day(0)];
    let members: Vec<String> = ["A.XSHG", "B.XSHG", "C.XSHG", "D.XSHG"]
        .into_iter()
        .map(Into::into)
        .collect();
    // Factor x=[1,2,2,3] has average ranks [0,1.5,1.5,3]; labels y=[1,2,4,8].
    let factor = factor_grid(&days, &members, |member, _| {
        Some([1.0, 2.0, 2.0, 3.0][member])
    });
    let labels = label_grid(&days, &members, |member, _, horizon| {
        (horizon == 1).then(|| [1.0, 2.0, 4.0, 8.0][member])
    });
    let output = statistics(&days, &members, &factor, &labels).unwrap();
    assert_eq!(output.len(), 3);
    let h1 = &output[0];
    assert_eq!(h1.rank_method, "average_rank_for_ties");
    let row = &h1.series[0];
    assert_eq!(row.status, "evaluated");
    assert_eq!(row.paired_count, 4);
    assert!((row.pearson_ic.unwrap() - 0.923_132_662_754_101_8).abs() < 1e-12);
    // Average-rank Spearman: 3/sqrt(10) = 0.9486832980505138, not the raw Pearson.
    assert!((row.rank_ic.unwrap() - 0.948_683_298_050_513_8).abs() < 1e-12);
    assert_eq!(h1.summary.evaluated_cross_sections, 1);
    assert_eq!(h1.summary.rank_ic_stddev, None);
    assert_eq!(h1.summary.reasons["rank_ic_stddev"], "insufficient_series");
    assert_eq!(h1.summary.reasons["rank_ic_ir"], "insufficient_series");
    // No H=5 label in this fixture: undefined is null with a sample reason, never 0.
    let h5 = &output[1];
    assert_eq!(h5.series[0].status, "insufficient_cross_section");
    assert!(h5.series[0].pearson_ic.is_none() && h5.series[0].rank_ic.is_none());
    assert_eq!(h5.summary.pearson_ic_mean, None);
    assert_eq!(
        h5.summary.reasons["pearson_ic_mean"],
        "no_evaluated_cross_sections"
    );
}

#[test]
fn zero_variance_insufficient_and_non_finite_are_null_with_reasons() {
    let days = [day(0)];
    let members: Vec<String> = ["A.XSHG", "B.XSHG", "C.XSHG"]
        .into_iter()
        .map(Into::into)
        .collect();
    let labels_for = |values: [f64; 3]| {
        move |member: usize, _: NaiveDate, horizon: usize| (horizon == 1).then(|| values[member])
    };
    let run = |factor_values: [f64; 3], label_values: [f64; 3]| {
        let factor = factor_grid(&days, &members, |member, _| Some(factor_values[member]));
        let labels = label_grid(&days, &members, labels_for(label_values));
        let output = statistics(&days, &members, &factor, &labels).unwrap();
        output.into_iter().next().unwrap().series.remove(0)
    };
    let constant_factor = run([2.0, 2.0, 2.0], [1.0, 2.0, 3.0]);
    assert_eq!(constant_factor.status, "zero_variance_factor");
    assert!(constant_factor.pearson_ic.is_none() && constant_factor.rank_ic.is_none());
    let constant_label = run([1.0, 2.0, 3.0], [2.0, 2.0, 2.0]);
    assert_eq!(constant_label.status, "zero_variance_label");
    assert!(constant_label.pearson_ic.is_none() && constant_label.rank_ic.is_none());
    let two_members: Vec<String> = members[..2].to_vec();
    let two_labels = label_grid(&days, &two_members, |member, _, horizon| {
        (horizon == 1).then(|| [1.0, 2.0][member])
    });
    let factor = factor_grid(&days, &two_members, |member, _| Some([1.0, 2.0][member]));
    let row = statistics(&days, &two_members, &factor, &two_labels).unwrap()[0].series[0].clone();
    assert_eq!(row.status, "insufficient_cross_section");
    assert_eq!(row.paired_count, 2);
    // Finite inputs can still overflow Pearson's centered sums. The undefined
    // Pearson value is null with a reason while the finite rank statistic remains.
    let huge = [1e308, -1e308, 1e308];
    let factor = factor_grid(&days, &members, |member, _| Some(huge[member]));
    let labels = label_grid(&days, &members, labels_for(huge));
    let row = statistics(&days, &members, &factor, &labels).unwrap()[0].series[0].clone();
    assert_eq!(row.status, "non_finite_statistic");
    assert!(row.pearson_ic.is_none());
    assert_eq!(row.rank_ic, Some(1.0));
}

#[test]
fn summary_mean_stddev_ir_and_positive_rate_use_defined_cross_sections_only() {
    let days = [day(0), day(1), day(2), day(3)];
    let members: Vec<String> = ["A.XSHG", "B.XSHG", "C.XSHG"]
        .into_iter()
        .map(Into::into)
        .collect();
    let factor = factor_grid(&days, &members, |member, date| {
        Some(if date == day(3) {
            1.0
        } else {
            [1.0, 2.0, 3.0][member]
        })
    });
    let labels = label_grid(&days, &members, |member, _, horizon| {
        (horizon == 1).then(|| [4.0, 9.0, 16.0][member])
    });
    let h1 = statistics(&days, &members, &factor, &labels)
        .unwrap()
        .remove(0);
    let summary = &h1.summary;
    assert_eq!(summary.evaluated_cross_sections, 3);
    assert_eq!(summary.unavailable_cross_sections, 1);
    assert!((summary.rank_ic_mean.unwrap() - 1.0).abs() < 1e-12);
    assert_eq!(summary.rank_ic_stddev, Some(0.0));
    assert_eq!(summary.rank_ic_ir, None);
    assert_eq!(summary.reasons["rank_ic_ir"], "zero_dispersion");
    assert_eq!(summary.rank_ic_positive_rate, Some(1.0));
    assert_eq!(h1.status_counts["zero_variance_factor"], 1);
    assert_eq!(h1.status_counts["evaluated"], 3);
    assert_eq!(h1.missing_pair_count, 0);
}

#[test]
fn statistics_reject_invalid_member_session_and_label_grids() {
    let days = [day(0), day(1)];
    let members: Vec<String> = ["A.XSHG", "B.XSHG", "C.XSHG"]
        .into_iter()
        .map(Into::into)
        .collect();
    let labels = label_grid(&days, &members, |member, date, horizon| {
        (horizon == 1).then(|| member as f64 + date.num_days_from_ce() as f64)
    });
    let factor = factor_grid(&days, &members, |member, date| {
        Some(member as f64 + date.num_days_from_ce() as f64)
    });
    assert!(statistics(&days[..1], &members, &factor, &labels).is_err());
    assert!(statistics(&[days[1], days[0]], &members, &factor, &labels).is_err());
    assert!(
        statistics(
            &days,
            &[members[0].clone(), members[0].clone()],
            &factor,
            &labels
        )
        .is_err()
    );
    let mut short = labels.clone();
    short.pop();
    assert!(statistics(&days, &members, &factor, &short).is_err());
    let mut non_finite_factor = factor.clone();
    non_finite_factor.insert((members[0].clone(), days[0]), f64::NAN);
    assert!(statistics(&days, &members, &non_finite_factor, &labels).is_err());
    let mut non_finite_label = labels.clone();
    non_finite_label[0].value = Some(f64::INFINITY);
    assert!(statistics(&days, &members, &factor, &non_finite_label).is_err());
}
