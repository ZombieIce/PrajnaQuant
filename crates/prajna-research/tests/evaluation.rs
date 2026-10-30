use chrono::{Datelike, Duration, NaiveDate, Weekday};
use prajna_research::evaluation::{HORIZONS, labels};
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
