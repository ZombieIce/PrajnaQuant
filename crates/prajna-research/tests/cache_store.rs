mod common;

use std::{collections::BTreeMap, fs};

use polars::prelude::{NamedFrom, Series};
use prajna_domain::VenueId;
use prajna_research::{
    Panel, StaticUniverse,
    cache::{
        key::FactorKey,
        store::{CacheError, FactorCache},
    },
    factor::{AvailabilityAssumption, Factor, FactorKind, MomentumParams, RotationScoreParams},
    load_panel,
};

fn rotation_score() -> Factor {
    Factor::new(
        FactorKind::RotationScore,
        RotationScoreParams {
            short: 1,
            long: 1,
            vol: 2,
            w_s: 1.0,
            w_l: 1.0,
            w_v: 0.0,
            trend: Some(2),
        },
    )
    .unwrap()
}

fn parquet_files(lake_root: &std::path::Path) -> BTreeMap<String, Vec<u8>> {
    fs::read_dir(lake_root.join("features"))
        .unwrap()
        .map(Result::unwrap)
        .filter(|entry| entry.file_type().unwrap().is_dir() && entry.file_name() != ".staging")
        .map(|entry| {
            let path = entry.path().join("values.parquet");
            (
                entry.file_name().to_string_lossy().into_owned(),
                fs::read(path).unwrap(),
            )
        })
        .collect()
}

fn fixture() -> (tempfile::TempDir, String, Panel, StaticUniverse, Factor) {
    let lake = tempfile::tempdir().unwrap();
    let dsv = common::publish_v1(lake.path());
    let panel = load_panel(lake.path(), &dsv, &VenueId::new("SYNTH").unwrap()).unwrap();
    let universe = StaticUniverse::new("all", panel.instruments.clone()).unwrap();
    (lake, dsv, panel, universe, rotation_score())
}

fn get_values(
    cache: &FactorCache,
    dsv: &str,
    panel: &Panel,
    universe: &StaticUniverse,
    factor: &Factor,
) -> Result<polars::prelude::DataFrame, CacheError> {
    cache.get_or_compute(
        panel,
        universe.members(),
        &universe.id().unwrap(),
        dsv,
        factor,
        AvailabilityAssumption::None,
    )
}

fn assert_staging_empty(lake_root: &std::path::Path) {
    assert!(
        fs::read_dir(lake_root.join("features/.staging"))
            .unwrap()
            .next()
            .is_none()
    );
}

fn momentum() -> Factor {
    Factor::new(FactorKind::Momentum, MomentumParams { n: 1 }).unwrap()
}

fn race(
    lake_root: &std::path::Path,
    dsv: &str,
    universe: &StaticUniverse,
    factor: &Factor,
    panels: &[&Panel],
) -> Vec<Result<polars::prelude::DataFrame, CacheError>> {
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(panels.len()));
    std::thread::scope(|scope| {
        let handles = panels
            .iter()
            .map(|panel| {
                let barrier = barrier.clone();
                scope.spawn(move || {
                    let cache = FactorCache::open(lake_root).unwrap();
                    barrier.wait();
                    get_values(&cache, dsv, panel, universe, factor)
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect()
    })
}

#[test]
fn computes_each_unique_factor_once_then_reuses_immutable_parquet() {
    let (lake, dsv, panel, universe, factor) = fixture();
    let universe_id = universe.id().unwrap();
    let assumption = AvailabilityAssumption::None;
    let cache = FactorCache::open(lake.path()).unwrap();

    let first = get_values(&cache, &dsv, &panel, &universe, &factor).unwrap();
    assert_eq!(cache.compute_count(), 4);
    assert_eq!(
        first.schema().as_ref(),
        &prajna_research::factor::values_schema().polars
    );
    let root_key = FactorKey::for_tree(&factor, &dsv, &universe_id, assumption);
    let root_hex = root_key.as_str().strip_prefix("fv:sha256:").unwrap();
    let first_root_bytes = fs::read(
        lake.path()
            .join("features")
            .join(root_hex)
            .join("values.parquet"),
    )
    .unwrap();
    let first_files = parquet_files(lake.path());
    assert_eq!(first_files.len(), 4);

    let second = get_values(&cache, &dsv, &panel, &universe, &factor).unwrap();

    assert_eq!(cache.compute_count(), 4);
    assert!(first.equals_missing(&second));
    assert_eq!(
        fs::read(
            lake.path()
                .join("features")
                .join(root_hex)
                .join("values.parquet")
        )
        .unwrap(),
        first_root_bytes
    );
    assert_eq!(parquet_files(lake.path()), first_files);
    assert_staging_empty(lake.path());
}

#[test]
fn rejects_a_values_file_whose_bytes_changed_without_recomputing() {
    let (lake, dsv, panel, universe, factor) = fixture();
    let cache = FactorCache::open(lake.path()).unwrap();
    get_values(&cache, &dsv, &panel, &universe, &factor).unwrap();
    let key = FactorKey::for_tree(
        &factor,
        &dsv,
        &universe.id().unwrap(),
        AvailabilityAssumption::None,
    );
    let hex = key.as_str().strip_prefix("fv:sha256:").unwrap();
    let values_path = lake
        .path()
        .join("features")
        .join(hex)
        .join("values.parquet");
    let mut bytes = fs::read(&values_path).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 1;
    fs::write(values_path, bytes).unwrap();

    assert!(matches!(
        get_values(&cache, &dsv, &panel, &universe, &factor),
        Err(CacheError::Integrity { .. })
    ));
    assert_eq!(cache.compute_count(), 4);
    assert_staging_empty(lake.path());
}

#[test]
fn rejects_metadata_with_a_different_factor_key_without_recomputing() {
    let (lake, dsv, panel, universe, factor) = fixture();
    let cache = FactorCache::open(lake.path()).unwrap();
    get_values(&cache, &dsv, &panel, &universe, &factor).unwrap();
    let key = FactorKey::for_tree(
        &factor,
        &dsv,
        &universe.id().unwrap(),
        AvailabilityAssumption::None,
    );
    let hex = key.as_str().strip_prefix("fv:sha256:").unwrap();
    let metadata_path = lake.path().join("features").join(hex).join("meta.json");
    let mut metadata: serde_json::Value =
        serde_json::from_slice(&fs::read(&metadata_path).unwrap()).unwrap();
    metadata["fv"] = serde_json::Value::String("fv:sha256:wrong".into());
    fs::write(metadata_path, serde_json::to_vec(&metadata).unwrap()).unwrap();

    assert!(matches!(
        get_values(&cache, &dsv, &panel, &universe, &factor),
        Err(CacheError::Integrity { .. })
    ));
    assert_eq!(cache.compute_count(), 4);
    assert_staging_empty(lake.path());
}

#[test]
fn concurrent_writers_reuse_the_same_logical_result() {
    let (lake, dsv, panel, universe, _) = fixture();
    let factor = momentum();
    let same_panels = vec![&panel; 8];
    let results = race(lake.path(), &dsv, &universe, &factor, &same_panels);
    assert!(results.iter().all(Result::is_ok));
    assert_staging_empty(lake.path());
}

#[test]
fn concurrent_writers_reject_different_logical_results() {
    let (lake, dsv, panel, universe, _) = fixture();
    let factor = momentum();
    let mut different_panel =
        load_panel(lake.path(), &dsv, &VenueId::new("SYNTH").unwrap()).unwrap();
    let close_column = different_panel.grid.column("close").unwrap().f64().unwrap();
    let mut closes = (0..close_column.len())
        .map(|index| close_column.get(index))
        .collect::<Vec<_>>();
    for close in closes.iter_mut().flatten() {
        *close += 5.0;
    }
    different_panel
        .grid
        .with_column(Series::new("close".into(), closes).into())
        .unwrap();

    let different_panels = [
        &panel,
        &different_panel,
        &panel,
        &different_panel,
        &panel,
        &different_panel,
        &panel,
        &different_panel,
    ];
    let results = race(lake.path(), &dsv, &universe, &factor, &different_panels);
    assert!(results.iter().any(Result::is_ok));
    assert!(
        results
            .iter()
            .any(|result| matches!(result, Err(CacheError::Conflict { .. })))
    );
    assert_staging_empty(lake.path());

    let expected_key = FactorKey::for_tree(
        &factor,
        &dsv,
        &universe.id().unwrap(),
        AvailabilityAssumption::None,
    );
    assert!(
        lake.path()
            .join("features")
            .join(expected_key.as_str().strip_prefix("fv:sha256:").unwrap())
            .join("meta.json")
            .exists()
    );
}
