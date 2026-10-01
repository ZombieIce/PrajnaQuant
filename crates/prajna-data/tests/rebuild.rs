use std::{fs, path::Path};

use arrow_array::{Array, Decimal128Array};
use prajna_data::{
    Manifest, NormalizationIssue, NormalizationOutput, Normalizer, NormalizerRegistry, RawInput,
    RawStore, SourceKind, SourceRecordInput, publish_dataset, read_parquet, rebuild_dataset,
};
use serde_json::json;
use sha2::{Digest, Sha256};

const FIXTURE: &[u8] = include_bytes!("../../../poc/poc0-benchmark/fixtures/dataset-v1.json");

fn source_record() -> SourceRecordInput {
    SourceRecordInput {
        content_type: "application/json".into(),
        source_kind: SourceKind::Fixture,
        source_id: "poc0-dataset-v1".into(),
        request: json!({}),
        observed_at: "synthetic".into(),
        ingested_by: "rebuild-integration-test".into(),
    }
}

fn object_path(root: &Path, hash: &str) -> std::path::PathBuf {
    let hex = hash.strip_prefix("sha256:").unwrap();
    root.join("raw/sha256").join(&hex[..2]).join(hex)
}

fn source_path(root: &Path, hash: &str) -> std::path::PathBuf {
    let hex = hash.strip_prefix("sha256:").unwrap();
    root.join("raw-sources/sha256")
        .join(&hex[..2])
        .join(format!("{hex}.jsonl"))
}

fn read_volumes(root: &Path, dsv: &str) -> Vec<i128> {
    let hex = dsv.strip_prefix("dsv:sha256:").unwrap();
    let path = root
        .join("normalized/bars")
        .join(hex)
        .join("part-00000.parquet");
    read_parquet(path, "bars", "1")
        .unwrap()
        .into_iter()
        .flat_map(|batch| {
            let volume = batch
                .column(9)
                .as_any()
                .downcast_ref::<Decimal128Array>()
                .unwrap();
            (0..volume.len())
                .map(|index| volume.value(index))
                .collect::<Vec<_>>()
        })
        .collect()
}

fn table_hash<'a>(manifest: &'a prajna_data::Manifest, table: &str) -> &'a str {
    &manifest
        .core
        .tables
        .iter()
        .find(|entry| entry.table == table)
        .unwrap()
        .logical_hash
}

fn publish_v1(root: &Path) -> (RawStore, prajna_data::Manifest) {
    let store = RawStore::open(root).unwrap();
    let raw_hash = store.put(FIXTURE, source_record()).unwrap();
    let manifest = publish_dataset(
        root,
        &store,
        &NormalizerRegistry::with_builtins(),
        "synthetic-etf-daily",
        "1",
        &[raw_hash],
        "2026-09-30T00:00:00Z",
    )
    .unwrap();
    (store, manifest)
}

#[test]
fn raw_v1_v2_publish_rebuild_preserves_identity_and_provenance() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let store = RawStore::open(root).unwrap();
    let raw_hash = store.put(FIXTURE, source_record()).unwrap();
    let registry = NormalizerRegistry::with_builtins();

    let v1 = publish_dataset(
        root,
        &store,
        &registry,
        "synthetic-etf-daily",
        "1",
        std::slice::from_ref(&raw_hash),
        "2026-09-30T00:00:00Z",
    )
    .unwrap();
    let v2 = publish_dataset(
        root,
        &store,
        &registry,
        "synthetic-etf-daily",
        "2",
        std::slice::from_ref(&raw_hash),
        "2026-09-30T00:00:00Z",
    )
    .unwrap();

    assert_ne!(v1.dsv, v2.dsv);
    assert_eq!(
        table_hash(&v1, "instruments"),
        table_hash(&v2, "instruments")
    );
    assert_eq!(table_hash(&v1, "sessions"), table_hash(&v2, "sessions"));
    assert_ne!(table_hash(&v1, "bars"), table_hash(&v2, "bars"));
    let v1_volumes = read_volumes(root, &v1.dsv);
    let v2_volumes = read_volumes(root, &v2.dsv);
    assert_eq!(v1_volumes.len(), 29);
    assert_eq!(v2_volumes.len(), 29);
    assert!(
        v1_volumes
            .iter()
            .zip(&v2_volumes)
            .all(|(v1, v2)| *v2 == *v1 * 100)
    );

    let raw_before = fs::read(object_path(root, &raw_hash)).unwrap();
    let sources_before = fs::read(source_path(root, &raw_hash)).unwrap();
    assert_eq!(
        format!("sha256:{:x}", Sha256::digest(&raw_before)),
        raw_hash
    );
    fs::remove_dir_all(root.join("normalized")).unwrap();

    assert_eq!(rebuild_dataset(&store, &registry, &v1).unwrap(), v1.dsv);
    assert_eq!(rebuild_dataset(&store, &registry, &v2).unwrap(), v2.dsv);
    assert_eq!(fs::read(object_path(root, &raw_hash)).unwrap(), raw_before);
    assert_eq!(
        fs::read(source_path(root, &raw_hash)).unwrap(),
        sources_before
    );
}

struct ConflictingConfig;

impl Normalizer for ConflictingConfig {
    fn id(&self) -> &str {
        "synthetic-etf-daily"
    }

    fn version(&self) -> &str {
        "1"
    }

    fn config_sha256(&self) -> String {
        "sha256:conflicting-config".into()
    }

    fn normalize(
        &self,
        _raw_inputs: &[RawInput],
    ) -> Result<NormalizationOutput, Vec<NormalizationIssue>> {
        panic!("normalization must not run when the config hash differs");
    }
}

#[test]
fn rebuild_reports_missing_normalizer_and_config_hash_mismatch() {
    let temp = tempfile::tempdir().unwrap();
    let (store, manifest) = publish_v1(temp.path());

    let missing = rebuild_dataset(&store, &NormalizerRegistry::new(), &manifest).unwrap_err();
    assert!(missing.to_string().contains("Normalizer not registered"));

    let mut conflicting_registry = NormalizerRegistry::new();
    assert!(conflicting_registry.register(ConflictingConfig));
    let mismatch = rebuild_dataset(&store, &conflicting_registry, &manifest).unwrap_err();
    let message = mismatch.to_string();
    assert!(message.contains("config_sha256 mismatch"));
    assert!(message.contains(&manifest.core.normalizer.config_sha256));
}

#[test]
fn rebuild_reports_missing_and_corrupt_raw_objects() {
    let missing_temp = tempfile::tempdir().unwrap();
    let (missing_store, manifest) = publish_v1(missing_temp.path());
    fs::remove_file(object_path(
        missing_temp.path(),
        &manifest.core.inputs[0].raw_sha256,
    ))
    .unwrap();
    let missing = rebuild_dataset(
        &missing_store,
        &NormalizerRegistry::with_builtins(),
        &manifest,
    )
    .unwrap_err();
    assert!(missing.to_string().contains("Raw object is missing"));

    let corrupt_temp = tempfile::tempdir().unwrap();
    let (corrupt_store, manifest) = publish_v1(corrupt_temp.path());
    fs::write(
        object_path(corrupt_temp.path(), &manifest.core.inputs[0].raw_sha256),
        b"tampered raw bytes",
    )
    .unwrap();
    let corrupt = rebuild_dataset(
        &corrupt_store,
        &NormalizerRegistry::with_builtins(),
        &manifest,
    )
    .unwrap_err();
    assert!(corrupt.to_string().contains("raw object is corrupt"));
}

#[test]
fn rebuild_identifies_the_manifest_core_item_that_changed() {
    let temp = tempfile::tempdir().unwrap();
    let (store, manifest) = publish_v1(temp.path());
    let mut altered_core = manifest.core.clone();
    altered_core
        .tables
        .iter_mut()
        .find(|table| table.table == "bars")
        .unwrap()
        .logical_hash = "sha256:changed".into();
    let altered_manifest = Manifest::new(altered_core, manifest.provenance.clone()).unwrap();

    let error = rebuild_dataset(
        &store,
        &NormalizerRegistry::with_builtins(),
        &altered_manifest,
    )
    .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("tables.bars.logical_hash"));
    assert!(message.contains("expected"));
    assert!(message.contains("got"));
}
