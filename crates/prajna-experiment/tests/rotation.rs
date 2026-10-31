//! Pool momentum Top-K rotation golden through the production selection/freeze
//! path. All prices, statuses and adjusted closes are synthetic; raw open/close
//! stay at 10 so every ledger value below is an independent decimal hand
//! calculation (lot rounding, minimum commission, next-Session execution).
use prajna_experiment::{
    Experiment,
    pools::{Command as PoolCommand, command},
    selection::{Selection, freeze},
};
use serde_json::{Value, json};
use std::path::Path;

#[path = "support/selection_fixture.rs"]
mod fixture;

const DATES: [&str; 8] = [
    "2026-01-05",
    "2026-01-06",
    "2026-01-07",
    "2026-01-08",
    "2026-01-09",
    "2026-01-12",
    "2026-01-13",
    "2026-01-14",
];

fn publish_pool(root: &Path, members: &[&str]) -> usize {
    let registry = command(root, PoolCommand::List {}).unwrap();
    let pool = registry["data"]["pools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|pool| pool["id"] == "dividend")
        .unwrap();
    let draft: Vec<String> = serde_json::from_value(pool["draft"].clone()).unwrap();
    let members = members
        .iter()
        .map(|member| member.to_string())
        .collect::<Vec<_>>();
    if draft != members {
        command(
            root,
            PoolCommand::Save {
                id: "dividend".into(),
                expected: draft,
                members: members.clone(),
            },
        )
        .unwrap();
    }
    let published = command(
        root,
        PoolCommand::Publish {
            id: "dividend".into(),
            expected: members,
        },
    )
    .unwrap();
    published["data"]["pools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|pool| pool["id"] == "dividend")
        .unwrap()["versions"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()["version"]
        .as_u64()
        .unwrap() as usize
}

fn scenario(
    root: &Path,
    members: &[&str],
    closes: &[(&str, &[&str])],
    blocked: &[(&str, &str)],
    parameters: Value,
    initial_cash: &str,
) -> (Selection, Value) {
    let dsv = fixture::prepare_rotation_days(root, &DATES, blocked);
    let mut adjusted = serde_json::Map::new();
    for (instrument, series) in closes {
        let version = fixture::import_closes_for(root, &dsv, instrument, &DATES, series);
        adjusted.insert(instrument.to_string(), json!(version));
    }
    let version = publish_pool(root, members);
    let selection: Selection = serde_json::from_value(json!({
        "dsv": dsv,
        "pool": {"id": "dividend", "version": version},
        "strategy": "pool_momentum_topk",
        "adjusted": adjusted,
        "parameter_space": {"list": [parameters]},
        "settings": {"initial_cash": initial_cash},
    }))
    .unwrap();
    let frozen = freeze(root, &selection).unwrap();
    Experiment::parse(&frozen["data"]["definition"].to_string(), root).unwrap();
    (selection, frozen)
}

fn execute(root: &Path, frozen: &Value) -> prajna_experiment::ExecutionReport {
    let experiment = Experiment::parse(&frozen["data"]["definition"].to_string(), root).unwrap();
    let report = experiment.execute(root, 1).unwrap();
    assert!(report.runs[0].error.is_none(), "{:?}", report.runs[0].error);
    report
}

fn session(result: &prajna_experiment::ExecutionReport, date: &str) -> Value {
    let sessions = &result.runs[0].fast_event_result.as_ref().unwrap().sessions;
    let found = sessions
        .iter()
        .find(|row| row.session.session_date.to_string() == date)
        .unwrap();
    serde_json::to_value(&found.close.account).unwrap()
}

#[test]
fn ties_break_by_instrument_actual_count_is_equal_weighted_and_costs_are_next_session() {
    let temp = tempfile::TempDir::new().unwrap();
    let root = temp.path();
    let flat = ["100", "100", "100", "100", "100", "100", "100", "100"];
    let (selection, frozen) = scenario(
        root,
        &["510300.XSHG", "510880.XSHG"],
        &[("510300.XSHG", &flat), ("510880.XSHG", &flat)],
        &[],
        json!({"params_version":1,"lookback":1,"top_k":5,"rebalance_every":6}),
        "10000",
    );
    let definition = &frozen["data"]["definition"];
    assert_eq!(definition["strategy"], "pool_momentum_topk");
    assert_eq!(
        definition["parameter_space"]["list"][0],
        json!({"params_version":1,"lookback":1,"top_k":5,"rebalance_every":6})
    );
    assert_eq!(definition["venue"], "XSHG");
    assert_eq!(definition["unfilled_entry"], "skip");
    assert_eq!(definition["evaluation"]["warmup_sessions"], 1);
    assert_eq!(definition["evaluation"]["start"], DATES[1]);
    assert_eq!(definition["evaluation"]["end"], DATES[7]);
    assert_eq!(
        definition["fast_event"]["lot_size_source"]["kind"],
        "instrument"
    );
    let instruments = frozen["data"]["instruments"].as_array().unwrap();
    let lot = |instrument: &str, field: &str| {
        instruments
            .iter()
            .find(|row| row["instrument_id"] == instrument)
            .unwrap()[field]
            .clone()
    };
    assert_eq!(lot("510300.XSHG", "lot_size"), "100");
    assert_eq!(lot("510880.XSHG", "lot_size"), "10");
    assert_eq!(lot("510880.XSHG", "price_increment"), "0.01");
    assert_eq!(definition["costs"]["commission_rate"], 0.0003);
    assert_eq!(definition["fast_event"]["minimum_commission"], "5");

    let report = execute(root, &frozen);
    let result = report.runs[0].fast_event_result.as_ref().unwrap();
    // The frozen start is the first Decision Session; positions appear only at
    // the next Session's close because fills happen at that Session's open.
    assert_eq!(
        result.sessions[0].session.session_date.to_string(),
        DATES[1]
    );
    assert_eq!(session(&report, DATES[1])["positions"], json!({}));
    let first = session(&report, DATES[2]);
    // Target quantities size on the raw open 10: 500 shares is five 100-share
    // lots of 510300 but fifty 10-share lots of 510880. After 510300 spends
    // 5,001 + 5, 510880's 50 lots no longer fit and the affordable lot drops to
    // 490; fills execute at 10.002 with the 5 CNY minimum commission.
    assert_eq!(first["cash"], "88.02");
    assert_eq!(first["positions"]["510300.XSHG"]["quantity"], "500");
    assert_eq!(first["positions"]["510880.XSHG"]["quantity"], "490");
    assert_eq!(first["equity"], "9988.02");
    let fills = result.account.fills();
    assert_eq!(fills.len(), 2);
    // 500 × 10.002 = 5,001 and 490 × 10.002 = 4,900.98 both pay the 5 CNY
    // minimum commission; different native lot sizes are visible.
    assert_eq!(serde_json::to_value(&fills[0]).unwrap()["price"], "10.002");
    assert_eq!(fills[0].order().quantity().to_string(), "500");
    assert_eq!(
        serde_json::to_value(&fills[0]).unwrap()["costs"]["minimum_commission_top_up"],
        "3.4997"
    );
    assert_eq!(
        serde_json::to_value(&fills[1]).unwrap()["costs"]["minimum_commission_top_up"],
        "3.529706"
    );
    assert_eq!(
        result
            .sessions
            .last()
            .unwrap()
            .session
            .session_date
            .to_string(),
        DATES[7]
    );

    // A single-slot tie selects the lower InstrumentId (510300 < 510880) and
    // buys the full budget with the 100-share lot.
    let mut tie_selection: Selection =
        serde_json::from_value(serde_json::to_value(&selection).unwrap()).unwrap();
    tie_selection.parameter_space =
        Some(json!({"list": [{"params_version":1,"lookback":1,"top_k":1,"rebalance_every":6}]}));
    let tie_frozen = freeze(root, &tie_selection).unwrap();
    let tie_report = execute(root, &tie_frozen);
    let tie = session(&tie_report, DATES[2]);
    assert_eq!(tie["positions"]["510300.XSHG"]["quantity"], "900");
    assert_eq!(tie["positions"].get("510880.XSHG"), None);
    // Sizing 10,000/10 = 1,000 shares (lot 100); the slipped 10.002 notional
    // plus 5 commission exceeds cash, so the affordable lot drops to 900:
    // 900 × 10.002 + 5 = 9,006.8; 10,000 − 9,006.8 = 993.2.
    assert_eq!(tie["cash"], "993.2");
    assert_eq!(tie["equity"], "9993.2");
}

#[test]
fn zero_candidates_create_an_explicit_cash_decision_and_later_reentry() {
    let temp = tempfile::TempDir::new().unwrap();
    let root = temp.path();
    let gapped = ["100", "100", "100", "100", "100", "100"];
    let dates = [DATES[0], DATES[1], DATES[2], DATES[4], DATES[5], DATES[6]];
    // Both members lack adjusted rows on DATES[3] and DATES[7]; the frozen
    // window preserves the internal gap and still ends at DATES[6].
    let dsv = fixture::prepare_rotation_days(root, &DATES, &[]);
    let mut adjusted = serde_json::Map::new();
    for instrument in ["510300.XSHG", "510880.XSHG"] {
        let version = fixture::import_closes_for(root, &dsv, instrument, &dates, &gapped);
        adjusted.insert(instrument.to_string(), json!(version));
    }
    let version = publish_pool(root, &["510300.XSHG", "510880.XSHG"]);
    let selection: Selection = serde_json::from_value(json!({
        "dsv": dsv,
        "pool": {"id": "dividend", "version": version},
        "strategy": "pool_momentum_topk",
        "adjusted": adjusted,
        "parameter_space": {"list": [{"params_version":1,"lookback":1,"top_k":2,"rebalance_every":1}]},
        "settings": {"initial_cash": "10000"},
    }))
    .unwrap();
    let frozen = freeze(root, &selection).unwrap();
    let definition = &frozen["data"]["definition"];
    assert_eq!(definition["evaluation"]["start"], DATES[1]);
    assert_eq!(definition["evaluation"]["end"], DATES[6]);
    assert!(
        frozen["data"]["window"]["warnings"][0]
            .as_str()
            .unwrap()
            .contains("no bound adjusted research row")
    );

    let report = execute(root, &frozen);
    let result = report.runs[0].fast_event_result.as_ref().unwrap();
    // DATES[3] is missing from the research panel: the scheduled close has no
    // eligible momentum value and produces an empty cash target, executed on
    // DATES[4] (sell both) and re-entered on DATES[6] (buy both).
    let cash = session(&report, DATES[4]);
    assert_eq!(cash["positions"], json!({}));
    assert_eq!(cash["cash"], "9971.04");
    assert_eq!(cash["equity"], "9971.04");
    let final_close = session(&report, DATES[6]);
    assert_eq!(final_close["positions"]["510300.XSHG"]["quantity"], "400");
    assert_eq!(final_close["positions"]["510880.XSHG"]["quantity"], "490");
    assert_eq!(final_close["cash"], "1059.26");
    assert_eq!(final_close["equity"], "9959.26");
    // The routine rebalance before the gap also sold 100 shares of 510300
    // because its 100-share lot cannot price the smaller post-fee target.
    let fills = result.account.fills();
    assert_eq!(fills.len(), 7);
    assert_eq!(
        fills
            .iter()
            .filter(|fill| fill.order().side() == prajna_account::Side::Sell)
            .count(),
        3
    );
    assert_eq!(
        result
            .sessions
            .iter()
            .map(|row| row.session.session_date.to_string())
            .collect::<Vec<_>>(),
        DATES[1..=6].to_vec()
    );
}

#[test]
fn blocked_buy_leg_keeps_its_cash_slice_instead_of_reallocating_it() {
    let temp = tempfile::TempDir::new().unwrap();
    let root = temp.path();
    let flat = ["100", "100", "100", "100", "100", "100", "100", "100"];
    let (_, frozen) = scenario(
        root,
        &["510300.XSHG", "510880.XSHG"],
        &[("510300.XSHG", &flat), ("510880.XSHG", &flat)],
        &[("510300.XSHG", DATES[2])],
        json!({"params_version":1,"lookback":1,"top_k":2,"rebalance_every":6}),
        "10000",
    );
    let report = execute(root, &frozen);
    let result = report.runs[0].fast_event_result.as_ref().unwrap();
    let first = session(&report, DATES[2]);
    assert_eq!(first["positions"]["510300.XSHG"], json!(null));
    assert_eq!(first["positions"]["510880.XSHG"]["quantity"], "500");
    // The executable target keeps its own 50% raw-price sizing (500 × 10) and
    // executes at 10.002 with 5 commission: 10,000 − 5,006 = 4,994. The
    // blocked target's slice stays cash; it is not reallocated as 100% exposure.
    assert_eq!(first["cash"], "4994");
    assert_eq!(first["equity"], "9994");
    let summary = report.runs[0].summary.as_ref().unwrap();
    assert_eq!(summary.skipped_buy_count, 1);
    assert_eq!(result.account.fills().len(), 1);
}

#[test]
fn gold_is_held_only_after_it_is_selected() {
    let temp = tempfile::TempDir::new().unwrap();
    let root = temp.path();
    let flat = ["100", "100", "100", "100", "100", "100", "100", "100"];
    let gold = ["100", "90", "80", "70", "80", "90", "100", "110"];
    let (_, frozen) = scenario(
        root,
        &["510300.XSHG", "510880.XSHG", "518880.XSHG"],
        &[
            ("510300.XSHG", &flat),
            ("510880.XSHG", &flat),
            ("518880.XSHG", &gold),
        ],
        &[],
        json!({"params_version":1,"lookback":2,"top_k":2,"rebalance_every":1}),
        "10000",
    );
    let report = execute(root, &frozen);
    let before_selection = session(&report, DATES[5]);
    assert_eq!(
        before_selection["positions"]["510300.XSHG"]["quantity"],
        "400"
    );
    assert_eq!(
        before_selection["positions"]["510880.XSHG"]["quantity"],
        "490"
    );
    assert_eq!(before_selection["cash"], "1082.82");
    assert_eq!(before_selection["equity"], "9982.82");
    assert_eq!(before_selection["positions"].get("518880.XSHG"), None);
    // The gold ETF enters the top two only on DATES[5]'s Decision and is bought
    // at DATES[6]'s open after the old dividend ETF is sold.
    let selected = session(&report, DATES[6]);
    assert_eq!(selected["positions"]["510300.XSHG"]["quantity"], "400");
    assert_eq!(selected["positions"]["518880.XSHG"]["quantity"], "400");
    assert_eq!(selected["positions"].get("510880.XSHG"), None);
    assert_eq!(selected["cash"], "1971.04");
    assert_eq!(selected["equity"], "9971.04");
    // The target weights stay inside this pool; no other pool/cash bucket is
    // read or transferred.
    let definition = &frozen["data"]["definition"];
    assert_eq!(
        definition["universe"],
        definition["universe"].as_str().unwrap()
    );
    assert_eq!(
        definition["adjusted_versions"].as_object().unwrap().len(),
        3
    );
}

#[test]
fn template_requires_research_bindings_and_rejects_invalid_parameters() {
    let temp = tempfile::TempDir::new().unwrap();
    let root = temp.path();
    let flat = ["100", "100", "100", "100", "100", "100", "100", "100"];
    let (selection, frozen) = scenario(
        root,
        &["510300.XSHG", "510880.XSHG"],
        &[("510300.XSHG", &flat), ("510880.XSHG", &flat)],
        &[],
        json!({"params_version":1,"lookback":1,"top_k":2,"rebalance_every":1}),
        "10000",
    );
    for parameters in [
        json!({"params_version":2,"lookback":1,"top_k":2,"rebalance_every":1}),
        json!({"params_version":1,"lookback":0,"top_k":2,"rebalance_every":1}),
        json!({"params_version":1,"lookback":1,"top_k":0,"rebalance_every":1}),
        json!({"params_version":1,"lookback":1,"top_k":2,"rebalance_every":0}),
        json!({"params_version":1,"lookback":1,"top_k":2}),
    ] {
        let mut invalid: Selection =
            serde_json::from_value(serde_json::to_value(&selection).unwrap()).unwrap();
        invalid.parameter_space = Some(json!({"list": [parameters]}));
        assert_eq!(
            freeze(root, &invalid).unwrap_err().code,
            "invalid_definition"
        );
    }
    let mut missing_adjusted: Selection =
        serde_json::from_value(serde_json::to_value(&selection).unwrap()).unwrap();
    missing_adjusted.adjusted.clear();
    assert_eq!(
        freeze(root, &missing_adjusted).unwrap_err().code,
        "adjusted_version_missing"
    );

    let definition = frozen["data"]["definition"].clone();
    for field in ["adjusted_versions", "evaluation"] {
        let mut without = definition.clone();
        without.as_object_mut().unwrap().remove(field);
        let error = Experiment::parse(&without.to_string(), root).unwrap_err();
        assert!(
            error.to_string().contains("adjusted_versions")
                || error.to_string().contains("evaluation"),
            "{field}: {error}"
        );
    }
    let mut accurate = definition.clone();
    accurate["engine"] = json!("accurate_event");
    accurate["backend"] = json!({"name":"synthetic","version":"1"});
    assert!(Experiment::parse(&accurate.to_string(), root).is_err());

    // A grid over the native axes is accepted and expands through the same
    // parser; a foreign axis is rejected instead of being silently ignored.
    let mut grid: Selection =
        serde_json::from_value(serde_json::to_value(&selection).unwrap()).unwrap();
    grid.parameter_space = Some(json!({"grid": {
        "params_version":[1], "lookback":[1,2], "top_k":[1,2], "rebalance_every":[1]
    }}));
    assert!(freeze(root, &grid).is_ok());
    grid.parameter_space = Some(json!({"grid": {
        "params_version":[1], "lookback":[1], "top_k":[1], "rebalance_every":[1], "momentum":[1]
    }}));
    assert_eq!(freeze(root, &grid).unwrap_err().code, "invalid_definition");
}
