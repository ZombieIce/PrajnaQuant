//! Local-only fixed-list cninfo collector for MVP-6. Never used in CI.
use prajna_data::{
    RawStore,
    cninfo::{Collector, FundCandidate, Poppler, Result, collect_and_accept},
};
use serde::Deserialize;
use std::{collections::BTreeSet, fs, path::PathBuf};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CandidateRow {
    code: String,
    short_name: String,
    active_start: String,
    active_end: String,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("fixed-list cninfo unresolved: {error}; consult local acceptance files");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if !(2..=3).contains(&args.len()) {
        return Err(
            "usage: prajna-cninfo-fixed CANDIDATES_CSV NETWORK_PATH [EXPLICIT_PDF_CATEGORIES]"
                .into(),
        );
    }
    let mut reader = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_path(&args[0])?;
    if reader.headers()?.iter().collect::<Vec<_>>()
        != ["code", "short_name", "active_start", "active_end"]
    {
        return Err("candidate CSV headers must be code,short_name,active_start,active_end".into());
    }
    let candidates = reader
        .deserialize::<CandidateRow>()
        .map(|row| -> Result<FundCandidate> {
            let row = row?;
            Ok(FundCandidate {
                code: row.code,
                short_name: row.short_name,
                active_start: row.active_start.parse()?,
                active_end: row.active_end.parse()?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    if candidates.is_empty() {
        return Err("explicit fixed candidate list is required".into());
    }

    let home = PathBuf::from(std::env::var("HOME")?);
    let root = home.join("PrajnaData/mvp-6");
    fs::create_dir_all(&root)?;
    if root.canonicalize()? != root {
        return Err("local data root must not contain symlinks".into());
    }
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join("cninfo.lock"))?;
    fs2::FileExt::try_lock_exclusive(&lock)?;
    let explicit = args
        .get(2)
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_else(BTreeSet::new);
    let raw = RawStore::open(root.clone())?;
    let store = raw.clone();
    let mut collector = Collector::direct_with_network_path(raw, args[1].clone())?;
    let acceptance = collect_and_accept(
        &root,
        &store,
        &mut collector,
        &candidates,
        &explicit,
        &mut Poppler,
    )?;
    if let Some(facts) = &acceptance.facts {
        eprintln!(
            "local facts validated; v4 cross-check mismatches: {}",
            facts.report.mismatches.len()
        );
    }
    if !acceptance.unresolved.is_empty() {
        return Err("local fixed-list report contains unresolved gaps".into());
    }
    Ok(())
}
