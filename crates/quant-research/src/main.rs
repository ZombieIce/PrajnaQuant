#[cfg(feature = "app")]
use anyhow::Context;
use anyhow::Result;
use clap::{Parser, Subcommand};
use quant_research::poc0_benchmark;
#[cfg(feature = "app")]
use quant_research::{
    batch::{self, BatchConfig},
    core::{ExperimentConfig, ForwardReturnMethod},
    data, runner, server,
    signal::{self, SignalResearchRequest},
};
#[cfg(feature = "app")]
use std::fs;
use std::path::PathBuf;
#[cfg(feature = "app")]
use uuid::Uuid;

#[derive(Parser)]
#[command(about = "Reproducible A-share factor research and daily backtesting")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Create an immutable ETF research snapshot from the warehouse.
    #[cfg(feature = "app")]
    Snapshot {
        #[arg(long, default_value = "data-core/market.duckdb")]
        warehouse: PathBuf,
        #[arg(long, default_value = "research-output")]
        output: PathBuf,
    },
    /// Run factor research and a next-open momentum backtest.
    #[cfg(feature = "app")]
    Run {
        #[arg(long, default_value = "data-core/market.duckdb")]
        warehouse: PathBuf,
        #[arg(long, default_value = "research-output")]
        output: PathBuf,
        #[arg(long, default_value = "configs/etf_momentum.json")]
        config: PathBuf,
    },
    /// Run a Cartesian grid of ETF strategy parameters in parallel from one snapshot.
    #[cfg(feature = "app")]
    RunGrid {
        #[arg(long, default_value = "data-core/market.duckdb")]
        warehouse: PathBuf,
        #[arg(long, default_value = "research-output")]
        output: PathBuf,
        #[arg(long, default_value = "configs/etf_rotation_grid.json")]
        config: PathBuf,
    },
    /// Evaluate one registered ETF signal independently from a strategy backtest.
    #[cfg(feature = "app")]
    SignalResearch {
        #[arg(long, default_value = "data-core/market.duckdb")]
        warehouse: PathBuf,
        #[arg(long, default_value = "research-output")]
        output: PathBuf,
        #[arg(long, default_value = "momentum_60")]
        signal: String,
        /// Comma-separated close-to-close holding periods, for example 1,5,10,20,60.
        #[arg(long, value_delimiter = ',', default_values_t = [1usize, 5, 10, 20, 60])]
        forward_periods: Vec<usize>,
        #[arg(long, default_value_t = 5)]
        quantiles: usize,
        #[arg(long, default_value = "next_open_to_forward_open", value_parser = ["close_to_close", "next_open_to_forward_open"])]
        label_method: String,
        #[arg(long)]
        universe_id: Option<Uuid>,
        #[arg(long)]
        version_id: Option<Uuid>,
        #[arg(long, default_value_t = false)]
        strict_pit: bool,
    },
    /// Run the fixed POC-0 dataset through the shared correctness-first benchmark harness.
    BenchmarkPoc0 {
        #[arg(long, default_value = "poc/poc0-benchmark/fixtures/dataset-v1.json")]
        dataset: PathBuf,
        #[arg(long, default_value = "poc/poc0-benchmark/fixtures/expected-v1.json")]
        expected: PathBuf,
        #[arg(long, default_value = "target/poc-0/benchmark-report.json")]
        output: PathBuf,
        #[arg(long, value_parser = ["reference", "soa", "arrow", "polars"], default_value = "reference")]
        candidate: String,
        /// Compare the Rust bar/open reference with Nautilus synthetic L1 quote fills.
        #[arg(long, value_parser = ["rust", "nautilus"], default_value = "rust")]
        backend: String,
        /// Python executable used by the optional Nautilus adapter.
        #[arg(long, default_value = ".venv/bin/python")]
        python: PathBuf,
    },
    /// Measure isolated, correctness-gated B2 strategy Runs with a fixed worker count.
    BenchmarkPoc0B2 {
        #[arg(long, value_parser = ["s2", "s3"])]
        strategy: String,
        /// Optional canonical source fixture identity (S2 bars or S3 MA20/60 parameters).
        #[arg(long)]
        dataset: Option<PathBuf>,
        /// Registered workload identity for reports and fail-closed fixture validation.
        #[arg(long)]
        dataset_version: Option<String>,
        #[arg(long, value_parser = ["serial", "parallel"], default_value = "parallel")]
        mode: String,
        #[arg(long, alias = "threads", default_value_t = 2)]
        workers: usize,
        #[arg(long, default_value_t = 6)]
        runs: usize,
        /// Skip in-process golden evaluation after a separate preflight process passed.
        #[arg(long)]
        skip_golden_preflight: bool,
        /// Expected projection checksum emitted by the separate golden preflight.
        #[arg(long, requires = "skip_golden_preflight")]
        expected_checksum: Option<String>,
        /// Include the verified ledger projection in this preflight report.
        #[arg(long)]
        include_projection: bool,
        #[arg(long, default_value = "target/poc-0/b2-throughput.json")]
        output: PathBuf,
    },
    /// Compare Rust Native and PyO3 Python per-bar callbacks on the fixed S1 event stream.
    #[cfg(feature = "b3-pyo3")]
    BenchmarkPoc0B3 {
        #[arg(long, default_value = "poc/poc0-benchmark/fixtures/dataset-v1.json")]
        dataset: PathBuf,
        #[arg(long, default_value = "poc/poc0-benchmark/fixtures/expected-v1.json")]
        expected: PathBuf,
        #[arg(long, default_value = "target/poc-0/b3-pyo3-callbacks.json")]
        output: PathBuf,
    },
    /// Compare Rust Native and PyO3 implementations of B3 S2/S3 and run-boundary choices.
    #[cfg(feature = "b3-pyo3")]
    BenchmarkPoc0B3Strategies {
        #[arg(long, default_value = "target/poc-0/b3-strategies-parallel.json")]
        output: PathBuf,
    },
    /// Generate a versioned Parquet fixture and benchmark the three B1 paths on one scan.
    BenchmarkPoc0Parquet {
        #[arg(long, default_value = "poc/poc0-benchmark/fixtures/dataset-v1.json")]
        dataset: PathBuf,
        #[arg(long, default_value = "poc/poc0-benchmark/fixtures/expected-v1.json")]
        expected: PathBuf,
        #[arg(long, default_value = "target/poc-0/dataset-v1.parquet")]
        parquet: PathBuf,
        #[arg(long, default_value = "target/poc-0/parquet-report.json")]
        output: PathBuf,
        #[arg(long)]
        symbol: Vec<String>,
        #[arg(long)]
        start: Option<chrono::NaiveDate>,
        #[arg(long)]
        end: Option<chrono::NaiveDate>,
        #[arg(long, default_value_t = false)]
        reuse: bool,
        /// Generate deterministic extra rows in bounded row groups.
        #[arg(long, default_value_t = 0)]
        filler_rows: usize,
    },
    /// Run the fixed B1 S2 parameter sweep across layouts and factor-cache states.
    BenchmarkPoc0Sweep {
        #[arg(long, default_value = "poc/poc0-benchmark/fixtures/dataset-v1.json")]
        dataset: PathBuf,
        #[arg(long, default_value = "poc/poc0-benchmark/fixtures/expected-v1.json")]
        expected: PathBuf,
        #[arg(long, default_value = "poc/poc0-benchmark/results/b1-sweep.json")]
        output: PathBuf,
        #[arg(long, default_value_t = 64)]
        instruments: usize,
        #[arg(long, default_value_t = 252)]
        sessions: usize,
    },
    /// Serve the local visualization workbench.
    #[cfg(feature = "app")]
    Serve {
        #[arg(long, default_value = "research-output")]
        output: PathBuf,
        /// Directory containing the warehouse's immutable published daily snapshots.
        #[arg(long, default_value = "data-core")]
        market_data_dir: PathBuf,
        #[arg(long, default_value = "127.0.0.1:7878")]
        address: String,
        #[arg(long, default_value = "apps/web/dist")]
        web_dist: PathBuf,
    },
}
#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        #[cfg(feature = "app")]
        Command::Snapshot { warehouse, output } => println!(
            "{}",
            serde_json::to_string_pretty(&data::create_etf_snapshot(&warehouse, &output)?)?
        ),
        #[cfg(feature = "app")]
        Command::Run {
            warehouse,
            output,
            config,
        } => {
            let config: ExperimentConfig = serde_json::from_slice(
                &fs::read(&config).with_context(|| format!("read config {}", config.display()))?,
            )?;
            config.validate()?;
            let snapshot = data::create_etf_snapshot(&warehouse, &output)?;
            // Keep pre-start warmup and post-end label windows available; the runner
            // limits only the actual backtest event loop to the configured range.
            let bars = data::load_bars(&snapshot.file, None, None)?;
            let benchmark_path = snapshot
                .benchmark_file
                .as_ref()
                .context("snapshot has no benchmark file")?;
            let benchmark_bars = data::load_bars(benchmark_path, config.start, config.end)?;
            let result = runner::run_experiment(config, snapshot, &bars, &benchmark_bars)?;
            let path = result.save(&output)?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({"experiment_id":result.experiment_id,"result":path,"metrics":result.backtest.metrics,"factor":{"mean_rank_ic":result.factor.mean_rank_ic,"rank_ic_ir":result.factor.rank_ic_ir,"evaluated_dates":result.factor.evaluated_dates}})
                )?
            );
        }
        #[cfg(feature = "app")]
        Command::RunGrid {
            warehouse,
            output,
            config,
        } => {
            let config: BatchConfig = serde_json::from_slice(
                &fs::read(&config).with_context(|| format!("read config {}", config.display()))?,
            )?;
            let snapshot = data::create_etf_snapshot(&warehouse, &output)?;
            let bars = data::load_bars(&snapshot.file, None, None)?;
            let benchmark_path = snapshot
                .benchmark_file
                .as_ref()
                .context("snapshot has no benchmark file")?;
            let benchmark_bars =
                data::load_bars(benchmark_path, config.base.start, config.base.end)?;
            let (batch, path) =
                batch::run_batch(&output, &config, &snapshot, &bars, &benchmark_bars)?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({"batch_id":batch.batch_id,"result":path,"experiment_count":batch.experiment_count,"experiments":batch.experiments})
                )?
            );
        }
        #[cfg(feature = "app")]
        Command::SignalResearch {
            warehouse,
            output,
            signal: signal_key,
            forward_periods,
            quantiles,
            label_method,
            universe_id,
            version_id,
            strict_pit,
        } => {
            let snapshot = data::create_etf_snapshot(&warehouse, &output)?;
            let bars = data::load_bars(&snapshot.file, None, None)?;
            anyhow::ensure!(
                universe_id.is_some() == version_id.is_some(),
                "--universe-id and --version-id must be specified together"
            );
            let label_method = match label_method.as_str() {
                "close_to_close" => ForwardReturnMethod::CloseToClose,
                "next_open_to_forward_open" => ForwardReturnMethod::NextOpenToForwardOpen,
                _ => anyhow::bail!("unsupported label method"),
            };
            let request = SignalResearchRequest {
                signal: signal_key,
                forward_periods,
                quantiles,
                label_method,
                universe_id,
                version_id,
                strict_pit,
            };
            let mut expected_by_date =
                std::collections::BTreeMap::<chrono::NaiveDate, usize>::new();
            for bar in &bars {
                *expected_by_date.entry(bar.trade_date).or_default() += 1;
            }
            let calendar = data::load_snapshot_trading_calendar(&snapshot)?
                .unwrap_or_else(|| quant_research::factor::observed_market_calendar(&bars));
            let calendar_basis = snapshot
                .trading_calendar_source
                .as_deref()
                .unwrap_or("observed_etf_bar_dates");
            let report = if let (Some(universe_id), Some(version_id)) = (universe_id, version_id) {
                let definition = server::load_universe_version(&output, universe_id, version_id)?;
                signal::analyze_with_universe(
                    &bars,
                    &request,
                    &definition,
                    strict_pit,
                    &snapshot.sha256,
                    &calendar,
                    calendar_basis,
                )?
            } else {
                anyhow::ensure!(
                    !strict_pit,
                    "--strict-pit requires a published Universe version"
                );
                signal::analyze_with_evaluation_calendar(
                    &bars,
                    &request,
                    &expected_by_date,
                    &calendar,
                    calendar_basis,
                )?
            };
            let report_id = Uuid::new_v4();
            let directory = output.join("signal-reports").join(report_id.to_string());
            fs::create_dir_all(&directory)?;
            let path = directory.join("report.json");
            fs::write(
                &path,
                serde_json::to_vec_pretty(&serde_json::json!({
                    "report_id": report_id,
                    "snapshot": snapshot,
                    "report": report,
                }))?,
            )?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "report_id": report_id,
                    "result": path,
                }))?
            );
        }
        Command::BenchmarkPoc0 {
            dataset,
            expected,
            output,
            candidate,
            backend,
            python,
        } => {
            let report = poc0_benchmark::run(&dataset, &expected, &candidate)?;
            let passed = poc0_benchmark::write_and_exit_status(&report, &output)?;
            if !passed {
                println!(
                    "POC-0 correctness {}: {}",
                    report.correctness_status(),
                    output.display()
                );
                std::process::exit(2);
            }
            if backend == "nautilus" {
                let adapter_status =
                    poc0_benchmark::run_nautilus_adapter(&dataset, &output, &python)?;
                println!(
                    "POC-0 Rust correctness {}; Nautilus comparison {}: {}",
                    report.correctness_status(),
                    adapter_status,
                    output.display()
                );
            } else {
                println!(
                    "POC-0 correctness {}; backend {}: {}",
                    report.correctness_status(),
                    backend,
                    output.display()
                );
            }
        }
        Command::BenchmarkPoc0B2 {
            strategy,
            dataset,
            dataset_version,
            mode,
            workers,
            runs,
            skip_golden_preflight,
            expected_checksum,
            include_projection,
            output,
        } => {
            let workers = if mode == "serial" { 1 } else { workers };
            let report = poc0_benchmark::measure_b2_parallel(poc0_benchmark::B2MeasureOptions {
                strategy: &strategy,
                dataset_override: dataset.as_deref(),
                dataset_version: dataset_version.as_deref(),
                threads: workers,
                runs,
                skip_golden_preflight,
                expected_checksum: expected_checksum.as_deref(),
                include_projection,
            })?;
            if let Some(parent) = output.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
            println!("POC-0 B2 {}: {}", report["status"], output.display());
        }
        #[cfg(feature = "b3-pyo3")]
        Command::BenchmarkPoc0B3 {
            dataset,
            expected,
            output,
        } => {
            let report = poc0_benchmark::run_b3(&dataset, &expected)?;
            if let Some(parent) = output.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
            println!("POC-0 B3 {}: {}", report["status"], output.display());
            if report["status"] != "correctness_passed_and_measured" {
                std::process::exit(2);
            }
        }
        #[cfg(feature = "b3-pyo3")]
        Command::BenchmarkPoc0B3Strategies { output } => {
            let report = poc0_benchmark::run_b3_strategies()?;
            if let Some(parent) = output.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
            println!(
                "POC-0 B3 strategies {}: {}",
                report["status"],
                output.display()
            );
            if report["status"] != "correctness_passed_and_measured" {
                std::process::exit(2);
            }
        }
        Command::BenchmarkPoc0Sweep {
            dataset,
            expected,
            output,
            instruments,
            sessions,
        } => {
            let report = poc0_benchmark::run_sweep(
                &dataset,
                &expected,
                poc0_benchmark::SweepOptions {
                    instruments,
                    sessions,
                },
            )?;
            if let Some(parent) = output.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
            println!("POC-0 B1 sweep {}: {}", report["status"], output.display());
            if report["status"] != "passed" {
                std::process::exit(2);
            }
        }
        Command::BenchmarkPoc0Parquet {
            dataset,
            expected,
            parquet,
            output,
            symbol,
            start,
            end,
            reuse,
            filler_rows,
        } => {
            let report = poc0_benchmark::run_parquet(
                &dataset,
                &expected,
                &parquet,
                poc0_benchmark::ParquetOptions {
                    symbols: &symbol,
                    start,
                    end,
                    reuse,
                    filler_rows,
                },
            )?;
            if let Some(parent) = output.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
            println!(
                "POC-0 Parquet correctness {}: {}",
                report["status"],
                output.display()
            );
            if report["status"] != "passed" {
                std::process::exit(2);
            }
        }
        #[cfg(feature = "app")]
        Command::Serve {
            output,
            market_data_dir,
            address,
            web_dist,
        } => server::serve(&address, output, market_data_dir, web_dist).await?,
    }
    Ok(())
}
