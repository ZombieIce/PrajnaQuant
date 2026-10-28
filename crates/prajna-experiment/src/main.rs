//! CLI adapter for the canonical Experiment execution and storage APIs.

use std::{fs, num::NonZeroUsize, path::PathBuf, process::ExitCode, sync::Arc, time::Instant};

use clap::{Parser, Subcommand};
use prajna_data::RawStore;
use prajna_experiment::{
    ExecutionIdentity, Experiment, ExperimentError, ResultLevel, RunStatus, StoredExecution,
    WorkerCommand, diff_executions, opening_evidence,
    pit::{PitContext, PitInput},
    promote_below_level, read_persisted_result,
};
use prajna_research::{
    mvp6_coverage::format_percent,
    pit_universe::{CatalogInput, HistoricalCatalog},
};
use serde_json::json;

const EXIT_CODES: &str = "Exit codes: 0 success; 1 I/O, provenance, execution, result read or diff identity failure; \
2 CLI, definition, expansion, result identity or invalid promotion request; 3 replay inconsistency; \
4 diff contains differences; 5 diff across Experiments. Failed individual Runs are \
recorded in the report and do not change exit code 0.";

#[derive(Parser)]
#[command(about = "Run, replay, promote and compare synthetic Vector, Fast Event and Accurate Event experiments", after_help = EXIT_CODES)]
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
        /// Rayon threads, or Accurate worker-process slots; excluded from identity.
        #[arg(long)]
        threads: Option<NonZeroUsize>,
        /// Python executable for Accurate Event; no implicit/fake fallback.
        #[arg(long, requires = "backend_worker")]
        backend_python: Option<PathBuf>,
        /// #139 stdio worker script; runtime path is excluded from Run identity.
        #[arg(long, requires = "backend_python")]
        backend_worker: Option<PathBuf>,
    },
    /// Promote every Run of the current Execution below --level to that level.
    #[command(after_help = EXIT_CODES)]
    Promote {
        experiment: PathBuf,
        #[arg(long)]
        lake: PathBuf,
        #[arg(long, value_parser = parse_level)]
        level: ResultLevel,
        /// Rayon threads, or Accurate worker-process slots; excluded from identity.
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
    /// Read a persisted Execution result without executing or promoting it.
    #[command(after_help = EXIT_CODES)]
    Result {
        exp: String,
        exe: String,
        #[arg(long)]
        lake: PathBuf,
        /// Restrict the envelope to one Run ID.
        #[arg(long)]
        run: Option<String>,
    },
    /// Generate local @3 opening evidence from a frozen Dataset and Catalog,
    /// then validate it through the canonical PitContext path.
    Mvp6OpeningEvidence {
        input: PathBuf,
        #[arg(long)]
        lake: PathBuf,
        #[arg(long)]
        dsv: String,
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

fn is_identity(value: &str, prefix: &str) -> bool {
    value
        .strip_prefix(prefix)
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
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

fn stored_output(
    stored: StoredExecution,
    threads: usize,
    started: Instant,
    definition_time_ms: u128,
) -> serde_json::Value {
    let statistics = stored.statistics;
    json!({
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
    })
}

fn run(cli: Cli) -> Result<u8, (u8, String)> {
    match cli.command {
        Commands::Run {
            experiment,
            lake,
            level,
            promote,
            threads,
            backend_python,
            backend_worker,
        } => {
            let started = Instant::now();
            let definition =
                fs::read_to_string(experiment).map_err(|error| (1, error.to_string()))?;
            let mut experiment = Experiment::parse(&definition, &lake)
                .map_err(|error| (error_code(&error), error.to_string()))?;
            if let (Some(python), Some(worker)) = (backend_python, backend_worker) {
                experiment = experiment
                    .with_backend_factory(Arc::new(WorkerCommand::new(python, worker)))
                    .map_err(|error| (error_code(&error), error.to_string()))?;
            }
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
            println!(
                "{}",
                stored_output(stored, threads, started, definition_time_ms)
            );
            Ok(0)
        }
        Commands::Promote {
            experiment,
            lake,
            level,
            threads,
        } => {
            if level == ResultLevel::Summary {
                return Err((2, "promotion requires --level standard or full".into()));
            }
            let started = Instant::now();
            let definition =
                fs::read_to_string(experiment).map_err(|error| (1, error.to_string()))?;
            let experiment = Experiment::parse(&definition, &lake)
                .map_err(|error| (error_code(&error), error.to_string()))?;
            experiment
                .expand(&lake)
                .map_err(|error| (error_code(&error), error.to_string()))?;
            let definition_time_ms = started.elapsed().as_millis();
            let repository = std::env::current_dir().map_err(|error| (1, error.to_string()))?;
            let threads = threads
                .map(NonZeroUsize::get)
                .unwrap_or_else(rayon::current_num_threads);
            let stored = promote_below_level(&experiment, &lake, &repository, level, threads)
                .map_err(|error| (error_code(&error), error.to_string()))?;
            println!(
                "{}",
                stored_output(stored, threads, started, definition_time_ms)
            );
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
        Commands::Result {
            exp,
            exe,
            lake,
            run,
        } => {
            for (identity, prefix) in [(&exp, "exp:sha256:"), (&exe, "exe:sha256:")] {
                if !is_identity(identity, prefix) {
                    return Err((2, format!("expected {prefix} identity, got {identity}")));
                }
            }
            if let Some(run_id) = &run {
                if !is_identity(run_id, "run:sha256:") {
                    return Err((2, format!("expected run:sha256: identity, got {run_id}")));
                }
            }
            let result = read_persisted_result(&lake, &exp, &exe, run.as_deref())
                .map_err(|error| (1, error.to_string()))?;
            println!(
                "{}",
                serde_json::to_string(&result).map_err(|error| (1, error.to_string()))?
            );
            Ok(0)
        }
        Commands::Mvp6OpeningEvidence { input, lake, dsv } => {
            let mut definition: serde_json::Value =
                serde_json::from_slice(&fs::read(&input).map_err(|error| (1, error.to_string()))?)
                    .map_err(|error| (2, error.to_string()))?;
            let catalog_input: CatalogInput = serde_json::from_value(
                definition
                    .get("catalog")
                    .cloned()
                    .ok_or_else(|| (2, "input has no catalog".into()))?,
            )
            .map_err(|error| (2, error.to_string()))?;
            let raw = RawStore::open(&lake).map_err(|error| (1, error.to_string()))?;
            let catalog = HistoricalCatalog::build(catalog_input, &raw)
                .map_err(|error| (2, error.to_string()))?;
            let evidence =
                opening_evidence::generate(&lake, &dsv, &catalog).map_err(|error| (1, error))?;
            let evidence_bytes =
                opening_evidence::serialize(&evidence).map_err(|error| (1, error))?;
            let acceptance_dir = lake.join("acceptance");
            fs::create_dir_all(&acceptance_dir).map_err(|error| (1, error.to_string()))?;
            let evidence_path = acceptance_dir.join("mvp6-opening-evidence-v3.json");
            fs::write(&evidence_path, &evidence_bytes).map_err(|error| (1, error.to_string()))?;
            definition["market"] =
                serde_json::to_value(evidence).map_err(|error| (1, error.to_string()))?;
            let pit_input: PitInput =
                serde_json::from_value(definition).map_err(|error| (2, error.to_string()))?;
            let context = PitContext::build(&pit_input, &lake, &dsv)
                .map_err(|error| (error_code(&error), error.to_string()))?;
            let by_etf = context
                .unknown_coverage
                .instruments
                .iter()
                .map(|instrument| {
                    json!({
                        "code": instrument.native_symbol,
                        "cells": instrument.cells,
                        "unknown_cells": instrument.unknown_cells,
                        "unknown_percent": format_percent(instrument.unknown_cells, instrument.cells),
                    })
                })
                .collect::<Vec<_>>();
            println!(
                "{}",
                json!({
                    "execution_status": "szse-derived-open@3",
                    "per_etf_unknown_coverage": by_etf,
                    "evidence_output": "acceptance/mvp6-opening-evidence-v3.json",
                })
            );
            Ok(0)
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
