use std::{fs, path::Path};

use arrow_array::{Array, Decimal128Array};
use prajna_data::{
    Manifest, NormalizationIssue, NormalizationOutput, Normalizer, NormalizerRegistry,
    PublishError, RawInput, RawStore, SourceKind, SourceRecordInput,
    execution_status_from_record_batch, publish_dataset, read_parquet, rebuild_dataset,
};
use prajna_domain::TimestampNs;
use serde_json::{Value, json};
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

fn assert_invalid_v3_fixture_has_no_published_files(bytes: &[u8], expected_code: &str) {
    let root = tempfile::tempdir().unwrap();
    let store = RawStore::open(root.path()).unwrap();
    let raw_hash = store.put(bytes, source_record()).unwrap();
    let error = publish_dataset(
        root.path(),
        &store,
        &NormalizerRegistry::with_builtins(),
        "synthetic-etf-daily",
        "3",
        &[raw_hash],
        "2026-09-30T00:00:00Z",
    )
    .unwrap_err();
    let issues = match error {
        PublishError::Validation(issues) => issues,
        other => panic!("expected structured validation error, got {other}"),
    };
    assert!(
        issues.iter().any(|issue| issue.code == expected_code),
        "expected issue {expected_code:?}, got {issues:?}"
    );
    assert!(!root.path().join("normalized").exists());
    assert!(!root.path().join("manifests").exists());
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

    assert_eq!(
        v1.dsv,
        "dsv:sha256:50b995757c7c66ce4848a332ace8b488cd34ab9acfd360821e53fe210838d8d0"
    );
    assert_eq!(
        v2.dsv,
        "dsv:sha256:51d32697cdc4788d26ab0777e52751ab0a5482fe1f7deedd4e9e33cfef66063d"
    );
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

#[test]
fn v3_publishes_execution_status_and_rebuilds_without_changing_bar_identity() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let store = RawStore::open(root).unwrap();
    let raw_hash = store.put(FIXTURE, source_record()).unwrap();
    let registry = NormalizerRegistry::with_builtins();

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
    let v3 = publish_dataset(
        root,
        &store,
        &registry,
        "synthetic-etf-daily",
        "3",
        std::slice::from_ref(&raw_hash),
        "2026-09-30T00:00:00Z",
    )
    .unwrap();

    assert_ne!(v2.dsv, v3.dsv);
    assert_ne!(
        v2.core.normalizer.config_sha256,
        v3.core.normalizer.config_sha256
    );
    assert_eq!(table_hash(&v2, "bars"), table_hash(&v3, "bars"));
    assert_eq!(
        v3.core
            .tables
            .iter()
            .map(|table| table.table.as_str())
            .collect::<Vec<_>>(),
        vec!["bars", "execution_status", "instruments", "sessions"]
    );
    assert_eq!(v3.provenance.files.len(), 5);
    assert!(
        !v3.provenance
            .ignored_input_fields
            .contains(&"timezone".into())
    );
    assert!(
        !v3.provenance
            .ignored_input_fields
            .contains(&"execution_status_default".into())
    );
    assert!(
        !v3.provenance
            .ignored_input_fields
            .contains(&"execution_status_overrides".into())
    );

    let dsv_hex = v3.dsv.strip_prefix("dsv:sha256:").unwrap();
    let status_path = root
        .join("normalized/execution_status")
        .join(dsv_hex)
        .join("part-00000.parquet");
    let rows = read_parquet(status_path, "execution_status", "1")
        .unwrap()
        .iter()
        .flat_map(|batch| execution_status_from_record_batch(batch).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 30);
    assert!(rows.iter().all(|row| {
        row.available_at
            == Some(TimestampNs::parse(&format!("{}T08:50:00+08:00", row.session_date)).unwrap())
    }));
    let c_override = rows
        .iter()
        .find(|row| {
            row.instrument_id.to_string() == "C.SYNTH"
                && row.session_date.to_string() == "2026-01-08"
        })
        .unwrap();
    assert_eq!(c_override.trade_status, "UNKNOWN");
    assert!(!c_override.is_tradable);
    assert_eq!(c_override.source, "poc0-fixture-override");
    let b_override = rows
        .iter()
        .find(|row| {
            row.instrument_id.to_string() == "B.SYNTH"
                && row.session_date.to_string() == "2026-01-13"
        })
        .unwrap();
    assert_eq!(b_override.trade_status, "HALTED");
    assert!(!b_override.is_tradable);
    assert_eq!(b_override.source, "poc0-fixture-override");
    assert_eq!(
        rows.iter()
            .filter(|row| row.trade_status == "TRADABLE" && row.is_tradable)
            .count(),
        28
    );
    assert!(rows.iter().any(|row| {
        row.instrument_id.to_string() == "B.SYNTH" && row.session_date.to_string() == "2026-01-16"
    }));

    let repeated = publish_dataset(
        root,
        &store,
        &registry,
        "synthetic-etf-daily",
        "3",
        &[raw_hash],
        "2026-09-30T00:00:00Z",
    )
    .unwrap();
    assert_eq!(repeated, v3);

    fs::remove_dir_all(root.join("normalized")).unwrap();
    assert_eq!(rebuild_dataset(&store, &registry, &v3).unwrap(), v3.dsv);
}

#[test]
fn v3_uses_instrument_opens_for_ohlc_rules() {
    let mut fixture: Value = serde_json::from_slice(FIXTURE).unwrap();
    fixture["instruments"][0]["opens"] = json!([
        99.0, 104.0, 97.0, 105.0, 98.0, 97.0, 99.0, 100.0, 103.0, 102.0
    ]);
    let bytes = serde_json::to_vec(&fixture).unwrap();
    let output = NormalizerRegistry::with_builtins()
        .get("synthetic-etf-daily", "3")
        .unwrap()
        .normalize(&[RawInput::from_bytes(bytes)])
        .unwrap();

    let bars = &output.tables.bars;
    let opens = bars
        .column(5)
        .as_any()
        .downcast_ref::<Decimal128Array>()
        .unwrap();
    let highs = bars
        .column(6)
        .as_any()
        .downcast_ref::<Decimal128Array>()
        .unwrap();
    let lows = bars
        .column(7)
        .as_any()
        .downcast_ref::<Decimal128Array>()
        .unwrap();
    let scale = 10_i128.pow(18);
    assert_eq!(opens.value(0), 99 * scale);
    assert_eq!(highs.value(0), 100 * scale);
    assert_eq!(lows.value(0), 99 * scale);
    assert_eq!(opens.value(1), 104 * scale);
    assert_eq!(highs.value(1), 104 * scale);
    assert_eq!(lows.value(1), 101 * scale);
}

#[test]
fn v3_invalid_opens_and_unknown_overrides_are_rejected_without_output() {
    let mut bad_opens: Value = serde_json::from_slice(FIXTURE).unwrap();
    bad_opens["instruments"][0]["opens"] = json!([99.0]);
    assert_invalid_v3_fixture_has_no_published_files(
        &serde_json::to_vec(&bad_opens).unwrap(),
        "invalid_opens_length",
    );

    let mut unknown_symbol: Value = serde_json::from_slice(FIXTURE).unwrap();
    unknown_symbol["execution_status_overrides"][0]["symbol"] = json!("MISSING");
    assert_invalid_v3_fixture_has_no_published_files(
        &serde_json::to_vec(&unknown_symbol).unwrap(),
        "execution_status_unknown_instrument",
    );

    let mut unknown_date: Value = serde_json::from_slice(FIXTURE).unwrap();
    unknown_date["execution_status_overrides"][0]["date"] = json!("2026-01-10");
    assert_invalid_v3_fixture_has_no_published_files(
        &serde_json::to_vec(&unknown_date).unwrap(),
        "execution_status_unknown_session",
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
