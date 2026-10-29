//! Read-only Dataset preview for the local research service.
//!
//! Machine-readable JSON is the product: stdout carries either
//! `{"schema_version":1,"data":...}` or `{"schema_version":1,"error":...}`.
//! This command never writes a Dataset, Raw object or archive.

use std::{collections::BTreeMap, path::PathBuf, process::ExitCode};

use chrono::NaiveDate;
use prajna_data::{
    corporate_facts,
    dataset_preview::{self, BarsRequest, PreviewError},
};

const USAGE: &str = "Usage: prajna-preview-dataset <catalog|show|bars|actions-catalog|actions> --root <LOCAL> \
[--dsv <DSV>] [--instrument <ID>] [--start YYYY-MM-DD] [--end YYYY-MM-DD] \
[--limit <1..=1000>] [--offset <N>] [--adjusted-version <ADJ_VERSION>] \
[--version <CAF_VERSION>]";

fn main() -> ExitCode {
    if std::env::args()
        .nth(1)
        .is_some_and(|argument| matches!(argument.as_str(), "--help" | "-h"))
    {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    match run() {
        Ok(data) => {
            print_json(&data);
            ExitCode::SUCCESS
        }
        Err(error) => {
            print_json(&serde_json::json!({
                "schema_version": dataset_preview::PREVIEW_SCHEMA_VERSION,
                "error": {"code": error.code(), "message": error.to_string()}
            }));
            ExitCode::from(error.exit_code())
        }
    }
}

fn print_json(value: &serde_json::Value) {
    println!(
        "{}",
        serde_json::to_string(value).expect("preview JSON is serializable")
    );
}

fn run() -> Result<serde_json::Value, PreviewError> {
    let mut args = std::env::args().skip(1);
    let mode = args.next().ok_or_else(|| invalid(USAGE))?;
    let mut options = BTreeMap::new();
    while let Some(flag) = args.next() {
        if ![
            "--root",
            "--dsv",
            "--instrument",
            "--start",
            "--end",
            "--limit",
            "--offset",
            "--adjusted-version",
            "--version",
        ]
        .contains(&flag.as_str())
        {
            return Err(invalid(&format!("unknown option {flag}; {USAGE}")));
        }
        let value = args
            .next()
            .ok_or_else(|| invalid(&format!("missing value for {flag}")))?;
        if options.contains_key(&flag) {
            return Err(invalid(&format!("duplicate option {flag}")));
        }
        options.insert(flag, value);
    }
    let root = PathBuf::from(
        options
            .get("--root")
            .ok_or_else(|| invalid("--root is required"))?,
    );
    match mode.as_str() {
        "catalog" => {
            if options.keys().any(|flag| flag != "--root") {
                return Err(invalid(&format!("catalog accepts only --root; {USAGE}")));
            }
            Ok(with_data(dataset_preview::catalog(&root)?))
        }
        "show" => {
            let dsv = options
                .get("--dsv")
                .ok_or_else(|| invalid("show requires --dsv"))?;
            if options
                .keys()
                .any(|flag| !matches!(flag.as_str(), "--root" | "--dsv"))
            {
                return Err(invalid(&format!("show accepts --root and --dsv; {USAGE}")));
            }
            Ok(with_data(dataset_preview::show(&root, dsv)?))
        }
        "bars" => {
            let dsv = options
                .get("--dsv")
                .ok_or_else(|| invalid("bars requires --dsv"))?
                .clone();
            let instrument = options
                .get("--instrument")
                .ok_or_else(|| invalid("bars requires --instrument"))?
                .clone();
            let request = BarsRequest {
                dsv,
                instrument_id: instrument,
                start: date_option(&options, "--start")?,
                end: date_option(&options, "--end")?,
                limit: usize_option(&options, "--limit")?.unwrap_or(250),
                offset: usize_option(&options, "--offset")?.unwrap_or(0),
            };
            if request.limit == 0 || request.limit > 1000 {
                return Err(invalid("--limit must be in 1..=1000"));
            }
            Ok(with_data(match options.get("--adjusted-version") {
                Some(version) => prajna_data::adjusted_series::bars(&root, &request, version)?,
                None => dataset_preview::bars(&root, &request)?,
            }))
        }
        "actions-catalog" => {
            if options.keys().any(|flag| flag != "--root") {
                return Err(invalid(&format!(
                    "actions-catalog accepts only --root; {USAGE}"
                )));
            }
            Ok(with_data(corporate_facts::catalog(&root)?))
        }
        "actions" => {
            let version = options
                .get("--version")
                .ok_or_else(|| invalid("actions requires --version"))?;
            let instrument = options
                .get("--instrument")
                .ok_or_else(|| invalid("actions requires --instrument"))?;
            if options.keys().any(|flag| {
                !matches!(
                    flag.as_str(),
                    "--root" | "--version" | "--instrument" | "--start" | "--end"
                )
            }) {
                return Err(invalid(&format!(
                    "actions accepts --version, --instrument, --start and --end; {USAGE}"
                )));
            }
            let start = date_option(&options, "--start")?
                .ok_or_else(|| invalid("actions requires --start"))?;
            let end =
                date_option(&options, "--end")?.ok_or_else(|| invalid("actions requires --end"))?;
            Ok(with_data(corporate_facts::query(
                &root, version, instrument, start, end,
            )?))
        }
        _ => Err(invalid(USAGE)),
    }
}

fn with_data(value: impl serde::Serialize) -> serde_json::Value {
    serde_json::json!({
        "schema_version": dataset_preview::PREVIEW_SCHEMA_VERSION,
        "data": value
    })
}

fn date_option(
    options: &BTreeMap<String, String>,
    flag: &str,
) -> Result<Option<NaiveDate>, PreviewError> {
    options
        .get(flag)
        .map(|value| {
            NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .map_err(|_| invalid(&format!("{flag} must be YYYY-MM-DD")))
        })
        .transpose()
}

fn usize_option(
    options: &BTreeMap<String, String>,
    flag: &str,
) -> Result<Option<usize>, PreviewError> {
    options
        .get(flag)
        .map(|value| {
            value
                .parse::<usize>()
                .map_err(|_| invalid(&format!("{flag} must be a non-negative integer")))
        })
        .transpose()
}

fn invalid(message: &str) -> PreviewError {
    PreviewError::InvalidInput(message.into())
}
