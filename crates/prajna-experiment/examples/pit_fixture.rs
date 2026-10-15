//! Synthetic-only #178 Python/CLI fixture; shares the #177 hand-worked inputs.
#[path = "../../prajna-research/tests/common/pit.rs"]
#[allow(dead_code)]
mod fixture;
use serde_json::json;
fn main() {
    let mut args = std::env::args().skip(1);
    let root = std::path::PathBuf::from(args.next().expect("lake path"));
    std::fs::create_dir_all(&root).unwrap();
    let f = if args.next().as_deref() == Some("--terminated") {
        fixture::fixture_options(&root, 1.0, true)
    } else {
        fixture::fixture(&root)
    };
    println!(
        "{}",
        json!({
            "experiment_version":1, "dsv":f.dsv, "universe":f.catalog.version().unwrap(),
            "venue":"SZSE", "strategy":"s2_pit_rotation", "engine":"vector",
            "availability_assumption":"none", "sessions_per_year":252,
            "vector_pit":{"initial_capital":"10000"},
            "costs":{"commission_rate":0.0003,"buy_slippage_bps":2.0,"sell_slippage_bps":2.0,
                "buy_tax_rate":0.0,"sell_tax_rate":0.0},
            "parameter_space":{"list":[{"lookback":1,"top_k":1,"rebalance_every":2}]},
            "pit":f.input, "ids":f.ids,
        })
    );
}
