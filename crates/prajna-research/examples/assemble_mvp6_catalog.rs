//! Offline MVP-6 Catalog assembly. Reads only local frozen inputs and writes
//! the Catalog input and the local assembly report; no network access.
use prajna_data::{
    RawStore,
    cninfo::{Mvp6Facts, Poppler, read_citations_csv},
};
use prajna_research::pit_catalog_assembly::assemble_mvp6_catalog;
use std::{env, fs, path::Path};

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args = env::args().collect::<Vec<_>>();
    if args.len() != 5 {
        return Err(
            "usage: assemble_mvp6_catalog <lake-root> <dsv> <facts-dir> <output-dir>".into(),
        );
    }
    let root = Path::new(&args[1]);
    let dsv = &args[2];
    let facts_dir = Path::new(&args[3]);
    let output = Path::new(&args[4]);

    let raw = RawStore::open(root)?;
    // The Catalog only consumes identity rows. The action/event tables are
    // optional here; fall back to the public header-only templates so a
    // Catalog-only local run does not need unrelated fact files.
    let actions = fs::read_to_string(facts_dir.join("actions.csv"))
        .unwrap_or_else(|_| include_str!("../../../configs/mvp-6/actions.csv").into());
    let events = fs::read_to_string(facts_dir.join("events.csv"))
        .unwrap_or_else(|_| include_str!("../../../configs/mvp-6/events.csv").into());
    let facts = Mvp6Facts::from_csv(
        &fs::read_to_string(facts_dir.join("identity.csv"))?,
        &actions,
        &events,
    )?;
    let mut converter = Poppler;
    let citations = read_citations_csv(
        &fs::read_to_string(facts_dir.join("citations.csv"))?,
        &raw,
        &mut converter,
    )?;
    let assembly = assemble_mvp6_catalog(root, dsv, &facts, &citations)?;

    fs::create_dir_all(output)?;
    fs::write(
        output.join("catalog-input.json"),
        serde_json::to_vec_pretty(&assembly.input)?,
    )?;
    fs::write(
        output.join("assembly-report.json"),
        serde_json::to_vec_pretty(&assembly.report)?,
    )?;
    assembly.catalog.store(output.join("catalog.json"))?;
    println!("{}", serde_json::to_string_pretty(&assembly.report)?);
    Ok(())
}
