use std::{fs, path::Path};

use polars::prelude::DataFrame;
use prajna_data::{NormalizerRegistry, RawStore, SourceKind, SourceRecordInput, publish_dataset};
use prajna_domain::VenueId;
use prajna_research::{
    Panel, StaticUniverse,
    cache::{key::FactorKey, store::FactorCache},
    executable,
    factor::{AvailabilityAssumption, Factor, FactorGraph, FactorKind, RotationScoreParams},
    load_execution_status, load_panel,
    strategy::{RankDirection, StrategyCapability, VectorStrategy, Weighting, execute},
    vector::{VectorCosts, run_vector},
};
use serde_json::Value;
use tempfile::TempDir;

const FIXTURE_V2: &[u8] =
    include_bytes!("../../../poc/poc0-benchmark/fixtures/b2-s2-scale-64x252-v2.json");

struct LargeFixture {
    lake: TempDir,
    dsv: String,
    panel: Panel,
    universe: StaticUniverse,
    universe_id: String,
    score: Factor,
}

struct CacheQuery<'a> {
    panel: &'a Panel,
    universe: &'a StaticUniverse,
    dsv: &'a str,
    score: &'a Factor,
    assumption: AvailabilityAssumption,
}

fn publish_v2(lake_root: &Path, source_identity: &str) -> String {
    let mut fixture: Value = serde_json::from_slice(FIXTURE_V2).unwrap();
    fixture["source_identity"] = Value::String(source_identity.to_owned());
    let bytes = serde_json::to_vec(&fixture).unwrap();

    let store = RawStore::open(lake_root).unwrap();
    let raw_hash = store
        .put(
            &bytes,
            SourceRecordInput {
                content_type: "application/json".into(),
                source_kind: SourceKind::Fixture,
                source_id: source_identity.into(),
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
        "3",
        &[raw_hash],
        "2026-09-30T00:00:00Z",
    )
    .unwrap()
    .dsv
}

fn large_fixture() -> LargeFixture {
    let lake = tempfile::tempdir().unwrap();
    let dsv = publish_v2(lake.path(), "cache-reuse-primary");
    let panel = load_panel(
        lake.path(),
        &dsv,
        &VenueId::new("SYNTH").expect("valid synthetic venue"),
    )
    .unwrap();
    assert_eq!(panel.instruments.len(), 64);
    assert_eq!(panel.sessions.len(), 252);

    let universe = StaticUniverse::new("all", panel.instruments.clone()).unwrap();
    universe.store(lake.path()).unwrap();
    let universe_id = universe.id().unwrap();
    let score = rotation_score(20, 60, 20, 1.0);

    LargeFixture {
        lake,
        dsv,
        panel,
        universe,
        universe_id,
        score,
    }
}

fn rotation_score(short: u32, long: u32, vol: u32, w_v: f64) -> Factor {
    Factor::new(
        FactorKind::RotationScore,
        RotationScoreParams {
            short,
            long,
            vol,
            w_s: 1.0,
            w_l: 1.0,
            w_v,
            trend: None,
        },
    )
    .unwrap()
}

fn cached_values(cache: &FactorCache, fixture: &LargeFixture, factor: &Factor) -> DataFrame {
    cache
        .get_or_compute(
            &fixture.panel,
            fixture.universe.members(),
            &fixture.universe_id,
            &fixture.dsv,
            factor,
            AvailabilityAssumption::None,
        )
        .unwrap()
}

fn values_path(lake_root: &Path, key: &FactorKey) -> std::path::PathBuf {
    let hex = key.as_str().strip_prefix("fv:sha256:").unwrap();
    lake_root.join("features").join(hex).join("values.parquet")
}

fn tree_keys(query: &CacheQuery<'_>) -> Vec<FactorKey> {
    let universe_id = query.universe.id().unwrap();
    FactorGraph::resolve(query.score.clone())
        .unwrap()
        .iter()
        .map(|factor| FactorKey::for_tree(factor, query.dsv, &universe_id, query.assumption))
        .collect()
}

fn assert_cache_misses(
    cache: &FactorCache,
    lake_root: &Path,
    query: &CacheQuery<'_>,
    expected_misses: &[FactorKey],
) {
    let keys = tree_keys(query);
    assert_eq!(keys.len(), 4);
    let root_key = FactorKey::for_tree(
        query.score,
        query.dsv,
        &query.universe.id().unwrap(),
        query.assumption,
    );
    assert!(
        expected_misses.contains(&root_key),
        "expected a root factor cache miss for {}",
        root_key.as_str()
    );
    assert!(
        expected_misses.iter().all(|key| keys.contains(key)),
        "every expected miss must belong to the requested factor tree"
    );
    for key in &keys {
        assert_eq!(
            values_path(lake_root, key).exists(),
            !expected_misses.contains(key),
            "unexpected cache state for {} before lookup",
            key.as_str()
        );
    }

    let count_before = cache.compute_count();
    cache
        .get_or_compute(
            query.panel,
            query.universe.members(),
            &query.universe.id().unwrap(),
            query.dsv,
            query.score,
            query.assumption,
        )
        .unwrap();
    assert_eq!(
        cache.compute_count() - count_before,
        expected_misses.len() as u64
    );
    for key in expected_misses {
        assert!(
            values_path(lake_root, key).exists(),
            "missing cache entry for {} after lookup",
            key.as_str()
        );
    }
}

fn assert_staging_empty(lake_root: &Path) {
    assert!(
        fs::read_dir(lake_root.join("features/.staging"))
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn rotation_score_cache_survives_reuse_and_reopen_without_mutating_parquet() {
    let fixture = large_fixture();
    let cache = FactorCache::open(fixture.lake.path()).unwrap();
    let expected_compute_count = FactorGraph::resolve(fixture.score.clone()).unwrap().len() as u64;
    assert_eq!(expected_compute_count, 4);

    let first = cached_values(&cache, &fixture, &fixture.score);
    assert_eq!(cache.compute_count(), expected_compute_count);
    let key = FactorKey::for_tree(
        &fixture.score,
        &fixture.dsv,
        &fixture.universe_id,
        AvailabilityAssumption::None,
    );
    let first_parquet = fs::read(values_path(fixture.lake.path(), &key)).unwrap();

    let second = cached_values(&cache, &fixture, &fixture.score);
    assert_eq!(cache.compute_count(), expected_compute_count);
    assert!(first.equals_missing(&second));

    let reopened = FactorCache::open(fixture.lake.path()).unwrap();
    let after_reopen = cached_values(&reopened, &fixture, &fixture.score);
    assert_eq!(reopened.compute_count(), 0);
    assert!(first.equals_missing(&after_reopen));
    assert_eq!(
        fs::read(values_path(fixture.lake.path(), &key)).unwrap(),
        first_parquet
    );
    assert_staging_empty(fixture.lake.path());
}

#[test]
fn factor_cache_keys_invalidate_only_the_changed_factor_subtree() {
    let fixture = large_fixture();
    let cache = FactorCache::open(fixture.lake.path()).unwrap();
    cached_values(&cache, &fixture, &fixture.score);
    assert_eq!(cache.compute_count(), 4);

    let changed_weight = rotation_score(20, 60, 20, 0.5);
    let changed_weight_query = CacheQuery {
        panel: &fixture.panel,
        universe: &fixture.universe,
        dsv: &fixture.dsv,
        score: &changed_weight,
        assumption: AvailabilityAssumption::None,
    };
    let changed_weight_root = FactorKey::for_tree(
        &changed_weight,
        &fixture.dsv,
        &fixture.universe_id,
        AvailabilityAssumption::None,
    );
    assert_cache_misses(
        &cache,
        fixture.lake.path(),
        &changed_weight_query,
        &[changed_weight_root],
    );

    let changed_dependency = rotation_score(21, 60, 20, 1.0);
    let changed_short_momentum = changed_dependency
        .dependencies()
        .into_iter()
        .find(|(role, _)| *role == "momentum_short")
        .unwrap()
        .1;
    let changed_dependency_keys = [
        FactorKey::for_tree(
            &changed_dependency,
            &fixture.dsv,
            &fixture.universe_id,
            AvailabilityAssumption::None,
        ),
        FactorKey::for_tree(
            &changed_short_momentum,
            &fixture.dsv,
            &fixture.universe_id,
            AvailabilityAssumption::None,
        ),
    ];
    let changed_dependency_query = CacheQuery {
        panel: &fixture.panel,
        universe: &fixture.universe,
        dsv: &fixture.dsv,
        score: &changed_dependency,
        assumption: AvailabilityAssumption::None,
    };
    assert_cache_misses(
        &cache,
        fixture.lake.path(),
        &changed_dependency_query,
        &changed_dependency_keys,
    );

    let other_dsv = publish_v2(fixture.lake.path(), "cache-reuse-second-dsv");
    assert_ne!(other_dsv, fixture.dsv);
    let other_panel = load_panel(
        fixture.lake.path(),
        &other_dsv,
        &VenueId::new("SYNTH").expect("valid synthetic venue"),
    )
    .unwrap();
    assert_eq!(other_panel.instruments, fixture.panel.instruments);
    assert_eq!(other_panel.sessions, fixture.panel.sessions);
    assert!(other_panel.grid.equals_missing(&fixture.panel.grid));
    let other_dsv_query = CacheQuery {
        panel: &other_panel,
        universe: &fixture.universe,
        dsv: &other_dsv,
        score: &fixture.score,
        assumption: AvailabilityAssumption::None,
    };
    let other_dsv_keys = tree_keys(&other_dsv_query);
    assert_cache_misses(
        &cache,
        fixture.lake.path(),
        &other_dsv_query,
        &other_dsv_keys,
    );

    let mut subset_members = fixture.universe.members().to_vec();
    subset_members.pop().expect("fixture universe has members");
    let subset = StaticUniverse::new("without-one", subset_members).unwrap();
    subset.store(fixture.lake.path()).unwrap();
    assert_ne!(subset.members().len(), fixture.universe.members().len());
    let subset_query = CacheQuery {
        panel: &fixture.panel,
        universe: &subset,
        dsv: &fixture.dsv,
        score: &fixture.score,
        assumption: AvailabilityAssumption::None,
    };
    let subset_keys = tree_keys(&subset_query);
    assert_cache_misses(&cache, fixture.lake.path(), &subset_query, &subset_keys);

    let assumption_query = CacheQuery {
        panel: &fixture.panel,
        universe: &fixture.universe,
        dsv: &fixture.dsv,
        score: &fixture.score,
        assumption: AvailabilityAssumption::TreatUnknownAsTsClose,
    };
    let assumption_keys = tree_keys(&assumption_query);
    assert_cache_misses(
        &cache,
        fixture.lake.path(),
        &assumption_query,
        &assumption_keys,
    );
    assert_staging_empty(fixture.lake.path());
}

#[test]
fn vector_strategy_grid_reuses_factor_values_across_all_six_runs() {
    let fixture = large_fixture();
    let statuses = load_execution_status(fixture.lake.path(), &fixture.dsv).unwrap();
    let cache = FactorCache::open(fixture.lake.path()).unwrap();
    let expected_compute_count = FactorGraph::resolve(fixture.score.clone()).unwrap().len() as u64;
    let costs = VectorCosts {
        commission_rate: 0.0,
        buy_slippage_bps: 0.0,
        sell_slippage_bps: 0.0,
        buy_tax_rate: 0.0,
        sell_tax_rate: 0.0,
    };

    for (run_index, top_k) in [1, 5, 10].into_iter().enumerate() {
        for rebalance_every in [1, 5] {
            let count_before = cache.compute_count();
            let values = cached_values(&cache, &fixture, &fixture.score);
            let expected_increment = if run_index == 0 && rebalance_every == 1 {
                expected_compute_count
            } else {
                0
            };
            assert_eq!(cache.compute_count() - count_before, expected_increment);

            let strategy = VectorStrategy {
                score: fixture.score.clone(),
                direction: RankDirection::Descending,
                top_k,
                rebalance_every,
                weighting: Weighting::EqualWeight,
                unfilled_entry: Default::default(),
                capabilities: [StrategyCapability::Vectorizable].into(),
            };
            let decisions = strategy.decide(&values, &fixture.panel.sessions).unwrap();
            let events = execute(
                &decisions,
                &fixture.panel.sessions,
                |instrument, session| {
                    executable(&fixture.panel, &statuses, instrument, session).unwrap()
                },
            );
            let result = run_vector(
                &fixture.panel,
                &decisions,
                &events,
                &costs,
                AvailabilityAssumption::None,
            )
            .unwrap();
            assert_eq!(result.sessions.len(), 252);
        }
    }

    assert_eq!(cache.compute_count(), expected_compute_count);
    assert_staging_empty(fixture.lake.path());
}
