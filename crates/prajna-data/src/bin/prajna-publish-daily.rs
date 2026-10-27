use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::ExitCode,
};

use prajna_data::daily_publish::{DailyPublicationSelection, publish_daily, rebuild_daily};

fn main() -> ExitCode {
    match run() {
        Ok(()) => {
            println!("Daily Dataset operation completed; evidence saved locally.");
            ExitCode::SUCCESS
        }
        Err(error) => {
            // Source paths, hashes and detailed coverage must never reach stdout/stderr.
            if let Some(root) = diagnostic_root() {
                let _ = save_diagnostic(&root, &error.to_string());
            }
            eprintln!(
                "Daily Dataset operation failed; inspect local diagnostics or input configuration."
            );
            ExitCode::from(2)
        }
    }
}

fn diagnostic_root() -> Option<PathBuf> {
    let args = std::env::args().collect::<Vec<_>>();
    args.windows(2)
        .find(|pair| pair[0] == "--diagnostics")
        .map(|pair| PathBuf::from(&pair[1]))
}

fn save_diagnostic(root: &Path, error: &str) -> std::io::Result<()> {
    prajna_data::session_calendar::CalendarStore::open(root)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    fs::create_dir_all(root)?;
    let mut file = tempfile::NamedTempFile::new_in(root)?;
    use std::io::Write;
    file.write_all(error.as_bytes())?;
    file.as_file().sync_all()?;
    file.persist(root.join(format!(
        "daily-publish-{}-{}.txt",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(),
        std::process::id()
    )))?;
    Ok(())
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let mode = args.next().ok_or("expected publish or rebuild")?;
    if mode == "--help" || mode == "-h" {
        println!(
            "Usage: prajna-publish-daily <publish|rebuild> --root <LOCAL> --receipt <RELATIVE_JSON> --diagnostics <LOCAL> [--selection <LOCAL_JSON>]"
        );
        return Ok(());
    }
    let mut options = BTreeMap::new();
    while let Some(flag) = args.next() {
        if !["--root", "--receipt", "--diagnostics", "--selection"].contains(&flag.as_str()) {
            return Err("unknown option".into());
        }
        let value = args.next().ok_or("missing option value")?;
        if options.insert(flag, value).is_some() {
            return Err("duplicate option".into());
        }
    }
    let root = Path::new(options.get("--root").ok_or("root is required")?);
    let receipt = Path::new(options.get("--receipt").ok_or("receipt is required")?);
    options
        .get("--diagnostics")
        .ok_or("independent local diagnostics directory is required")?;
    match mode.as_str() {
        "publish" => {
            let selection: DailyPublicationSelection = serde_json::from_slice(&fs::read(
                options.get("--selection").ok_or("selection is required")?,
            )?)?;
            publish_daily(root, selection, receipt)?;
        }
        "rebuild" if !options.contains_key("--selection") => rebuild_daily(root, receipt)?,
        _ => return Err("expected publish with selection, or rebuild without selection".into()),
    }
    Ok(())
}
