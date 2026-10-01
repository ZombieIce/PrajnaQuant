//! Publish the checked-in synthetic dataset-v1 fixture for cross-language tests.

use std::{env, fs, path::PathBuf};

use prajna_data::{NormalizerRegistry, RawStore, SourceKind, SourceRecordInput, publish_dataset};
use serde_json::json;

const FIXTURE: &[u8] = include_bytes!("../../../poc/poc0-benchmark/fixtures/dataset-v1.json");

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = PathBuf::from(
        env::args()
            .nth(1)
            .ok_or("usage: fixture_v1 <output-directory>")?,
    );
    fs::create_dir_all(&output)?;
    let store = RawStore::open(&output)?;
    // observed_at/published_at are fixed placeholders: this fixture feeds a
    // deterministic cross-language contract test, not a point-in-time dataset,
    // so the values only need to be stable, not real collection timestamps.
    let raw_hash = store.put(
        FIXTURE,
        SourceRecordInput {
            content_type: "application/json".into(),
            source_kind: SourceKind::Fixture,
            source_id: "poc0-dataset-v1".into(),
            request: json!({}),
            observed_at: "synthetic".into(),
            ingested_by: "fixture-v1-example".into(),
        },
    )?;
    let manifest = publish_dataset(
        &output,
        &store,
        &NormalizerRegistry::with_builtins(),
        "synthetic-etf-daily",
        "1",
        &[raw_hash],
        "2026-09-30T00:00:00Z",
    )?;
    let dsv = manifest
        .dsv
        .strip_prefix("dsv:sha256:")
        .ok_or("published manifest has an invalid DSV")?;
    println!(
        "manifest_path={}",
        output
            .join("manifests")
            .join(format!("{dsv}.json"))
            .display()
    );
    println!("dsv={}", manifest.dsv);
    Ok(())
}
