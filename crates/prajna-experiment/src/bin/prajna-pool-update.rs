use clap::Parser;
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
    #[arg(long)]
    input: Option<PathBuf>,
}
fn run(cli: Cli) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let reader: Box<dyn Read> = match cli.input {
        Some(path) => Box::new(std::fs::File::open(path)?),
        None => Box::new(io::stdin()),
    };
    let mut input = Vec::new();
    reader.take(65537).read_to_end(&mut input)?;
    if input.len() > 65536 {
        return Err("update command exceeds 64 KiB".into());
    }
    prajna_experiment::pool_update::command(&cli.lake, serde_json::from_slice(&input)?)
}
fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(value) => {
            println!("{value}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            println!(
                "{}",
                json!({"schema_version":1,"error":{"code":"pool_update_failed","message":error.to_string()}})
            );
            ExitCode::from(2)
        }
    }
}
