//! Explicitly collects Sina qfq/hfq Raw for the local v4 candidate universe.

use std::{
    error::Error,
    fs, io,
    path::{Path, PathBuf},
};

use chrono::NaiveDate;
use prajna_data::{RawStore, lock_mvp6_coverage_report, sina_adjust_factor::Collector};
use serde_json::{Value, json};

fn main() {
    if run().is_err() {
        eprintln!("Sina collection unresolved; consult local acceptance files");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let args = parse_args()?;
    let root = args.root.canonicalize()?;
    if root != args.root || !root.ends_with("PrajnaData/mvp-6") {
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
    if symbols.is_empty() || symbols.iter().any(|symbol| !valid_symbol(symbol)) {
        return Err(invalid("candidate symbol file is missing or outside the v4 TDX scope").into());
    }
    let raw = RawStore::open(&root)?;
    let mut collector = Collector::direct_with_network_path(raw, args.network_path.clone())?;
    let mut pairs = Vec::new();
    let mut failure = None;
    for symbol in &symbols {
        match collector.collect(std::slice::from_ref(symbol), args.start, args.end) {
            Ok(mut completed) => pairs.append(&mut completed),
            Err(_) => {
                failure = Some(symbol.clone());
                break;
            }
        }
    }

    let complete = failure.is_none() && pairs.len() == symbols.len();
    let completed_symbols = pairs
        .iter()
        .map(|pair| pair.symbol.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let remaining = symbols
        .iter()
        .filter(|symbol| {
            !completed_symbols.contains(symbol.as_str())
                && failure.as_deref() != Some(symbol.as_str())
        })
        .cloned()
        .collect::<Vec<_>>();
    let pair_records = pairs
        .iter()
        .map(|pair| {
            json!({
                "symbol": pair.symbol,
                "qfq_raw_sha256": pair.qfq_raw_sha256,
                "hfq_raw_sha256": pair.hfq_raw_sha256
            })
        })
        .collect::<Vec<_>>();
    let local_result = json!({
        "contract": "mvp-6/#206",
        "window": {"start": args.start.to_string(), "end": args.end.to_string()},
        "network_path": args.network_path,
        "status": if complete { "complete" } else { "source_stopped_unresolved" },
        "candidate_symbols": symbols,
        "completed_pairs": pair_records,
        "failed_symbol": failure,
        "remaining_symbols_not_attempted": remaining
    });
    write_json(&root.join("acceptance/sina/collection.json"), &local_result)?;
    update_coverage(&root, complete, failure.as_deref())?;
    if complete {
        Ok(())
    } else {
        Err(invalid("Sina source did not complete every symbol").into())
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

fn update_coverage(
    root: &Path,
    complete: bool,
    failed_symbol: Option<&str>,
) -> Result<(), Box<dyn Error>> {
    let _coverage_lock = lock_mvp6_coverage_report(root)?;
    let path = root.join("acceptance/mvp6-coverage.json");
    let mut report: Value = serde_json::from_slice(&fs::read(&path)?)?;
    report["source_status"]["sina_adjustment_factors"] = json!(if complete {
        "captured_raw_pairs_not_normalized"
    } else {
        "source_stopped_unresolved"
    });
    if let Some(symbol) = failed_symbol {
        report["sina_failures"] = json!([symbol]);
    } else {
        report["sina_failures"] = json!([]);
    }
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
