use std::{error::Error, path::PathBuf, process::ExitCode};

use prajna_data::{
    PublicationStatus, SecurityEvidenceSnapshot, SecurityEvidenceStore, SecurityEvidenceVersionId,
    SecuritySnapshotId,
};
use prajna_domain::InstrumentId;

fn main() -> ExitCode {
    match run(std::env::args().skip(1)) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("security evidence selection failed: {error}");
            ExitCode::from(2)
        }
    }
}

fn run(arguments: impl Iterator<Item = String>) -> Result<ExitCode, Box<dyn Error>> {
    let Some(args) = parse_args(arguments)? else {
        print_usage();
        return Ok(ExitCode::SUCCESS);
    };
    let store = SecurityEvidenceStore::open(&args.root)?;
    let (snapshot_id, snapshot) = match args.mode {
        Mode::Select(selections) => store.select_snapshot(selections)?,
        Mode::Read(snapshot_id) => {
            let snapshot = store.get_snapshot(&snapshot_id)?;
            (snapshot_id, snapshot)
        }
    };
    print_snapshot(&snapshot_id, &snapshot);
    Ok(ExitCode::SUCCESS)
}

fn print_snapshot(snapshot_id: &SecuritySnapshotId, snapshot: &SecurityEvidenceSnapshot) {
    let publishable = snapshot
        .instruments
        .iter()
        .filter(|instrument| {
            matches!(
                instrument.publication,
                PublicationStatus::Publishable { .. }
            )
        })
        .count();
    println!(
        "snapshot={snapshot_id} instruments={} publishable={publishable} blocked={}",
        snapshot.instruments.len(),
        snapshot.instruments.len() - publishable
    );
    for instrument in &snapshot.instruments {
        if let PublicationStatus::NotPublishable { issues } = &instrument.publication {
            let reasons = issues
                .iter()
                .map(|issue| format!("{}:{}", issue.field, issue.code.as_str()))
                .collect::<Vec<_>>()
                .join(",");
            println!(
                "blocked={} reasons={reasons}",
                instrument.record.instrument_id
            );
        }
    }
}

#[derive(Debug)]
struct Args {
    root: PathBuf,
    mode: Mode,
}

#[derive(Debug)]
enum Mode {
    Select(Vec<(InstrumentId, SecurityEvidenceVersionId)>),
    Read(SecuritySnapshotId),
}

fn parse_args(arguments: impl Iterator<Item = String>) -> Result<Option<Args>, Box<dyn Error>> {
    let mut root = None;
    let mut selections = Vec::new();
    let mut read = None;
    let mut args = arguments;
    while let Some(flag) = args.next() {
        if flag == "--help" || flag == "-h" {
            return Ok(None);
        }
        let value = args
            .next()
            .ok_or_else(|| invalid(format!("missing value for {flag}")))?;
        match flag.as_str() {
            "--root" if root.is_none() => root = Some(PathBuf::from(value)),
            "--select" => selections.push(parse_selection(&value)?),
            "--read" if read.is_none() => read = Some(SecuritySnapshotId::parse(value)?),
            "--root" | "--read" => {
                return Err(invalid(format!("{flag} may be supplied only once")).into());
            }
            _ => return Err(invalid(format!("unknown option {flag}")).into()),
        }
    }
    let root = root.ok_or_else(|| invalid("required option missing: --root"))?;
    let mode = match (selections.is_empty(), read) {
        (false, None) => Mode::Select(selections),
        (true, Some(snapshot_id)) => Mode::Read(snapshot_id),
        (false, Some(_)) => {
            return Err(invalid("--select and --read are mutually exclusive").into());
        }
        (true, None) => return Err(invalid("one of --select or --read is required").into()),
    };
    Ok(Some(Args { root, mode }))
}

fn parse_selection(
    value: &str,
) -> Result<(InstrumentId, SecurityEvidenceVersionId), Box<dyn Error>> {
    let (instrument, version) = value
        .split_once('=')
        .ok_or_else(|| invalid("--select expects <INSTRUMENT_ID>=<sha256:VERSION>"))?;
    Ok((
        instrument.parse::<InstrumentId>()?,
        SecurityEvidenceVersionId::parse(version.to_owned())?,
    ))
}

fn invalid(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, message.into())
}

fn print_usage() {
    println!(
        "Usage:\n  prajna-select-security-evidence --root <LOCAL_PATH> \\\n    --select <INSTRUMENT_ID>=<sha256:EVIDENCE_VERSION> [--select ...]\n  \
         prajna-select-security-evidence --root <LOCAL_PATH> --read <sha256:SNAPSHOT_ID>"
    );
}
