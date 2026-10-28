//! Production JSON-in / JSON-out selection boundary, called by the local service.
use clap::Parser;
use prajna_experiment::selection::{Selection, freeze};
use serde_json::json;
use std::{
    io::{self, Read},
    path::PathBuf,
    process::ExitCode,
};

#[derive(Parser)]
struct Cli {
    #[arg(long)]
    lake: PathBuf,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let mut input = String::new();
    let selection = io::stdin()
        .take(65537)
        .read_to_string(&mut input)
        .map_err(|e| e.to_string())
        .and_then(|_| {
            if input.len() > 65536 {
                return Err("selection exceeds 64 KiB".into());
            }
            serde_json::from_str::<Selection>(&input).map_err(|e| e.to_string())
        });
    let result = selection
        .map_err(|message| json!({"code":"invalid_selection","message":message}))
        .and_then(|selection| freeze(&cli.lake, &selection).map_err(|e| json!(e)));
    match result {
        Ok(value) => {
            println!("{value}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            println!("{}", json!({"schema_version":1,"error":error}));
            ExitCode::from(2)
        }
    }
}
