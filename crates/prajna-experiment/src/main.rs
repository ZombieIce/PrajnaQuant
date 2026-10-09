//! CLI adapter for the canonical Experiment execution and storage APIs.

use std::{fs, num::NonZeroUsize, path::PathBuf, process::ExitCode, time::Instant};

use clap::{Parser, Subcommand};
use prajna_experiment::{
    ExecutionIdentity, Experiment, ExperimentError, ResultLevel, RunStatus, diff_executions,
};
use serde_json::json;

const EXIT_CODES: &str = "Exit codes: 0 success; 1 I/O, provenance or execution failure; \
2 CLI, definition, expansion or invalid promotion request; 3 replay inconsistency; \
4 diff contains differences; 5 diff across Experiments. Failed individual Runs are \
recorded in the report and do not change exit code 0.";

#[derive(Parser)]
#[command(about = "Run, replay, promote and compare synthetic Vector experiments", after_help = EXIT_CODES)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Execute an experiment, or promote selected Runs in an existing Execution.
    #[command(after_help = EXIT_CODES)]
    Run {
        experiment: PathBuf,
        #[arg(long)]
        lake: PathBuf,
        /// Default level for new Executions; promotion requires a higher level.
        #[arg(long, default_value = "summary", value_parser = parse_level)]
        level: ResultLevel,
        /// Promote only; never creates a missing Execution. Accepts multiple Run IDs.
        #[arg(long, num_args = 1..)]
        promote: Vec<String>,
        /// Rayon default if omitted; recorded but excluded from identity.
        #[arg(long)]
        threads: Option<NonZeroUsize>,
    },
    /// Compare two Executions from the same Experiment, aligned by Run Spec.
    #[command(after_help = EXIT_CODES)]
    Diff {
        exe_a: String,
        exe_b: String,
        #[arg(long)]
        lake: PathBuf,
    },
}

fn parse_level(value: &str) -> Result<ResultLevel, String> {
    match value {
        "summary" => Ok(ResultLevel::Summary),
        "standard" => Ok(ResultLevel::Standard),
        "full" => Ok(ResultLevel::Full),
        _ => Err("level must be summary, standard or full".into()),
    }
}

fn error_code(error: &ExperimentError) -> u8 {
    match error {
        ExperimentError::ReplayMismatch(_) => 3,
        ExperimentError::DifferentExperiments(_) => 5,
        ExperimentError::Json(_)
        | ExperimentError::Manifest(_)
        | ExperimentError::Universe(_)
        | ExperimentError::InvalidDefinition(_)
        | ExperimentError::Canonical(_) => 2,
        _ => 1,
    }
}

fn run(cli: Cli) -> Result<u8, (u8, String)> {
    match cli.command {
        Commands::Run {
            experiment,
            lake,
            level,
            promote,
            threads,
        } => {
            let started = Instant::now();
            let definition =
                fs::read_to_string(experiment).map_err(|error| (1, error.to_string()))?;
            let experiment = Experiment::parse(&definition, &lake)
                .map_err(|error| (error_code(&error), error.to_string()))?;
            // Expansion errors reject the entire definition before any execution writes.
            experiment
                .expand(&lake)
                .map_err(|error| (error_code(&error), error.to_string()))?;
            let definition_time_ms = started.elapsed().as_millis();
            let repository = std::env::current_dir().map_err(|error| (1, error.to_string()))?;
            let threads = threads
                .map(NonZeroUsize::get)
                .unwrap_or_else(rayon::current_num_threads);
            let stored = if promote.is_empty() {
                experiment.execute_and_store(&lake, &repository, level, threads)
            } else {
                if level == ResultLevel::Summary {
                    return Err((2, "promotion requires --level standard or full".into()));
                }
                let identity = ExecutionIdentity::capture(
                    &experiment
                        .id()
                        .map_err(|error| (error_code(&error), error.to_string()))?,
                    &repository,
                )
                .map_err(|error| (error_code(&error), error.to_string()))?;
                experiment.promote(&lake, &repository, &identity.id, &promote, level, threads)
            }
            .map_err(|error| (error_code(&error), error.to_string()))?;
            let statistics = stored.statistics;
            let output = json!({
                "exp": stored.manifest.experiment_id,
                "exe": stored.manifest.execution.id,
                "reproducible": stored.manifest.execution.reproducible,
                "run_count": stored.manifest.runs.len(),
                "failed_count": stored.manifest.runs.iter().filter(|run| run.status == RunStatus::Failed).count(),
                "action": stored.action,
                "threads": threads,
                "timings_ms": {
                    "definition": definition_time_ms,
                    "factors": statistics.factor_time_ms,
                    "runs": statistics.run_time_ms,
                    "total": started.elapsed().as_millis(),
                },
                "cache": {"compute_count": statistics.compute_count, "hit_count": statistics.cache_hit_count},
                "written_bytes": statistics.written_bytes,
                "written_files": statistics.written_files,
            });
            println!("{output}");
            Ok(0)
        }
        Commands::Diff { exe_a, exe_b, lake } => {
            let diff = diff_executions(lake, &exe_a, &exe_b)
                .map_err(|error| (error_code(&error), error.to_string()))?;
            let changed = diff.summary_changed
                || !diff.only_left.is_empty()
                || !diff.only_right.is_empty()
                || diff.runs.iter().any(|run| {
                    run.status_changed
                        || run.pending_at_end_changed
                        || !run.table_differences.is_empty()
                        || run
                            .metric_deltas
                            .values()
                            .any(|delta| delta.is_some_and(|value| value != 0.0))
                });
            println!(
                "{}",
                serde_json::to_string(&diff).map_err(|error| (1, error.to_string()))?
            );
            Ok(if changed { 4 } else { 0 })
        }
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => ExitCode::from(code),
        Err((code, message)) => {
            eprintln!("{message}");
            ExitCode::from(code)
        }
    }
}
