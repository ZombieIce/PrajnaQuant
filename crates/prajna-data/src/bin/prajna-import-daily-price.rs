use std::{collections::BTreeMap, fs, io::Write, path::Path, process::ExitCode};

use prajna_data::{
    daily_price_import::{Acquisition, import_daily_price},
    daily_raw::checked_output_path,
};

fn main() -> ExitCode {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args == ["--help"] || args == ["-h"] {
        println!(
            "Usage: prajna-import-daily-price --root <LAKE> --input-root <LOCAL> --config <JSON> --receipt <NAME.json> (--fetch | --replay <LOCAL_RELATIVE_DIR>)"
        );
        return ExitCode::SUCCESS;
    }
    match run(&args) {
        Ok(()) => {
            println!("Daily price import completed. Inspect the local receipt for Session gaps.");
            ExitCode::SUCCESS
        }
        Err(error) => {
            if let Some(pair) = args.windows(2).find(|pair| pair[0] == "--input-root") {
                let _ = save_error(Path::new(&pair[1]), &error.to_string());
            }
            eprintln!("Daily price import failed. Inspect local import diagnostics or use --help.");
            ExitCode::from(2)
        }
    }
}

fn save_error(root: &Path, message: &str) -> Result<(), Box<dyn std::error::Error>> {
    // CalendarStore applies the same protected archive path validation as the importer.
    prajna_data::session_calendar::CalendarStore::open(root)?;
    let dir = checked_output_path(root, Path::new("daily-price/diagnostics"))?;
    fs::create_dir_all(&dir)?;
    let mut file = tempfile::Builder::new()
        .prefix("import-")
        .suffix(".txt")
        .tempfile_in(dir)?;
    file.write_all(message.as_bytes())?;
    file.as_file().sync_all()?;
    file.keep()?;
    Ok(())
}

fn run(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut iter = args.iter().map(String::as_str);
    let mut options = BTreeMap::new();
    while let Some(flag) = iter.next() {
        let value = match flag {
            "--fetch" => "",
            "--root" | "--input-root" | "--config" | "--receipt" | "--replay" => iter
                .next()
                .filter(|value| !value.starts_with("--"))
                .ok_or("missing option value")?,
            _ => return Err("unknown option".into()),
        };
        if options.insert(flag, value).is_some() {
            return Err("duplicate option".into());
        }
    }
    let acquisition = match (options.contains_key("--fetch"), options.get("--replay")) {
        (true, None) => Acquisition::Fetch,
        (false, Some(path)) => Acquisition::Replay(Path::new(path)),
        _ => return Err("select exactly one of --fetch or --replay".into()),
    };
    import_daily_price(
        Path::new(options.get("--root").ok_or("--root is required")?),
        Path::new(
            options
                .get("--input-root")
                .ok_or("--input-root is required")?,
        ),
        Path::new(options.get("--config").ok_or("--config is required")?),
        Path::new(options.get("--receipt").ok_or("--receipt is required")?),
        acquisition,
    )
}
