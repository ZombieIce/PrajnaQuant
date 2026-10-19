//! Local-only fixed-list cninfo collector for MVP-6. Never used in CI.
use prajna_data::{
    RawStore,
    cninfo::{Collector, FundCandidate, Poppler, Result},
};
use serde::Deserialize;
use serde_json::json;
use std::{collections::BTreeSet, fs, io::Write, path::PathBuf};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CandidateRow {
    code: String,
    short_name: String,
    active_start: String,
    active_end: String,
}

fn main() {
    if run().is_err() {
        eprintln!("fixed-list cninfo collection unresolved; consult local acceptance files");
        std::process::exit(1);
    }
}

fn run() -> prajna_data::cninfo::Result<()> {
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
    let mut collector = Collector::direct_with_network_path(raw, args[1].clone())?;
    let result = collector.collect_fixed_list(&candidates, &explicit, &mut Poppler);

    let announcements = result
        .announcements
        .iter()
        .map(|((code, org_id), rows)| {
            json!({
                "code": code, "org_id": org_id, "announcements": rows,
            })
        })
        .collect::<Vec<_>>();
    let has_gaps = !result.unresolved.is_empty();
    let report = json!({
        "contract":"mvp-6-fixed-cninfo-v1",
        "resolutions":result.resolutions,
        "announcements":announcements,
        "pdf_raw_sha256_by_announcement":result.pdf_raw_sha256_by_announcement,
        "unresolved":result.unresolved,
    });
    let dir = root
        .join("acceptance/cninfo-fixed")
        .join(chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ").to_string());
    fs::create_dir_all(&dir)?;
    let bytes = serde_json::to_vec_pretty(&report)?;
    let mut file = tempfile::NamedTempFile::new_in(&dir)?;
    file.write_all(&bytes)?;
    file.as_file().sync_all()?;
    file.persist(dir.join("collection.json"))
        .map_err(|error| error.error)?;
    if has_gaps {
        return Err("local fixed-list report contains unresolved gaps".into());
    }
    Ok(())
}
