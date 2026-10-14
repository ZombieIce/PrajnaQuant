//! Test-only Rust Decision oracle for the two checked-in synthetic MVP-5 fixtures.
use std::{collections::BTreeSet, error::Error, path::Path};

use prajna_data::{NormalizerRegistry, RawStore, SourceKind, SourceRecordInput, publish_dataset};
use prajna_domain::VenueId;
use prajna_research::{
    StaticUniverse,
    factor::{AvailabilityAssumption, Factor, FactorKind, RotationScoreParams},
    load_panel,
    strategy::{
        Decision, MaCrossoverParams, RankDirection, StrategyCapability, TopKRank, VectorStrategy,
        Weighting,
    },
};
use serde_json::{Value, json};

fn decisions_json(decisions: Vec<Decision>) -> Value {
    Value::Array(
        decisions
            .into_iter()
            .map(|d| {
                json!({
                    "session_date": d.decision_session.session_date.to_string(),
                    "targets": d.targets.into_iter().map(|(id, w)| (id.to_string(), w))
                        .collect::<std::collections::BTreeMap<_, _>>(),
                    "ranked": d.ranked.into_iter().map(|(id, score, rank)|
                        (id.to_string(), score, rank)).collect::<Vec<_>>(),
                })
            })
            .collect(),
    )
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 2 {
        return Err("usage: acceptance_fixture <lake> <dataset-v1|64x252>".into());
    }
    let (bytes, short, long, vol, rebalance_every): (&[u8], _, _, _, _) = match args[1].as_str() {
        "dataset-v1" => (
            include_bytes!("../../../poc/poc0-benchmark/fixtures/dataset-v1.json"),
            1,
            2,
            2,
            2,
        ),
        "64x252" => (
            include_bytes!("../../../poc/poc0-benchmark/fixtures/b2-s2-scale-64x252-v2.json"),
            5,
            40,
            10,
            5,
        ),
        _ => return Err("unknown acceptance fixture".into()),
    };
    let lake = Path::new(&args[0]);
    let store = RawStore::open(lake)?;
    let raw = store.put(
        bytes,
        SourceRecordInput {
            content_type: "application/json".into(),
            source_kind: SourceKind::Fixture,
            source_id: args[1].clone(),
            request: json!({}),
            observed_at: "synthetic".into(),
            ingested_by: "mvp5-acceptance".into(),
        },
    )?;
    let manifest = publish_dataset(
        lake,
        &store,
        &NormalizerRegistry::with_builtins(),
        "synthetic-etf-daily",
        "3",
        &[raw],
        "2026-10-14T00:00:00Z",
    )?;
    let panel = load_panel(lake, &manifest.dsv, &VenueId::new("SYNTH")?)?;
    let universe = StaticUniverse::new("all", panel.instruments.clone())?;
    universe.store(lake)?;
    let factor_params = RotationScoreParams {
        short,
        long,
        vol,
        w_s: 1.0,
        w_l: 1.0,
        w_v: 1.0,
        trend: None,
    };
    let mut cases = Vec::new();
    for top_k in [1, 3] {
        let strategy = VectorStrategy::TopKRank(TopKRank {
            score: Factor::new(FactorKind::RotationScore, factor_params.clone())?,
            direction: RankDirection::Descending,
            top_k,
            rebalance_every,
            weighting: Weighting::EqualWeight,
            unfilled_entry: Default::default(),
            capabilities: BTreeSet::from([StrategyCapability::Vectorizable]),
        });
        cases.push(json!({
            "params": {"short": short, "long": long, "vol": vol,
                "w_s": 1.0, "w_l": 1.0, "w_v": 1.0, "trend": null,
                "top_k": top_k, "rebalance_every": rebalance_every},
            "decisions": decisions_json(strategy.decide_panel(
                &panel, universe.members(), AvailabilityAssumption::None)?),
        }));
    }
    let ma = VectorStrategy::MaCrossover(MaCrossoverParams {
        params_version: 1,
        short_window: 20,
        long_window: 60,
    });
    let definition = json!({
        "experiment_version": 1, "dsv": manifest.dsv, "universe": universe.id()?,
        "venue": "SYNTH", "strategy": "s2_rotation", "engine": "vector",
        "availability_assumption": "none", "sessions_per_year": 252,
        "costs": {"commission_rate": 0.001, "buy_slippage_bps": 10.0,
            "sell_slippage_bps": 10.0, "buy_tax_rate": 0.0, "sell_tax_rate": 0.0},
        "parameter_space": {"list": cases.iter().map(|c| c["params"].clone()).collect::<Vec<_>>()},
    });
    println!(
        "{}",
        json!({
            "definition": definition, "members": universe.members(),
            "session_count": panel.sessions.len(), "s2": cases,
            "s3": decisions_json(ma.decide_panel(
                &panel, universe.members(), AvailabilityAssumption::None)?),
        })
    );
    Ok(())
}
