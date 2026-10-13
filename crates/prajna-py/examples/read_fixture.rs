use std::{fs::File, path::PathBuf};

use polars_io::prelude::{ParquetCompression, ParquetWriter};
use prajna_data::{NormalizerRegistry, RawStore, SourceKind, SourceRecordInput, publish_dataset};
use prajna_domain::VenueId;
use prajna_research::{
    StaticUniverse,
    cache::key::FactorKey,
    compute,
    factor::{
        AvailabilityAssumption, Factor, FactorKind, MomentumParams, RotationScoreParams,
        TrendFilterParams, VolatilityParams,
    },
    load_panel,
};
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let lake = PathBuf::from(
        std::env::args()
            .nth(1)
            .ok_or("usage: read_fixture <lake>")?,
    );
    let store = RawStore::open(&lake)?;
    let mut fixture: serde_json::Value = serde_json::from_slice(include_bytes!(
        "../../../poc/poc0-benchmark/fixtures/dataset-v1.json"
    ))?;
    fixture["execution_status_default"] = json!({
        "trade_status": "TRADABLE", "is_tradable": true,
        "sources": "python-read-fixture", "available_at": "08:50:00+08:00"
    });
    let raw = store.put(
        &serde_json::to_vec(&fixture)?,
        SourceRecordInput {
            content_type: "application/json".into(),
            source_kind: SourceKind::Fixture,
            source_id: "dataset-v1".into(),
            request: json!({}),
            observed_at: "synthetic".into(),
            ingested_by: "python-read-test".into(),
        },
    )?;
    let manifest = publish_dataset(
        &lake,
        &store,
        &NormalizerRegistry::with_builtins(),
        "synthetic-etf-daily",
        "3",
        &[raw],
        "2026-10-13T00:00:00Z",
    )?;
    let mut panel = load_panel(&lake, &manifest.dsv, &VenueId::new("SYNTH")?)?;
    let universe = StaticUniverse::new("all", panel.instruments.clone())?;
    universe.store(&lake)?;
    let subset = StaticUniverse::new("first", vec![panel.instruments[0].clone()])?;
    subset.store(&lake)?;
    ParquetWriter::new(File::create(lake.join("panel.parquet"))?)
        .with_compression(ParquetCompression::Uncompressed)
        .finish(&mut panel.grid)?;
    let factors = [
        Factor::new(FactorKind::Momentum, MomentumParams { n: 2 })?,
        Factor::new(FactorKind::Volatility, VolatilityParams { n: 2 })?,
        Factor::new(FactorKind::TrendFilter, TrendFilterParams { n: 2 })?,
        Factor::new(
            FactorKind::RotationScore,
            RotationScoreParams {
                short: 2,
                long: 3,
                vol: 2,
                w_s: 1.0,
                w_l: 1.0,
                w_v: 1.0,
                trend: None,
            },
        )?,
    ];
    let mut keys = serde_json::Map::new();
    for factor in factors {
        let mut values = compute(
            &panel,
            universe.members(),
            &factor,
            AvailabilityAssumption::None,
        )?;
        ParquetWriter::new(File::create(
            lake.join(format!("{}.parquet", factor.kind())),
        )?)
        .with_compression(ParquetCompression::Uncompressed)
        .finish(&mut values)?;
        let key = FactorKey::for_tree(
            &factor,
            &manifest.dsv,
            &universe.id()?,
            AvailabilityAssumption::None,
        );
        keys.insert(factor.kind().to_string(), json!(key.as_str()));
    }
    std::fs::write(
        lake.join("experiment.json"),
        serde_json::to_vec(&json!({
            "experiment_version": 1, "dsv": manifest.dsv, "universe": universe.id()?,
            "venue": "SYNTH", "strategy": "s2_rotation", "engine": "vector",
            "availability_assumption": "none", "sessions_per_year": 252,
            "costs": {"commission_rate": 0.001, "buy_slippage_bps": 10.0,
                "sell_slippage_bps": 10.0, "buy_tax_rate": 0.0, "sell_tax_rate": 0.0},
            "parameter_space": {"grid": {
                "short": [2], "long": [3], "vol": [2], "w_s": [1.0],
                "w_l": [1.0], "w_v": [1.0], "trend": [null], "top_k": [1, 2],
                "rebalance_every": [2]
            }}
        }))?,
    )?;
    println!(
        "{}",
        json!({"dsv": manifest.dsv, "universe": universe.id()?,
            "subset": subset.id()?, "factor_keys": keys})
    );
    Ok(())
}
