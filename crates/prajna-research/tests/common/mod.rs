use std::path::Path;

use prajna_data::{NormalizerRegistry, RawStore, SourceKind, SourceRecordInput, publish_dataset};

const FIXTURE: &[u8] = include_bytes!("../../../../poc/poc0-benchmark/fixtures/dataset-v1.json");

pub fn publish_v1(lake_root: &Path) -> String {
    publish_version(lake_root, "1")
}

pub fn publish_version(lake_root: &Path, version: &str) -> String {
    publish_fixture(lake_root, FIXTURE, version)
}

pub fn publish_fixture(lake_root: &Path, fixture: &[u8], version: &str) -> String {
    let store = RawStore::open(lake_root).unwrap();
    let raw_hash = store
        .put(
            fixture,
            SourceRecordInput {
                content_type: "application/json".into(),
                source_kind: SourceKind::Fixture,
                source_id: "poc0-dataset-v1".into(),
                request: Default::default(),
                observed_at: "synthetic".into(),
                ingested_by: "prajna-research-test".into(),
            },
        )
        .unwrap();
    publish_dataset(
        lake_root,
        &store,
        &NormalizerRegistry::with_builtins(),
        "synthetic-etf-daily",
        version,
        &[raw_hash],
        "2026-09-30T00:00:00Z",
    )
    .unwrap()
    .dsv
}
