//! Exports the v4 TDX universe as local candidate files for Sina and cninfo.

use std::{collections::BTreeSet, error::Error, fs, io, path::PathBuf};

use prajna_data::{Manifest, read_parquet, tdx_code_table_from_record_batch};
use serde_json::Value;

fn main() {
    if run().is_err() {
        eprintln!("MVP-6 candidate export unresolved; consult local acceptance files");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let root = args.next().map(PathBuf::from).unwrap_or_else(default_root);
    if args.next().is_some() {
        return Err(invalid("usage: prajna-mvp6-export-codes [MVP6_ROOT]").into());
    }
    let coverage: Value =
        serde_json::from_slice(&fs::read(root.join("acceptance/mvp6-coverage.json"))?)?;
    if !spec_window_complete(&coverage) {
        return Err(
            invalid("candidate export requires the complete MVP-6 specification window").into(),
        );
    }
    let dsv = coverage["dsv"]
        .as_str()
        .ok_or_else(|| invalid("local coverage report has no published DSV"))?;
    if coverage["dataset_published"] != true {
        return Err(invalid("local DSV is not published").into());
    }
    let manifest: Manifest = serde_json::from_slice(&fs::read(manifest_path(&root, dsv)?)?)?;
    manifest.validate()?;
    if manifest.core.normalizer.id != "szse-etf-frozen" || manifest.core.normalizer.version != "4" {
        return Err(invalid("candidate export requires szse-etf-frozen v4").into());
    }
    if !manifest
        .core
        .tables
        .iter()
        .any(|table| table.table == "tdx_code_table")
    {
        return Err(invalid("v4 manifest has no TDX code table").into());
    }
    let hex = dsv
        .strip_prefix("dsv:sha256:")
        .ok_or_else(|| invalid("invalid DSV"))?;
    let parquet_path = root
        .join("normalized/tdx_code_table")
        .join(hex)
        .join("part-00000.parquet");
    let batches = read_parquet(&parquet_path, "tdx_code_table", "1")?;
    let mut codes = BTreeSet::new();
    for batch in batches {
        for row in tdx_code_table_from_record_batch(&batch)? {
            codes.extend(row.native_symbols);
        }
    }
    if codes.is_empty() {
        return Err(invalid("v4 TDX universe is empty").into());
    }
    let acceptance = root.join("acceptance");
    fs::create_dir_all(&acceptance)?;
    write_code_file(&acceptance.join("mvp6-candidate-codes.txt"), &codes)?;
    let sina = codes
        .iter()
        .map(|code| format!("sz{code}"))
        .collect::<BTreeSet<_>>();
    write_code_file(&acceptance.join("mvp6-sina-symbols.txt"), &sina)?;
    Ok(())
}

fn spec_window_complete(coverage: &Value) -> bool {
    coverage["spec_window_complete"] == true
        && coverage["window"]["start"] == "2022-10-01"
        && coverage["window"]["end"] == "2026-09-30"
}

fn write_code_file(path: &std::path::Path, codes: &BTreeSet<String>) -> io::Result<()> {
    let mut contents = codes.iter().cloned().collect::<Vec<_>>().join("\n");
    contents.push('\n');
    fs::write(path, contents)
}

fn manifest_path(root: &std::path::Path, dsv: &str) -> io::Result<PathBuf> {
    let hex = dsv
        .strip_prefix("dsv:sha256:")
        .ok_or_else(|| invalid("invalid DSV"))?;
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid("invalid DSV hash"));
    }
    Ok(root.join("manifests").join(format!("{hex}.json")))
}

fn default_root() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("PrajnaData/mvp-6")
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::spec_window_complete;

    #[test]
    fn candidate_export_requires_the_full_spec_window() {
        assert!(spec_window_complete(&json!({
            "spec_window_complete": true,
            "window": {"start": "2022-10-01", "end": "2026-09-30"}
        })));
        assert!(!spec_window_complete(&json!({
            "spec_window_complete": false,
            "window": {"start": "2022-10-10", "end": "2026-09-30"}
        })));
        assert!(!spec_window_complete(&json!({
            "window": {"start": "2022-10-01", "end": "2026-09-30"}
        })));
    }
}
