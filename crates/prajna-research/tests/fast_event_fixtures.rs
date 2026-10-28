mod common;

use std::collections::{BTreeMap, BTreeSet};

use chrono::{Datelike, Duration, NaiveDate};
use prajna_account::{Side, VirtualPortfolioId};
use prajna_domain::{Currency, InstrumentId, VenueId};
use prajna_research::{
    Panel, executable,
    factor::{AvailabilityAssumption, Factor, FactorKind, RotationScoreParams},
    fast_event::{
        FastEventConfig, FastEventCosts, PortfolioRun, PriceField, Rebalance, Sizing,
        run_fast_event,
    },
    load_execution_status, load_panel,
    strategy::{
        BuyAndHoldParams, Decision, ExecutionEvent, MaCrossoverParams, RankDirection,
        StrategyCapability, TopKRank, UnfilledEntry, VectorStrategy, Weighting,
    },
    vector::{VectorCosts, run_vector_with_policy},
};
use serde_json::{Value, json};

const FIXTURE: &str = include_str!("../../../poc/poc0-benchmark/fixtures/dataset-v1.json");
const MA: &str = include_str!("../../../poc/poc0-benchmark/fixtures/b2-ma20-60-v1.json");

#[test]
fn full_target_matches_vector_and_independent_python_on_every_session_and_fill() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cases = [
        (
            "dataset-v1",
            FIXTURE.as_bytes().to_vec(),
            vec!["s1", "s2", "s3"],
        ),
        ("b2-ma20-60-v1", ma_fixture(), vec!["s3"]),
        (
            "hand-v1",
            include_bytes!("../../../poc/mvp3-golden/fixtures/hand-v1.json").to_vec(),
            vec!["s1"],
        ),
        (
            "hand-retry-v1",
            include_bytes!("../../../poc/mvp3-golden/fixtures/hand-retry-v1.json").to_vec(),
            vec!["s1"],
        ),
        (
            "hand-rebalance-v1",
            include_bytes!("../../../poc/mvp3-golden/fixtures/hand-rebalance-v1.json").to_vec(),
            vec!["s2"],
        ),
        (
            "hand-fee-only-v1",
            include_bytes!("../../../poc/mvp3-golden/fixtures/hand-fee-only-v1.json").to_vec(),
            vec!["s2"],
        ),
    ];
    for (name, bytes, strategies) in cases {
        let fixture: Value = serde_json::from_slice(&bytes).unwrap();
        let lake = tempfile::tempdir().unwrap();
        let dsv = common::publish_fixture(lake.path(), &normalizer_fixture(&fixture), "3");
        let panel = load_panel(lake.path(), &dsv, &VenueId::new("SYNTH").unwrap()).unwrap();
        let statuses = load_execution_status(lake.path(), &dsv).unwrap();
        for label in strategies {
            let strategy = fixture_strategy(&fixture, label);
            let decisions = strategy
                .decide_panel(&panel, &panel.instruments, AvailabilityAssumption::None)
                .unwrap();
            for (sizing, suffix) in [
                (Sizing::Lot, "lot"),
                (Sizing::VectorParity, "vector_parity"),
            ] {
                let context = format!("{name}/{label}/{suffix}");
                let expected: Value = serde_json::from_slice(
                    &std::fs::read(root.join(format!(
                        "poc/mvp3-golden/expected/{name}.{label}.{suffix}.json"
                    )))
                    .unwrap(),
                )
                .unwrap();
                let costs = &fixture["costs"];
                let decimal = |v: &Value| v.to_string().parse().unwrap();
                let initial = decimal(&fixture["account"]["initial_cash"]);
                let parameters = FastEventConfig {
                    initial_cash: initial,
                    costs: FastEventCosts {
                        commission_rate: decimal(&costs["commission_rate"]),
                        minimum_commission: decimal(&costs["minimum_commission"]),
                        buy_tax_rate: decimal(&costs["buy_tax_rate"]),
                        sell_tax_rate: decimal(&costs["sell_tax_rate"]),
                        buy_slippage_rate: decimal(&json!(
                            costs["buy_slippage_bps"].as_f64().unwrap() / 10000.0
                        )),
                        sell_slippage_rate: decimal(&json!(
                            costs["sell_slippage_bps"].as_f64().unwrap() / 10000.0
                        )),
                    },
                    lot_sizes: fixture["instruments"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|row| {
                            (
                                format!("{}.SYNTH", row["symbol"].as_str().unwrap())
                                    .parse()
                                    .unwrap(),
                                row.get("lot_size")
                                    .unwrap_or(&fixture["account"]["lot_size"])
                                    .to_string()
                                    .parse()
                                    .unwrap(),
                            )
                        })
                        .collect(),
                    ..config(&panel, sizing)
                };
                let fast = run_fast_event(
                    &panel,
                    &statuses,
                    &parameters,
                    &[PortfolioRun {
                        id: VirtualPortfolioId(1),
                        capital: initial,
                        decisions: decisions.clone(),
                        unfilled_entry: strategy.unfilled_entry(),
                        rebalance: Rebalance::FullTarget,
                    }],
                )
                .unwrap_or_else(|error| panic!("{context}: {error}"));
                compare_decisions(&decisions, &expected, &context);
                let mut executed_decisions = std::collections::BTreeSet::new();
                let attempts = fast
                    .attempts
                    .iter()
                    .filter_map(|attempt| {
                        if let ExecutionEvent::RetryableExecution {
                            decision_session, ..
                        } = &attempt.event
                        {
                            executed_decisions.insert(decision_session.session_date);
                        }
                        attempt_row(&attempt.event, &executed_decisions)
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    attempts,
                    expected["executions"].as_array().unwrap().clone(),
                    "{context}: execution trace"
                );
                let pending = fast
                    .attempts
                    .iter()
                    .find_map(|attempt| match &attempt.event {
                        ExecutionEvent::PendingAtEnd {
                            decision_session,
                            targets,
                        } => Some(json!({
                            "decision_session": decision_session.session_date.to_string(),
                            "targets": targets,
                        })),
                        _ => None,
                    });
                let expected_pending = (!expected["pending_at_end"].is_null()).then(|| {
                    json!({
                        "decision_session": expected["pending_at_end"]["decision_session"],
                        "targets": expected["pending_at_end"]["targets"],
                    })
                });
                assert_eq!(pending, expected_pending, "{context}: pending");
                assert_eq!(
                    fast.sessions.len(),
                    expected["sessions"].as_array().unwrap().len(),
                    "{context}: Sessions"
                );
                for (session, row) in fast
                    .sessions
                    .iter()
                    .zip(expected["sessions"].as_array().unwrap())
                {
                    let day = session.session.session_date.to_string();
                    assert_eq!(day, row["session_date"], "{context}: valuation Session");
                    for (point, value, sources) in [
                        ("open", &session.open_after, &session.open_sources),
                        ("close", &session.close, &session.close_sources),
                    ] {
                        let path = format!("{context}/{day}/{point}");
                        value.validate().unwrap();
                        assert_eq!(
                            value.account,
                            value.portfolios[&VirtualPortfolioId(1)],
                            "{path}: account/VP"
                        );
                        compare_money(
                            value.account.cash.to_f64(),
                            &row[point]["cash"],
                            &format!("{path}/cash"),
                        );
                        compare_money(
                            value.account.equity.to_f64(),
                            &row[point]["equity"],
                            &format!("{path}/equity"),
                        );
                        let nav = value.account.equity.to_f64() / initial.to_f64();
                        let expected_nav = row[point]["nav"].as_f64().unwrap();
                        assert!(
                            (nav - expected_nav).abs() / expected_nav <= 1e-9,
                            "{path}/nav: actual={nav}, Python={expected_nav}"
                        );
                        assert_eq!(
                            value
                                .account
                                .positions
                                .keys()
                                .map(ToString::to_string)
                                .collect::<Vec<_>>(),
                            row[point]["holdings"]
                                .as_object()
                                .unwrap()
                                .keys()
                                .cloned()
                                .collect::<Vec<_>>(),
                            "{path}: holdings",
                        );
                        for (id, position) in &value.account.positions {
                            let expected_position = &row[point]["holdings"][id.to_string()];
                            compare_quantity(
                                position.quantity,
                                expected_position,
                                &format!("{path}/{id}/quantity"),
                            );
                            assert_eq!(
                                position.price.to_f64(),
                                expected_position["price"].as_f64().unwrap(),
                                "{path}/{id}/price"
                            );
                            compare_money(
                                position.market_value.to_f64(),
                                &expected_position["value"],
                                &format!("{path}/{id}/value"),
                            );
                            let source = &sources[id];
                            assert_eq!(
                                source.session_date.to_string(),
                                expected_position["price_source"]["session_date"],
                                "{path}/{id}/price_source"
                            );
                            assert_eq!(
                                if source.session_date == session.session.session_date {
                                    "raw"
                                } else {
                                    "carried"
                                },
                                expected_position["price_source"]["kind"],
                                "{path}/{id}/price_source_kind"
                            );
                            assert_eq!(
                                match source.field {
                                    PriceField::Open => "open",
                                    PriceField::Close => "close",
                                },
                                expected_position["price_source"]["field"],
                                "{path}/{id}/price_field"
                            );
                        }
                    }
                }
                compare_fills(&fast, &expected, &context);
                if sizing == Sizing::VectorParity {
                    let vector = run_vector_with_policy(
                        &panel,
                        &decisions,
                        strategy.unfilled_entry(),
                        &VectorCosts {
                            commission_rate: costs["commission_rate"].as_f64().unwrap(),
                            buy_slippage_bps: costs["buy_slippage_bps"].as_f64().unwrap(),
                            sell_slippage_bps: costs["sell_slippage_bps"].as_f64().unwrap(),
                            buy_tax_rate: costs["buy_tax_rate"].as_f64().unwrap(),
                            sell_tax_rate: costs["sell_tax_rate"].as_f64().unwrap(),
                        },
                        AvailabilityAssumption::None,
                        |id, session| executable(&panel, &statuses, id, session).unwrap(),
                    )
                    .unwrap();
                    let vector_pending = vector.pending_at_end.as_ref().map(|pending| {
                        json!({
                            "decision_session": pending.decision_session,
                            "targets": pending.targets,
                        })
                    });
                    assert_eq!(
                        vector_pending, expected_pending,
                        "{context}: Vector pending"
                    );
                    for (v, f) in vector.sessions.iter().zip(&fast.sessions) {
                        // Vector rows include the next open mark, but not its execution cost.
                        let aligned = v.nav / (1.0 + v.gross_return);
                        let nav = f.open_after.account.equity.to_f64() / initial.to_f64();
                        assert!(
                            (aligned - nav).abs() / nav <= 1e-9,
                            "{context}/{}/nav: Vector={aligned} Fast={nav}",
                            v.session_date
                        );
                    }
                    let vector_trace = vector.executions.iter().map(|e| json!({
                        "session_date": e.session_date,
                        "kind": match e.kind {
                            prajna_research::vector::VectorExecutionKind::Executed => "executed",
                            prajna_research::vector::VectorExecutionKind::Deferred => "deferred",
                            prajna_research::vector::VectorExecutionKind::EntriesRetried => "executed",
                            prajna_research::vector::VectorExecutionKind::RetryDeferred => "deferred",
                        },
                        "blocked": e.blocked, "skipped_buys": e.skipped_buys,
                        "retry_only": matches!(e.kind,
                            prajna_research::vector::VectorExecutionKind::EntriesRetried
                            | prajna_research::vector::VectorExecutionKind::RetryDeferred),
                    })).collect::<Vec<_>>();
                    let fast_trace = attempts
                        .iter()
                        .map(|row| {
                            json!({
                                "session_date": row["session_date"], "kind": row["kind"],
                                "blocked": row["blocked"], "skipped_buys": row["skipped_buys"],
                                "retry_only": row["retry_only"],
                            })
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(vector_trace, fast_trace, "{context}: Vector/Fast trace");
                }
            }
        }
    }
}

fn fixture_strategy(fixture: &Value, label: &str) -> VectorStrategy {
    match label {
        "s1" => VectorStrategy::BuyAndHold(BuyAndHoldParams::default()),
        "s3" => VectorStrategy::MaCrossover(MaCrossoverParams::default()),
        "s2" => {
            let p = &fixture["strategy"];
            VectorStrategy::TopKRank(TopKRank {
                score: Factor::new(
                    FactorKind::RotationScore,
                    RotationScoreParams {
                        short: p["momentum_short_days"]
                            .as_u64()
                            .unwrap()
                            .try_into()
                            .unwrap(),
                        long: p["momentum_long_days"]
                            .as_u64()
                            .unwrap()
                            .try_into()
                            .unwrap(),
                        vol: p["volatility_window"].as_u64().unwrap().try_into().unwrap(),
                        w_s: p["short_momentum_weight"].as_f64().unwrap(),
                        w_l: p["long_momentum_weight"].as_f64().unwrap(),
                        w_v: p["volatility_weight"].as_f64().unwrap(),
                        trend: p["trend_filter"]
                            .as_bool()
                            .unwrap()
                            .then(|| p["trend_window"].as_u64().unwrap().try_into().unwrap()),
                    },
                )
                .unwrap(),
                direction: RankDirection::Descending,
                top_k: p["top_n"].as_u64().unwrap().try_into().unwrap(),
                rebalance_every: p["rebalance_every"].as_u64().unwrap().try_into().unwrap(),
                weighting: Weighting::EqualWeight,
                unfilled_entry: UnfilledEntry::Skip,
                capabilities: std::collections::BTreeSet::from([StrategyCapability::Vectorizable]),
            })
        }
        _ => panic!("unknown test strategy {label}"),
    }
}

fn normalizer_fixture(fixture: &Value) -> Vec<u8> {
    let defaults: Value = serde_json::from_str(FIXTURE).unwrap();
    let mut fixture = fixture.clone();
    for key in ["timezone", "missing_bars", "execution_status_overrides"] {
        if fixture.get(key).is_none() {
            fixture[key] = if key == "timezone" {
                defaults[key].clone()
            } else {
                json!([])
            };
        }
    }
    for key in ["volume", "amount", "high_rule", "low_rule"] {
        fixture["bar_defaults"][key] = defaults["bar_defaults"][key].clone();
    }
    fixture["account"]["lot_size"] = defaults["account"]["lot_size"].clone();
    fixture["execution_status_default"]["sources"] =
        defaults["execution_status_default"]["sources"].clone();
    for row in fixture["execution_status_overrides"]
        .as_array_mut()
        .unwrap()
    {
        row["sources"] = defaults["execution_status_default"]["sources"].clone();
    }
    for row in fixture["instruments"].as_array_mut().unwrap() {
        for key in ["currency", "price_precision", "quantity_precision"] {
            row[key] = defaults["instruments"][0][key].clone();
        }
    }
    serde_json::to_vec(&fixture).unwrap()
}

fn compare_money(actual: f64, expected: &Value, path: &str) {
    let expected = expected.as_f64().unwrap();
    assert!(
        actual.is_finite() && (actual - expected).abs() <= 1e-6,
        "{path}: actual={actual}, Python={expected}"
    );
}

fn compare_quantity(actual: prajna_domain::Quantity, expected: &Value, path: &str) {
    if let Some(exact) = expected.get("quantity_decimal") {
        assert_eq!(actual, exact.as_str().unwrap().parse().unwrap(), "{path}");
    } else {
        assert_eq!(
            actual.to_f64(),
            expected["quantity"].as_f64().unwrap(),
            "{path}"
        );
    }
}

fn compare_decisions(decisions: &[Decision], expected: &Value, context: &str) {
    let rows = expected["decisions"].as_array().unwrap();
    assert_eq!(decisions.len(), rows.len(), "{context}: decision count");
    for (decision, row) in decisions.iter().zip(rows) {
        let day = decision.decision_session.session_date.to_string();
        assert_eq!(day, row["session_date"], "{context}: decision Session");
        assert_eq!(
            json!(decision.targets),
            row["targets"],
            "{context}/{day}: targets"
        );
        if let Some(ranked) = row["ranked"].as_array() {
            assert_eq!(
                decision.ranked.len(),
                ranked.len(),
                "{context}/{day}: rank count"
            );
            for ((id, score, rank), row) in decision.ranked.iter().zip(ranked) {
                assert_eq!(
                    id.to_string(),
                    row["instrument_id"],
                    "{context}/{day}: ranked ID"
                );
                assert_eq!(
                    *rank,
                    row["rank"].as_u64().unwrap() as u32,
                    "{context}/{day}/{id}: rank"
                );
                assert!(
                    (score - row["score"].as_f64().unwrap()).abs() <= 1e-12,
                    "{context}/{day}/{id}: score"
                );
            }
        }
    }
}

fn attempt_row(
    event: &ExecutionEvent,
    executed: &std::collections::BTreeSet<NaiveDate>,
) -> Option<Value> {
    let (decision, session, kind, blocked, skipped, retry) = match event {
        ExecutionEvent::Deferred {
            decision_session,
            attempt_session,
            blocked,
        } => (
            decision_session,
            attempt_session,
            "deferred",
            json!(blocked),
            json!([]),
            executed.contains(&decision_session.session_date),
        ),
        ExecutionEvent::Executed {
            decision_session,
            attempt_session,
            skipped_buys,
            ..
        } => (
            decision_session,
            attempt_session,
            "executed",
            json!([]),
            json!(skipped_buys),
            false,
        ),
        ExecutionEvent::RetryableExecution {
            decision_session,
            attempt_session,
            skipped_buys,
            is_retry,
            ..
        } => (
            decision_session,
            attempt_session,
            "executed",
            json!([]),
            json!(skipped_buys),
            *is_retry,
        ),
        ExecutionEvent::PendingAtEnd { .. } => return None,
    };
    Some(json!({
        "session_date": session.session_date.to_string(),
        "decision_session": decision.session_date.to_string(),
        "kind": kind, "blocked": blocked, "skipped_buys": skipped, "retry_only": retry,
    }))
}

fn compare_fills(
    fast: &prajna_research::fast_event::FastEventResult,
    expected: &Value,
    context: &str,
) {
    let fills = expected["fills"].as_array().unwrap();
    assert_eq!(fast.fills.len(), fills.len(), "{context}: Fill count");
    assert_eq!(
        fast.account.fills().len(),
        fast.fills.len(),
        "{context}: account Fill count"
    );
    for (registered, returned) in fast.account.fills().iter().zip(&fast.fills) {
        assert_eq!(
            registered,
            &returned.fill,
            "{context}/{}/{}: account Fill history",
            registered.executed_at(),
            registered.order().instrument_id()
        );
    }
    let orders = expected["orders"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["status"] == "filled")
        .collect::<Vec<_>>();
    assert_eq!(
        fast.account.orders().len(),
        orders.len(),
        "{context}: Order count"
    );
    for ((f, row), order) in fast.fills.iter().zip(fills).zip(orders) {
        let fill = &f.fill;
        let id = fill.order().instrument_id();
        let session = fast
            .sessions
            .iter()
            .find(|s| s.session.ts_open == fill.executed_at())
            .unwrap();
        let day = session.session.session_date.to_string();
        let path = format!("{context}/{day}/{id}");
        let side = match fill.order().side() {
            Side::Buy => "buy",
            Side::Sell => "sell",
        };
        assert_eq!(day, row["session_date"], "{path}: Fill Session");
        assert_eq!(day, order["session_date"], "{path}: Order Session");
        assert_eq!(id.to_string(), row["instrument_id"], "{path}: Fill ID");
        assert_eq!(id.to_string(), order["instrument_id"], "{path}: Order ID");
        assert_eq!(side, row["side"], "{path}: Fill side");
        assert_eq!(side, order["side"], "{path}: Order side");
        compare_quantity(
            fill.order().quantity(),
            row,
            &format!("{path}/Fill quantity"),
        );
        compare_quantity(
            fill.order().quantity(),
            order,
            &format!("{path}/Order quantity"),
        );
        assert_eq!(
            fill.order().portfolio_id(),
            VirtualPortfolioId(1),
            "{path}: VP"
        );
        assert_eq!(
            fill.executed_at(),
            fill.order().submitted_at(),
            "{path}: Fill time"
        );
        assert_eq!(
            &fast.account.orders()[&fill.order().id()],
            fill.order(),
            "{path}: registered Order"
        );
        if fast.config.sizing == Sizing::VectorParity {
            assert_eq!(
                match f.fee_side {
                    Side::Buy => "buy",
                    Side::Sell => "sell",
                },
                row["fee_side"],
                "{path}: fee side"
            );
        }
        for (field, value) in [
            ("raw_open", fill.raw_price().to_f64()),
            ("price", fill.price().to_f64()),
            ("notional", fill.trade_notional().to_f64()),
            ("fee_basis", f.fee_basis.to_f64()),
            (
                "commission",
                fill.costs().total_commission().unwrap().to_f64(),
            ),
            (
                "minimum_commission_top_up",
                fill.costs().minimum_commission_top_up.to_f64(),
            ),
            ("tax", fill.costs().tax.to_f64()),
            ("slippage", fill.slippage().to_f64()),
            ("cash_change", fill.cash_delta().to_f64()),
            ("total_cost", fill.total_fees().to_f64()),
            (
                "cash_fees",
                fill.total_fees().to_f64()
                    - if fast.config.sizing == Sizing::Lot {
                        fill.slippage().to_f64()
                    } else {
                        0.0
                    },
            ),
        ] {
            compare_money(value, &row[field], &format!("{path}/{field}"));
        }
    }
    let settlements = expected
        .get("fee_settlements")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        fast.account.fee_settlements().len(),
        settlements.len(),
        "{context}: fee-only settlements"
    );
    for (actual, expected) in fast.account.fee_settlements().iter().zip(settlements) {
        let session = fast
            .sessions
            .iter()
            .find(|s| s.session.ts_open == actual.executed_at())
            .unwrap();
        let id = actual.instrument_id();
        let path = format!(
            "{context}/{}/{id}/fee_settlement",
            session.session.session_date
        );
        assert_eq!(
            session.session.session_date.to_string(),
            expected["session_date"],
            "{path}: Session"
        );
        assert_eq!(id.to_string(), expected["instrument_id"], "{path}: ID");
        assert_eq!(actual.portfolio_id(), VirtualPortfolioId(1), "{path}: VP");
        assert_eq!(
            match actual.fee_side() {
                Side::Buy => "buy",
                Side::Sell => "sell",
            },
            expected["fee_side"],
            "{path}: side"
        );
        for (field, value) in [
            ("fee_basis", actual.fee_basis().to_f64()),
            (
                "commission",
                actual.costs().total_commission().unwrap().to_f64(),
            ),
            ("tax", actual.costs().tax.to_f64()),
            ("slippage", actual.slippage().to_f64()),
            ("cash_change", actual.cash_delta().to_f64()),
        ] {
            compare_money(value, &expected[field], &format!("{path}/{field}"));
        }
    }
}

fn config(panel: &Panel, sizing: Sizing) -> FastEventConfig {
    FastEventConfig {
        currency: Currency::new("CNY").unwrap(),
        venues: BTreeSet::from([VenueId::new("SYNTH").unwrap()]),
        initial_cash: "100000".parse().unwrap(),
        sizing,
        costs: FastEventCosts {
            commission_rate: "0.001".parse().unwrap(),
            buy_slippage_rate: "0.001".parse().unwrap(),
            sell_slippage_rate: "0.001".parse().unwrap(),
            minimum_commission: "100".parse().unwrap(),
            ..FastEventCosts::ZERO
        },
        lot_sizes: panel
            .instruments
            .iter()
            .cloned()
            .map(|id| (id, "100".parse().unwrap()))
            .collect(),
    }
}

fn decision(panel: &Panel, index: usize, targets: BTreeMap<InstrumentId, f64>) -> Decision {
    Decision {
        decision_session: panel.sessions[index].clone(),
        ranked: vec![],
        targets,
    }
}

fn golden_decisions(panel: &Panel, expected: &str) -> Vec<Decision> {
    let golden: Value = serde_json::from_str(expected).unwrap();
    golden["decisions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            let date = NaiveDate::parse_from_str(row["session_date"].as_str().unwrap(), "%Y-%m-%d")
                .unwrap();
            let index = panel
                .sessions
                .iter()
                .position(|s| s.session_date == date)
                .unwrap();
            decision(
                panel,
                index,
                row["targets"]
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(id, w)| (id.parse().unwrap(), w.as_f64().unwrap()))
                    .collect(),
            )
        })
        .collect()
}

fn ma_fixture() -> Vec<u8> {
    let source: Value = serde_json::from_str(MA).unwrap();
    let mut fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let flat = source["flat_sessions"].as_u64().unwrap() as usize;
    let high = source["high_sessions"].as_u64().unwrap() as usize;
    let count = flat + high + source["low_sessions"].as_u64().unwrap() as usize;
    let mut date =
        NaiveDate::parse_from_str(source["start_date"].as_str().unwrap(), "%Y-%m-%d").unwrap();
    let mut calendar = Vec::new();
    while calendar.len() < count {
        if date.weekday().num_days_from_monday() < 5 {
            calendar.push(date.to_string());
        }
        date += Duration::days(1);
    }
    fixture["calendar"] = json!(calendar);
    for instrument in fixture["instruments"].as_array_mut().unwrap() {
        let rising = instrument["symbol"] == source["rising_symbol"];
        instrument["closes"] = Value::Array(
            (0..count)
                .map(|i| {
                    source[if !rising || i < flat {
                        "flat_close"
                    } else if i < flat + high {
                        "high_close"
                    } else {
                        "low_close"
                    }]
                    .clone()
                })
                .collect(),
        );
    }
    fixture["missing_bars"] = json!([]);
    fixture["execution_status_overrides"] = json!([]);
    serde_json::to_vec(&fixture).unwrap()
}

#[test]
fn s1_s2_s3_decision_tapes_have_exact_two_level_identities_at_both_daily_marks() {
    let goldens = [
        include_str!("../../../poc/mvp3-golden/expected/dataset-v1.s1.vector_parity.json"),
        include_str!("../../../poc/mvp3-golden/expected/dataset-v1.s2.vector_parity.json"),
        include_str!("../../../poc/mvp3-golden/expected/dataset-v1.s3.vector_parity.json"),
    ];
    for count in [10, 130] {
        let lake = tempfile::tempdir().unwrap();
        let dsv = if count == 10 {
            common::publish_version(lake.path(), "3")
        } else {
            common::publish_fixture(lake.path(), &ma_fixture(), "3")
        };
        let panel = load_panel(lake.path(), &dsv, &VenueId::new("SYNTH").unwrap()).unwrap();
        let statuses = load_execution_status(lake.path(), &dsv).unwrap();
        let equal = || {
            panel
                .instruments
                .iter()
                .cloned()
                .map(|id| (id, 1.0 / 3.0))
                .collect()
        };
        for (strategy, golden) in goldens.iter().enumerate() {
            let decisions = if count == 10 {
                golden_decisions(&panel, golden)
            } else {
                match strategy {
                    0 => vec![decision(&panel, 0, equal())],
                    // This fixture's S2 scores tie at zero except the high/low transition.
                    // Descending rank / ascending ID selects A, except the drop on index 80.
                    1 => (2..count)
                        .map(|i| {
                            decision(
                                &panel,
                                i,
                                BTreeMap::from([(
                                    if i == 80 { "B.SYNTH" } else { "A.SYNTH" }.parse().unwrap(),
                                    1.0,
                                )]),
                            )
                        })
                        .collect(),
                    _ => golden_decisions(
                        &panel,
                        include_str!(
                            "../../../poc/mvp3-golden/expected/b2-ma20-60-v1.s3.vector_parity.json"
                        ),
                    ),
                }
            };
            for sizing in [Sizing::VectorParity, Sizing::Lot] {
                let result = run_fast_event(
                    &panel,
                    &statuses,
                    &config(&panel, sizing),
                    &[PortfolioRun {
                        id: VirtualPortfolioId(1),
                        capital: "100000".parse().unwrap(),
                        decisions: decisions.clone(),
                        unfilled_entry: if strategy == 0 {
                            UnfilledEntry::Retry
                        } else {
                            UnfilledEntry::Skip
                        },
                        rebalance: Rebalance::FullTarget,
                    }],
                )
                .unwrap_or_else(|error| {
                    panic!("{count} Sessions / S{} / {sizing:?}: {error}", strategy + 1)
                });
                assert_eq!(result.sessions.len(), count);
                for session in &result.sessions {
                    session.open_after.validate().unwrap();
                    session.close.validate().unwrap();
                }
                assert!(
                    result
                        .fills
                        .iter()
                        .all(|f| f.fill.executed_at() == f.fill.order().submitted_at())
                );
                if count == 130 && strategy == 2 {
                    assert_eq!(result.fills.len(), 2);
                    assert_eq!(
                        result.fills[0].fill.executed_at(),
                        panel.sessions[61].ts_open
                    );
                    assert_eq!(
                        result.fills[1].fill.executed_at(),
                        panel.sessions[86].ts_open
                    );
                    assert_eq!(result.fills[1].fill.order().side(), Side::Sell);
                }
            }
        }
    }
}

#[test]
fn missing_marks_carry_same_field_and_halted_holdings_defer_the_whole_attempt() {
    let lake = tempfile::tempdir().unwrap();
    let dsv = common::publish_version(lake.path(), "3");
    let panel = load_panel(lake.path(), &dsv, &VenueId::new("SYNTH").unwrap()).unwrap();
    let statuses = load_execution_status(lake.path(), &dsv).unwrap();
    let b: InstrumentId = "B.SYNTH".parse().unwrap();
    let result = run_fast_event(
        &panel,
        &statuses,
        &config(&panel, Sizing::Lot),
        &[PortfolioRun {
            id: VirtualPortfolioId(1),
            capital: "100000".parse().unwrap(),
            decisions: vec![
                decision(&panel, 0, BTreeMap::from([(b.clone(), 1.0)])),
                decision(
                    &panel,
                    5,
                    BTreeMap::from([("A.SYNTH".parse().unwrap(), 1.0)]),
                ),
                decision(&panel, 7, BTreeMap::from([(b.clone(), 1.0)])),
                decision(&panel, 8, BTreeMap::new()),
            ],
            unfilled_entry: UnfilledEntry::Skip,
            rebalance: Rebalance::FullTarget,
        }],
    )
    .unwrap();
    assert!(result.attempts.iter().any(|a| matches!(&a.event,
        ExecutionEvent::Deferred { attempt_session, blocked, .. }
        if attempt_session == &panel.sessions[6] && blocked.contains(&b))));
    assert!(
        !result
            .fills
            .iter()
            .any(|f| f.fill.executed_at() == panel.sessions[6].ts_open
                || f.fill.executed_at() == panel.sessions[9].ts_open)
    );
    let last = &result.sessions[9];
    assert_eq!(
        last.open_sources[&b].session_date,
        panel.sessions[8].session_date
    );
    assert_eq!(last.open_sources[&b].field, PriceField::Open);
    assert_eq!(
        last.close_sources[&b].session_date,
        panel.sessions[8].session_date
    );
    assert_eq!(last.close_sources[&b].field, PriceField::Close);
    assert_eq!(
        last.open_after.account.positions[&b].price,
        "100".parse().unwrap()
    );
    assert_eq!(
        last.close.account.positions[&b].price,
        "99".parse().unwrap()
    );
    assert!(last.close.account.equity > "0".parse().unwrap());
}

#[test]
fn f8_fee_only_leg_preserves_quantity_and_charges_exact_turnover_cost() {
    let lake = tempfile::tempdir().unwrap();
    let mut fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    fixture["bar_defaults"]["open"] = json!(1);
    let dsv = common::publish_fixture(lake.path(), &serde_json::to_vec(&fixture).unwrap(), "3");
    let panel = load_panel(lake.path(), &dsv, &VenueId::new("SYNTH").unwrap()).unwrap();
    let statuses = load_execution_status(lake.path(), &dsv).unwrap();
    let mut parameters = config(&panel, Sizing::VectorParity);
    parameters.initial_cash = "1000".parse().unwrap();
    parameters.costs = FastEventCosts {
        commission_rate: "0.01".parse().unwrap(),
        ..FastEventCosts::ZERO
    };
    let a: InstrumentId = "A.SYNTH".parse().unwrap();
    let result = run_fast_event(
        &panel,
        &statuses,
        &parameters,
        &[PortfolioRun {
            id: VirtualPortfolioId(1),
            capital: "1000".parse().unwrap(),
            decisions: vec![
                decision(&panel, 0, BTreeMap::from([(a.clone(), 0.4975)])),
                decision(
                    &panel,
                    1,
                    BTreeMap::from([(a.clone(), 0.5), ("B.SYNTH".parse().unwrap(), 0.4975)]),
                ),
            ],
            unfilled_entry: UnfilledEntry::Skip,
            rebalance: Rebalance::FullTarget,
        }],
    )
    .unwrap();
    assert_eq!(
        result.sessions[1].open_after.account.equity,
        "995.025".parse().unwrap()
    );
    assert_eq!(
        result.sessions[2].open_after.account.equity,
        "990.049875".parse().unwrap()
    );
    assert_eq!(
        result.sessions[2].open_after.account.positions[&a].quantity,
        "495.0249375".parse().unwrap()
    );
    assert_eq!(result.fills.len(), 2);
    assert_eq!(result.account.fee_settlements().len(), 1);
    let settlement = &result.account.fee_settlements()[0];
    assert_eq!(settlement.instrument_id(), &a);
    assert_eq!(settlement.total_fees(), "0.024875625".parse().unwrap());
    assert!(
        result
            .account
            .orders()
            .values()
            .all(|o| o.quantity() > prajna_domain::Quantity::ZERO)
    );
}

#[test]
fn representable_tiny_target_is_not_suppressed_by_an_absolute_weight_threshold() {
    let lake = tempfile::tempdir().unwrap();
    let mut fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    fixture["bar_defaults"]["open"] = json!(1);
    let dsv = common::publish_fixture(lake.path(), &serde_json::to_vec(&fixture).unwrap(), "3");
    let panel = load_panel(lake.path(), &dsv, &VenueId::new("SYNTH").unwrap()).unwrap();
    let statuses = load_execution_status(lake.path(), &dsv).unwrap();
    let mut parameters = config(&panel, Sizing::VectorParity);
    parameters.initial_cash = "1000000000".parse().unwrap();
    parameters.costs = FastEventCosts {
        commission_rate: "0.01".parse().unwrap(),
        ..FastEventCosts::ZERO
    };
    let result = run_fast_event(
        &panel,
        &statuses,
        &parameters,
        &[PortfolioRun {
            id: VirtualPortfolioId(1),
            capital: "1000000000".parse().unwrap(),
            decisions: vec![decision(
                &panel,
                0,
                BTreeMap::from([("A.SYNTH".parse().unwrap(), 1e-12)]),
            )],
            unfilled_entry: UnfilledEntry::Skip,
            rebalance: Rebalance::FullTarget,
        }],
    )
    .unwrap();
    assert_eq!(result.fills.len(), 1);
    assert_eq!(
        result.fills[0].fill.total_fees(),
        "0.00001".parse().unwrap()
    );
    assert_eq!(
        result.fills[0].fill.order().quantity(),
        "0.00099999999999999".parse().unwrap()
    );
}

#[test]
fn panel_preserves_scale_18_source_prices_which_f64_cannot_represent() {
    let lake = tempfile::tempdir().unwrap();
    let dsv = common::publish_v1(lake.path());
    let venue = VenueId::new("SYNTH").unwrap();
    let original = load_panel(lake.path(), &dsv, &venue).unwrap();
    let mut fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    fixture["bar_defaults"]["open"] = serde_json::from_str("100.000000000000000001").unwrap();
    for instrument in fixture["instruments"].as_array_mut().unwrap() {
        instrument["price_precision"] = json!(18);
    }
    let dsv = common::publish_fixture(lake.path(), &serde_json::to_vec(&fixture).unwrap(), "3");
    let panel = load_panel(lake.path(), &dsv, &venue).unwrap();
    assert_eq!(
        panel.grid.column("open").unwrap().f64().unwrap().get(0),
        original.grid.column("open").unwrap().f64().unwrap().get(0)
    );
    assert_eq!(
        panel
            .grid
            .column("open_decimal")
            .unwrap()
            .str()
            .unwrap()
            .get(0),
        Some("100.000000000000000001")
    );
    let statuses = load_execution_status(lake.path(), &dsv).unwrap();
    let result = run_fast_event(
        &panel,
        &statuses,
        &config(&panel, Sizing::Lot),
        &[PortfolioRun {
            id: VirtualPortfolioId(1),
            capital: "100000".parse().unwrap(),
            decisions: vec![decision(
                &panel,
                0,
                BTreeMap::from([("A.SYNTH".parse().unwrap(), 1.0)]),
            )],
            unfilled_entry: UnfilledEntry::Skip,
            rebalance: Rebalance::FullTarget,
        }],
    )
    .unwrap();
    assert_eq!(
        result.fills[0].fill.raw_price(),
        "100.000000000000000001".parse().unwrap()
    );
}
