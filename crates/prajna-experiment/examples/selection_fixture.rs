//! Synthetic #350 inputs through the actual production publisher, for HTTP E2E.
#[path = "../../prajna-data/tests/support/opening_fixture.rs"]
mod fixture;
#[allow(dead_code)]
#[path = "../tests/support/selection_fixture.rs"]
mod research_fixture;
use prajna_domain::InstrumentKind;
use serde_json::{Value, json};
use std::{fs, path::PathBuf, process::Command};

fn main() {
    let root = PathBuf::from(std::env::args().nth(1).expect("synthetic fixture root"));
    let publisher = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(format!(
            "prajna-publish-daily{}",
            std::env::consts::EXE_SUFFIX
        ));
    let mut selections = serde_json::Map::new();
    for venue in ["XSHG", "XSHE"] {
        let input = root.join("synthetic-input").join(venue);
        let (selection, snapshot) =
            fixture::prepare(&input, venue, InstrumentKind::Etf, false, None);
        let path = input.join("selection.json");
        fs::write(&path, serde_json::to_vec(&selection).unwrap()).unwrap();
        let receipt = format!("{venue}-receipt.json");
        let output = Command::new(&publisher)
            .arg("publish")
            .arg("--root")
            .arg(&root)
            .arg("--selection")
            .arg(path)
            .arg("--receipt")
            .arg(&receipt)
            .arg("--diagnostics")
            .arg(input.join("diagnostics"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "synthetic production publication failed"
        );
        let receipt: Value =
            serde_json::from_slice(&fs::read(root.join(receipt)).unwrap()).unwrap();
        let dsv = receipt["dsv"].as_str().unwrap();
        let instrument = snapshot["instrument_id"].as_str().unwrap();
        for dates in [
            vec!["2026-01-05", "2026-01-06", "2026-01-07"],
            vec!["2026-01-05", "2026-01-06"],
            vec!["2026-01-05", "2026-01-07"],
        ] {
            research_fixture::import_adjusted(
                &root,
                dsv,
                instrument,
                "2026-01-05",
                "2026-01-07",
                &dates,
            );
        }
        selections.insert(
            snapshot["instrument_id"].as_str().unwrap().into(),
            receipt["dsv"].clone(),
        );
    }
    println!("{}", json!(selections));
}
