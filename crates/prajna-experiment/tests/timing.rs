//! Independent decimal hand-ledger. Raw open/close=10 throughout; adjusted
//! closes alone generate signals. 900 shares at 10.01 + 5 commission + 9.009
//! buy tax leaves 976.991; exit at 9.99 - 5 - 17.982 leaves 9945.009.
use prajna_data::daily_publish::{DailyPublicationSelection, publish_daily};
use prajna_domain::InstrumentKind;
use prajna_experiment::{
    Experiment,
    selection::{Selection, freeze},
};
use serde_json::json;
use std::path::Path;
use tempfile::TempDir;
#[path = "support/selection_fixture.rs"]
mod fixture;
#[path = "../../prajna-data/tests/support/opening_fixture.rs"]
mod opening;
const DATES: [&str; 10] = [
    "2026-01-05",
    "2026-01-06",
    "2026-01-07",
    "2026-01-08",
    "2026-01-09",
    "2026-01-12",
    "2026-01-13",
    "2026-01-14",
    "2026-01-15",
    "2026-01-16",
];

fn setup(root: &Path, blocked: bool) -> Selection {
    setup_scenario(
        root,
        if blocked { &[4] } else { &[] },
        &["10", "12", "12", "12", "8", "8", "14", "14", "14", "10"],
    )
}
fn setup_scenario(root: &Path, blocked: &[usize], closes: &[&str]) -> Selection {
    let input = root.join("input");
    let bars = [("10", "10", "10", "10"); 10];
    let (mut selection, mut snapshot) = opening::prepare(
        &input,
        "XSHG",
        InstrumentKind::Etf,
        false,
        Some((&DATES, &bars)),
    );
    if !blocked.is_empty() {
        for index in blocked {
            snapshot["facts"][*index]["state"] = json!("HALTED");
        }
        selection["status_snapshot"] = json!(opening::put(&input, &snapshot));
    }
    let selection: DailyPublicationSelection = serde_json::from_value(selection).unwrap();
    let dsv = publish_daily(root, selection, Path::new("receipt.json"))
        .unwrap()
        .dsv;
    let adjusted = fixture::import_closes(root, &dsv, &DATES, closes);
    serde_json::from_value(json!({"dsv":dsv,"instrument":"510300.XSHG","strategy":"single_etf_timing",
        "adjusted":{"510300.XSHG":adjusted},"parameter_space":{"list":[{"params_version":1,"ma_window":2,"rebalance_every":2}]},
        "settings":{"initial_cash":"10000","commission_rate":0.0003,"minimum_commission":"5","buy_slippage_bps":10,"sell_slippage_bps":10,"buy_tax_rate":0.001,"sell_tax_rate":0.002}})).unwrap()
}

#[test]
fn strict_threshold_warmup_cash_cost_lots_and_deferred_exit_match_hand_ledger() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    let selection = setup(root, true);
    let frozen = freeze(root, &selection).unwrap();
    let definition = &frozen["data"]["definition"];
    assert_eq!(definition["evaluation"]["warmup_sessions"], 1);
    assert_eq!(definition["evaluation"]["start"], DATES[1]);
    assert_eq!(
        definition["parameter_space"]["list"][0]["rebalance_every"],
        2
    );
    assert_eq!(
        definition["fast_event"]["lot_size_source"]["kind"],
        "instrument"
    );
    assert_eq!(definition["costs"]["sell_tax_rate"], 0.002);
    let experiment = Experiment::parse(&definition.to_string(), root).unwrap();
    let report = experiment.execute(root, 1).unwrap();
    let run = &report.runs[0];
    assert!(run.error.is_none(), "{:?}", run.error);
    let result = run.fast_event_result.as_ref().unwrap();
    assert_eq!(result.sessions.len(), 9); // no warm-up valuation or trade
    let expected_cash = [
        "10000", "976.991", "976.991", "976.991", "9945.009", "9945.009", "9945.009", "9945.009",
        "9945.009",
    ];
    let expected_equity = [
        "10000", "9976.991", "9976.991", "9976.991", "9945.009", "9945.009", "9945.009",
        "9945.009", "9945.009",
    ];
    for (i, session) in result.sessions.iter().enumerate() {
        let row = serde_json::to_value(&session.close.account).unwrap();
        assert_eq!(session.session.session_date.to_string(), DATES[i + 1]);
        assert_eq!(row["cash"], expected_cash[i]);
        assert_eq!(row["equity"], expected_equity[i]);
        assert_eq!(row["dividend_receivable"], "0");
        if (1..=3).contains(&i) {
            assert_eq!(row["positions"]["510300.XSHG"]["quantity"], "900");
        } else {
            assert_eq!(row["positions"], json!({}));
        }
    }
    assert_eq!(result.account.fills().len(), 2);
    for (index, fill) in result.account.fills().iter().enumerate() {
        let value = serde_json::to_value(fill).unwrap();
        assert_eq!(value["raw_price"], "10");
        assert_eq!(value["price"], if index == 0 { "10.01" } else { "9.99" });
        assert_eq!(value["slippage"], "9");
        assert_eq!(
            value["costs"]["commission"],
            if index == 0 { "2.7027" } else { "2.6973" }
        );
        assert_eq!(
            value["costs"]["minimum_commission_top_up"],
            if index == 0 { "2.2973" } else { "2.3027" }
        );
        assert_eq!(
            value["costs"]["tax"],
            if index == 0 { "9.009" } else { "17.982" }
        );
    }

    let summary = run.summary.as_ref().unwrap();
    assert_eq!(summary.deferred_count, 1);
    assert!(summary.pending_at_end);
    assert!((summary.total_return.unwrap() + 0.0054991).abs() < 1e-10);
    // The later above signal at index6 lies on an unscheduled Session and
    // does not buy. Equality at index3 requests cash, with a blocked
    // next open at index4; the same pending sell executes at the following open.
    let specs = experiment.run_definitions(root).unwrap();
    assert!(specs[0].run_spec["adjusted_versions"].is_object());
    let mut changed = definition.clone();
    changed["parameter_space"]["list"][0]["rebalance_every"] = json!(1);
    let other = Experiment::parse(&changed.to_string(), root).unwrap();
    assert_ne!(experiment.id().unwrap(), other.id().unwrap());
    assert_ne!(
        specs[0].run_id,
        other.run_definitions(root).unwrap()[0].run_id
    );
}

#[test]
fn invalid_inputs_fail_closed_and_all_equal_closes_stay_cash() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    let mut selection = setup(root, false);
    let frozen = freeze(root, &selection).unwrap();
    let definition = frozen["data"]["definition"].clone();
    for (key, value) in [
        ("ma_window", 0),
        ("rebalance_every", 0),
        ("params_version", 2),
    ] {
        let mut invalid = definition.clone();
        invalid["parameter_space"]["list"][0][key] = json!(value);
        assert!(Experiment::parse(&invalid.to_string(), root).is_err());
    }
    let mut raw = definition.clone();
    raw.as_object_mut().unwrap().remove("adjusted_versions");
    raw.as_object_mut().unwrap().remove("evaluation");
    assert!(Experiment::parse(&raw.to_string(), root).is_err());
    let mut accurate = definition.clone();
    accurate["engine"] = json!("accurate_event");
    assert!(Experiment::parse(&accurate.to_string(), root).is_err());
    selection.parameter_space =
        Some(json!({"list":[{"params_version":1,"ma_window":100,"rebalance_every":2}]}));
    assert_eq!(
        freeze(root, &selection).unwrap_err().code,
        "evaluation_window_not_runnable"
    );
    selection.parameter_space =
        Some(json!({"list":[{"params_version":1,"ma_window":2,"rebalance_every":2}]}));
    selection.adjusted.clear();
    assert!(freeze(root, &selection).is_err());
    let constant = fixture::import_closes(root, &selection.dsv, &DATES, &["10"; 10]);
    selection.adjusted.insert("510300.XSHG".into(), constant);
    let frozen = freeze(root, &selection).unwrap();
    let experiment = Experiment::parse(&frozen["data"]["definition"].to_string(), root).unwrap();
    let report = experiment.execute(root, 1).unwrap();
    let run = &report.runs[0];
    assert!(run.error.is_none(), "{:?}", run.error);
    let result = run.fast_event_result.as_ref().unwrap();
    assert!(result.account.fills().is_empty());
    for session in &result.sessions {
        let row = serde_json::to_value(&session.close.account).unwrap();
        assert_eq!(row["cash"], "10000");
        assert_eq!(row["equity"], "10000");
    }
}

#[test]
fn skipped_buy_retains_cash_and_later_signal_replaces_a_deferred_exit() {
    let temp = TempDir::new().unwrap();
    let selection = setup_scenario(
        temp.path(),
        &[2],
        &["10", "12", "12", "12", "8", "8", "14", "14", "14", "10"],
    );
    let frozen = freeze(temp.path(), &selection).unwrap();
    let report = Experiment::parse(&frozen["data"]["definition"].to_string(), temp.path())
        .unwrap()
        .execute(temp.path(), 1)
        .unwrap();
    let run = &report.runs[0];
    assert!(run.error.is_none(), "{:?}", run.error);
    assert_eq!(run.summary.as_ref().unwrap().skipped_buy_count, 1);
    let result = run.fast_event_result.as_ref().unwrap();
    assert!(result.account.fills().is_empty());
    for session in &result.sessions {
        assert_eq!(session.close.account.cash.to_string(), "10000");
    }

    let temp = TempDir::new().unwrap();
    // Cash target at index3 defers at4 and5; the long target at5 replaces
    // it before open6. Holdings remain 900 until equality at7 exits at8.
    let selection = setup_scenario(
        temp.path(),
        &[4, 5],
        &["10", "12", "12", "12", "8", "16", "16", "16", "16", "10"],
    );
    let frozen = freeze(temp.path(), &selection).unwrap();
    let report = Experiment::parse(&frozen["data"]["definition"].to_string(), temp.path())
        .unwrap()
        .execute(temp.path(), 1)
        .unwrap();
    let run = &report.runs[0];
    assert!(run.error.is_none(), "{:?}", run.error);
    let result = run.fast_event_result.as_ref().unwrap();
    assert_eq!(run.summary.as_ref().unwrap().deferred_count, 2);
    assert_eq!(result.account.fills().len(), 2);
    for index in 1..=6 {
        assert_eq!(
            result.sessions[index].close.account.cash.to_string(),
            "976.991"
        );
        assert_eq!(
            result.sessions[index].close.account.positions[&"510300.XSHG".parse().unwrap()]
                .quantity
                .to_string(),
            "900"
        );
    }
    assert_eq!(
        result.sessions[7].close.account.cash.to_string(),
        "9945.009"
    );
}
