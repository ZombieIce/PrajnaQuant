#[path = "../../prajna-research/tests/common/mod.rs"]
#[allow(dead_code)]
mod common;

#[path = "support/exact_execution.rs"]
mod exact_execution;
use exact_execution::exact_projection;

use std::{collections::BTreeMap, path::Path, process::Command};

use prajna_account::{Side, VirtualPortfolioId};
use prajna_adapter::{AdapterResult, RunPacket, run_adapter};
use prajna_domain::{Currency, Notional, VenueId};
use prajna_experiment::{BackendDiagnostics, BackendFactory, BackendIdentity, WorkerCommand};
use prajna_research::{
    fast_event::{
        EntryBudget, FastEventConfig, FastEventCosts, FastEventResult, PortfolioRun, Rebalance,
        Sizing, run_fast_event,
    },
    load_execution_status, load_panel,
    strategy::{Decision, UnfilledEntry},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[test]
fn real_nautilus_matches_independent_goldens_and_classifies_every_difference() {
    let python = std::env::var_os("PRAJNA_NAUTILUS_PYTHON")
        .expect("real correctness requires PRAJNA_NAUTILUS_PYTHON; no fake or skip permitted");
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let worker = WorkerCommand::new(&python, root.join("backends/nautilus/worker.py"));
    let identity = BackendIdentity {
        name: "nautilus_trader".into(),
        version: "2.0.0rc6".into(),
    };
    worker.validate(&identity).unwrap();
    let mut cases = vec![];
    for (name, strategy) in [
        ("hand-v1", "s1"),
        ("hand-retry-v1", "s1"),
        ("hand-rebalance-v1", "s2"),
        ("hand-fee-only-v1", "s2"),
        ("dataset-v1", "s1"),
        ("dataset-v1", "s2"),
        ("dataset-v1", "s3"),
        ("b2-ma20-60-v1", "s3"),
    ] {
        cases.push((name, strategy, "full_target", false, false));
    }
    for (name, strategy, poc_timing) in [
        ("dataset-v1", "s1", false),
        ("dataset-v1", "s1", true),
        ("dataset-v1", "s2", false),
        ("b2-ma20-60-v1", "s3", false),
    ] {
        cases.push((name, strategy, "entry_only", poc_timing, false));
    }
    cases.push(("hand-v1", "s1", "full_target", false, true));
    let mut report = json!({
        "schema_version":"mvp4.correctness@1", "backend":identity,
        "host":std::env::consts::OS, "architecture":std::env::consts::ARCH,
        "scope":"synthetic Decision tapes; not PIT, total return, production or 64x252 evidence",
        "tolerances":{"quantity":"exact","money_absolute_cny":1e-6,"nav_relative":1e-9},
        "source_sha256":([
            "Cargo.lock",
            "crates/prajna-experiment/tests/accurate_correctness.rs",
            "poc/mvp4-correctness/prepare_fixture.py",
            "poc/mvp3-golden/fast_event_golden.py",
            "crates/prajna-adapter/src/execution.rs",
            "crates/prajna-research/src/fast_event.rs",
            "backends/nautilus/worker.py",
            "backends/nautilus/packet.py"
        ].into_iter().map(|path| (path,format!("{:x}",Sha256::digest(std::fs::read(root.join(path)).unwrap()))))
            .collect::<BTreeMap<_,_>>()),
        "cases":[]
    });
    for (name, strategy, rebalance, poc_timing, two_vp) in cases {
        let source = root.join(if name.starts_with("hand") {
            format!("poc/mvp3-golden/fixtures/{name}.json")
        } else {
            format!("poc/poc0-benchmark/fixtures/{name}.json")
        });
        let temp = tempfile::tempdir().unwrap();
        let regenerated = temp.path().join("golden.json");
        let mut command = Command::new(&python);
        command
            .arg(root.join("poc/mvp3-golden/fast_event_golden.py"))
            .args(["--fixture"])
            .arg(&source)
            .args([
                "--strategy",
                strategy,
                "--sizing",
                "lot",
                "--rebalance",
                rebalance,
                "--out",
            ])
            .arg(&regenerated);
        if poc_timing {
            command.arg("--poc0-s1-timing");
        }
        assert!(
            command.status().unwrap().success(),
            "independent oracle failed"
        );
        let suffix = if rebalance == "full_target" {
            "lot"
        } else if poc_timing {
            "poc0.entry_only.lot"
        } else {
            "entry_only.lot"
        };
        let golden = read_json(&regenerated);
        assert_eq!(
            golden,
            read_json(&root.join(format!(
                "poc/mvp3-golden/expected/{name}.{strategy}.{suffix}.json"
            ))),
            "committed oracle must reproduce before it can be evidence"
        );
        let normalized = temp.path().join("fixture.json");
        assert!(
            Command::new(&python)
                .arg(root.join("poc/mvp4-correctness/prepare_fixture.py"))
                .arg(&source)
                .arg(&normalized)
                .status()
                .unwrap()
                .success()
        );
        let dsv = common::publish_fixture(temp.path(), &std::fs::read(normalized).unwrap(), "3");
        let venue = VenueId::new("SYNTH").unwrap();
        let panel = load_panel(temp.path(), &dsv, &venue).unwrap();
        let statuses = load_execution_status(temp.path(), &dsv).unwrap();
        let packet = RunPacket::from_dataset(temp.path(), &dsv).unwrap();
        let parameters = &golden["parameters"];
        let costs = &parameters["costs"];
        let decimal = |value: &Value| value.to_string().parse::<Notional>().unwrap();
        let rate = |field: &str| {
            decimal(&costs[field])
                .checked_div_decimal("10000".parse().unwrap())
                .unwrap()
        };
        let capital = decimal(&parameters["initial_cash"]);
        let config = FastEventConfig {
            venue,
            currency: Currency::new("CNY").unwrap(),
            initial_cash: if two_vp {
                "2500".parse().unwrap()
            } else {
                capital
            },
            sizing: Sizing::Lot,
            lot_sizes: packet
                .instruments
                .iter()
                .map(|i| (i.id().clone(), i.lot_size().unwrap()))
                .collect(),
            costs: FastEventCosts {
                commission_rate: decimal(&costs["commission_rate"]),
                minimum_commission: decimal(&costs["minimum_commission"]),
                buy_tax_rate: decimal(&costs["buy_tax_rate"]),
                sell_tax_rate: decimal(&costs["sell_tax_rate"]),
                buy_slippage_rate: rate("buy_slippage_bps"),
                sell_slippage_rate: rate("sell_slippage_bps"),
            },
        };
        let portfolio = PortfolioRun {
            id: VirtualPortfolioId(1),
            capital,
            decisions: golden["decisions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| Decision {
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
                        .map(|(id, weight)| (id.parse().unwrap(), weight.as_f64().unwrap()))
                        .collect(),
                })
                .collect(),
            unfilled_entry: if parameters["unfilled_entry"] == "retry" {
                UnfilledEntry::Retry
            } else {
                UnfilledEntry::Skip
            },
            rebalance: if rebalance == "full_target" {
                Rebalance::FullTarget
            } else {
                Rebalance::EntryOnly(if strategy == "s1" {
                    EntryBudget::FixedInitial
                } else {
                    EntryBudget::RemainingCash
                })
            },
        };
        let mut portfolios = vec![portfolio.clone()];
        if two_vp {
            portfolios.push(PortfolioRun {
                id: VirtualPortfolioId(2),
                ..portfolio
            });
        }
        let fast = run_fast_event(&panel, &statuses, &config, &portfolios).unwrap();
        let mut backend = worker.create(&identity, &config, &dsv).unwrap();
        let actual = run_adapter(
            &packet,
            &panel,
            &statuses,
            &config,
            &portfolios,
            backend.as_mut(),
        )
        .unwrap();
        let diagnostics = backend.finish().unwrap();
        assert_eq!(diagnostics.evidence, "nautilus_trader");
        let mut comparisons = Comparison::default();
        for portfolio in &portfolios {
            comparisons.compare(
                "independent_python",
                &projection(&actual.execution, portfolio.id, portfolio.capital),
                &golden_projection(&golden),
                false,
            );
        }
        comparisons.compare(
            "fast_event",
            &exact_projection(&actual.execution),
            &exact_projection(&fast),
            true,
        );
        if rebalance == "entry_only" && (strategy != "s1" || poc_timing) {
            compare_poc(&root, strategy, &actual.execution, &mut comparisons);
        }
        compare_native(&actual, &diagnostics, &mut comparisons);
        if two_vp {
            let fills = actual.execution.account.fills();
            assert_eq!(fills.len(), 2);
            assert_ne!(fills[0].order().id(), fills[1].order().id());
            assert_ne!(
                fills[0].order().portfolio_id(),
                fills[1].order().portfolio_id()
            );
            assert_eq!(fills[0].executed_at(), fills[1].executed_at());
            assert_eq!(
                fills[0].order().instrument_id(),
                fills[1].order().instrument_id()
            );
            let mark = &actual.execution.sessions[1].close;
            assert_eq!(mark.account.cash, "627.46".parse().unwrap());
            assert_eq!(
                mark.account.positions[&"A.SYNTH".parse().unwrap()].quantity,
                "180".parse().unwrap()
            );
            assert_eq!(mark.account.equity, "2787.46".parse().unwrap());
            assert_eq!(mark.unallocated_capital, "500".parse().unwrap());
        }
        report["cases"].as_array_mut().unwrap().push(json!({
            "fixture":name,"strategy":strategy,"rebalance":rebalance,"poc0_s1_timing":poc_timing,
            "virtual_portfolios":portfolios.len(),"source_sha256":golden["fixture_sha256"],
            "dsv":dsv,"fills":actual.execution.fills.len(),"sessions":actual.execution.sessions.len(),
            "checked_fields":comparisons.checked,"differences":comparisons.differences,
            "adapter_events":actual.adapter_events.iter().map(|event| format!("{event:?}")).collect::<Vec<_>>(),
            "adapter_constructions":diagnostics.adapter_constructions,
            "backend_native_events":diagnostics.native_events,
            "backend_comparisons":diagnostics.comparisons,
        }));
    }
    let failures = report["cases"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|case| case["differences"].as_array().unwrap())
        .filter(|difference| difference["class"] == "adapter_defect")
        .count();
    report["adapter_defects"] = failures.into();
    report["unclassified_differences"] = 0.into();
    report["conclusion"] = if failures == 0 { "pass" } else { "fail" }.into();
    if let Some(path) = std::env::var_os("PRAJNA_CORRECTNESS_REPORT") {
        std::fs::write(path, serde_json::to_string_pretty(&report).unwrap() + "\n").unwrap();
    }
    assert_eq!(
        failures, 0,
        "classified Adapter defects; inspect PRAJNA_CORRECTNESS_REPORT"
    );
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[derive(Default)]
struct Comparison {
    checked: usize,
    differences: Vec<Value>,
}

impl Comparison {
    fn difference(
        &mut self,
        path: &str,
        actual: &Value,
        expected: &Value,
        class: &str,
        evidence: Value,
    ) {
        self.differences.push(json!({
            "path":path,"actual":actual,"expected":expected,"class":class,"evidence":evidence,
        }));
    }

    fn compare(&mut self, path: &str, actual: &Value, expected: &Value, exact: bool) {
        match (actual, expected) {
            (Value::Object(a), Value::Object(b)) if a.keys().eq(b.keys()) => {
                for (key, value) in a {
                    self.compare(&format!("{path}/{key}"), value, &b[key], exact);
                }
            }
            (Value::Array(a), Value::Array(b)) if a.len() == b.len() => {
                for (index, (a, b)) in a.iter().zip(b).enumerate() {
                    self.compare(&format!("{path}/{index}"), a, b, exact);
                }
            }
            _ => {
                self.checked += 1;
                if actual == expected {
                    return;
                }
                if actual.is_number()
                    && expected.is_number()
                    && actual.to_string().parse::<Notional>().unwrap()
                        == expected.to_string().parse::<Notional>().unwrap()
                {
                    return;
                }
                let field = path.rsplit('/').next().unwrap();
                let within = !exact
                    && field != "quantity"
                    && actual
                        .as_f64()
                        .zip(expected.as_f64())
                        .is_some_and(|(a, b)| {
                            a.is_finite()
                                && b.is_finite()
                                && if field == "nav" {
                                    b != 0.0 && (a - b).abs() / b.abs() <= 1e-9
                                } else {
                                    (a - b).abs() <= 1e-6
                                }
                        });
                self.difference(path, actual, expected, if within { "within_tolerance" } else { "adapter_defect" },
                    json!("F7: exact shapes/quantities; money <=1e-6 CNY; relative NAV <=1e-9; Fast scale-18 exact"));
            }
        }
    }
}

fn projection(result: &FastEventResult, vp: VirtualPortfolioId, capital: Notional) -> Value {
    assert_eq!(result.account.fills().len(), result.fills.len());
    assert!(
        result.account.fee_settlements().is_empty(),
        "lot must not fabricate fee-only settlements"
    );
    for (registered, returned) in result.account.fills().iter().zip(&result.fills) {
        assert_eq!(
            registered, &returned.fill,
            "account must count every Fill exactly once"
        );
    }
    let fills = result
        .fills
        .iter()
        .filter(|f| f.fill.order().portfolio_id() == vp)
        .map(|f| {
            let fill = &f.fill;
            let session = result
                .sessions
                .iter()
                .find(|s| s.session.ts_open == fill.executed_at())
                .unwrap();
            assert_eq!(fill.executed_at(), fill.order().submitted_at());
            let commission = fill.costs().total_commission().unwrap();
            json!({
                "session_date":session.session.session_date.to_string(),
                "instrument_id":fill.order().instrument_id(),
                "side":if fill.order().side() == Side::Buy {"buy"} else {"sell"},
                "quantity":fill.order().quantity().to_f64(),
                "raw_open":fill.raw_price().to_f64(),"price":fill.price().to_f64(),
                "notional":fill.trade_notional().to_f64(),"fee_basis":f.fee_basis.to_f64(),
                "commission":commission.to_f64(),
                "minimum_commission_top_up":fill.costs().minimum_commission_top_up.to_f64(),
                "tax":fill.costs().tax.to_f64(),"slippage":fill.slippage().to_f64(),
                "cash_change":fill.cash_delta().to_f64(),"total_cost":fill.total_fees().to_f64(),
                "cash_fees":add(commission, fill.costs().tax).to_f64(),
            })
        })
        .collect::<Vec<_>>();
    let sessions = result.sessions.iter().map(|s| {
        let mut row = json!({"session_date":s.session.session_date.to_string()});
        for (name, mark, sources) in [
            ("open",&s.open_after,&s.open_sources),("close",&s.close,&s.close_sources)
        ] {
            mark.validate().unwrap();
            let value = &mark.portfolios[&vp];
            assert!(value.cash >= Notional::ZERO);
            row[name] = json!({
                "cash":value.cash.to_f64(),"equity":value.equity.to_f64(),
                "nav":value.equity.to_f64()/capital.to_f64(),
                "holdings":value.positions.iter().map(|(id, p)| {
                    assert!(p.quantity > prajna_domain::Quantity::ZERO);
                    let source = &sources[id];
                    (id.to_string(),json!({
                        "quantity":p.quantity.to_f64(),"price":p.price.to_f64(),"value":p.market_value.to_f64(),
                        "price_source":{"session_date":source.session_date.to_string(),
                            "kind":if source.session_date == s.session.session_date {"raw"} else {"carried"},
                            "field":match source.field {prajna_research::fast_event::PriceField::Open => "open",
                                prajna_research::fast_event::PriceField::Close => "close"}}
                    }))
                }).collect::<BTreeMap<_,_>>()
            });
        }
        row
    }).collect::<Vec<_>>();
    let orders = result.account.orders().values().filter(|o| o.portfolio_id() == vp).map(|order| {
    let session = result.sessions.iter().find(|s| s.session.ts_open == order.submitted_at()).unwrap();
    json!({
        "session_date":session.session.session_date.to_string(),"instrument_id":order.instrument_id(),
        "side":if order.side() == Side::Buy {"buy"} else {"sell"},
        "quantity":order.quantity().to_f64(),
    })
    }).collect::<Vec<_>>();
    json!({"fills":fills,"orders":orders,"sessions":sessions})
}

fn golden_projection(golden: &Value) -> Value {
    let mut value = json!({"fills":golden["fills"],"sessions":golden["sessions"],
    "orders":golden["orders"].as_array().unwrap().iter().filter(|o| o["status"] == "filled")
        .map(|o|json!({"session_date":o["session_date"],"instrument_id":o["instrument_id"],
            "side":o["side"],"quantity":o["quantity"]})).collect::<Vec<_>>()});
    for row in value["fills"].as_array_mut().unwrap() {
        for field in ["order_id", "vp_id", "fee_side"] {
            row.as_object_mut().unwrap().remove(field);
        }
    }
    for row in value["sessions"].as_array_mut().unwrap() {
        row.as_object_mut()
            .unwrap()
            .retain(|key, _| ["session_date", "open", "close"].contains(&key.as_str()));
    }
    value
}

fn rounded_cny(value: Notional) -> Notional {
    let unit = 10_i128.pow(16);
    let quotient = value.mantissa() / unit;
    let remainder = value.mantissa() % unit;
    let up = remainder.abs() * 2 > unit || (remainder.abs() * 2 == unit && quotient % 2 != 0);
    Notional::from_mantissa((quotient + if up { value.mantissa().signum() } else { 0 }) * unit)
        .unwrap()
}

fn add(left: Notional, right: Notional) -> Notional {
    Notional::from_mantissa(left.mantissa().checked_add(right.mantissa()).unwrap()).unwrap()
}

fn subtract(left: Notional, right: Notional) -> Notional {
    Notional::from_mantissa(left.mantissa().checked_sub(right.mantissa()).unwrap()).unwrap()
}

fn compare_native(
    actual: &AdapterResult,
    diagnostics: &BackendDiagnostics,
    comparison: &mut Comparison,
) {
    let native_fills = diagnostics
        .native_events
        .iter()
        .filter(|e| e["event"] == "fill")
        .collect::<Vec<_>>();
    assert_eq!(
        native_fills.len(),
        actual.execution.fills.len(),
        "missing native Fill evidence"
    );
    let fee_models = diagnostics
        .comparisons
        .iter()
        .filter(|e| e["event"] == "fee_model")
        .collect::<Vec<_>>();
    assert_eq!(
        fee_models.len(),
        native_fills.len(),
        "missing FeeModel evidence"
    );
    let decimal = |v: &Value| v.as_str().unwrap().parse::<Notional>().unwrap();
    let mut cash_delta = Notional::ZERO;
    let barrier = diagnostics
        .adapter_constructions
        .iter()
        .find(|e| e["event"] == "lockstep_barrier")
        .expect("native timestamp mapping requires construction evidence");
    assert_eq!(barrier["offset_ns"], "1000000");
    for ((native, model), platform) in native_fills
        .iter()
        .zip(&fee_models)
        .zip(&actual.execution.fills)
    {
        let fill = &platform.fill;
        let fee = add(fill.costs().total_commission().unwrap(), fill.costs().tax);
        assert_eq!(
            decimal(&model["expected_fee"]),
            fee,
            "comparison FeeModel must match platform"
        );
        assert_eq!(decimal(&model["native_fee"]), decimal(&native["fee"]));
        assert_eq!(
            decimal(&native["fee"]),
            rounded_cny(fee),
            "unexplained native fee difference"
        );
        assert_eq!(native["order_id"], fill.order().id().0.to_string());
        assert_eq!(
            native["instrument_id"],
            fill.order().instrument_id().to_string()
        );
        assert_eq!(
            native["portfolio_id"],
            fill.order().portfolio_id().0.to_string()
        );
        assert_eq!(
            native["quantity"]
                .as_str()
                .unwrap()
                .parse::<prajna_domain::Quantity>()
                .unwrap(),
            fill.order().quantity()
        );
        assert_eq!(
            native["price"]
                .as_str()
                .unwrap()
                .parse::<prajna_domain::Price>()
                .unwrap(),
            fill.price()
        );
        let requested = fill.executed_at().as_unix_nanos();
        assert_eq!(native["protocol_executed_at"], requested.to_string());
        let native_time = native["native_ts_event"]
            .as_str()
            .unwrap()
            .parse::<i64>()
            .unwrap();
        assert_eq!(
            native_time,
            requested + 1_000_000,
            "unexplained native callback timing"
        );
        comparison.difference(&format!("backend_native/fill/{}/timestamp", fill.order().id().0),
            &json!(native_time.to_string()), &json!(requested.to_string()), "backend_native",
            json!({"native_fill":native,"adapter_mapping":barrier,
                "rule":"native callback at the registered open +1ms lockstep barrier; Domain maps to requested open"}));
        let native_fee = decimal(&native["fee"]);
        let fee_delta = subtract(native_fee, fee);
        if fee_delta != Notional::ZERO {
            comparison.difference(&format!("backend_native/fill/{}/fee", fill.order().id().0),
                &json!(native_fee), &json!(fee), "backend_native",
                json!({"fee_model":model,"native_fill":native,"rule":"CNY Money half-even precision 2"}));
        }
    }
    let snapshots = diagnostics
        .comparisons
        .iter()
        .filter(|e| e["event"] == "account_balance")
        .collect::<Vec<_>>();
    assert_eq!(
        snapshots.len(),
        actual.execution.sessions.len(),
        "one Trading Account comparison per Session"
    );
    for (snapshot, session) in snapshots.iter().zip(&actual.execution.sessions) {
        let day = session.session.session_date.to_string();
        assert_eq!(snapshot["session_date"], day);
        assert_eq!(snapshot["currency"], "CNY");
        for (native, platform) in native_fills
            .iter()
            .zip(&actual.execution.fills)
            .filter(|(_, p)| p.fill.executed_at() == session.session.ts_open)
        {
            let fill = &platform.fill;
            let fee = add(fill.costs().total_commission().unwrap(), fill.costs().tax);
            let fee_delta = subtract(fee, decimal(&native["fee"]));
            let notional_delta =
                subtract(rounded_cny(fill.trade_notional()), fill.trade_notional());
            let signed = if fill.order().side() == Side::Buy {
                subtract(Notional::ZERO, notional_delta)
            } else {
                notional_delta
            };
            cash_delta = add(add(cash_delta, fee_delta), signed);
        }
        let cash = decimal(&snapshot["cash"]);
        let expected = add(session.close.account.cash, cash_delta);
        comparison.compare(
            &format!("backend_native/{day}/cash_reconciliation"),
            &json!(cash),
            &json!(expected),
            true,
        );
        let positions = session
            .close
            .account
            .positions
            .iter()
            .map(|(id, p)| (id.to_string(), json!(p.quantity)))
            .collect::<BTreeMap<_, _>>();
        let native_positions = snapshot["positions"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(id, quantity)| {
                (
                    id.clone(),
                    json!(
                        quantity
                            .as_str()
                            .unwrap()
                            .parse::<prajna_domain::Quantity>()
                            .unwrap()
                    ),
                )
            })
            .collect::<BTreeMap<_, _>>();
        comparison.compare(
            &format!("backend_native/{day}/account_positions"),
            &json!(native_positions),
            &json!(positions),
            true,
        );
        if cash != session.close.account.cash {
            comparison.difference(&format!("backend_native/{day}/cash"), &json!(cash),
                &json!(session.close.account.cash), "backend_native", json!({
                    "snapshot":snapshot,"expected_cumulative_cash_delta":cash_delta,
                    "rule":"Trading Account only; cumulative currency-rounded notionals and native Fill fees",
                    "fill_evidence":"backend_native_events and fee_model rows for this case"
                }));
        }
    }
    assert!(!diagnostics.adapter_constructions.is_empty());
    assert!(
        diagnostics
            .native_events
            .iter()
            .all(|e| ["fill", "instrument_status"].contains(&e["event"].as_str().unwrap())),
        "native rejection/lifecycle difference needs explicit attribution"
    );
}

fn compare_poc(root: &Path, strategy: &str, result: &FastEventResult, comparison: &mut Comparison) {
    let reference = read_json(&root.join("poc/poc0-benchmark/results/b2-matched-decision-loads-reset-selected-2026-09-28.preflight.json"));
    let key = match strategy {
        "s1" => "buy_hold",
        "s2" => "momentum_rotation",
        "s3" => "ma20_60",
        _ => unreachable!(),
    };
    let reference = &reference[format!("b2_fast_event_{key}")]["projection"];
    let mut expected = json!({"fills":[],"sessions":[]});
    for row in reference["fills"]
        .as_array()
        .expect("POC-0 preflight Fill evidence")
    {
        expected["fills"].as_array_mut().unwrap().push(json!({
            "session_date":row["date"],"instrument_id":format!("{}.SYNTH",row["symbol"].as_str().unwrap()),
            "side":row["side"].as_str().unwrap().to_lowercase(),"quantity":row["quantity"],
            "raw_open":row["reference_price"],"price":row["fill_price"],"notional":row["gross_value"],
            "commission":row["commission"],"tax":row["tax"],"slippage":row["slippage_cost"]
        }));
    }
    for row in reference["ledger"].as_array().unwrap() {
        expected["sessions"].as_array_mut().unwrap().push(json!({
            "session_date":row["date"],"close":{"cash":row["cash"],"equity":row["nav"],
                "holdings":row["holdings"].as_array().unwrap().iter().map(|p| (
                    format!("{}.SYNTH",p["symbol"].as_str().unwrap()),
                    json!({"quantity":p["quantity"],"price":p["mark_price"]})
                )).collect::<BTreeMap<_,_>>()}
        }));
    }
    let mut actual = projection(result, VirtualPortfolioId(1), result.config.initial_cash);
    actual.as_object_mut().unwrap().remove("orders");
    for row in actual["fills"].as_array_mut().unwrap() {
        row.as_object_mut()
            .unwrap()
            .retain(|key, _| expected["fills"][0].get(key).is_some());
    }
    for row in actual["sessions"].as_array_mut().unwrap() {
        row.as_object_mut().unwrap().remove("open");
        let close = row["close"].as_object_mut().unwrap();
        close.remove("nav");
        for p in close["holdings"].as_object_mut().unwrap().values_mut() {
            p.as_object_mut()
                .unwrap()
                .retain(|key, _| ["quantity", "price"].contains(&key.as_str()));
        }
    }
    comparison.compare("poc0_b2", &actual, &expected, false);
}

#[test]
fn classification_does_not_hide_wrong_quantities_shapes_or_out_of_tolerance_values() {
    let mut comparison = Comparison::default();
    comparison.compare("money", &json!(1.0 + 5e-7), &json!(1.0), false);
    comparison.compare("fill/quantity", &json!(100.0 + 5e-7), &json!(100.0), false);
    comparison.compare("money", &json!(1.0 + 2e-6), &json!(1.0), false);
    comparison.compare("nav", &json!(1.0 + 2e-9), &json!(1.0), false);
    comparison.compare("shape", &json!([]), &json!([1]), false);
    comparison.compare("fast", &json!("1.000000000000000001"), &json!("1"), true);
    assert_eq!(comparison.differences[0]["class"], "within_tolerance");
    assert!(
        comparison.differences[1..]
            .iter()
            .all(|d| d["class"] == "adapter_defect")
    );
}
