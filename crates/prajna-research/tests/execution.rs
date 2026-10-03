mod common;

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use prajna_domain::{InstrumentId, TimestampNs, VenueId};
use prajna_research::{
    Panel, PanelError, PanelSession, executable, load_execution_status, load_panel,
    strategy::{Decision, ExecutionEvent, execute},
};

fn fixture_panel(lake_root: &std::path::Path) -> (Panel, String) {
    let dsv = common::publish_version(lake_root, "3");
    let panel = load_panel(lake_root, &dsv, &VenueId::new("SYNTH").unwrap()).unwrap();
    (panel, dsv)
}

fn instrument(panel: &Panel, id: &str) -> InstrumentId {
    panel
        .instruments
        .iter()
        .find(|instrument| instrument.to_string() == id)
        .unwrap()
        .clone()
}

fn session(panel: &Panel, date: &str) -> PanelSession {
    let date = NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap();
    panel
        .sessions
        .iter()
        .find(|session| session.session_date == date)
        .unwrap()
        .clone()
}

fn decision(panel: &Panel, date: &str, targets: &[(&str, f64)]) -> Decision {
    Decision {
        decision_session: session(panel, date),
        ranked: Vec::new(),
        targets: targets
            .iter()
            .map(|(id, weight)| (id.parse().unwrap(), *weight))
            .collect(),
    }
}

fn weights(targets: &[(&str, f64)]) -> BTreeMap<InstrumentId, f64> {
    targets
        .iter()
        .map(|(id, weight)| (id.parse().unwrap(), *weight))
        .collect()
}

fn ids(instruments: &[&str]) -> BTreeSet<InstrumentId> {
    instruments
        .iter()
        .map(|instrument| instrument.parse().unwrap())
        .collect()
}

#[test]
fn loads_v3_execution_status_for_each_instrument_and_session() {
    let lake = tempfile::tempdir().unwrap();
    let dsv = common::publish_version(lake.path(), "3");

    let statuses = load_execution_status(lake.path(), &dsv).unwrap();

    assert_eq!(statuses.len(), 30);
    let c_unknown = statuses
        .values()
        .find(|row| {
            row.instrument_id.to_string() == "C.SYNTH"
                && row.session_date == NaiveDate::from_ymd_opt(2026, 1, 8).unwrap()
        })
        .unwrap();
    assert_eq!(c_unknown.trade_status, "UNKNOWN");
    assert!(!c_unknown.is_tradable);
    let b_halted = statuses
        .values()
        .find(|row| {
            row.instrument_id.to_string() == "B.SYNTH"
                && row.session_date == NaiveDate::from_ymd_opt(2026, 1, 13).unwrap()
        })
        .unwrap();
    assert_eq!(b_halted.trade_status, "HALTED");
    assert!(!b_halted.is_tradable);
    assert!(statuses.values().any(|row| {
        row.instrument_id.to_string() == "B.SYNTH"
            && row.session_date == NaiveDate::from_ymd_opt(2026, 1, 16).unwrap()
    }));
}

#[test]
fn rejects_a_dsv_without_an_execution_status_table() {
    for version in 1..=2 {
        let lake = tempfile::tempdir().unwrap();
        let dsv = match version {
            1 => common::publish_v1(lake.path()),
            _ => common::publish_version(lake.path(), "2"),
        };

        let error = load_execution_status(lake.path(), &dsv).unwrap_err();

        assert!(matches!(error, PanelError::InvalidManifest(_)));
        assert!(
            error
                .to_string()
                .contains("missing execution_status table descriptor")
        );
    }
}

#[test]
fn executable_requires_a_bar_a_tradable_status_and_availability_by_session_open() {
    let lake = tempfile::tempdir().unwrap();
    let (panel, dsv) = fixture_panel(lake.path());
    let mut statuses = load_execution_status(lake.path(), &dsv).unwrap();
    let a = instrument(&panel, "A.SYNTH");
    let b = instrument(&panel, "B.SYNTH");
    let c = instrument(&panel, "C.SYNTH");
    let jan_5 = session(&panel, "2026-01-05");
    let jan_8 = session(&panel, "2026-01-08");
    let jan_13 = session(&panel, "2026-01-13");
    let jan_16 = session(&panel, "2026-01-16");

    assert!(executable(&panel, &statuses, &a, &jan_5).unwrap());
    assert!(!executable(&panel, &statuses, &c, &jan_8).unwrap());
    assert!(!executable(&panel, &statuses, &b, &jan_13).unwrap());
    assert!(!executable(&panel, &statuses, &b, &jan_16).unwrap());

    let a_status = statuses[&(a.clone(), jan_5.session_date)].clone();
    statuses.remove(&(a.clone(), jan_5.session_date));
    assert!(!executable(&panel, &statuses, &a, &jan_5).unwrap());
    statuses.insert((a.clone(), jan_5.session_date), a_status);
    statuses
        .get_mut(&(a.clone(), jan_5.session_date))
        .unwrap()
        .available_at = Some(TimestampNs::from_unix_nanos(
        jan_5.ts_open.as_unix_nanos() + 1,
    ));
    assert!(!executable(&panel, &statuses, &a, &jan_5).unwrap());
    statuses
        .get_mut(&(a.clone(), jan_5.session_date))
        .unwrap()
        .available_at = None;
    assert!(!executable(&panel, &statuses, &a, &jan_5).unwrap());
}

#[test]
fn executes_the_hand_derived_three_by_ten_status_events() {
    let lake = tempfile::tempdir().unwrap();
    let (panel, dsv) = fixture_panel(lake.path());
    let statuses = load_execution_status(lake.path(), &dsv).unwrap();
    let decisions = [
        decision(&panel, "2026-01-07", &[("C.SYNTH", 1.0)]),
        decision(&panel, "2026-01-08", &[("B.SYNTH", 1.0)]),
        decision(&panel, "2026-01-09", &[("B.SYNTH", 1.0)]),
        decision(&panel, "2026-01-12", &[("C.SYNTH", 1.0)]),
        decision(&panel, "2026-01-13", &[("A.SYNTH", 1.0)]),
        decision(&panel, "2026-01-14", &[("A.SYNTH", 1.0)]),
        decision(&panel, "2026-01-15", &[("A.SYNTH", 0.5), ("B.SYNTH", 0.5)]),
        decision(&panel, "2026-01-16", &[("C.SYNTH", 1.0)]),
    ];
    let events = execute(&decisions, &panel.sessions, |instrument, session| {
        executable(&panel, &statuses, instrument, session).unwrap()
    });

    // Jan 8 skips UNKNOWN C; B becomes held before its Jan 13 HALTED deferral.
    // The Jan 13 decision replaces C and executes as A on Jan 14; Jan 16's
    // missing B bar skips only that half-weight buy, without renormalizing A.
    assert_eq!(
        events,
        [
            ExecutionEvent::Executed {
                decision_session: session(&panel, "2026-01-07"),
                attempt_session: session(&panel, "2026-01-08"),
                applied: BTreeMap::new(),
                skipped_buys: ids(&["C.SYNTH"]),
            },
            ExecutionEvent::Executed {
                decision_session: session(&panel, "2026-01-08"),
                attempt_session: session(&panel, "2026-01-09"),
                applied: weights(&[("B.SYNTH", 1.0)]),
                skipped_buys: BTreeSet::new(),
            },
            ExecutionEvent::Executed {
                decision_session: session(&panel, "2026-01-09"),
                attempt_session: session(&panel, "2026-01-12"),
                applied: weights(&[("B.SYNTH", 1.0)]),
                skipped_buys: BTreeSet::new(),
            },
            ExecutionEvent::Deferred {
                decision_session: session(&panel, "2026-01-12"),
                attempt_session: session(&panel, "2026-01-13"),
                blocked: ids(&["B.SYNTH"]),
            },
            ExecutionEvent::Executed {
                decision_session: session(&panel, "2026-01-13"),
                attempt_session: session(&panel, "2026-01-14"),
                applied: weights(&[("A.SYNTH", 1.0)]),
                skipped_buys: BTreeSet::new(),
            },
            ExecutionEvent::Executed {
                decision_session: session(&panel, "2026-01-14"),
                attempt_session: session(&panel, "2026-01-15"),
                applied: weights(&[("A.SYNTH", 1.0)]),
                skipped_buys: BTreeSet::new(),
            },
            ExecutionEvent::Executed {
                decision_session: session(&panel, "2026-01-15"),
                attempt_session: session(&panel, "2026-01-16"),
                applied: weights(&[("A.SYNTH", 0.5)]),
                skipped_buys: ids(&["B.SYNTH"]),
            },
            ExecutionEvent::PendingAtEnd {
                decision_session: session(&panel, "2026-01-16"),
                targets: weights(&[("C.SYNTH", 1.0)]),
            },
        ]
    );
}

#[test]
fn retries_a_deferred_target_when_the_held_instrument_becomes_executable() {
    let lake = tempfile::tempdir().unwrap();
    let (panel, _) = fixture_panel(lake.path());
    let a = instrument(&panel, "A.SYNTH");
    let decisions = [
        decision(&panel, "2026-01-05", &[("A.SYNTH", 1.0)]),
        decision(&panel, "2026-01-06", &[("B.SYNTH", 1.0)]),
    ];
    let blocked_session = session(&panel, "2026-01-07");
    let events = execute(&decisions, &panel.sessions, |instrument, attempt| {
        instrument != &a || attempt != &blocked_session
    });

    assert_eq!(
        events,
        [
            ExecutionEvent::Executed {
                decision_session: session(&panel, "2026-01-05"),
                attempt_session: session(&panel, "2026-01-06"),
                applied: weights(&[("A.SYNTH", 1.0)]),
                skipped_buys: BTreeSet::new(),
            },
            ExecutionEvent::Deferred {
                decision_session: session(&panel, "2026-01-06"),
                attempt_session: blocked_session,
                blocked: ids(&["A.SYNTH"]),
            },
            ExecutionEvent::Executed {
                decision_session: session(&panel, "2026-01-06"),
                attempt_session: session(&panel, "2026-01-08"),
                applied: weights(&[("B.SYNTH", 1.0)]),
                skipped_buys: BTreeSet::new(),
            },
        ]
    );
}
