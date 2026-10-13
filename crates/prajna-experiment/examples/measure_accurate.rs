#[path = "../../prajna-research/tests/common/mod.rs"]
#[allow(dead_code)]
mod common;

#[path = "../tests/support/exact_execution.rs"]
mod exact_execution;
use exact_execution::exact_projection as projection;

use std::{
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
    fast_event::{
        FastEventConfig, FastEventCosts, PortfolioRun, Rebalance, Sizing, run_fast_event,
    },
    load_execution_status, load_panel,
    strategy::{Decision, UnfilledEntry},
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
    portfolios: Vec<PortfolioRun>,
    expected: Value,
    dsv: String,
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
) -> (u128, prajna_adapter::AdapterResult, Option<Value>, Value) {
    let start = Instant::now();
    let conversion = if end_to_end {
        write_packet(workload, &worker.packet);
        Some(worker.prepare())
    } else {
        None
    };
    let portfolios = workload.portfolios.clone();
    let actual = run_adapter(
        &workload.packet,
        &workload.panel,
        &workload.statuses,
        &workload.config,
        &portfolios,
        worker,
    )
    .unwrap();
    let usage = worker.usage();
    let ns = start.elapsed().as_nanos();
    (ns, actual, conversion, usage)
}

fn measurement(
    workload: &Workload,
    (ns, actual, conversion, usage): (u128, prajna_adapter::AdapterResult, Option<Value>, Value),
) -> Value {
    let result = projection(&actual.execution);
    assert_eq!(
        result, workload.expected,
        "cached/fresh Accurate differs from Fast at scale 18"
    );
    let checksum = format!("{:x}", Sha256::digest(serde_json::to_vec(&result).unwrap()));
    json!({"wall_ns":ns,"backend_conversion":conversion,"usage":usage,"checksum":checksum})
}

fn sample(workload: &Workload, worker: &mut Worker, end_to_end: bool) -> Value {
    measurement(workload, execute(workload, worker, end_to_end))
}
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    assert_eq!(args.len(), 4, "python normalized-fixture golden output");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let python = Path::new(&args[0]);
    let temp = tempfile::tempdir().unwrap();
    let dsv = common::publish_fixture(temp.path(), &std::fs::read(&args[1]).unwrap(), "3");
    let venue = VenueId::new("SYNTH").unwrap();
    let panel = load_panel(temp.path(), &dsv, &venue).unwrap();
    let statuses = load_execution_status(temp.path(), &dsv).unwrap();
    let packet = RunPacket::from_dataset(temp.path(), &dsv).unwrap();
    let golden: Value = serde_json::from_slice(&std::fs::read(&args[2]).unwrap()).unwrap();
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
                ranked: vec![],
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
    let workload = Workload {
        packet,
        panel,
        statuses,
        config,
        portfolios,
        expected,
        dsv,
    };
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
            "main":main_samples,"end_to_end":end_to_end_samples,"parallel":parallel,
            "parity":"exact Fast projection; robustness only, not independent correctness evidence",
        }))
        .unwrap(),
    )
    .unwrap();
}
