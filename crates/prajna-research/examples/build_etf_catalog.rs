//! Offline build: references must already exist in RawStore. No network/data download.
use prajna_data::RawStore;
use prajna_research::pit_universe::{CatalogInput, HistoricalCatalog};
use std::{env, fs};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args().collect();
    if args.len() != 5 {
        return Err("usage: build_etf_catalog <raw-root> <extracted-input.json> <new-frozen-catalog.json> <decision-YYYY-MM-DD>".into());
    }
    let raw = RawStore::open(&args[1])?;
    let input: CatalogInput = serde_json::from_slice(&fs::read(&args[2])?)?;
    let catalog = HistoricalCatalog::build(input, &raw)?;
    eprintln!(
        "{}",
        serde_json::to_string_pretty(
            &serde_json::json!({"catalog_version": catalog.version()?, "differences": catalog.differences(), "coverage_gaps": catalog.coverage_gaps()})
        )?
    );
    catalog.store(&args[3])?;
    println!(
        "{}",
        serde_json::to_string_pretty(&catalog.universe(args[4].parse()?)?)?
    );
    Ok(())
}
