use std::{fs, path::Path};

use prajna_data::{NormalizerRegistry, RawStore, SourceKind, SourceRecordInput, publish_dataset};
use prajna_domain::VenueId;
use prajna_research::{StaticUniverse, load_panel};
use serde_json::{Value, json};

const FIXTURE: &[u8] =
    include_bytes!("../../../../poc/poc0-benchmark/fixtures/b2-s2-scale-64x252-v2.json");

/// Publish the checked-in 64 × 252 v2 fixture and save its Static Universe and definition.
pub fn prepare(lake: &Path, definition_path: &Path) -> Result<Value, Box<dyn std::error::Error>> {
    let store = RawStore::open(lake)?;
    let raw_hash = store.put(
        FIXTURE,
        SourceRecordInput {
            content_type: "application/json".into(),
            source_kind: SourceKind::Fixture,
            source_id: "b2-s2-scale-64x252-v2".into(),
            request: json!({}),
            observed_at: "synthetic".into(),
            ingested_by: "prajna-experiment-fixture-example".into(),
        },
    )?;
    let manifest = publish_dataset(
        lake,
        &store,
        &NormalizerRegistry::with_builtins(),
        "synthetic-etf-daily",
        "3",
        &[raw_hash],
        "2026-10-03T00:00:00Z",
    )?;
    let panel = load_panel(lake, &manifest.dsv, &VenueId::new("SYNTH")?)?;
    let universe = StaticUniverse::new("all", panel.instruments)?;
    universe.store(lake)?;
    let definition = json!({
        "experiment_version": 1,
        "dsv": manifest.dsv,
        "universe": universe.id()?,
        "venue": "SYNTH", "strategy": "s2_rotation", "engine": "vector",
        "availability_assumption": "none", "sessions_per_year": 252,
        "costs": {"commission_rate": 0.001, "buy_slippage_bps": 10.0,
            "sell_slippage_bps": 10.0, "buy_tax_rate": 0.0, "sell_tax_rate": 0.0},
        "parameter_space": {"grid": {
            "short": [5], "long": [40], "vol": [10],
            "w_s": [1.0], "w_l": [1.0], "w_v": [1.0],
            "trend": [null], "top_k": [1, 3], "rebalance_every": [5]
        }}
    });
    fs::write(definition_path, serde_json::to_vec_pretty(&definition)?)?;
    Ok(definition)
}
