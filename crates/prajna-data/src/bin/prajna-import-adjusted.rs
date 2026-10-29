//! Explicit local supplier-adjusted research-price import; no network or cash events.
use std::{collections::BTreeMap, path::PathBuf, process::ExitCode};
fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut options = BTreeMap::new();
    while let Some(key) = args.next() {
        if key == "--help" {
            println!("prajna-import-adjusted --root <LOCAL_LAKE> --input <SUPPLIER_JSON>");
            return ExitCode::SUCCESS;
        }
        if !matches!(key.as_str(), "--root" | "--input") || options.contains_key(&key) {
            eprintln!("invalid or duplicate option");
            return ExitCode::from(2);
        }
        let Some(value) = args.next() else {
            return ExitCode::from(2);
        };
        options.insert(key, PathBuf::from(value));
    }
    let (Some(root), Some(input)) = (options.get("--root"), options.get("--input")) else {
        eprintln!("--root and --input are required");
        return ExitCode::from(2);
    };
    let Ok(bytes) = std::fs::read(input) else {
        eprintln!("cannot read adjusted input file");
        return ExitCode::FAILURE;
    };
    match prajna_data::adjusted_series::import(root, &bytes) {
        Ok(version) => {
            println!(
                "{}",
                serde_json::json!({"schema_version": 1, "version": version})
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("adjusted import failed ({})", error.code());
            ExitCode::from(error.exit_code())
        }
    }
}
