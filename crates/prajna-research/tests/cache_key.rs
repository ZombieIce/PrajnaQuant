use serde_json::json;

use prajna_research::{
    cache::key::{FactorKey, POLARS_VERSION},
    factor::{
        AvailabilityAssumption, Factor, FactorKind, MomentumParams, RotationScoreParams,
        TrendFilterParams, TrendFilterStrictParams,
    },
};

fn momentum(n: u32) -> Factor {
    Factor::new(FactorKind::Momentum, MomentumParams { n }).unwrap()
}

fn score(short: u32) -> Factor {
    Factor::new(
        FactorKind::RotationScore,
        RotationScoreParams {
            short,
            long: 10,
            vol: 5,
            w_s: 1.0,
            w_l: 0.5,
            w_v: 0.2,
            trend: None,
        },
    )
    .unwrap()
}

fn key(
    factor: &Factor,
    dsv: &str,
    universe: &str,
    assumption: AvailabilityAssumption,
) -> FactorKey {
    FactorKey::new(factor, &[], dsv, universe, assumption)
}

#[test]
fn factor_cache_key_is_stable_and_tracks_each_input_identity() {
    let root = score(3);
    let baseline = key(&root, "dsv:one", "uni:one", AvailabilityAssumption::None);
    assert_eq!(
        baseline,
        key(&root, "dsv:one", "uni:one", AvailabilityAssumption::None)
    );

    let different_factor = key(
        &score(4),
        "dsv:one",
        "uni:one",
        AvailabilityAssumption::None,
    );
    let different_dsv = key(&root, "dsv:two", "uni:one", AvailabilityAssumption::None);
    let different_universe = key(&root, "dsv:one", "uni:two", AvailabilityAssumption::None);
    let different_assumption = key(
        &root,
        "dsv:one",
        "uni:one",
        AvailabilityAssumption::TreatUnknownAsTsClose,
    );
    let dep_one = key(
        &momentum(3),
        "dsv:one",
        "uni:one",
        AvailabilityAssumption::None,
    );
    let dep_two = key(
        &momentum(4),
        "dsv:one",
        "uni:one",
        AvailabilityAssumption::None,
    );
    let with_dep_one = FactorKey::new(
        &root,
        &[("momentum_short", dep_one)],
        "dsv:one",
        "uni:one",
        AvailabilityAssumption::None,
    );
    let with_dep_two = FactorKey::new(
        &root,
        &[("momentum_short", dep_two)],
        "dsv:one",
        "uni:one",
        AvailabilityAssumption::None,
    );

    for changed in [
        &different_factor,
        &different_dsv,
        &different_universe,
        &different_assumption,
        &with_dep_two,
    ] {
        assert_ne!(&baseline, changed);
    }
    assert_ne!(with_dep_one, with_dep_two);
}

#[test]
fn factor_cache_key_uses_versioned_hash_string() {
    let value = key(
        &momentum(3),
        "dsv:one",
        "uni:one",
        AvailabilityAssumption::None,
    );
    let key = value.as_str();
    assert!(key.starts_with("fv:sha256:"));
    let digest = key.strip_prefix("fv:sha256:").unwrap();
    assert_eq!(digest.len(), 64);
    assert!(
        digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );
}

#[test]
fn polars_version_matches_the_locked_dependency() {
    let lock = include_str!("../../../Cargo.lock");
    let locked_version = lock
        .split("[[package]]")
        .filter_map(|package| {
            let mut lines = package.lines();
            let name = lines.find(|line| line.starts_with("name = "))?;
            (name == "name = \"polars\"")
                .then(|| lines.find(|line| line.starts_with("version = ")))
                .flatten()
                .and_then(|line| line.strip_prefix("version = \"")?.strip_suffix('"'))
        })
        .next()
        .expect("Cargo.lock contains the polars package");
    assert_eq!(POLARS_VERSION, locked_version);
}

#[test]
fn strict_trend_has_its_own_cache_key_and_leaves_trend_filter_v1_key_unchanged() {
    let trend = Factor::new(FactorKind::TrendFilter, TrendFilterParams { n: 60 }).unwrap();
    let strict = Factor::new(
        FactorKind::TrendFilterStrict,
        TrendFilterStrictParams { n: 60 },
    )
    .unwrap();
    let trend_key = key(&trend, "dsv:one", "uni:one", AvailabilityAssumption::None);
    let strict_key = key(&strict, "dsv:one", "uni:one", AvailabilityAssumption::None);
    assert_ne!(trend_key, strict_key);
    assert_eq!(
        trend_key.key_object()["factor"],
        json!({"kind": "trend_filter", "version": "1", "params": {"n": 60}})
    );
    assert_eq!(
        strict_key.key_object()["factor"],
        json!({"kind": "trend_filter_strict", "version": "1", "params": {"n": 60}})
    );
}
