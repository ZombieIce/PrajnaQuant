use std::fs;

use prajna_domain::InstrumentId;
use prajna_research::{StaticUniverse, UniverseError};

mod common;

fn fixture_universe() -> StaticUniverse {
    StaticUniverse::new(
        "synthetic",
        ["A.SYNTH", "B.SYNTH"]
            .map(|id| id.parse::<InstrumentId>().unwrap())
            .to_vec(),
    )
    .unwrap()
}

#[test]
fn canonicalizes_members_and_uses_label_and_members_for_identity() {
    let universe = StaticUniverse::new(
        "synthetic",
        ["B.SYNTH", "A.SYNTH", "A.SYNTH"]
            .map(|id| id.parse::<InstrumentId>().unwrap())
            .to_vec(),
    )
    .unwrap();
    let same_members = StaticUniverse::new(
        "synthetic",
        ["A.SYNTH", "B.SYNTH"]
            .map(|id| id.parse::<InstrumentId>().unwrap())
            .to_vec(),
    )
    .unwrap();
    let different_label = StaticUniverse::new(
        "another",
        ["A.SYNTH", "B.SYNTH"]
            .map(|id| id.parse::<InstrumentId>().unwrap())
            .to_vec(),
    )
    .unwrap();

    assert_eq!(
        universe
            .members()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["A.SYNTH", "B.SYNTH"]
    );
    assert_eq!(
        String::from_utf8(universe.canonical_json().unwrap()).unwrap(),
        r#"{"label":"synthetic","members":["A.SYNTH","B.SYNTH"],"universe_version":1}"#
    );
    assert_eq!(
        universe.id().unwrap(),
        "uni:sha256:8b5f045f9b756572c1e3023c8a05e13616aa8b3253e252b2ddf183f6236fe1e7"
    );
    assert_eq!(universe.id().unwrap(), same_members.id().unwrap());
    assert_ne!(universe.id().unwrap(), different_label.id().unwrap());
    assert!(StaticUniverse::new("empty", Vec::new()).is_err());
}

#[test]
fn stores_idempotently_and_rejects_tampered_content() {
    let lake = tempfile::tempdir().unwrap();
    let universe = fixture_universe();

    let path = universe.store(lake.path()).unwrap();
    assert_eq!(path, universe.store(lake.path()).unwrap());
    assert_eq!(
        StaticUniverse::load(lake.path(), &universe.id().unwrap()).unwrap(),
        universe
    );

    let tampered = fs::read_to_string(&path)
        .unwrap()
        .replace("synthetic", "tampered");
    fs::write(path, tampered).unwrap();
    assert!(StaticUniverse::load(lake.path(), &universe.id().unwrap()).is_err());
    assert!(universe.store(lake.path()).is_err());
}

#[test]
fn validates_members_against_the_dsv_instruments_table() {
    let lake = tempfile::tempdir().unwrap();
    let dsv = common::publish_v1(lake.path());
    let valid = StaticUniverse::new(
        "fixture",
        ["A.SYNTH", "B.SYNTH", "C.SYNTH"]
            .map(|id| id.parse::<InstrumentId>().unwrap())
            .to_vec(),
    )
    .unwrap();
    let missing_member = StaticUniverse::new(
        "fixture",
        ["A.SYNTH", "B.SYNTH", "D.SYNTH"]
            .map(|id| id.parse::<InstrumentId>().unwrap())
            .to_vec(),
    )
    .unwrap();

    valid.validate_against(lake.path(), &dsv).unwrap();
    let Err(UniverseError::MissingMembers(missing)) =
        missing_member.validate_against(lake.path(), &dsv)
    else {
        panic!("expected the DSV to report its missing universe member");
    };
    assert_eq!(missing, ["D.SYNTH"]);
}
