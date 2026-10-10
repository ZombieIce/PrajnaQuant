use std::collections::BTreeSet;

use chrono::NaiveDate;
use polars::prelude::*;
use prajna_domain::VenueId;
use prajna_research::{
    Panel, PanelSession, compute,
    factor::{
        AvailabilityAssumption, Factor, FactorKind, FactorStatus, MomentumParams,
        RotationScoreParams,
    },
    load_panel,
    strategy::{
        EngineError, RankDirection, StrategyCapability, TopKRank, VectorEngine, VectorStrategy,
        Weighting,
    },
};

mod common;

fn fixture_panel() -> Panel {
    let lake = tempfile::tempdir().unwrap();
    let dsv = common::publish_v1(lake.path());
    load_panel(lake.path(), &dsv, &VenueId::new("SYNTH").unwrap()).unwrap()
}

fn strategy(capabilities: BTreeSet<StrategyCapability>) -> TopKRank {
    TopKRank {
        score: Factor::new(FactorKind::Momentum, MomentumParams { n: 1 }).unwrap(),
        direction: RankDirection::Descending,
        top_k: 1,
        rebalance_every: 1,
        weighting: Weighting::EqualWeight,
        unfilled_entry: Default::default(),
        capabilities,
    }
}

fn configured_strategy(top_k: u32, rebalance_every: u32) -> TopKRank {
    let mut strategy = strategy(BTreeSet::from([StrategyCapability::Vectorizable]));
    strategy.top_k = top_k;
    strategy.rebalance_every = rebalance_every;
    strategy
}

fn fixture_values(
    sessions: &[PanelSession],
    values: &[(usize, &str, f64, Option<i64>)],
) -> DataFrame {
    values_for_instruments(sessions, &["A.SYNTH", "B.SYNTH", "C.SYNTH"], values)
}

fn values_for_instruments(
    sessions: &[PanelSession],
    instruments: &[&str],
    values: &[(usize, &str, f64, Option<i64>)],
) -> DataFrame {
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
    let mut ids = Vec::new();
    let mut session_dates = Vec::new();
    let mut scores = Vec::new();
    let mut available_at = Vec::new();
    let mut statuses = Vec::new();

    for instrument_id in instruments {
        for (session_index, session) in sessions.iter().enumerate() {
            let date = i32::try_from((session.session_date - epoch).num_days()).unwrap();
            let score = values
                .iter()
                .find(|(index, id, _, _)| *index == session_index && *id == *instrument_id);
            ids.push(*instrument_id);
            session_dates.push(date);
            if let Some((_, _, score, available)) = score {
                scores.push(Some(*score));
                available_at.push(Some(
                    available.unwrap_or_else(|| session.ts_close.as_unix_nanos()),
                ));
                statuses.push(FactorStatus::Ok.as_str());
            } else {
                scores.push(None);
                available_at.push(None);
                statuses.push(FactorStatus::MissingInput.as_str());
            }
        }
    }

    DataFrame::new(
        sessions.len() * instruments.len(),
        vec![
            Series::new("instrument_id".into(), ids).into(),
            Series::new("session_date".into(), session_dates)
                .cast(&DataType::Date)
                .unwrap()
                .into(),
            Series::new("value".into(), scores).into(),
            Series::new("available_at".into(), available_at)
                .i64()
                .unwrap()
                .clone()
                .into_datetime(TimeUnit::Nanoseconds, Some(TimeZone::UTC))
                .into_series()
                .into(),
            Series::new("status".into(), statuses).into(),
        ],
    )
    .unwrap()
}

#[test]
fn vector_engine_rejects_a_strategy_without_vectorizable_capability() {
    let strategy = strategy(BTreeSet::from([StrategyCapability::EventDriven]));

    assert_eq!(
        VectorEngine::validate(&VectorStrategy::TopKRank(strategy.clone())),
        Err(EngineError::UnsupportedCapability)
    );
}

#[test]
fn vector_engine_accepts_vectorizable_strategies_and_rejects_zero_parameters() {
    assert_eq!(
        VectorEngine::validate(&VectorStrategy::TopKRank(configured_strategy(1, 1))),
        Ok(())
    );
    assert_eq!(
        VectorEngine::validate(&VectorStrategy::TopKRank(configured_strategy(0, 1))),
        Err(EngineError::InvalidTopK)
    );
    assert_eq!(
        VectorEngine::validate(&VectorStrategy::TopKRank(configured_strategy(1, 0))),
        Err(EngineError::InvalidRebalanceEvery)
    );
}

#[test]
fn decisions_start_at_first_usable_score_and_follow_venue_session_indices() {
    let panel = fixture_panel();
    let scores = fixture_values(
        &panel.sessions,
        &[
            (2, "B.SYNTH", 7.0, None),
            (2, "A.SYNTH", 7.0, None),
            (2, "C.SYNTH", 6.0, None),
            (8, "C.SYNTH", 9.0, None),
        ],
    );

    let decisions = configured_strategy(5, 3)
        .decide(&scores, &panel.sessions)
        .unwrap();

    assert_eq!(
        decisions
            .iter()
            .map(|decision| decision.decision_session.session_date)
            .collect::<Vec<_>>(),
        [2, 5, 8].map(|index| panel.sessions[index].session_date)
    );
    let first = &decisions[0];
    assert_eq!(
        first
            .ranked
            .iter()
            .map(|(instrument, _, rank)| (instrument.to_string(), *rank))
            .collect::<Vec<_>>(),
        [
            ("A.SYNTH".to_owned(), 1),
            ("B.SYNTH".to_owned(), 2),
            ("C.SYNTH".to_owned(), 3),
        ]
    );
    assert_eq!(first.targets.len(), 3);
    assert!(first.targets.values().all(|weight| *weight == 1.0 / 3.0));
    assert!(decisions[1].ranked.is_empty());
    assert!(decisions[1].targets.is_empty());
    assert_eq!(decisions[2].ranked.len(), 1);
    assert_eq!(decisions[2].targets.len(), 1);
    assert_eq!(decisions[2].targets.values().next(), Some(&1.0));
}

#[test]
fn tied_scores_use_the_canonical_instrument_id_string_order() {
    let panel = fixture_panel();
    let scores = values_for_instruments(
        &panel.sessions,
        &["A.SYNTH", "A-.SYNTH"],
        &[(0, "A.SYNTH", 1.0, None), (0, "A-.SYNTH", 1.0, None)],
    );

    let decisions = configured_strategy(1, 1)
        .decide(&scores, &panel.sessions)
        .unwrap();

    assert_eq!(decisions[0].ranked[0].0.to_string(), "A-.SYNTH");
    assert_eq!(
        decisions[0].targets.keys().next().unwrap().to_string(),
        "A-.SYNTH"
    );
}

#[test]
fn scores_not_available_by_the_session_close_do_not_trigger_or_enter_a_decision() {
    let panel = fixture_panel();
    let late_close_value = panel.sessions[2].ts_close.as_unix_nanos() + 1;
    let scores = fixture_values(
        &panel.sessions,
        &[
            (2, "A.SYNTH", 100.0, Some(late_close_value)),
            (3, "B.SYNTH", 1.0, None),
        ],
    );

    let decisions = configured_strategy(1, 3)
        .decide(&scores, &panel.sessions)
        .unwrap();

    assert_eq!(
        decisions
            .iter()
            .map(|decision| decision.decision_session.session_date)
            .collect::<Vec<_>>(),
        [3, 6, 9].map(|index| panel.sessions[index].session_date)
    );
    assert_eq!(
        decisions[0]
            .targets
            .keys()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["B.SYNTH"]
    );
}

#[test]
fn ok_scores_must_be_finite() {
    let panel = fixture_panel();
    let scores = fixture_values(&panel.sessions, &[(2, "A.SYNTH", f64::NAN, None)]);

    assert!(matches!(
        configured_strategy(1, 1).decide(&scores, &panel.sessions),
        Err(EngineError::InvalidScoreValues(_))
    ));
}

#[test]
fn three_by_ten_fixture_decisions_match_the_independent_golden() {
    let panel = fixture_panel();
    let score_factor = Factor::new(
        FactorKind::RotationScore,
        RotationScoreParams {
            short: 1,
            long: 1,
            vol: 2,
            w_s: 1.0,
            w_l: 1.0,
            w_v: 0.0,
            trend: None,
        },
    )
    .unwrap();
    let score_values = compute(
        &panel,
        &panel.instruments,
        &score_factor,
        AvailabilityAssumption::None,
    )
    .unwrap();
    let mut strategy = configured_strategy(1, 1);
    strategy.score = score_factor;

    let decisions = strategy.decide(&score_values, &panel.sessions).unwrap();
    let expected_rankings = [
        ["C.SYNTH", "A.SYNTH", "B.SYNTH"].as_slice(),
        ["B.SYNTH", "A.SYNTH", "C.SYNTH"].as_slice(),
        ["B.SYNTH", "C.SYNTH", "A.SYNTH"].as_slice(),
        ["C.SYNTH", "B.SYNTH", "A.SYNTH"].as_slice(),
        ["A.SYNTH", "C.SYNTH", "B.SYNTH"].as_slice(),
        ["A.SYNTH", "C.SYNTH", "B.SYNTH"].as_slice(),
        ["A.SYNTH", "C.SYNTH", "B.SYNTH"].as_slice(),
        ["C.SYNTH", "A.SYNTH"].as_slice(),
    ];

    assert_eq!(decisions.len(), expected_rankings.len());
    for (offset, (decision, expected)) in decisions.iter().zip(expected_rankings).enumerate() {
        assert_eq!(decision.decision_session, panel.sessions[offset + 2]);
        assert_eq!(
            decision
                .ranked
                .iter()
                .map(|(id, _, _)| id.to_string())
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(
            decision
                .ranked
                .iter()
                .map(|(_, _, rank)| *rank)
                .collect::<Vec<_>>(),
            (1..=expected.len() as u32).collect::<Vec<_>>()
        );
        assert_eq!(
            decision
                .targets
                .keys()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            [expected[0]]
        );
        assert_eq!(decision.targets.values().next(), Some(&1.0));
    }
}
