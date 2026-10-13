use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

use prajna_adapter::{
    AdapterError, Backend, FakeBackend, RunPacket,
    protocol::{Request, Response, VERSION, decode_line, encode_line},
};
use prajna_research::fast_event::{FastEventConfig, FastEventCosts};

pub fn backend(config: &FastEventConfig) -> Box<dyn Backend> {
    match std::env::var_os("PRAJNA_NAUTILUS_PYTHON") {
        None => Box::new(FakeBackend::default()),
        Some(python) => Box::new(Worker {
            python,
            cash: config.initial_cash.to_string(),
            directory: tempfile::tempdir().unwrap(),
            child: None,
            input: None,
            output: None,
        }),
    }
}

struct Worker {
    python: std::ffi::OsString,
    cash: String,
    directory: tempfile::TempDir,
    child: Option<Child>,
    input: Option<ChildStdin>,
    output: Option<BufReader<ChildStdout>>,
}

impl Backend for Worker {
    fn begin_run(&mut self, packet: &RunPacket, costs: FastEventCosts) -> Result<(), AdapterError> {
        let path = self.directory.path().join("packet");
        packet.write(&path, &format!("dsv:sha256:{}", "0".repeat(64)))?;
        std::fs::write(
            path.join("worker.json"),
            serde_json::to_vec(&serde_json::json!({
                "initial_cash": self.cash,
                "costs": {
                    "commission_rate": costs.commission_rate.to_string(),
                    "minimum_commission": costs.minimum_commission.to_string(),
                    "buy_tax_rate": costs.buy_tax_rate.to_string(),
                    "sell_tax_rate": costs.sell_tax_rate.to_string(),
                    "buy_slippage_rate": costs.buy_slippage_rate.to_string(),
                    "sell_slippage_rate": costs.sell_slippage_rate.to_string(),
                }
            }))?,
        )?;
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut child = Command::new(&self.python)
            .arg(root.join("backends/nautilus/worker.py"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        self.input = child.stdin.take();
        self.output = child.stdout.take().map(BufReader::new);
        self.child = Some(child);
        let response: Response =
            decode_line(&self.exchange(&encode_line(&Request::BeginRun {
                protocol: VERSION.into(),
                packet: path.to_str().unwrap().into(),
            })?)?)?;
        assert_eq!(
            response,
            Response::Ready {
                protocol: VERSION.into()
            }
        );
        Ok(())
    }

    fn exchange(&mut self, request: &str) -> Result<String, AdapterError> {
        self.input.as_mut().unwrap().write_all(request.as_bytes())?;
        self.input.as_mut().unwrap().flush()?;
        let mut response = String::new();
        self.output.as_mut().unwrap().read_line(&mut response)?;
        if matches!(decode_line::<Response>(&response), Ok(Response::Done)) {
            assert!(self.child.as_mut().unwrap().wait()?.success());
            let diagnostics = std::fs::read_to_string(
                self.directory
                    .path()
                    .join("packet/backend-diagnostics.jsonl"),
            )?;
            let events = diagnostics
                .lines()
                .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                .collect::<Vec<_>>();
            assert!(
                events
                    .iter()
                    .any(|e| e["category"] == "adapter_construction")
            );
            assert!(events.iter().any(|e| e["event"] == "account_balance"));
        }
        Ok(response)
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            if child.try_wait().ok().flatten().is_none() {
                child.kill().expect("terminate test worker");
                child.wait().expect("reap test worker");
            }
        }
    }
}
