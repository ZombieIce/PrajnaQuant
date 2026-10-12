use std::collections::BTreeSet;

use prajna_research::{
    engine::Engine,
    factor::{Factor, FactorKind, MomentumParams},
    fast_event::{EntryBudget, Rebalance, Sizing},
    strategy::{RankDirection, StrategyCapability, TopKRank, VectorStrategy, Weighting},
};

mod common;

fn strategy(capabilities: &[StrategyCapability]) -> VectorStrategy {
    VectorStrategy::TopKRank(TopKRank {
        score: Factor::new(FactorKind::Momentum, MomentumParams { n: 1 }).unwrap(),
        direction: RankDirection::Descending,
        top_k: 1,
        rebalance_every: 1,
        weighting: Weighting::EqualWeight,
        unfilled_entry: Default::default(),
        capabilities: capabilities.iter().copied().collect::<BTreeSet<_>>(),
    })
}

#[test]
fn event_driven_decisions_use_event_routing_without_weakening_vector() {
    use prajna_research::{factor::AvailabilityAssumption, load_panel};

    let lake = tempfile::tempdir().unwrap();
    let dsv = common::publish_v1(lake.path());
    let panel = load_panel(lake.path(), &dsv, &"SYNTH".parse().unwrap()).unwrap();
    let vector_strategy = strategy(&[StrategyCapability::Vectorizable]);
    let event_strategy = strategy(&[StrategyCapability::EventDriven]);
    let engine = Engine::FastEvent {
        sizing: Sizing::Lot,
        rebalance: Rebalance::FullTarget,
    };
    let assumption = AvailabilityAssumption::TreatUnknownAsTsClose;
    let expected = vector_strategy
        .decide_panel(&panel, &panel.instruments, assumption)
        .unwrap();
    assert!(!expected.is_empty());
    assert_eq!(
        engine
            .decide_panel(&event_strategy, &panel, &panel.instruments, assumption)
            .unwrap(),
        expected
    );
    let scores = prajna_research::compute(
        &panel,
        &panel.instruments,
        &Factor::new(FactorKind::Momentum, MomentumParams { n: 1 }).unwrap(),
        assumption,
    )
    .unwrap();
    assert_eq!(
        engine
            .decide(&event_strategy, &scores, &panel.sessions)
            .unwrap(),
        expected
    );
    assert!(Engine::Vector.validate(&event_strategy).is_err());
    assert!(event_strategy.decide(&scores, &panel.sessions).is_err());
    assert!(
        event_strategy
            .decide_panel(&panel, &panel.instruments, assumption)
            .is_err()
    );
    let unsupported = strategy(&[
        StrategyCapability::Vectorizable,
        StrategyCapability::OrderBookDriven,
    ]);
    assert!(
        engine
            .decide(&unsupported, &scores, &panel.sessions)
            .is_err()
    );
    assert!(
        engine
            .decide_panel(&unsupported, &panel, &panel.instruments, assumption)
            .is_err()
    );
}

#[test]
fn fast_event_requires_a_decision_capability_and_valid_sizing_mode() {
    use StrategyCapability::*;
    for sizing in [Sizing::Lot, Sizing::VectorParity] {
        for rebalance in [
            Rebalance::FullTarget,
            Rebalance::EntryOnly(EntryBudget::FixedInitial),
            Rebalance::EntryOnly(EntryBudget::RemainingCash),
        ] {
            let engine = Engine::FastEvent { sizing, rebalance };
            for capabilities in [
                vec![],
                vec![Vectorizable],
                vec![EventDriven],
                vec![Vectorizable, EventDriven],
                vec![OrderBookDriven],
                vec![Vectorizable, OrderBookDriven],
                vec![EventDriven, OrderBookDriven],
                vec![Vectorizable, EventDriven, OrderBookDriven],
            ] {
                let supported = !capabilities.is_empty()
                    && !capabilities.contains(&OrderBookDriven)
                    && (sizing == Sizing::Lot || rebalance == Rebalance::FullTarget);
                let result = engine.validate(&strategy(&capabilities));
                assert_eq!(
                    result.is_ok(),
                    supported,
                    "{engine:?} {capabilities:?}: {result:?}"
                );
                if let Err(error) = result {
                    let message = error.to_string();
                    assert!(message.contains("fast_event"), "{message}");
                    assert!(message.contains("top_k_rank"), "{message}");
                    assert!(message.contains("sizing="), "{message}");
                    assert!(message.contains("rebalance="), "{message}");
                }
            }
        }
    }
}

#[test]
fn fast_event_rejects_order_book_requirements_even_with_vectorizable() {
    let engine = Engine::FastEvent {
        sizing: Sizing::Lot,
        rebalance: Rebalance::FullTarget,
    };
    let error = engine
        .validate(&strategy(&[
            StrategyCapability::Vectorizable,
            StrategyCapability::OrderBookDriven,
        ]))
        .unwrap_err()
        .to_string();
    for part in [
        "fast_event",
        "top_k_rank",
        "OrderBookDriven",
        "lot",
        "full_target",
    ] {
        assert!(error.contains(part), "{error}");
    }
}

#[test]
fn accurate_event_registers_only_lot_decision_modes() {
    use StrategyCapability::*;
    for sizing in [Sizing::Lot, Sizing::VectorParity] {
        for rebalance in [
            Rebalance::FullTarget,
            Rebalance::EntryOnly(EntryBudget::FixedInitial),
            Rebalance::EntryOnly(EntryBudget::RemainingCash),
        ] {
            let engine = Engine::AccurateEvent { sizing, rebalance };
            for capabilities in [
                vec![],
                vec![Vectorizable],
                vec![EventDriven],
                vec![Vectorizable, EventDriven],
                vec![OrderBookDriven],
                vec![Vectorizable, OrderBookDriven],
                vec![EventDriven, OrderBookDriven],
                vec![Vectorizable, EventDriven, OrderBookDriven],
            ] {
                let supported = sizing == Sizing::Lot
                    && !capabilities.is_empty()
                    && !capabilities.contains(&OrderBookDriven);
                let result = engine.validate(&strategy(&capabilities));
                assert_eq!(
                    result.is_ok(),
                    supported,
                    "{engine:?} {capabilities:?}: {result:?}"
                );
                if let Err(error) = result {
                    let message = error.to_string();
                    for part in ["accurate_event", "top_k_rank", "sizing=", "rebalance="] {
                        assert!(message.contains(part), "{message}");
                    }
                }
            }
        }
    }
}
