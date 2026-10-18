//! Explicitly collects Sina qfq/hfq Raw for the local v4 candidate universe.

use std::{
    collections::BTreeSet,
    error::Error,
    fs, io,
    path::{Path, PathBuf},
};

use chrono::NaiveDate;
use prajna_data::{
    RawStore, lock_mvp6_coverage_report,
    sina_adjust_factor::{CollectionReport, CollectionStatus, Collector, SymbolOutcome},
};
use serde_json::{Value, json};

fn main() {
    if let Err(error) = run() {
        eprintln!("Sina collection unresolved: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let args = parse_args()?;
    let root = args.root.canonicalize()?;
    if root != args.root || root != default_root() {
        return Err(
            invalid("collection root must be the non-symlink local MVP-6 directory").into(),
        );
    }
    let _lock = lock(&root.join("sina.lock"))?;
    let symbol_file = root.join("acceptance/mvp6-sina-symbols.txt");
    let symbols = fs::read_to_string(symbol_file)?
        .lines()
        .map(str::trim)
        .filter(|symbol| !symbol.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let unique_symbols = symbols.iter().collect::<BTreeSet<_>>();
    if symbols.is_empty() || symbols.iter().any(|symbol| !valid_symbol(symbol)) {
        return Err(invalid("candidate symbol file is missing or outside the v4 TDX scope").into());
    }
    if unique_symbols.len() != symbols.len() {
        return Err(invalid("candidate symbol file contains duplicate symbols").into());
    }
    let mut symbols = symbols;
    symbols.sort();
    let raw = RawStore::open(&root)?;
    let mut collector = Collector::direct_with_network_path(raw, args.network_path.clone())?;
    let report = collector.collect(&symbols, args.start, args.end)?;
    let local_result = json!({
        "contract": "mvp-6/#215",
        "window": {"start": args.start.to_string(), "end": args.end.to_string()},
        "network_path": args.network_path,
        "status": report.status,
        "candidate_symbols": symbols,
        "candidate_results": &report.outcomes,
        "source_stop_reason": &report.source_stop_reason
    });
    write_json(&root.join("acceptance/sina/collection.json"), &local_result)?;
    update_coverage(&root, &report)?;
    match report.status {
        CollectionStatus::Complete => Ok(()),
        CollectionStatus::CompletedWithGaps => {
            let gaps = report
                .outcomes
                .iter()
                .filter_map(|outcome| match outcome {
                    SymbolOutcome::Gap { symbol, reason } => Some(format!("{symbol}: {reason}")),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("; ");
            Err(io::Error::other(format!("Sina collection has per-symbol gaps: {gaps}")).into())
        }
        CollectionStatus::SourceStopped => Err(io::Error::other(format!(
            "Sina source stopped: {}",
            report
                .source_stop_reason
                .as_deref()
                .unwrap_or("unresolved source stop")
        ))
        .into()),
    }
}

struct Args {
    root: PathBuf,
    start: NaiveDate,
    end: NaiveDate,
    network_path: String,
}

fn parse_args() -> Result<Args, Box<dyn Error>> {
    let mut values = std::env::args().skip(1);
    let root = values
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(default_root);
    let start = parse_date(&values.next().ok_or_else(|| {
        invalid("usage: prajna-mvp6-collect-sina [ROOT] START END NETWORK_PATH")
    })?)?;
    let end = parse_date(&values.next().ok_or_else(|| {
        invalid("usage: prajna-mvp6-collect-sina [ROOT] START END NETWORK_PATH")
    })?)?;
    let network_path = values
        .next()
        .ok_or_else(|| invalid("operator-confirmed direct domestic network path is required"))?;
    if values.next().is_some()
        || start != parse_date("2022-10-01")?
        || end != parse_date("2026-09-30")?
    {
        return Err(invalid("invalid Sina collection window or arguments").into());
    }
    if network_path.trim().is_empty() {
        return Err(invalid("operator-confirmed direct domestic network path is required").into());
    }
    Ok(Args {
        root,
        start,
        end,
        network_path,
    })
}

fn valid_symbol(symbol: &str) -> bool {
    symbol.len() == 8
        && symbol.starts_with("sz")
        && symbol[2..].bytes().all(|byte| byte.is_ascii_digit())
        && matches!(&symbol[2..5], "158" | "159")
}

fn update_coverage(root: &Path, collection: &CollectionReport) -> Result<(), Box<dyn Error>> {
    let _coverage_lock = lock_mvp6_coverage_report(root)?;
    let path = root.join("acceptance/mvp6-coverage.json");
    let mut report: Value = serde_json::from_slice(&fs::read(&path)?)?;
    report["source_status"]["sina_adjustment_factors"] = json!(match collection.status {
        CollectionStatus::Complete => "captured_raw_pairs_not_normalized",
        CollectionStatus::CompletedWithGaps => "captured_with_symbol_gaps",
        CollectionStatus::SourceStopped => "source_stopped_unresolved",
    });
    report["sina_failures"] = json!(
        collection
            .outcomes
            .iter()
            .filter_map(|outcome| match outcome {
                SymbolOutcome::Gap { symbol, reason } => {
                    Some(json!({"symbol": symbol, "reason": reason}))
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    );
    report["sina_not_attempted"] = json!(
        collection
            .outcomes
            .iter()
            .filter_map(|outcome| match outcome {
                SymbolOutcome::NotAttempted {
                    symbol,
                    source_stop_reason,
                } => Some(json!({
                    "symbol": symbol,
                    "source_stop_reason": source_stop_reason
                })),
                _ => None,
            })
            .collect::<Vec<_>>()
    );
    report["sina_source_stop_reason"] = json!(collection.source_stop_reason);
    write_json(&path, &report)
}

fn write_json(path: &Path, value: &Value) -> Result<(), Box<dyn Error>> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("missing report directory"))?;
    fs::create_dir_all(parent)?;
    let mut stage = tempfile::NamedTempFile::new_in(parent)?;
    use std::io::Write;
    stage.write_all(&serde_json::to_vec_pretty(value)?)?;
    stage.as_file().sync_all()?;
    stage.persist(path)?;
    Ok(())
}

fn lock(path: &Path) -> io::Result<fs::File> {
    use fs2::FileExt;
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)?;
    file.try_lock_exclusive()?;
    Ok(file)
}

fn default_root() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("PrajnaData/mvp-6")
}

fn parse_date(value: &str) -> Result<NaiveDate, Box<dyn Error>> {
    Ok(NaiveDate::parse_from_str(value, "%Y-%m-%d")?)
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_report_distinguishes_complete_gaps_and_source_stop() {
        let root = tempfile::tempdir().unwrap();
        let acceptance = root.path().join("acceptance");
        fs::create_dir_all(&acceptance).unwrap();
        let path = acceptance.join("mvp6-coverage.json");
        let cases = [
            (
                CollectionReport {
                    status: CollectionStatus::Complete,
                    outcomes: vec![SymbolOutcome::Completed {
                        symbol: "sz159901".into(),
                        qfq_raw_sha256: "qfq".into(),
                        hfq_raw_sha256: "hfq".into(),
                    }],
                    source_stop_reason: None,
                },
                "captured_raw_pairs_not_normalized",
                0,
                0,
                Value::Null,
            ),
            (
                CollectionReport {
                    status: CollectionStatus::CompletedWithGaps,
                    outcomes: vec![SymbolOutcome::Gap {
                        symbol: "sz159901".into(),
                        reason: "HTTP 404".into(),
                    }],
                    source_stop_reason: None,
                },
                "captured_with_symbol_gaps",
                1,
                0,
                Value::Null,
            ),
            (
                CollectionReport {
                    status: CollectionStatus::SourceStopped,
                    outcomes: vec![
                        SymbolOutcome::Gap {
                            symbol: "sz159901".into(),
                            reason: "access control returned HTTP 403".into(),
                        },
                        SymbolOutcome::NotAttempted {
                            symbol: "sz159902".into(),
                            source_stop_reason: "access control returned HTTP 403".into(),
                        },
                    ],
                    source_stop_reason: Some("access control returned HTTP 403".into()),
                },
                "source_stopped_unresolved",
                1,
                1,
                json!("access control returned HTTP 403"),
            ),
        ];

        for (collection, expected_status, failure_count, not_attempted_count, stop_reason) in cases
        {
            fs::write(&path, br#"{"source_status":{}}"#).unwrap();
            update_coverage(root.path(), &collection).unwrap();
            let report: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(
                report["source_status"]["sina_adjustment_factors"],
                expected_status
            );
            assert_eq!(
                report["sina_failures"].as_array().unwrap().len(),
                failure_count
            );
            assert_eq!(
                report["sina_not_attempted"].as_array().unwrap().len(),
                not_attempted_count
            );
            assert_eq!(report["sina_source_stop_reason"], stop_reason);
        }
    }
}
