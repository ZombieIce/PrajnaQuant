use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::factor::{AvailabilityAssumption, Factor, VALUES_SCHEMA_VERSION};

const KEY_VERSION: u32 = 1;
pub const POLARS_VERSION: &str = "0.55.2";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactorKey {
    value: String,
    key_object: Value,
}

impl FactorKey {
    pub fn new(
        factor: &Factor,
        dep_keys: &[(&str, FactorKey)],
        dsv: &str,
        universe_id: &str,
        assumption: AvailabilityAssumption,
    ) -> Self {
        Self::with_versions(
            factor,
            dep_keys,
            dsv,
            universe_id,
            assumption,
            KEY_VERSION,
            VALUES_SCHEMA_VERSION,
        )
    }

    pub fn for_tree(
        root: &Factor,
        dsv: &str,
        universe_id: &str,
        assumption: AvailabilityAssumption,
    ) -> Self {
        fn build(
            factor: &Factor,
            dsv: &str,
            universe_id: &str,
            assumption: AvailabilityAssumption,
        ) -> FactorKey {
            let deps = factor
                .dependencies()
                .into_iter()
                .map(|(role, dependency)| (role, build(&dependency, dsv, universe_id, assumption)))
                .collect::<Vec<_>>();
            FactorKey::new(factor, &deps, dsv, universe_id, assumption)
        }

        build(root, dsv, universe_id, assumption)
    }

    pub fn as_str(&self) -> &str {
        &self.value
    }

    pub fn key_object(&self) -> &Value {
        &self.key_object
    }

    fn with_versions(
        factor: &Factor,
        dep_keys: &[(&str, FactorKey)],
        dsv: &str,
        universe_id: &str,
        assumption: AvailabilityAssumption,
        key_version: u32,
        values_schema_version: u32,
    ) -> Self {
        let mut deps = dep_keys
            .iter()
            .map(|(role, key)| (*role, key.as_str()))
            .collect::<Vec<_>>();
        deps.sort_unstable();

        let key_object = json!({
            "key_version": key_version,
            "factor": factor.canonical_json(),
            "deps": deps.into_iter().map(|(role, fv)| json!({"role": role, "fv": fv})).collect::<Vec<_>>(),
            "dsv": dsv,
            "universe": universe_id,
            "availability_assumption": assumption.canonical_str(),
            "implementation": {
                "prajna_research": env!("CARGO_PKG_VERSION"),
                "polars": POLARS_VERSION,
            },
            "values_schema_version": values_schema_version,
        });
        let canonical = prajna_data::restricted_jcs(&key_object)
            .expect("Factor cache key object uses only restricted JCS values");
        let digest = Sha256::digest(canonical);
        Self {
            value: format!("fv:sha256:{digest:x}"),
            key_object,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FactorKey, KEY_VERSION};
    use crate::factor::{
        AvailabilityAssumption, Factor, FactorKind, MomentumParams, VALUES_SCHEMA_VERSION,
    };
    use sha2::Digest;

    fn momentum(n: u32) -> Factor {
        Factor::new(FactorKind::Momentum, MomentumParams { n }).unwrap()
    }

    #[test]
    fn changing_any_cache_identity_input_changes_the_key() {
        let factor = momentum(3);
        let dependency = FactorKey::with_versions(
            &momentum(5),
            &[],
            "dsv",
            "universe",
            AvailabilityAssumption::None,
            KEY_VERSION,
            VALUES_SCHEMA_VERSION,
        );
        let changed_dependency = FactorKey::with_versions(
            &momentum(6),
            &[],
            "dsv",
            "universe",
            AvailabilityAssumption::None,
            KEY_VERSION,
            VALUES_SCHEMA_VERSION,
        );
        let key_for = |factor: &Factor,
                       dependency: &FactorKey,
                       dsv: &str,
                       universe: &str,
                       assumption: AvailabilityAssumption,
                       key_version: u32,
                       values_schema_version: u32| {
            FactorKey::with_versions(
                factor,
                &[("input", dependency.clone())],
                dsv,
                universe,
                assumption,
                key_version,
                values_schema_version,
            )
        };
        let baseline = key_for(
            &factor,
            &dependency,
            "dsv",
            "universe",
            AvailabilityAssumption::None,
            KEY_VERSION,
            VALUES_SCHEMA_VERSION,
        );
        let changed_inputs = [
            (
                "factor parameters",
                key_for(
                    &momentum(4),
                    &dependency,
                    "dsv",
                    "universe",
                    AvailabilityAssumption::None,
                    KEY_VERSION,
                    VALUES_SCHEMA_VERSION,
                ),
            ),
            (
                "dependency parameters",
                key_for(
                    &factor,
                    &changed_dependency,
                    "dsv",
                    "universe",
                    AvailabilityAssumption::None,
                    KEY_VERSION,
                    VALUES_SCHEMA_VERSION,
                ),
            ),
            (
                "DSV",
                key_for(
                    &factor,
                    &dependency,
                    "other-dsv",
                    "universe",
                    AvailabilityAssumption::None,
                    KEY_VERSION,
                    VALUES_SCHEMA_VERSION,
                ),
            ),
            (
                "universe",
                key_for(
                    &factor,
                    &dependency,
                    "dsv",
                    "other-universe",
                    AvailabilityAssumption::None,
                    KEY_VERSION,
                    VALUES_SCHEMA_VERSION,
                ),
            ),
            (
                "availability assumption",
                key_for(
                    &factor,
                    &dependency,
                    "dsv",
                    "universe",
                    AvailabilityAssumption::TreatUnknownAsTsClose,
                    KEY_VERSION,
                    VALUES_SCHEMA_VERSION,
                ),
            ),
            (
                "key version",
                key_for(
                    &factor,
                    &dependency,
                    "dsv",
                    "universe",
                    AvailabilityAssumption::None,
                    KEY_VERSION + 1,
                    VALUES_SCHEMA_VERSION,
                ),
            ),
            (
                "values schema version",
                key_for(
                    &factor,
                    &dependency,
                    "dsv",
                    "universe",
                    AvailabilityAssumption::None,
                    KEY_VERSION,
                    VALUES_SCHEMA_VERSION + 1,
                ),
            ),
        ];

        for (input, changed) in changed_inputs {
            assert_ne!(baseline, changed, "changing {input} must change the key");
        }
    }

    #[test]
    fn rotation_score_key_does_not_collide_with_version_one_semantics() {
        let factor = Factor::new(
            FactorKind::RotationScore,
            crate::factor::RotationScoreParams {
                short: 3,
                long: 10,
                vol: 5,
                w_s: 1.0,
                w_l: 1.0,
                w_v: 1.0,
                trend: None,
            },
        )
        .unwrap();
        let key = FactorKey::with_versions(
            &factor,
            &[],
            "dsv",
            "universe",
            AvailabilityAssumption::None,
            KEY_VERSION,
            VALUES_SCHEMA_VERSION,
        );

        // Version "1" cached null-status rows with a non-null available_at.
        let mut legacy = key.key_object().clone();
        legacy["factor"]["version"] = serde_json::json!("1");
        let canonical = prajna_data::restricted_jcs(&legacy).unwrap();
        let legacy_key = format!("fv:sha256:{:x}", sha2::Sha256::digest(canonical));

        assert_eq!(key.key_object()["factor"]["version"], "2");
        assert_ne!(key.as_str(), legacy_key);
    }

    #[test]
    fn for_tree_includes_roles_and_recursive_dependency_keys() {
        let factor = Factor::new(
            FactorKind::RotationScore,
            crate::factor::RotationScoreParams {
                short: 3,
                long: 10,
                vol: 5,
                w_s: 1.0,
                w_l: 0.5,
                w_v: 0.2,
                trend: Some(8),
            },
        )
        .unwrap();
        let key = FactorKey::for_tree(&factor, "dsv:one", "uni:one", AvailabilityAssumption::None);
        let dependencies = key.key_object()["deps"].as_array().unwrap();
        let roles = dependencies
            .iter()
            .map(|dependency| dependency["role"].as_str().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(
            roles,
            ["momentum_long", "momentum_short", "trend", "volatility"]
        );
        assert!(dependencies.iter().all(|dependency| {
            dependency["fv"]
                .as_str()
                .is_some_and(|value| value.starts_with("fv:sha256:"))
        }));
    }
}
