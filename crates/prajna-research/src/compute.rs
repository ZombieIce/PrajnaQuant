use std::collections::BTreeMap;

use polars::prelude::DataFrame;
use prajna_domain::InstrumentId;

use crate::{
    Panel, compute_base, compute_composite,
    factor::{AvailabilityAssumption, Factor, FactorError, FactorGraph, FactorKind},
};

/// Evaluate the complete dependency DAG in topological order, without persistent caching.
/// Shared definitions are evaluated once per call, including zero-weight dependencies.
pub fn compute(
    panel: &Panel,
    members: &[InstrumentId],
    factor: &Factor,
    assumption: AvailabilityAssumption,
) -> Result<DataFrame, FactorError> {
    let mut computed = BTreeMap::<String, DataFrame>::new();
    for node in FactorGraph::resolve(factor.clone())? {
        let values = if node.kind() == FactorKind::RotationScore {
            let deps = node
                .dependencies()
                .into_iter()
                .map(|(role, dependency)| {
                    computed
                        .get(&dependency.canonical_json().to_string())
                        .cloned()
                        .map(|values| (role, values))
                        .ok_or_else(|| {
                            FactorError::Evaluation(format!("uncomputed dependency {role}"))
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            compute_composite(&node, &deps)?
        } else {
            compute_base(panel, members, &node, assumption)?
        };
        computed.insert(node.canonical_json().to_string(), values);
    }
    computed
        .remove(&factor.canonical_json().to_string())
        .ok_or_else(|| FactorError::Evaluation("factor graph did not produce its root".into()))
}
