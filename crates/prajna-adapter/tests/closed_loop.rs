#[path = "../../prajna-research/tests/common/mod.rs"]
#[allow(dead_code)]
mod common;

use prajna_account::VirtualPortfolioId;
use prajna_adapter::{FakeBackend, RunPacket, run_adapter};
use prajna_domain::{Currency, VenueId};
use prajna_research::{
    factor::AvailabilityAssumption,
    fast_event::{
        FastEventConfig, FastEventCosts, PortfolioRun, Rebalance, Sizing, run_fast_event,
    },
    load_execution_status, load_panel,
    strategy::{BuyAndHoldParams, UnfilledEntry, VectorStrategy},
};
use std::collections::BTreeMap;

#[test]
fn a_packet_from_different_prices_cannot_drive_the_same_run() {
    let lake = tempfile::tempdir().unwrap();
    let dsv = common::publish_version(lake.path(), "3");
    let venue = VenueId::new("SYNTH").unwrap();
    let panel = load_panel(lake.path(), &dsv, &venue).unwrap();
    let statuses = load_execution_status(lake.path(), &dsv).unwrap();
    let mut fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../poc/poc0-benchmark/fixtures/dataset-v1.json"
    ))
    .unwrap();
    fixture["bar_defaults"]["open"] = serde_json::json!(101);
    let other_dsv =
        common::publish_fixture(lake.path(), &serde_json::to_vec(&fixture).unwrap(), "3");
    let packet = RunPacket::from_dataset(lake.path(), &other_dsv).unwrap();
    let config = config(&packet, "100000");
    let portfolio = run(&panel, UnfilledEntry::Skip, &[(0, "A.SYNTH")]);
    let error = run_adapter(
        &packet,
        &panel,
        &statuses,
        &config,
        &[portfolio],
        &mut FakeBackend::default(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("differ"));
    let canonical = RunPacket::from_dataset(lake.path(), &dsv).unwrap();
    let mut config = config;
    config.currency = Currency::new("USD").unwrap();
    let run = run(&panel, UnfilledEntry::Skip, &[(0, "A.SYNTH")]);
    assert!(
        run_adapter(
            &canonical,
            &panel,
            &statuses,
            &config,
            &[run],
            &mut FakeBackend::default()
        )
        .is_err()
    );
}

fn config(packet: &RunPacket, cash: &str) -> FastEventConfig {
    FastEventConfig {
        venue: VenueId::new("SYNTH").unwrap(),
        currency: Currency::new("CNY").unwrap(),
        initial_cash: cash.parse().unwrap(),
        sizing: Sizing::Lot,
        lot_sizes: packet
            .instruments
            .iter()
            .map(|i| (i.id().clone(), i.lot_size().unwrap()))
            .collect(),
        costs: FastEventCosts::ZERO,
    }
}

fn run(
    panel: &prajna_research::Panel,
    policy: UnfilledEntry,
    targets: &[(usize, &str)],
) -> PortfolioRun {
    PortfolioRun {
        id: VirtualPortfolioId(1),
        capital: "100000".parse().unwrap(),
        decisions: targets
            .iter()
            .map(|(index, id)| prajna_research::strategy::Decision {
                decision_session: panel.sessions[*index].clone(),
                ranked: vec![],
                targets: BTreeMap::from([(id.parse().unwrap(), 1.0)]),
            })
            .collect(),
        unfilled_entry: policy,
        rebalance: Rebalance::FullTarget,
    }
}

#[test]
fn rejected_buy_is_not_booked_and_obeys_skip_retry_and_entry_only() {
    use prajna_account::OrderId;
    use prajna_research::fast_event::EntryBudget;
    let lake = tempfile::tempdir().unwrap();
    let dsv = common::publish_version(lake.path(), "3");
    let panel = load_panel(lake.path(), &dsv, &VenueId::new("SYNTH").unwrap()).unwrap();
    let statuses = load_execution_status(lake.path(), &dsv).unwrap();
    let packet = RunPacket::from_dataset(lake.path(), &dsv).unwrap();
    let config = config(&packet, "100000");
    for (policy, rebalance, expected_fills) in [
        (UnfilledEntry::Skip, Rebalance::FullTarget, 0),
        (UnfilledEntry::Retry, Rebalance::FullTarget, 1),
        (
            UnfilledEntry::Retry,
            Rebalance::EntryOnly(EntryBudget::FixedInitial),
            1,
        ),
    ] {
        let mut run = run(&panel, policy, &[(0, "A.SYNTH")]);
        run.rebalance = rebalance;
        let mut fake = FakeBackend::default();
        fake.reject_orders.insert(OrderId(1));
        let result = run_adapter(&packet, &panel, &statuses, &config, &[run], &mut fake).unwrap();
        assert_eq!(result.execution.account.fills().len(), expected_fills);
        assert_eq!(
            result.execution.sessions[1].open_after.account.cash,
            "100000".parse().unwrap()
        );
        assert!(
            result.execution.sessions[1]
                .open_after
                .account
                .positions
                .is_empty()
        );
        assert!(matches!(
            result.backend_events[0],
            prajna_adapter::protocol::Response::Reject(_)
        ));
        if expected_fills == 1 {
            let fill = &result.execution.account.fills()[0];
            assert_eq!(fill.order().id(), OrderId(2));
            assert_eq!(fill.executed_at(), panel.sessions[2].ts_open);
            assert_eq!(fill.order().quantity(), "1000".parse().unwrap());
        }
    }
}

#[test]
fn rejected_exit_defers_remaining_orders_and_replans_from_actual_holdings() {
    use prajna_account::{OrderId, Side};
    let lake = tempfile::tempdir().unwrap();
    let dsv = common::publish_version(lake.path(), "3");
    let panel = load_panel(lake.path(), &dsv, &VenueId::new("SYNTH").unwrap()).unwrap();
    let statuses = load_execution_status(lake.path(), &dsv).unwrap();
    let packet = RunPacket::from_dataset(lake.path(), &dsv).unwrap();
    let config = config(&packet, "100000");
    let run = run(
        &panel,
        UnfilledEntry::Skip,
        &[(0, "A.SYNTH"), (1, "B.SYNTH")],
    );
    let mut fake = FakeBackend::default();
    fake.reject_orders.insert(OrderId(2));
    let result = run_adapter(&packet, &panel, &statuses, &config, &[run], &mut fake).unwrap();
    let fills = result.execution.account.fills();
    assert_eq!(fills.len(), 3);
    assert_eq!(fills[0].order().instrument_id().to_string(), "A.SYNTH");
    assert_eq!(fills[1].order().side(), Side::Sell);
    assert_eq!(fills[1].executed_at(), panel.sessions[3].ts_open);
    assert_eq!(fills[2].order().instrument_id().to_string(), "B.SYNTH");
    assert_eq!(fills[2].executed_at(), panel.sessions[3].ts_open);
    assert_eq!(
        result.execution.sessions[2]
            .open_after
            .account
            .positions
            .len(),
        1
    );
    assert_eq!(
        result.execution.sessions[2].open_after.account.cash,
        prajna_domain::Notional::ZERO
    );
    assert!(
        result
            .execution
            .account
            .orders()
            .values()
            .all(|o| o.submitted_at() != panel.sessions[2].ts_open || o.side() == Side::Sell)
    );
}

#[test]
fn s1_s2_s3_full_target_fake_matches_fast_event_every_fill_and_both_daily_marks() {
    let fixtures = [
        (
            "dataset-v1",
            include_bytes!("../../../poc/poc0-benchmark/fixtures/dataset-v1.json").to_vec(),
            vec!["s1", "s2", "s3"],
        ),
        ("b2-ma20-60-v1", ma_fixture(), vec!["s3"]),
    ];
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (name, bytes, strategies) in fixtures {
        let lake = tempfile::tempdir().unwrap();
        let dsv = common::publish_fixture(lake.path(), &bytes, "3");
        let panel = load_panel(lake.path(), &dsv, &VenueId::new("SYNTH").unwrap()).unwrap();
        let statuses = load_execution_status(lake.path(), &dsv).unwrap();
        let packet = RunPacket::from_dataset(lake.path(), &dsv).unwrap();
        let mut config = config(&packet, "100000");
        config.costs = FastEventCosts {
            commission_rate: "0.001".parse().unwrap(),
            minimum_commission: "100".parse().unwrap(),
            buy_slippage_rate: "0.001".parse().unwrap(),
            sell_slippage_rate: "0.001".parse().unwrap(),
            ..FastEventCosts::ZERO
        };
        for label in strategies {
            let golden: serde_json::Value = serde_json::from_slice(
                &std::fs::read(
                    root.join(format!("poc/mvp3-golden/expected/{name}.{label}.lot.json")),
                )
                .unwrap(),
            )
            .unwrap();
            let mut run = run(&panel, UnfilledEntry::Skip, &[]);
            run.decisions = golden["decisions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| prajna_research::strategy::Decision {
                    decision_session: panel
                        .sessions
                        .iter()
                        .find(|s| {
                            s.session_date.to_string() == row["session_date"].as_str().unwrap()
                        })
                        .unwrap()
                        .clone(),
                    ranked: vec![],
                    targets: row["targets"]
                        .as_object()
                        .unwrap()
                        .iter()
                        .map(|(id, w)| (id.parse().unwrap(), w.as_f64().unwrap()))
                        .collect(),
                })
                .collect();
            let fast = run_fast_event(&panel, &statuses, &config, &[run.clone()]).unwrap();
            let result = run_adapter(
                &packet,
                &panel,
                &statuses,
                &config,
                &[run],
                &mut FakeBackend::default(),
            )
            .unwrap();
            assert_eq!(result.execution.account, fast.account, "{name}/{label}");
            assert_eq!(result.execution.sessions.len(), panel.sessions.len());
            assert_eq!(
                result
                    .execution
                    .attempts
                    .iter()
                    .map(|a| (a.portfolio_id, &a.event))
                    .collect::<Vec<_>>(),
                fast.attempts
                    .iter()
                    .map(|a| (a.portfolio_id, &a.event))
                    .collect::<Vec<_>>()
            );
            for (a, b) in result.execution.sessions.iter().zip(&fast.sessions) {
                assert_eq!(a.open_after, b.open_after);
                assert_eq!(a.close, b.close);
                assert_eq!(a.open_sources, b.open_sources);
                assert_eq!(a.close_sources, b.close_sources);
                for mark in [&a.open_after, &a.close] {
                    exact_identities(mark);
                }
            }
            if name == "b2-ma20-60-v1" {
                assert!(!result.execution.fills.is_empty());
            }
        }
    }
}

fn exact_identities(mark: &prajna_account::AccountValuation) {
    for vp in std::iter::once(&mark.account).chain(mark.portfolios.values()) {
        assert_eq!(
            vp.equity.mantissa(),
            vp.cash.mantissa()
                + vp.positions
                    .values()
                    .map(|p| p.market_value.mantissa())
                    .sum::<i128>()
        );
    }
    assert_eq!(
        mark.account.cash.mantissa(),
        mark.unallocated_capital.mantissa()
            + mark
                .portfolios
                .values()
                .map(|vp| vp.cash.mantissa())
                .sum::<i128>()
    );
    assert_eq!(
        mark.account.equity.mantissa(),
        mark.unallocated_capital.mantissa()
            + mark
                .portfolios
                .values()
                .map(|vp| vp.equity.mantissa())
                .sum::<i128>()
    );
    for (id, position) in &mark.account.positions {
        assert_eq!(
            position.quantity.mantissa(),
            mark.portfolios
                .values()
                .filter_map(|vp| vp.positions.get(id))
                .map(|p| p.quantity.mantissa())
                .sum::<i128>()
        );
    }
}

fn ma_fixture() -> Vec<u8> {
    use chrono::{Datelike, Duration, NaiveDate};
    use serde_json::{Value, json};
    let source: Value = serde_json::from_str(include_str!(
        "../../../poc/poc0-benchmark/fixtures/b2-ma20-60-v1.json"
    ))
    .unwrap();
    let mut fixture: Value = serde_json::from_str(include_str!(
        "../../../poc/poc0-benchmark/fixtures/dataset-v1.json"
    ))
    .unwrap();
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
fn two_portfolios_receive_independent_fills_and_exact_daily_fast_event_balances() {
    let lake = tempfile::tempdir().unwrap();
    let dsv = common::publish_version(lake.path(), "3");
    let venue = VenueId::new("SYNTH").unwrap();
    let panel = load_panel(lake.path(), &dsv, &venue).unwrap();
    let statuses = load_execution_status(lake.path(), &dsv).unwrap();
    let packet = RunPacket::from_dataset(lake.path(), &dsv).unwrap();
    let decisions = VectorStrategy::BuyAndHold(BuyAndHoldParams::default())
        .decide_panel(&panel, &panel.instruments, AvailabilityAssumption::None)
        .unwrap();
    let config = FastEventConfig {
        venue,
        currency: Currency::new("CNY").unwrap(),
        initial_cash: "2000000".parse().unwrap(),
        sizing: Sizing::Lot,
        lot_sizes: packet
            .instruments
            .iter()
            .map(|i| (i.id().clone(), i.lot_size().unwrap()))
            .collect(),
        costs: FastEventCosts {
            commission_rate: "0.0003".parse().unwrap(),
            minimum_commission: "5".parse().unwrap(),
            buy_slippage_rate: "0.001".parse().unwrap(),
            sell_slippage_rate: "0.001".parse().unwrap(),
            sell_tax_rate: "0.001".parse().unwrap(),
            ..FastEventCosts::ZERO
        },
    };
    let runs = [1, 2].map(|id| PortfolioRun {
        id: VirtualPortfolioId(id),
        capital: "1000000".parse().unwrap(),
        decisions: decisions.clone(),
        unfilled_entry: UnfilledEntry::Skip,
        rebalance: Rebalance::FullTarget,
    });
    let fast = run_fast_event(&panel, &statuses, &config, &runs).unwrap();
    let actual = run_adapter(
        &packet,
        &panel,
        &statuses,
        &config,
        &runs,
        &mut FakeBackend::default(),
    )
    .unwrap();
    assert_eq!(actual.execution.account, fast.account);
    assert_eq!(
        actual
            .execution
            .fills
            .iter()
            .map(|f| &f.fill)
            .collect::<Vec<_>>(),
        fast.fills.iter().map(|f| &f.fill).collect::<Vec<_>>()
    );
    for (actual, expected) in actual.execution.sessions.iter().zip(&fast.sessions) {
        assert_eq!(actual.open_after, expected.open_after);
        assert_eq!(actual.close, expected.close);
        exact_identities(&actual.open_after);
        exact_identities(&actual.close);
    }
    let mut grouped = BTreeMap::new();
    for fill in actual.execution.account.fills() {
        grouped
            .entry((fill.executed_at(), fill.order().instrument_id().clone()))
            .or_insert_with(Vec::new)
            .push(fill.order().portfolio_id());
    }
    assert!(
        grouped
            .values()
            .any(|ids| ids == &[VirtualPortfolioId(1), VirtualPortfolioId(2)])
    );
    assert!(!actual.adapter_events.is_empty());
    assert_eq!(actual.backend_events.len(), actual.execution.fills.len());
}

struct ChangedResponse {
    fake: FakeBackend,
    field: &'static str,
    value: serde_json::Value,
}
impl prajna_adapter::Backend for ChangedResponse {
    fn begin_run(
        &mut self,
        packet: &RunPacket,
        costs: FastEventCosts,
    ) -> Result<(), prajna_adapter::AdapterError> {
        prajna_adapter::Backend::begin_run(&mut self.fake, packet, costs)
    }
    fn exchange(&mut self, request: &str) -> Result<String, prajna_adapter::AdapterError> {
        let line = prajna_adapter::Backend::exchange(&mut self.fake, request)?;
        let mut value: serde_json::Value = serde_json::from_str(&line)?;
        if value["type"] == "fill" {
            value[self.field] = self.value.clone();
        }
        prajna_adapter::protocol::encode_line(&value).map_err(Into::into)
    }
}

#[test]
fn backend_fee_is_diagnostic_and_invalid_fill_identity_quantity_or_time_aborts() {
    let lake = tempfile::tempdir().unwrap();
    let dsv = common::publish_version(lake.path(), "3");
    let panel = load_panel(lake.path(), &dsv, &VenueId::new("SYNTH").unwrap()).unwrap();
    let statuses = load_execution_status(lake.path(), &dsv).unwrap();
    let packet = RunPacket::from_dataset(lake.path(), &dsv).unwrap();
    let mut config = config(&packet, "100000");
    config.costs.minimum_commission = "5".parse().unwrap();
    let run = run(&panel, UnfilledEntry::Skip, &[(0, "A.SYNTH")]);
    let mut backend = ChangedResponse {
        fake: FakeBackend::default(),
        field: "fee",
        value: serde_json::json!("9999"),
    };
    let result = run_adapter(
        &packet,
        &panel,
        &statuses,
        &config,
        std::slice::from_ref(&run),
        &mut backend,
    )
    .unwrap();
    assert_eq!(result.execution.account.cash(), "9995".parse().unwrap());
    assert_eq!(
        result.execution.account.fills()[0]
            .costs()
            .total_commission()
            .unwrap(),
        "5".parse().unwrap()
    );
    let prajna_adapter::protocol::Response::Fill(fill) = &result.backend_events[0] else {
        panic!("expected Fill")
    };
    assert_eq!(fill.fee, "9999".parse().unwrap());
    for (field, value) in [
        ("portfolio_id", serde_json::json!("2")),
        ("order_id", serde_json::json!("99")),
        ("instrument_id", serde_json::json!("B.SYNTH")),
        ("side", serde_json::json!("sell")),
        ("quantity", serde_json::json!("100")),
        ("quantity", serde_json::json!(900.0)),
        ("executed_at", serde_json::json!("0")),
        ("fee", serde_json::json!("-1")),
    ] {
        let mut backend = ChangedResponse {
            fake: FakeBackend::default(),
            field,
            value,
        };
        assert!(
            run_adapter(
                &packet,
                &panel,
                &statuses,
                &config,
                std::slice::from_ref(&run),
                &mut backend
            )
            .is_err(),
            "{field}"
        );
    }
}
