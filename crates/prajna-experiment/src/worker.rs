use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver, SyncSender},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use prajna_adapter::{
    AdapterError, Backend, RunPacket,
    protocol::{self, Request, Response, decode_line, encode_line},
};
use prajna_research::fast_event::{FastEventConfig, FastEventCosts};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use rustix::process::{Pid, Signal, WaitId, WaitIdOptions, kill_process_group, waitid};
use serde_json::{Value, json};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::os::unix::process::CommandExt;
use tempfile::TempDir;

use crate::{BackendDiagnostics, BackendFactory, BackendIdentity, ExperimentBackend};

fn invalid(message: impl Into<String>) -> AdapterError {
    AdapterError::Invalid(message.into())
}

/// One new process per Run; paths and transport deadlines do not affect Run identity.
#[derive(Debug, Clone)]
pub struct WorkerCommand {
    pub python: PathBuf,
    pub worker: PathBuf,
    pub timeout: Duration,
}

impl WorkerCommand {
    pub fn new(python: impl Into<PathBuf>, worker: impl Into<PathBuf>) -> Self {
        Self {
            python: python.into(),
            worker: worker.into(),
            timeout: Duration::from_secs(60),
        }
    }

    fn command(&self) -> Result<Command, AdapterError> {
        if self.timeout.is_zero() {
            return Err(invalid("worker timeout must be positive"));
        }
        let worker = self.worker.canonicalize()?;
        if !worker.is_file() {
            return Err(invalid("worker path must be a file"));
        }
        let python = if self.python.components().count() > 1 {
            // Resolving the interpreter symlink would escape its virtual environment.
            std::path::absolute(&self.python)?
        } else {
            self.python.clone()
        };
        let mut command = Command::new(python);
        // Keep the explicitly selected venv, but ignore host Python path/home/startup
        // overrides and user-site packages. The worker's own sibling imports remain valid.
        command.args(["-E", "-s"]);
        command.current_dir(
            worker
                .parent()
                .ok_or_else(|| invalid("worker has no parent"))?,
        );
        Ok(command)
    }
}

impl BackendFactory for WorkerCommand {
    fn validate(&self, identity: &BackendIdentity) -> Result<(), AdapterError> {
        if identity.name != "nautilus_trader" {
            return Err(invalid(
                "Python worker only supports Backend name nautilus_trader",
            ));
        }
        let mut command = self.command()?;
        command.args([
            "-c",
            "import importlib.metadata; print(importlib.metadata.version('nautilus_trader'), flush=True)",
        ]);
        let mut transport = Transport::spawn(&mut command, self.timeout)?;
        let version = transport.read_line()?;
        transport.wait_success()?;
        if version.trim() != identity.version {
            return Err(invalid(format!(
                "Backend version mismatch: Run Spec requires {}, Python has {}",
                identity.version,
                version.trim()
            )));
        }
        Ok(())
    }

    fn create(
        &self,
        _identity: &BackendIdentity,
        config: &FastEventConfig,
        dsv: &str,
    ) -> Result<Box<dyn ExperimentBackend>, AdapterError> {
        Ok(Box::new(ProcessBackend {
            command: self.clone(),
            directory: tempfile::tempdir()?,
            dsv: dsv.into(),
            initial_cash: config.initial_cash.to_string(),
            transport: None,
        }))
    }
}

struct Transport {
    child: Child,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    process_group: Pid,
    input: Option<SyncSender<WriteRequest>>,
    writer: Option<JoinHandle<()>>,
    output: Option<Receiver<Result<Option<String>, String>>>,
    reader: Option<JoinHandle<()>>,
    timeout: Duration,
}

struct WriteRequest {
    line: String,
    acknowledgement: SyncSender<Result<(), String>>,
}

impl Transport {
    fn spawn(command: &mut Command, timeout: Duration) -> Result<Self, AdapterError> {
        if !cfg!(any(target_os = "macos", target_os = "linux")) {
            return Err(invalid(
                "worker process isolation requires Unix (macOS/Linux)",
            ));
        }
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        command.process_group(0);
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        let process_group = Pid::from_child(&child);
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| invalid("worker stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| invalid("worker stdout unavailable"))?;
        let (sender, output) = mpsc::sync_channel(1);
        let (input, writes) = mpsc::sync_channel::<WriteRequest>(1);
        let writer = thread::spawn(move || {
            for request in writes {
                let result = stdin
                    .write_all(request.line.as_bytes())
                    .and_then(|()| stdin.flush())
                    .map_err(|error| format!("worker stdin: {error}"));
                let terminal = result.is_err();
                if request.acknowledgement.send(result).is_err() || terminal {
                    break;
                }
            }
        });
        let reader = thread::spawn(move || {
            let mut stdout = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                let result = match stdout.read_line(&mut line) {
                    Ok(0) => Ok(None),
                    Ok(_) => Ok(Some(line)),
                    Err(error) => Err(format!("worker stdout: {error}")),
                };
                let terminal = !matches!(&result, Ok(Some(_)));
                if sender.send(result).is_err() || terminal {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            process_group,
            input: Some(input),
            writer: Some(writer),
            output: Some(output),
            reader: Some(reader),
            timeout,
        })
    }

    fn read_line(&mut self) -> Result<String, AdapterError> {
        self.output
            .as_ref()
            .ok_or_else(|| invalid("worker stdout unavailable"))?
            .recv_timeout(self.timeout)
            .map_err(|error| invalid(format!("worker response timeout/disconnect: {error}")))?
            .map_err(invalid)?
            .ok_or_else(|| invalid("worker stdout reached EOF"))
    }

    fn exchange(&mut self, request: &str) -> Result<String, AdapterError> {
        let input = self
            .input
            .as_ref()
            .ok_or_else(|| invalid("worker stdin closed"))?;
        let (acknowledgement, result) = mpsc::sync_channel(1);
        input
            .send(WriteRequest {
                line: request.into(),
                acknowledgement,
            })
            .map_err(|error| invalid(format!("worker writer disconnected: {error}")))?;
        result
            .recv_timeout(self.timeout)
            .map_err(|error| invalid(format!("worker write timeout/disconnect: {error}")))?
            .map_err(invalid)?;
        let line = self.read_line()?;
        let value: Value = serde_json::from_str(&line)?;
        if value["type"] == "error" {
            return Err(invalid(format!(
                "worker terminal error: {}",
                value["reason"]
            )));
        }
        Ok(line)
    }

    fn wait_success(&mut self) -> Result<(), AdapterError> {
        self.input.take();
        let deadline = Instant::now() + self.timeout;
        loop {
            if let Some(success) = self.observe_exit()? {
                if !success {
                    return Err(invalid("worker exited with nonzero status or signal"));
                }
                let trailing = self
                    .output
                    .as_ref()
                    .ok_or_else(|| invalid("worker stdout unavailable"))?
                    .recv_timeout(self.timeout)
                    .map_err(|error| invalid(format!("worker stdout completion: {error}")))?
                    .map_err(invalid)?;
                return if let Some(line) = trailing {
                    Err(invalid(format!(
                        "unexpected worker output after completion: {line}"
                    )))
                } else {
                    Ok(())
                };
            }
            if Instant::now() >= deadline {
                return Err(invalid("worker exit timeout"));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn observe_exit(&self) -> Result<Option<bool>, AdapterError> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            // Keep the leader unreaped, so its group ID cannot be reused before cleanup.
            let status = waitid(
                WaitId::Pid(self.process_group),
                WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
            )
            .map_err(|error| AdapterError::Io(error.into()))?;
            Ok(status.map(|status| status.exit_status() == Some(0)))
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            Err(invalid("worker process isolation requires macOS/Linux"))
        }
    }
}

impl Drop for Transport {
    fn drop(&mut self) {
        self.input.take();
        self.output.take();
        let deadline = Instant::now() + self.timeout;
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if !self.process_group.is_init() {
            // A new group contains only this worker and its inherited descendants.
            if let Err(error) = kill_process_group(self.process_group, Signal::KILL) {
                if error != rustix::io::Errno::SRCH {
                    eprintln!("worker process-group cleanup failed: {error}");
                }
            }
        }
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Err(error) => {
                    eprintln!("worker cleanup status failed: {error}");
                    break;
                }
                Ok(None) => {}
            }
            if Instant::now() >= deadline {
                if let Err(error) = self.child.kill() {
                    eprintln!("worker cleanup kill failed: {error}");
                }
                eprintln!("worker could not be reaped within cleanup deadline");
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        for (handle, name) in [
            (self.reader.take(), "stdout reader"),
            (self.writer.take(), "stdin writer"),
        ] {
            if let Some(handle) = handle {
                while !handle.is_finished() && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(10));
                }
                if !handle.is_finished() {
                    eprintln!("worker {name} did not stop within cleanup deadline");
                } else if handle.join().is_err() {
                    eprintln!("worker {name} panicked during cleanup");
                }
            }
        }
    }
}

struct ProcessBackend {
    command: WorkerCommand,
    directory: TempDir,
    dsv: String,
    initial_cash: String,
    transport: Option<Transport>,
}

impl Backend for ProcessBackend {
    fn begin_run(&mut self, packet: &RunPacket, costs: FastEventCosts) -> Result<(), AdapterError> {
        if self.transport.is_some() {
            return Err(invalid("worker Run already initialized"));
        }
        let path = self.directory.path().join("packet");
        packet.write(&path, &self.dsv)?;
        fs::write(
            path.join("worker.json"),
            serde_json::to_vec(&json!({
                "initial_cash":self.initial_cash,
                "costs":{
                    "commission_rate":costs.commission_rate.to_string(),
                    "minimum_commission":costs.minimum_commission.to_string(),
                    "buy_tax_rate":costs.buy_tax_rate.to_string(),
                    "sell_tax_rate":costs.sell_tax_rate.to_string(),
                    "buy_slippage_rate":costs.buy_slippage_rate.to_string(),
                    "sell_slippage_rate":costs.sell_slippage_rate.to_string()
                }
            }))?,
        )?;
        let mut command = self.command.command()?;
        command.arg(self.command.worker.canonicalize()?);
        let mut transport = Transport::spawn(&mut command, self.command.timeout)?;
        let response: Response =
            decode_line(&transport.exchange(&encode_line(&Request::BeginRun {
                protocol: protocol::VERSION.into(),
                packet: path.to_string_lossy().into_owned(),
            })?)?)?;
        if response
            != (Response::Ready {
                protocol: protocol::VERSION.into(),
            })
        {
            return Err(invalid("worker did not acknowledge Run initialization"));
        }
        self.transport = Some(transport);
        Ok(())
    }

    fn exchange(&mut self, request: &str) -> Result<String, AdapterError> {
        self.transport
            .as_mut()
            .ok_or_else(|| invalid("worker Run not initialized"))?
            .exchange(request)
    }
}

impl ExperimentBackend for ProcessBackend {
    fn finish(mut self: Box<Self>) -> Result<BackendDiagnostics, AdapterError> {
        self.transport
            .as_mut()
            .ok_or_else(|| invalid("worker Run not initialized"))?
            .wait_success()?;
        let file = fs::File::open(
            self.directory
                .path()
                .join("packet/backend-diagnostics.jsonl"),
        )?;
        let mut diagnostics = BackendDiagnostics {
            evidence: "nautilus_trader".into(),
            adapter_constructions: vec![],
            native_events: vec![],
            comparisons: vec![],
        };
        for line in BufReader::new(file).lines() {
            let value: Value = serde_json::from_str(&line?)?;
            if value["event"].as_str().is_none_or(str::is_empty) {
                return Err(invalid("Backend diagnostic requires an event"));
            }
            match value["category"].as_str() {
                Some("adapter_construction") => diagnostics.adapter_constructions.push(value),
                Some("backend_native") => diagnostics.native_events.push(value),
                Some("backend_comparison") => diagnostics.comparisons.push(value),
                Some("adapter_error") => {
                    return Err(invalid(format!("worker diagnostic failure: {value}")));
                }
                _ => return Err(invalid("unmapped Backend diagnostic category")),
            }
        }
        Ok(diagnostics)
    }
}
