//! Local-only structured corporate-action version import and scoped reading.
use prajna_data::{corporate_facts, dataset_preview::PreviewError};
use std::{collections::BTreeMap, path::Path, process::ExitCode};
fn execute() -> Result<serde_json::Value, PreviewError> {
    let invalid = || {
        PreviewError::InvalidInput("usage: prajna-corporate-facts import --root ROOT --input JSON | read --root ROOT --version VERSION --instrument ID --start DATE --end DATE".into())
    };
    let mut args = std::env::args().skip(1);
    let command = args.next().ok_or_else(invalid)?;
    let keys: &[&str] = match command.as_str() {
        "import" => &["--root", "--input"],
        "read" => &["--root", "--version", "--instrument", "--start", "--end"],
        _ => return Err(invalid()),
    };
    let mut options = BTreeMap::new();
    while let Some(key) = args.next() {
        if !keys.contains(&key.as_str()) || options.contains_key(&key) {
            return Err(invalid());
        }
        options.insert(key, args.next().ok_or_else(invalid)?);
    }
    if keys.iter().any(|key| !options.contains_key(*key)) {
        return Err(invalid());
    }
    let root = Path::new(&options["--root"]);
    if command == "import" {
        let bytes = std::fs::read(&options["--input"])
            .map_err(|_| PreviewError::Io("cannot read fact input file".into()))?;
        Ok(serde_json::json!({"schema_version":1,"version":corporate_facts::import(root, &bytes)?}))
    } else {
        corporate_facts::query(
            root,
            &options["--version"],
            &options["--instrument"],
            options["--start"].parse().map_err(|_| invalid())?,
            options["--end"].parse().map_err(|_| invalid())?,
        )
    }
}
fn main() -> ExitCode {
    match execute() {
        Ok(value) => {
            println!("{value}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            println!(
                "{}",
                serde_json::json!({"schema_version":1,"error":{"code":error.code(),"message":error.to_string()}})
            );
            ExitCode::from(error.exit_code())
        }
    }
}
