use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use quant_research::{
    batch::{self, BatchConfig},
    core::ExperimentConfig,
    data, runner, server,
    signal::{self, SignalResearchRequest},
};
use std::{fs, path::PathBuf};
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
    Snapshot {
        #[arg(long, default_value = "data-core/market.duckdb")]
        warehouse: PathBuf,
        #[arg(long, default_value = "research-output")]
        output: PathBuf,
    },
    /// Run factor research and a next-open momentum backtest.
    Run {
        #[arg(long, default_value = "data-core/market.duckdb")]
        warehouse: PathBuf,
        #[arg(long, default_value = "research-output")]
        output: PathBuf,
        #[arg(long, default_value = "configs/etf_momentum.json")]
        config: PathBuf,
    },
    /// Run a Cartesian grid of ETF strategy parameters in parallel from one snapshot.
    RunGrid {
        #[arg(long, default_value = "data-core/market.duckdb")]
        warehouse: PathBuf,
        #[arg(long, default_value = "research-output")]
        output: PathBuf,
        #[arg(long, default_value = "configs/etf_rotation_grid.json")]
        config: PathBuf,
    },
    /// Evaluate one registered ETF signal independently from a strategy backtest.
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
    },
    /// Serve the local visualization workbench.
    Serve {
        #[arg(long, default_value = "research-output")]
        output: PathBuf,
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
        Command::Snapshot { warehouse, output } => println!(
            "{}",
            serde_json::to_string_pretty(&data::create_etf_snapshot(&warehouse, &output)?)?
        ),
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
            let bars = data::load_bars(&snapshot.file, config.start, config.end)?;
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
        Command::RunGrid {
            warehouse,
            output,
            config,
        } => {
            let config: BatchConfig = serde_json::from_slice(
                &fs::read(&config).with_context(|| format!("read config {}", config.display()))?,
            )?;
            let snapshot = data::create_etf_snapshot(&warehouse, &output)?;
            let bars = data::load_bars(&snapshot.file, config.base.start, config.base.end)?;
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
        Command::SignalResearch {
            warehouse,
            output,
            signal: signal_key,
            forward_periods,
            quantiles,
        } => {
            let snapshot = data::create_etf_snapshot(&warehouse, &output)?;
            let bars = data::load_bars(&snapshot.file, None, None)?;
            let report = signal::analyze(
                &bars,
                &SignalResearchRequest {
                    signal: signal_key,
                    forward_periods,
                    quantiles,
                },
            )?;
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
        Command::Serve {
            output,
            address,
            web_dist,
        } => server::serve(&address, output, web_dist).await?,
    }
    Ok(())
}
