//! Entirely synthetic publisher CLI → reader → Experiment CLI → persisted result.
#[path = "../../prajna-data/tests/support/opening_fixture.rs"]
mod fixture;
use prajna_domain::InstrumentKind;
use prajna_research::StaticUniverse;
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn success(output: Output) -> Vec<u8> {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}
fn experiment(repo: &Path, lake: &Path, args: &[&str]) -> Value {
    serde_json::from_slice(&success(
        Command::new(env!("CARGO_BIN_EXE_prajna-experiment"))
            .args(args)
            .arg("--lake")
            .arg(lake)
            .current_dir(repo)
            .env("RAYON_NUM_THREADS", "1")
            .output()
            .unwrap(),
    ))
    .unwrap()
}

#[test]
fn production_opening_evidence_cli_chain_matches_hand_calculation_and_gates() {
    // Build the actual publisher on a clean checkout too; no pre-existing artifact prerequisite.
    let target = Path::new(env!("CARGO_BIN_EXE_prajna-experiment"))
        .parent()
        .unwrap();
    success(
        Command::new(env!("CARGO"))
            .args([
                "build",
                "-p",
                "prajna-data",
                "--bin",
                "prajna-publish-daily",
                "--locked",
                "--offline",
                "--target-dir",
            ])
            .arg(target.parent().unwrap())
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .unwrap(),
    );
    let publisher = target.join(format!(
        "prajna-publish-daily{}",
        std::env::consts::EXE_SUFFIX
    ));
    for venue in ["XSHG", "XSHE"] {
        for gate in [
            "ok", "before", "late", "null", "halted", "missing", "unknown", "conflict", "no_bar",
        ] {
            let temp = tempfile::tempdir().unwrap();
            let input = temp.path().join("input");
            let lake = temp.path().join("lake");
            let repo = temp.path().join("repo");
            fs::create_dir(&repo).unwrap();
            success(
                Command::new("git")
                    .args(["init", "--quiet"])
                    .arg(&repo)
                    .output()
                    .unwrap(),
            );
            fs::write(repo.join("synthetic.txt"), "synthetic CLI verification").unwrap();
            success(
                Command::new("git")
                    .args(["add", "."])
                    .current_dir(&repo)
                    .output()
                    .unwrap(),
            );
            success(
                Command::new("git")
                    .args([
                        "-c",
                        "user.name=Synthetic",
                        "-c",
                        "user.email=synthetic@example.invalid",
                        "-c",
                        "commit.gpgsign=false",
                        "commit",
                        "--quiet",
                        "-m",
                        "synthetic",
                    ])
                    .current_dir(&repo)
                    .output()
                    .unwrap(),
            );
            let (mut selection, mut snapshot) =
                fixture::prepare(&input, venue, InstrumentKind::Etf, gate == "no_bar");
            // Block all execution opens in negative cases, including retry on S3.
            for fact in snapshot["facts"].as_array_mut().unwrap() {
                match gate {
                    "before" => {
                        let date = fact["session_date"].as_str().unwrap();
                        fact["available_at"] = json!(format!("{date}T09:29:00+08:00"));
                    }
                    "late" => {
                        let date = fact["session_date"].as_str().unwrap();
                        fact["available_at"] = json!(format!("{date}T09:30:00.000000001+08:00"));
                    }
                    "null" => fact["available_at"] = Value::Null,
                    "halted" => fact["state"] = json!("HALTED"),
                    "unknown" => fact["state"] = json!("UNKNOWN"),
                    "conflict" => fact["state"] = json!("CONFLICT"),
                    _ => {}
                }
            }
            if gate == "missing" {
                snapshot["facts"] = json!([]);
            }
            if gate == "no_bar" {
                snapshot["facts"][2]["state"] = json!("HALTED");
            }
            selection["status_snapshot"] = json!(fixture::put(&input, &snapshot));
            fs::write(
                input.join("selection.json"),
                serde_json::to_vec(&selection).unwrap(),
            )
            .unwrap();
            success(
                Command::new(&publisher)
                    .arg("publish")
                    .arg("--root")
                    .arg(&lake)
                    .arg("--selection")
                    .arg(input.join("selection.json"))
                    .args(["--receipt", "receipt.json", "--diagnostics"])
                    .arg(temp.path().join("diagnostics"))
                    .output()
                    .unwrap(),
            );
            let receipt: Value =
                serde_json::from_slice(&fs::read(lake.join("receipt.json")).unwrap()).unwrap();
            let dsv = receipt["dsv"].as_str().unwrap();
            let statuses = prajna_research::load_execution_status(&lake, dsv).unwrap();
            assert_eq!(statuses.len(), 3);
            let member = snapshot["instrument_id"].as_str().unwrap();
            let panel = prajna_research::load_panel(&lake, dsv, &venue.parse().unwrap()).unwrap();
            let instrument = member.parse().unwrap();
            let mut missing = statuses.clone();
            missing.remove(&(instrument, panel.sessions[1].session_date));
            assert!(
                !prajna_research::executable(
                    &panel,
                    &missing,
                    &member.parse().unwrap(),
                    &panel.sessions[1]
                )
                .unwrap()
            );

            let universe = StaticUniverse::new("synthetic", vec![member.parse().unwrap()]).unwrap();
            universe.store(&lake).unwrap();
            let mut definition = json!({"experiment_version":1,"dsv":dsv,"universe":universe.id().unwrap(),
                "venue":venue,"strategy":"buy_and_hold","engine":"fast_event",
                "availability_assumption":"treat_unknown_as_ts_close","sessions_per_year":252,
                "costs":{"commission_rate":0.01,"buy_slippage_bps":0.0,"sell_slippage_bps":0.0,"buy_tax_rate":0.0,"sell_tax_rate":0.0},
                "parameter_space":{"list":[{"params_version":1}]},"unfilled_entry":"retry",
                "fast_event":{"currency":"CNY","initial_cash":"10000","sizing":"vector_parity","rebalance":"full_target",
                    "minimum_commission":"0","portfolios":[{"id":1,"capital":"10000"}],"lot_size_source":{"kind":"instrument"}}});
            let path = repo.join("experiment.json");
            fs::write(&path, serde_json::to_vec(&definition).unwrap()).unwrap();
            let run = experiment(
                &repo,
                &lake,
                &["run", path.to_str().unwrap(), "--level", "standard"],
            );
            assert_eq!(run["failed_count"], 0, "{venue} {gate}");
            let result = experiment(
                &repo,
                &lake,
                &[
                    "result",
                    run["exp"].as_str().unwrap(),
                    run["exe"].as_str().unwrap(),
                ],
            );
            let values = result["data"]["runs"][0]["series"]["account_valuations"]
                .as_array()
                .unwrap();
            assert_eq!(values.len(), 6);
            // S1 close decision → S2 open at 10: budget 10,000 less 1% = 9,900;
            // buy 990 units, fee 100, cash 0; close 10.1 → equity 9,999.
            for row in values {
                let holds = matches!(gate, "ok" | "before") && row["session_date"] != "2026-01-05";
                assert_eq!(
                    row["cash"],
                    if holds { "0" } else { "10000" },
                    "{gate}: {row}"
                );
                let equity = if holds {
                    if row["valuation_point"] == "close" {
                        "9999"
                    } else {
                        "9900"
                    }
                } else {
                    "10000"
                };
                assert_eq!(row["equity"], equity, "{gate}: {row}");
                let positions = row["positions"].as_object().unwrap();
                assert_eq!(positions.len(), usize::from(holds));
                if holds {
                    assert_eq!(positions[member]["quantity"], "990");
                }
            }
            if gate == "ok" {
                // Promote the same persisted Standard execution to inspect its authoritative Fill.
                let promoted = experiment(
                    &repo,
                    &lake,
                    &["promote", path.to_str().unwrap(), "--level", "full"],
                );
                assert_eq!(promoted["exe"], run["exe"]);
                let run_id = result["data"]["runs"][0]["run_id"].as_str().unwrap();
                let (_, batches) = prajna_experiment::read_run_table(
                    &lake,
                    run["exp"].as_str().unwrap(),
                    run["exe"].as_str().unwrap(),
                    run_id,
                    prajna_experiment::ResultLevel::Full,
                    "fills",
                )
                .unwrap();
                let payloads = batches
                    .iter()
                    .flat_map(|batch| {
                        let column = batch
                            .column_by_name("payload")
                            .unwrap()
                            .as_any()
                            .downcast_ref::<arrow_array::StringArray>()
                            .unwrap();
                        column
                            .iter()
                            .map(|value| serde_json::from_str::<Value>(value.unwrap()).unwrap())
                            .collect::<Vec<_>>()
                    })
                    .collect::<Vec<_>>();
                assert_eq!(payloads.len(), 1);
                let fill = &payloads[0]["fill"];
                assert_eq!(fill["order"]["quantity"], "990");
                assert_eq!(fill["raw_price"], "10");
                assert_eq!(fill["costs"]["commission"], "100");
                assert_eq!(fill["total_fees"], "100");
                assert_eq!(fill["cash_delta"], "-10000");
                definition["engine"] = json!("vector");
                definition.as_object_mut().unwrap().remove("fast_event");
                fs::write(&path, serde_json::to_vec(&definition).unwrap()).unwrap();
                let vector = experiment(
                    &repo,
                    &lake,
                    &["run", path.to_str().unwrap(), "--level", "standard"],
                );
                assert_eq!(vector["failed_count"], 0);
                let vector = experiment(
                    &repo,
                    &lake,
                    &[
                        "result",
                        vector["exp"].as_str().unwrap(),
                        vector["exe"].as_str().unwrap(),
                    ],
                );
                // Vector ends on the terminal open; compare that shared endpoint,
                // not Fast Event's terminal close (which includes the 10 → 10.1 mark).
                let total_return = vector["data"]["runs"][0]["summary"]["total_return"]
                    .as_f64()
                    .unwrap();
                assert!((total_return - (9900.0 / 10000.0 - 1.0)).abs() < 1e-12);
            }
        }
    }
}
