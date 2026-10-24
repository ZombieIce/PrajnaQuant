//! Synthetic-only #178 Python/CLI fixture; shares the #177 hand-worked inputs.
#[path = "../../prajna-research/tests/common/pit.rs"]
#[allow(dead_code)]
mod fixture;
use prajna_data::RawStore;
use prajna_research::pit_universe::{CatalogInput, HistoricalCatalog};
use serde_json::{Value, json};

/// Rebuild the Catalog version when a test removes the broad-index list, so the
/// Experiment definition's Universe still equals the Catalog content identity.
fn without_broad_index(root: &std::path::Path, input: &mut Value) -> String {
    input["catalog"]["broad_indices"] = Value::Null;
    let catalog_input: CatalogInput = serde_json::from_value(input["catalog"].clone()).unwrap();
    let raw = RawStore::open(root).unwrap();
    HistoricalCatalog::build(catalog_input, &raw)
        .unwrap()
        .version()
        .unwrap()
}

fn main() {
    let mut args = std::env::args().skip(1);
    let root = std::path::PathBuf::from(args.next().expect("lake path"));
    std::fs::create_dir_all(&root).unwrap();
    let flags: Vec<String> = args.collect();
    let f = if flags.iter().any(|flag| flag == "--terminated") {
        fixture::fixture_options(&root, 1.0, true)
    } else {
        fixture::fixture(&root)
    };
    let mut input = f.input;
    let universe = if flags.iter().any(|flag| flag == "--no-broad-index") {
        without_broad_index(&root, &mut input)
    } else {
        f.catalog.version().unwrap()
    };
    println!(
        "{}",
        json!({
            "experiment_version":1, "dsv":f.dsv, "universe":universe,
            "venue":"SZSE", "strategy":"s2_pit_rotation", "engine":"vector",
            "availability_assumption":"none", "sessions_per_year":252,
            "vector_pit":{"initial_capital":"10000"},
            "costs":{"commission_rate":0.0003,"buy_slippage_bps":2.0,"sell_slippage_bps":2.0,
                "buy_tax_rate":0.0,"sell_tax_rate":0.0},
            "parameter_space":{"list":[{"lookback":1,"top_k":1,"rebalance_every":2}]},
            "pit":input, "ids":f.ids,
        })
    );
}
