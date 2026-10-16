//! Explicit local-only collection command; never runs during tests or CI.
use prajna_data::{
    ManifestProvenance, RawStore,
    cninfo::{Collector, IndexDataset, Poppler},
    write_manifest,
};
use serde_json::json;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

fn main() {
    if run().is_err() {
        // Never print response text, identifiers, hashes or counts to CI/PR logs.
        eprintln!("cninfo collection unresolved; consult local acceptance files");
        std::process::exit(1);
    }
}
fn run() -> prajna_data::cninfo::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 5 {
        return Err(
            "usage: prajna-cninfo START END CODES_FILE NETWORK_PATH (one historical code per line)"
                .into(),
        );
    }
    let start = args[1].parse()?;
    let end = args[2].parse()?;
    if start != "2022-10-01".parse()? || end != "2026-09-30".parse()? {
        return Err("cninfo collection requires the complete MVP-6 specification window".into());
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
    let raw = RawStore::open(root.clone())?;
    let mut collector = Collector::direct_with_network_path(raw, args[4].clone())?;
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
    install_local(
        &acceptance.join("coverage.json"),
        &serde_json::to_vec_pretty(&dataset)?,
    )?;
    let mut parquet_bytes = Vec::new();
    let mut writer =
        parquet::arrow::ArrowWriter::try_new(&mut parquet_bytes, batch.schema(), None)?;
    writer.write(&batch)?;
    writer.close()?;
    install_local(&acceptance.join("cninfo_index.parquet"), &parquet_bytes)?;
    write_manifest(&acceptance, &manifest)?;
    update_mvp6_coverage(&root, &dataset, complete)?;
    if !complete {
        return Err("local coverage report contains unknowns".into());
    }
    Ok(())
}

fn update_mvp6_coverage(
    root: &Path,
    dataset: &IndexDataset,
    complete: bool,
) -> prajna_data::cninfo::Result<()> {
    let path = root.join("acceptance/mvp6-coverage.json");
    let Ok(bytes) = fs::read(&path) else {
        return Ok(());
    };
    let mut report: serde_json::Value = serde_json::from_slice(&bytes)?;
    let missing_org_ids: Vec<_> = dataset
        .funds
        .iter()
        .filter(|fund| {
            fund.org_ids.is_empty() || fund.announcements.iter().any(|item| item.org_id.is_none())
        })
        .map(|fund| fund.code.clone())
        .collect();
    let missing_legal_names: Vec<_> = dataset
        .funds
        .iter()
        .filter(|fund| {
            fund.full_names.is_empty()
                || fund.unknown.iter().any(|item| item == "legal_name_unknown")
        })
        .map(|fund| fund.code.clone())
        .collect();
    report["source_status"]["cninfo"] = serde_json::json!(if complete {
        "captured_index"
    } else {
        "captured_index_with_unknowns"
    });
    report["missing_org_id_codes"] = serde_json::json!(missing_org_ids);
    report["missing_legal_names"] = serde_json::json!(missing_legal_names);
    report["cninfo_unknown_facts"] = serde_json::json!(dataset.unknown);
    write_local(&path, &serde_json::to_vec_pretty(&report)?)
}

// Stage complete bytes before installation; never accept a truncated prior file.
fn install_local(path: &Path, bytes: &[u8]) -> prajna_data::cninfo::Result<()> {
    let mut stage = tempfile::NamedTempFile::new_in(path.parent().ok_or("missing parent")?)?;
    stage.write_all(bytes)?;
    stage.as_file().sync_all()?;
    match stage.persist_noclobber(path) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            if fs::read(path)? == bytes {
                Ok(())
            } else {
                Err("existing local artifact conflicts with DSV".into())
            }
        }
        Err(error) => Err(error.error.into()),
    }
}

fn write_local(path: &Path, bytes: &[u8]) -> prajna_data::cninfo::Result<()> {
    let parent = path.parent().ok_or("missing parent")?;
    fs::create_dir_all(parent)?;
    let mut stage = tempfile::NamedTempFile::new_in(parent)?;
    stage.write_all(bytes)?;
    stage.as_file().sync_all()?;
    stage.persist(path).map_err(|error| error.error)?;
    Ok(())
}
