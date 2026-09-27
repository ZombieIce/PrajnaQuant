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
        } => {
            let report = poc0_benchmark::run(&dataset, &expected, &candidate)?;
            let passed = poc0_benchmark::write_and_exit_status(&report, &output)?;
            println!(
                "POC-0 correctness {}: {}",
                report.correctness_status(),
                output.display()
            );
            if !passed {
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
