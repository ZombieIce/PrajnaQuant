//! Explicit local-only collection command; never runs during tests or CI.
use prajna_data::{
    ManifestProvenance, RawStore,
    cninfo::{Collector, IndexDataset, Poppler},
    write_manifest,
};
use serde_json::json;
use std::{fs, path::PathBuf};

fn main() {
    if run().is_err() {
        // Never print response text, identifiers, hashes or counts to CI/PR logs.
        eprintln!("cninfo collection unresolved; consult local acceptance files");
        std::process::exit(1);
    }
}
fn run() -> prajna_data::cninfo::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 4 {
        return Err(
            "usage: prajna-cninfo START END CODES_FILE (one historical code per line)".into(),
        );
    }
    let start = args[1].parse()?;
    let end = args[2].parse()?;
    if start < "2022-10-01".parse()? || end > "2026-09-30".parse()? {
        return Err("outside spec window".into());
    }
    let home = PathBuf::from(std::env::var("HOME")?);
    let root = home.join("PrajnaData/mvp-6");
    fs::create_dir_all(&root)?;
    // Reject symlinks into the repo or a synced directory, including parents.
    if root.canonicalize()? != root {
        return Err("local data root must not contain symlinks".into());
    }
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join("cninfo.lock"))?;
    fs2::FileExt::try_lock_exclusive(&lock)?;
    let codes: Vec<_> = fs::read_to_string(&args[3])?
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    if codes.is_empty() {
        return Err("historical candidate set is required".into());
    }
    let raw = RawStore::open(root.join("raw"))?;
    let mut collector = Collector::direct(raw)?;
    let dataset = collector.collect(IndexDataset::new(start, end)?, &codes, &mut Poppler);
    let complete =
        dataset.unknown.is_empty() && dataset.funds.iter().all(|f| f.require_usable().is_ok());
    let provenance: ManifestProvenance = serde_json::from_value(
        json!({"created_at":chrono::Utc::now().to_rfc3339(), "writer":{"tool":"prajna-cninfo/1"}}),
    )?;
    let (manifest, batch) = dataset.manifest(provenance)?;
    let acceptance = root
        .join("acceptance/cninfo")
        .join(manifest.dsv.trim_start_matches("dsv:sha256:"));
    fs::create_dir_all(&acceptance)?;
    let report = acceptance.join("coverage.json");
    if !report.exists() {
        fs::write(report, serde_json::to_vec_pretty(&dataset)?)?;
    }
    let table = acceptance.join("cninfo_index.parquet");
    if !table.exists() {
        let mut writer =
            parquet::arrow::ArrowWriter::try_new(fs::File::create(&table)?, batch.schema(), None)?;
        writer.write(&batch)?;
        writer.close()?;
    }
    write_manifest(&acceptance, &manifest)?;
    if !complete {
        return Err("local coverage report contains unknowns".into());
    }
    Ok(())
}
