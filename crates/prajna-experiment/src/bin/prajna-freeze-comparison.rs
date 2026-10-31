//! Production JSON-in / JSON-out strategy-benchmark freeze boundary.
use clap::Parser;
use prajna_experiment::comparison::{self, Request};
use serde_json::json;
use std::{io, io::Read, path::PathBuf, process::ExitCode};

#[derive(Parser)]
struct Cli {
    #[arg(long)]
    lake: PathBuf,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let mut input = String::new();
    let request = io::stdin()
        .take(65537)
        .read_to_string(&mut input)
        .map_err(|error| (2, "invalid_request", error.to_string()))
        .and_then(|_| {
            if input.len() > 65536 {
                return Err((
                    2,
                    "invalid_request",
                    "comparison request exceeds 64 KiB".into(),
                ));
            }
            serde_json::from_str::<Request>(&input)
                .map_err(|error| (2, "invalid_request", error.to_string()))
        });
    let result = request.and_then(|request| {
        comparison::freeze(&cli.lake, &request).map_err(|error| (2, error.code, error.message))
    });
    match result {
        Ok(value) => {
            println!("{value}");
            ExitCode::SUCCESS
        }
        Err((code, error, message)) => {
            println!(
                "{}",
                json!({"schema_version": 1, "error": {"code": error, "message": message}})
            );
            ExitCode::from(code)
        }
    }
}
