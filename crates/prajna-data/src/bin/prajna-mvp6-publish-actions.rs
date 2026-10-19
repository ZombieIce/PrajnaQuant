//! Offline publisher for the MVP-6 corporate-action Dataset.
//!
//! Reads only local frozen inputs and performs no network access. The local
//! acceptance report keeps the DSV, P21 findings and hashes; stdout/stderr do
//! not print real-data details.

use std::{error::Error, fs, path::PathBuf};

use chrono::Utc;
use fs2::FileExt;
use prajna_data::{default_mvp6_root, publish_corporate_actions};

fn main() {
    if run().is_err() {
        // Details stay in the local acceptance report.
        eprintln!("corporate action publish unresolved; consult local acceptance files");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if !(2..=3).contains(&args.len()) {
        return Err(
            invalid("usage: prajna-mvp6-publish-actions [ROOT] V4_DSV FACTS_SESSION").into(),
        );
    }
    let (requested_root, market_dsv, facts_session) = match args.as_slice() {
        [dsv, facts] => (default_mvp6_root(), dsv.clone(), PathBuf::from(facts)),
        [root, dsv, facts] => (PathBuf::from(root), dsv.clone(), PathBuf::from(facts)),
        _ => unreachable!("argument count checked"),
    };
    let default = default_mvp6_root();
    if !default.is_absolute() {
        return Err(invalid("HOME is not set; cannot resolve ~/PrajnaData/mvp-6").into());
    }
    fs::create_dir_all(&requested_root)?;
    let root = requested_root.canonicalize()?;
    if root != requested_root {
        return Err(invalid("MVP-6 data root must not contain symlinks").into());
    }
    let temp_root = std::env::temp_dir().canonicalize()?;
    if root != default && !root.starts_with(&temp_root) {
        return Err(invalid("MVP-6 corporate actions must use ~/PrajnaData/mvp-6").into());
    }
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join("mvp6-actions.lock"))?;
    lock.try_lock_exclusive()?;
    let created_at = Utc::now().to_rfc3339();
    publish_corporate_actions(&root, &market_dsv, &facts_session, &created_at, &created_at)?;
    eprintln!("corporate action Dataset published; consult local acceptance report");
    Ok(())
}

fn invalid(message: impl Into<String>) -> std::io::Error {
    std::io::Error::other(message.into())
}
