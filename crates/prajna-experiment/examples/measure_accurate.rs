#[path = "../../prajna-research/tests/common/mod.rs"]
#[allow(dead_code)]
mod common;

#[path = "../tests/support/exact_execution.rs"]
mod exact_execution;
use exact_execution::exact_projection as projection;

use std::{
    collections::BTreeSet,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{Arc, Barrier},
    time::Instant,
};

use prajna_account::VirtualPortfolioId;
use prajna_adapter::{AdapterError, Backend, RunPacket, run_adapter};
use prajna_domain::{Currency, Notional, VenueId};
use prajna_research::{
    ExecutionStatusMap, Panel,
    factor::{AvailabilityAssumption, Factor, FactorKind, RotationScoreParams},
    fast_event::{
        FastEventConfig, FastEventCosts, PortfolioRun, Rebalance, Sizing, run_fast_event,
    },
    load_execution_status, load_panel,
    strategy::{
        BuyAndHoldParams, Decision, MaCrossoverParams, RankDirection, StrategyCapability, TopKRank,
        UnfilledEntry, VectorStrategy, Weighting,
    },
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

struct Worker {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    packet: PathBuf,
    baseline: Value,
}

impl Worker {
    fn new(python: &Path, root: &Path, packet: PathBuf) -> Self {
        let mut child = Command::new(python)
            .arg(root.join("poc/mvp4-cost/prepared_worker.py"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        let mut worker = Self {
            child,
            input,
            output,
            packet,
            baseline: Value::Null,
        };
        worker.baseline = worker.read();
        assert_eq!(worker.baseline["type"], "baseline");
        worker
    }

    fn read(&mut self) -> Value {
        let mut line = String::new();
        assert!(self.output.read_line(&mut line).unwrap() > 0, "worker EOF");
        let row: Value = serde_json::from_str(&line).unwrap();
        assert_ne!(row["type"], "error", "{row}");
        row
    }

    fn send(&mut self, row: &Value) {
        writeln!(self.input, "{row}").unwrap();
        self.input.flush().unwrap();
    }

    fn prepare(&mut self) -> Value {
        self.send(&json!({"type":"prepare","protocol":"prajna-adapter@1","packet":self.packet}));
        let row = self.read();
        assert_eq!(row["type"], "prepared");
        row
    }

    fn usage(&mut self) -> Value {
        let row = self.read();
        assert_eq!(row["type"], "usage");
        row
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if let Err(error) = self.child.kill() {
            if error.kind() != std::io::ErrorKind::InvalidInput {
                eprintln!("benchmark worker cleanup: {error}");
            }
        }
        if let Err(error) = self.child.wait() {
            eprintln!("benchmark worker reap: {error}");
        }
    }
}

impl Backend for Worker {
    fn begin_run(
        &mut self,
        _packet: &RunPacket,
        _costs: FastEventCosts,
    ) -> Result<(), AdapterError> {
        self.send(&json!({"type":"begin_run","protocol":"prajna-adapter@1","packet":self.packet}));
        let ready = self.read();
        assert_eq!(ready, json!({"type":"ready","protocol":"prajna-adapter@1"}));
        Ok(())
    }

    fn exchange(&mut self, request: &str) -> Result<String, AdapterError> {
        self.input.write_all(request.as_bytes())?;
        self.input.flush()?;
        let mut line = String::new();
        if self.output.read_line(&mut line)? == 0 {
            return Err(AdapterError::Invalid("benchmark worker EOF".into()));
        }
        Ok(line)
    }
}

struct Workload {
    packet: RunPacket,
    panel: Panel,
    statuses: ExecutionStatusMap,
    config: FastEventConfig,
    parameters: Value,
    expected_decisions: Vec<Decision>,
    expected: Value,
    dsv: String,
}

#[cfg(test)]
thread_local! {
    static RESEARCH_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl Workload {
    fn portfolios(&self) -> Vec<PortfolioRun> {
        let strategy = match self.parameters["strategy"].as_str().unwrap() {
            "s1" => VectorStrategy::BuyAndHold(BuyAndHoldParams::default()),
            "s2" => {
                let p = &self.parameters["strategy_parameters"];
                let integer = |key: &str| u32::try_from(p[key].as_u64().unwrap()).unwrap();
                assert_eq!(
                    p["trend_filter"], false,
                    "fixture requires explicit trend mapping"
                );
                VectorStrategy::TopKRank(TopKRank {
                    score: Factor::new(
                        FactorKind::RotationScore,
                        RotationScoreParams {
                            short: integer("momentum_short_days"),
                            long: integer("momentum_long_days"),
                            vol: integer("volatility_window"),
                            w_s: p["short_momentum_weight"].as_f64().unwrap(),
                            w_l: p["long_momentum_weight"].as_f64().unwrap(),
                            w_v: p["volatility_weight"].as_f64().unwrap(),
                            trend: None,
                        },
                    )
                    .unwrap(),
                    direction: RankDirection::Descending,
                    top_k: integer("top_n"),
                    rebalance_every: integer("rebalance_every"),
                    weighting: Weighting::EqualWeight,
                    unfilled_entry: self.policy(),
                    capabilities: BTreeSet::from([StrategyCapability::Vectorizable]),
                })
            }
            "s3" => VectorStrategy::MaCrossover(MaCrossoverParams {
                params_version: 1,
                short_window: 20,
                long_window: 60,
            }),
            strategy => panic!("unsupported measured strategy: {strategy}"),
        };
        #[cfg(test)]
        RESEARCH_CALLS.with(|calls| calls.set(calls.get() + 1));
        let decisions = strategy
            .decide_panel(
                &self.panel,
                &self.panel.instruments,
                AvailabilityAssumption::None,
            )
            .unwrap();
        vec![PortfolioRun {
            id: VirtualPortfolioId(1),
            capital: self.config.initial_cash,
            rebalance: Rebalance::FullTarget,
            unfilled_entry: self.policy(),
            decisions,
        }]
    }

    fn policy(&self) -> UnfilledEntry {
        match self.parameters["unfilled_entry"].as_str().unwrap() {
            "retry" => UnfilledEntry::Retry,
            "skip" => UnfilledEntry::Skip,
            policy => panic!("unsupported unfilled_entry: {policy}"),
        }
    }
}

struct Timed<T> {
    wall_ns: u128,
    research_ns: u128,
    portfolios: Vec<PortfolioRun>,
    result: T,
}

fn timed_run<T>(
    workload: &Workload,
    clock: &mut impl FnMut() -> u128,
    run: impl FnOnce(&[PortfolioRun]) -> T,
) -> Timed<T> {
    let start = clock();
    let portfolios = workload.portfolios();
    let research_end = clock();
    let result = run(&portfolios);
    let end = clock();
    Timed {
        wall_ns: end - start,
        research_ns: research_end - start,
        portfolios,
        result,
    }
}

fn assert_decisions(actual: &[Decision], expected: &[Decision]) {
    assert_eq!(actual.len(), expected.len(), "independent Decision count");
    for (actual, expected) in actual.iter().zip(expected) {
        assert_eq!(actual.decision_session, expected.decision_session);
        assert_eq!(
            actual.targets, expected.targets,
            "independent target weights"
        );
        assert_eq!(actual.ranked.len(), expected.ranked.len());
        for ((id, score, rank), (expected_id, expected_score, expected_rank)) in
            actual.ranked.iter().zip(&expected.ranked)
        {
            assert_eq!(id, expected_id);
            assert_eq!(rank, expected_rank);
            assert!(
                (score - expected_score).abs() <= 1e-12,
                "independent score differs: {score} vs {expected_score}"
            );
        }
    }
}

fn write_packet(workload: &Workload, path: &Path) {
    // The packet writer requires a new destination; only benchmark-owned data is replaced.
    if path.exists() {
        std::fs::remove_dir_all(path).unwrap();
    }
    workload.packet.write(path, &workload.dsv).unwrap();
    let costs = workload.config.costs;
    std::fs::write(path.join("worker.json"), serde_json::to_vec(&json!({
        "initial_cash":workload.config.initial_cash.to_string(),
        "costs":{
            "commission_rate":costs.commission_rate.to_string(),
            "minimum_commission":costs.minimum_commission.to_string(),
            "buy_tax_rate":costs.buy_tax_rate.to_string(),"sell_tax_rate":costs.sell_tax_rate.to_string(),
            "buy_slippage_rate":costs.buy_slippage_rate.to_string(),
            "sell_slippage_rate":costs.sell_slippage_rate.to_string()
        }
    })).unwrap()).unwrap();
}

fn execute(
    workload: &Workload,
    worker: &mut Worker,
    end_to_end: bool,
) -> Timed<(prajna_adapter::AdapterResult, Option<Value>, Value)> {
    let epoch = Instant::now();
    timed_run(workload, &mut || epoch.elapsed().as_nanos(), |portfolios| {
        let conversion = if end_to_end {
            write_packet(workload, &worker.packet);
            Some(worker.prepare())
        } else {
            None
        };
        let actual = run_adapter(
            &workload.packet,
            &workload.panel,
            &workload.statuses,
            &workload.config,
            portfolios,
            worker,
        )
        .unwrap();
        let usage = worker.usage();
        (actual, conversion, usage)
    })
}

fn measurement(
    workload: &Workload,
    timed: Timed<(prajna_adapter::AdapterResult, Option<Value>, Value)>,
) -> Value {
    assert_decisions(&timed.portfolios[0].decisions, &workload.expected_decisions);
    let (actual, conversion, usage) = timed.result;
    let result = projection(&actual.execution);
    assert_eq!(
        result, workload.expected,
        "cached/fresh Accurate differs from Fast at scale 18"
    );
    let checksum = format!("{:x}", Sha256::digest(serde_json::to_vec(&result).unwrap()));
    let decisions = timed.portfolios[0]
        .decisions
        .iter()
        .map(|d| json!({"session_date":d.decision_session.session_date,"targets":d.targets}))
        .collect::<Vec<_>>();
    let decision_checksum = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&decisions).unwrap())
    );
    json!({
        "wall_ns":timed.wall_ns,"research_ns":timed.research_ns,
        "research_invocations":1,"decision_checksum":decision_checksum,
        "backend_conversion":conversion,"usage":usage,"checksum":checksum
    })
}

fn sample(workload: &Workload, worker: &mut Worker, end_to_end: bool) -> Value {
    measurement(workload, execute(workload, worker, end_to_end))
}
fn load_workload(fixture: &Path, golden_path: &Path, lake: &Path) -> Workload {
    let dsv = common::publish_fixture(lake, &std::fs::read(fixture).unwrap(), "3");
    let venue = VenueId::new("SYNTH").unwrap();
    let panel = load_panel(lake, &dsv, &venue).unwrap();
    let statuses = load_execution_status(lake, &dsv).unwrap();
    let packet = RunPacket::from_dataset(lake, &dsv).unwrap();
    let golden: Value = serde_json::from_slice(&std::fs::read(golden_path).unwrap()).unwrap();
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
        initial_cash: capital,
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
    let portfolios = vec![PortfolioRun {
        id: VirtualPortfolioId(1),
        capital,
        rebalance: Rebalance::FullTarget,
        unfilled_entry: if parameters["unfilled_entry"] == "retry" {
            UnfilledEntry::Retry
        } else {
            UnfilledEntry::Skip
        },
        decisions: golden["decisions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| Decision {
                decision_session: panel
                    .sessions
                    .iter()
                    .find(|s| s.session_date.to_string() == row["session_date"].as_str().unwrap())
                    .unwrap()
                    .clone(),
                ranked: row["ranked"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|r| {
                        (
                            r["instrument_id"].as_str().unwrap().parse().unwrap(),
                            r["score"].as_f64().unwrap(),
                            u32::try_from(r["rank"].as_u64().unwrap()).unwrap(),
                        )
                    })
                    .collect(),
                targets: row["targets"]
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(id, weight)| (id.parse().unwrap(), weight.as_f64().unwrap()))
                    .collect(),
            })
            .collect(),
    }];
    let expected = projection(&run_fast_event(&panel, &statuses, &config, &portfolios).unwrap());
    Workload {
        packet,
        panel,
        statuses,
        config,
        parameters: parameters.clone(),
        expected_decisions: portfolios[0].decisions.clone(),
        expected,
        dsv,
    }
}

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    assert_eq!(args.len(), 4, "python normalized-fixture golden output");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let python = Path::new(&args[0]);
    let temp = tempfile::tempdir().unwrap();
    let workload = load_workload(Path::new(&args[1]), Path::new(&args[2]), temp.path());
    let path = temp.path().join("packet-serial");
    write_packet(&workload, &path);
    let mut worker = Worker::new(python, &root, path);
    let preparation = worker.prepare();
    for _ in 0..2 {
        sample(&workload, &mut worker, false);
    }
    let main_samples = (0..20)
        .map(|_| sample(&workload, &mut worker, false))
        .collect::<Vec<_>>();
    let end_to_end_samples = (0..20)
        .map(|_| sample(&workload, &mut worker, true))
        .collect::<Vec<_>>();
    let serial_baseline = worker.baseline.clone();
    drop(worker);
    let mut parallel = vec![];
    for group in 0..5 {
        let barrier = Arc::new(Barrier::new(3));
        std::thread::scope(|scope| {
            let mut handles = vec![];
            for index in 0..2 {
                let workload = &workload;
                let root = &root;
                let path = temp.path().join(format!("packet-{group}-{index}"));
                let barrier = barrier.clone();
                handles.push(scope.spawn(move || {
                    write_packet(workload, &path);
                    let mut worker = Worker::new(python, root, path);
                    worker.prepare();
                    for _ in 0..2 {
                        sample(workload, &mut worker, false);
                    }
                    barrier.wait();
                    // Ready workers wait again until the controller has started its timer.
                    barrier.wait();
                    let results = (0..6)
                        .map(|_| execute(workload, &mut worker, false))
                        .collect::<Vec<_>>();
                    // The second barrier excludes teardown from group wall time.
                    barrier.wait();
                    let samples = results
                        .into_iter()
                        .map(|r| measurement(workload, r))
                        .collect::<Vec<_>>();
                    json!({"baseline":worker.baseline,"samples":samples})
                }));
            }
            barrier.wait();
            let start = Instant::now();
            barrier.wait();
            barrier.wait();
            let ns = start.elapsed().as_nanos();
            let workers = handles
                .into_iter()
                .map(|h| h.join().unwrap())
                .collect::<Vec<_>>();
            parallel.push(json!({"group":group,"wall_ns":ns,"workers":workers}));
        });
    }
    std::fs::write(
        &args[3],
        serde_json::to_vec_pretty(&json!({
            "dsv":workload.dsv, "bars":workload.packet.bars.len(),
            "sessions":workload.panel.sessions.len(),"instruments":workload.panel.instruments.len(),
            "baseline":serial_baseline,"preparation":preparation,
            "research":{
                "implementation":"Rust VectorStrategy::decide_panel; fresh strategy and factor DAG per Run; no FactorCache",
                "strategy":workload.parameters["strategy"],
                "parameters":workload.parameters["strategy_parameters"],
                "work":if workload.parameters["strategy"] == "s1" {
                    "first-session equal-weight allocation only; no rolling factor workload, including hand S1 fixtures"
                } else {
                    "actual rolling factors/signals and Decisions, including hand S2 fixtures"
                },
                "decision_parity":"every timed Run matches independent oracle: exact sessions/targets/rank order; absolute score tolerance 1e-12"
            },
            "main":main_samples,"end_to_end":end_to_end_samples,"parallel":parallel,
            "parity":"exact Fast projection; robustness only, not independent correctness evidence",
        }))
        .unwrap(),
    )
    .unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_run_computes_rust_signals_between_timer_reads() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let lake = tempfile::tempdir().unwrap();
        let workload = load_workload(
            &root.join("poc/poc0-benchmark/fixtures/dataset-v1.json"),
            &root.join("poc/mvp3-golden/expected/dataset-v1.s2.lot.json"),
            lake.path(),
        );
        RESEARCH_CALLS.with(|calls| calls.set(0));
        for run in 0..2 {
            let mut reads = 0;
            let mut clock = || {
                RESEARCH_CALLS.with(|calls| {
                    assert_eq!(calls.get(), run + usize::from(reads > 0));
                });
                reads += 1;
                reads * 10
            };
            let timed = timed_run(&workload, &mut clock, |portfolios| {
                assert_decisions(&portfolios[0].decisions, &workload.expected_decisions);
            });
            assert_eq!(timed.wall_ns, 20);
            assert_eq!(timed.research_ns, 10);
            assert_eq!(reads, 3);
        }
    }

    #[test]
    fn a_new_run_uses_current_factor_parameters_not_cached_decisions() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let lake = tempfile::tempdir().unwrap();
        let mut workload = load_workload(
            &root.join("poc/poc0-benchmark/fixtures/dataset-v1.json"),
            &root.join("poc/mvp3-golden/expected/dataset-v1.s2.lot.json"),
            lake.path(),
        );
        let mut tick = 0;
        let mut clock = || {
            tick += 1;
            tick
        };
        let first = timed_run(&workload, &mut clock, |_| ());
        assert_eq!(
            first.portfolios[0].decisions[0].targets,
            [("C.SYNTH".parse().unwrap(), 1.0)].into()
        );
        workload.parameters["strategy_parameters"]["short_momentum_weight"] = json!(-1.0);
        workload.parameters["strategy_parameters"]["long_momentum_weight"] = json!(-1.0);
        let next = timed_run(&workload, &mut clock, |_| ());
        // Negating momentum reverses this independently known three-Instrument ranking.
        assert_eq!(
            next.portfolios[0].decisions[0].targets,
            [("B.SYNTH".parse().unwrap(), 1.0)].into()
        );
    }
}
