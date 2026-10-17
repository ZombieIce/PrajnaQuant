use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

use prajna_data::{Manifest, RawStore};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use zip::{ZipWriter, write::SimpleFileOptions};

const START: &str = "2022-10-10";
const END: &str = "2022-10-10";

#[test]
fn local_capture_import_publishes_v4_and_keeps_failed_responses_out_of_dataset_inputs() {
    let root = TempDir::new().unwrap();
    let tdx = tdx_package();
    let failed = b"access denied";
    let calendar = calendar_month();
    let tdx_sha = sha256(&tdx);
    let failed_sha = sha256(failed);
    let calendar_sha = sha256(&calendar);

    write_capture_tree(
        root.path(),
        &tdx,
        failed,
        &calendar,
        &tdx_sha,
        &failed_sha,
        &calendar_sha,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_prajna-mvp6-freeze"))
        .args([
            "import-existing",
            "--test-root",
            root.path().to_str().unwrap(),
            "--start",
            START,
            "--end",
            END,
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "CLI failed: {}; report={}",
        String::from_utf8_lossy(&output.stderr),
        fs::read_to_string(root.path().join("acceptance/mvp6-coverage.json")).unwrap_or_default()
    );

    let store = RawStore::open(root.path()).unwrap();
    let tdx_id = format!("sha256:{tdx_sha}");
    let failed_id = format!("sha256:{failed_sha}");
    let calendar_id = format!("sha256:{calendar_sha}");
    assert_eq!(store.get(&tdx_id).unwrap(), tdx);
    assert_eq!(store.get(&failed_id).unwrap(), failed);
    assert_eq!(store.get(&calendar_id).unwrap(), calendar);

    let tdx_source = store.sources(&tdx_id).unwrap().remove(0);
    assert_eq!(tdx_source.source_id, "tdx_daily");
    assert_eq!(tdx_source.request["http_status"], 200);
    assert_eq!(
        tdx_source.request["url"],
        "https://www.tdx.com.cn/products/data/data/g4day/20221010.zip"
    );
    assert_eq!(tdx_source.observed_at, "2026-10-15T11:00:00Z");

    let failed_source = store.sources(&failed_id).unwrap().remove(0);
    assert_eq!(failed_source.request["http_status"], 403);
    assert_eq!(failed_source.request["outcome"], "failed_response");

    let manifest_paths = manifest_paths(root.path());
    assert_eq!(manifest_paths.len(), 1);
    let manifest: Manifest =
        serde_json::from_slice(&fs::read(&manifest_paths[0]).unwrap()).unwrap();
    assert_eq!(manifest.core.normalizer.id, "szse-etf-frozen");
    assert_eq!(manifest.core.normalizer.version, "4");
    let report = coverage_report(root.path());
    assert_eq!(report["spec_window_complete"], false);
    assert_eq!(report["mvp6_complete"], false);
    let input_hashes: Vec<_> = manifest
        .core
        .inputs
        .iter()
        .map(|input| input.raw_sha256.as_str())
        .collect();
    assert!(input_hashes.contains(&tdx_id.as_str()));
    assert!(input_hashes.contains(&calendar_id.as_str()));
    assert!(!input_hashes.contains(&failed_id.as_str()));

    let export = Command::new(env!("CARGO_BIN_EXE_prajna-mvp6-export-codes"))
        .args(["--test-root", root.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!export.status.success());
    assert!(
        !root
            .path()
            .join("acceptance/mvp6-candidate-codes.txt")
            .exists()
    );

    let rerun = Command::new(env!("CARGO_BIN_EXE_prajna-mvp6-freeze"))
        .args([
            "import-existing",
            "--test-root",
            root.path().to_str().unwrap(),
            "--start",
            START,
            "--end",
            END,
        ])
        .output()
        .unwrap();
    assert_eq!(store.sources(&tdx_id).unwrap().len(), 1);
    assert_eq!(store.sources(&failed_id).unwrap().len(), 1);
    assert_eq!(store.sources(&calendar_id).unwrap().len(), 1);
    assert!(
        rerun.status.success(),
        "CLI failed: {}; report={}",
        String::from_utf8_lossy(&rerun.stderr),
        fs::read_to_string(root.path().join("acceptance/mvp6-coverage.json")).unwrap_or_default()
    );

    let report_path = root.path().join("acceptance/mvp6-coverage.json");
    let mut previous: Value = serde_json::from_slice(&fs::read(&report_path).unwrap()).unwrap();
    previous["source_status"]["sina_adjustment_factors"] =
        serde_json::json!("captured_raw_pairs_not_normalized");
    previous["source_status"]["cninfo"] = serde_json::json!("captured_index_with_unknowns");
    previous["sina_failures"] = serde_json::json!(["sz159900"]);
    previous["p21_mismatches"] = serde_json::json!(["synthetic discrepancy"]);
    let previous_dsv = previous["dsv"].clone();
    fs::write(&report_path, serde_json::to_vec_pretty(&previous).unwrap()).unwrap();
    fs::remove_file(root.path().join("acceptance/tdx-daily-requests.csv")).unwrap();

    let failed_import = Command::new(env!("CARGO_BIN_EXE_prajna-mvp6-freeze"))
        .args([
            "import-existing",
            "--test-root",
            root.path().to_str().unwrap(),
            "--start",
            START,
            "--end",
            END,
        ])
        .output()
        .unwrap();
    assert!(!failed_import.status.success());
    let after_failure = coverage_report(root.path());
    assert_eq!(after_failure["import_status"], "unresolved");
    assert_eq!(after_failure["dsv"], previous_dsv);
    assert_eq!(after_failure["dataset_published"], true);
    assert_eq!(
        after_failure["source_status"]["sina_adjustment_factors"],
        "captured_raw_pairs_not_normalized"
    );
    assert_eq!(
        after_failure["source_status"]["cninfo"],
        "captured_index_with_unknowns"
    );
    assert_eq!(
        after_failure["sina_failures"],
        serde_json::json!(["sz159900"])
    );
    assert_eq!(
        after_failure["p21_mismatches"],
        serde_json::json!(["synthetic discrepancy"])
    );
}

#[test]
fn local_capture_import_fails_closed_when_a_requested_session_has_no_valid_package() {
    let root = TempDir::new().unwrap();
    let calendar = calendar_month();
    let calendar_sha = sha256(&calendar);
    write_calendar_capture(root.path(), &calendar, &calendar_sha);
    fs::write(
        root.path().join("acceptance/szse-open-dates.json"),
        format!("[\"{START}\"]\n"),
    )
    .unwrap();
    write_tdx_logs(
        root.path(),
        &[format!(
            "{START},1,2026-10-15T11:00:00Z,https://www.tdx.com.cn/products/data/data/g4day/20221010.zip,200,,1,"
        )],
        &[],
    );

    let output = Command::new(env!("CARGO_BIN_EXE_prajna-mvp6-freeze"))
        .args([
            "import-existing",
            "--test-root",
            root.path().to_str().unwrap(),
            "--start",
            START,
            "--end",
            END,
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(manifest_paths(root.path()).is_empty());
    let report = coverage_report(root.path());
    assert_eq!(report["dataset_published"], false);
    assert_eq!(
        report["missing_tdx_sessions"],
        serde_json::json!([START]),
        "report={report}"
    );
    assert_eq!(report["open_date_list_status"], "matched");
    assert_eq!(
        report["source_status"]["szse_calendar"],
        "validated_local_captures"
    );
    assert_eq!(report["source_status"]["tdx"], "local_captures_unresolved");
    assert!(
        report["invalid_tdx_requests"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| issue["reason"] == "invalid_or_missing_sha256")
    );
}

#[test]
fn local_capture_import_rejects_a_tdx_package_with_the_wrong_embedded_session() {
    let root = TempDir::new().unwrap();
    let tdx = tdx_package_for("221011");
    let failed = b"access denied";
    let calendar = calendar_month();
    let tdx_sha = sha256(&tdx);
    let failed_sha = sha256(failed);
    let calendar_sha = sha256(&calendar);
    write_capture_tree(
        root.path(),
        &tdx,
        failed,
        &calendar,
        &tdx_sha,
        &failed_sha,
        &calendar_sha,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_prajna-mvp6-freeze"))
        .args([
            "import-existing",
            "--test-root",
            root.path().to_str().unwrap(),
            "--start",
            START,
            "--end",
            END,
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(manifest_paths(root.path()).is_empty());
    let report = coverage_report(root.path());
    assert_eq!(report["missing_tdx_sessions"], serde_json::json!([START]));
    assert!(
        report["invalid_tdx_sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| issue["reason"] == "package_session_members_missing_or_mismatched")
    );
}

#[test]
fn local_capture_import_accepts_only_explicit_tencent_cells_absent_from_tdx_package() {
    let root = TempDir::new().unwrap();
    let tdx = tdx_package();
    let failed = b"access denied";
    let calendar = calendar_month();
    let tdx_sha = sha256(&tdx);
    let failed_sha = sha256(failed);
    let calendar_sha = sha256(&calendar);
    write_capture_tree(
        root.path(),
        &tdx,
        failed,
        &calendar,
        &tdx_sha,
        &failed_sha,
        &calendar_sha,
    );
    let tencent = tencent_response("159002");
    let tencent_sha = write_tencent_capture(root.path(), "159002", &tencent);

    let output = Command::new(env!("CARGO_BIN_EXE_prajna-mvp6-freeze"))
        .args([
            "import-existing",
            "--test-root",
            root.path().to_str().unwrap(),
            "--start",
            START,
            "--end",
            END,
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "CLI failed: {}; report={}",
        String::from_utf8_lossy(&output.stderr),
        fs::read_to_string(root.path().join("acceptance/mvp6-coverage.json")).unwrap_or_default()
    );
    let manifest: Manifest =
        serde_json::from_slice(&fs::read(&manifest_paths(root.path())[0]).unwrap()).unwrap();
    assert!(
        manifest
            .core
            .inputs
            .iter()
            .any(|input| input.raw_sha256 == format!("sha256:{tencent_sha}"))
    );
    assert_eq!(
        RawStore::open(root.path())
            .unwrap()
            .sources(&format!("sha256:{tencent_sha}"))
            .unwrap()[0]
            .source_id,
        "tencent_kline"
    );
}

#[test]
fn local_capture_import_rejects_tencent_when_tdx_key_exists() {
    let root = TempDir::new().unwrap();
    let tdx = tdx_package();
    let failed = b"access denied";
    let calendar = calendar_month();
    let tdx_sha = sha256(&tdx);
    let failed_sha = sha256(failed);
    let calendar_sha = sha256(&calendar);
    write_capture_tree(
        root.path(),
        &tdx,
        failed,
        &calendar,
        &tdx_sha,
        &failed_sha,
        &calendar_sha,
    );
    let tencent = tencent_response("159001");
    write_tencent_capture(root.path(), "159001", &tencent);

    let output = Command::new(env!("CARGO_BIN_EXE_prajna-mvp6-freeze"))
        .args([
            "import-existing",
            "--test-root",
            root.path().to_str().unwrap(),
            "--start",
            START,
            "--end",
            END,
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(manifest_paths(root.path()).is_empty());
    let report = coverage_report(root.path());
    assert!(
        report["tencent_fallback_issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| issue["reason"] == "tdx_keyed_observation_blocks_tencent_fallback")
    );
}

#[test]
fn local_capture_import_rejects_conflicting_successful_tencent_revisions() {
    let root = TempDir::new().unwrap();
    let tdx = tdx_package();
    let failed = b"access denied";
    let calendar = calendar_month();
    let tdx_sha = sha256(&tdx);
    let failed_sha = sha256(failed);
    let calendar_sha = sha256(&calendar);
    write_capture_tree(
        root.path(),
        &tdx,
        failed,
        &calendar,
        &tdx_sha,
        &failed_sha,
        &calendar_sha,
    );
    let first = tencent_response("159002");
    write_tencent_capture(root.path(), "159002", &first);
    let second = tencent_response_with_close("159002", "1.025");
    append_tencent_capture(root.path(), "159002", &second);

    let output = Command::new(env!("CARGO_BIN_EXE_prajna-mvp6-freeze"))
        .args([
            "import-existing",
            "--test-root",
            root.path().to_str().unwrap(),
            "--start",
            START,
            "--end",
            END,
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(manifest_paths(root.path()).is_empty());
    let report = coverage_report(root.path());
    assert_eq!(report["tencent_fallback_plan_status"], "unresolved");
    assert!(
        report["tencent_fallback_issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| issue["reason"]
                == "planned_cell_has_conflicting_successful_response_revisions")
    );
}

#[test]
fn local_capture_import_rejects_unrequested_tencent_symbols() {
    let root = TempDir::new().unwrap();
    let tdx = tdx_package();
    let failed = b"access denied";
    let calendar = calendar_month();
    let tdx_sha = sha256(&tdx);
    let failed_sha = sha256(failed);
    let calendar_sha = sha256(&calendar);
    write_capture_tree(
        root.path(),
        &tdx,
        failed,
        &calendar,
        &tdx_sha,
        &failed_sha,
        &calendar_sha,
    );
    let mut response: Value = serde_json::from_slice(&tencent_response("159002")).unwrap();
    response["data"]["sz159003"] = response["data"]["sz159002"].clone();
    write_tencent_capture(
        root.path(),
        "159002",
        &serde_json::to_vec(&response).unwrap(),
    );

    let output = Command::new(env!("CARGO_BIN_EXE_prajna-mvp6-freeze"))
        .args([
            "import-existing",
            "--test-root",
            root.path().to_str().unwrap(),
            "--start",
            START,
            "--end",
            END,
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(manifest_paths(root.path()).is_empty());
    let report = coverage_report(root.path());
    assert!(
        report["tencent_fallback_issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| issue["reason"] == "response_not_one_exact_unadjusted_cell")
    );
}

#[test]
fn local_capture_import_rejects_whitespace_only_tencent_network_provenance() {
    let root = TempDir::new().unwrap();
    let tdx = tdx_package();
    let failed = b"access denied";
    let calendar = calendar_month();
    let tdx_sha = sha256(&tdx);
    let failed_sha = sha256(failed);
    let calendar_sha = sha256(&calendar);
    write_capture_tree(
        root.path(),
        &tdx,
        failed,
        &calendar,
        &tdx_sha,
        &failed_sha,
        &calendar_sha,
    );
    append_tencent_capture_with_network_path(
        root.path(),
        "159002",
        &tencent_response("159002"),
        "   ",
    );
    fs::write(
        root.path().join("acceptance/tencent-fallback-plan.json"),
        serde_json::to_vec(&serde_json::json!([{
            "symbol": "159002",
            "session": START,
            "tdx_observation": "missing"
        }]))
        .unwrap(),
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_prajna-mvp6-freeze"))
        .args([
            "import-existing",
            "--test-root",
            root.path().to_str().unwrap(),
            "--start",
            START,
            "--end",
            END,
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(manifest_paths(root.path()).is_empty());
    let report = coverage_report(root.path());
    assert!(
        report["tencent_fallback_issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| issue["reason"] == "request_provenance_incomplete")
    );
}

#[test]
fn cninfo_cli_rejects_a_shortened_collection_window_before_network_access() {
    let root = TempDir::new().unwrap();
    let codes = root.path().join("codes.txt");
    fs::write(&codes, "159001\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_prajna-cninfo"))
        .args([
            "2022-10-02",
            "2026-09-30",
            codes.to_str().unwrap(),
            "direct domestic route",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
}

fn write_capture_tree(
    root: &Path,
    tdx: &[u8],
    failed: &[u8],
    calendar: &[u8],
    tdx_sha: &str,
    failed_sha: &str,
    calendar_sha: &str,
) {
    fs::create_dir_all(root.join("raw/tdx-daily")).unwrap();
    fs::create_dir_all(root.join("raw/szse-calendar-rerun")).unwrap();
    fs::create_dir_all(root.join("acceptance")).unwrap();
    fs::write(root.join("raw/tdx-daily/20221010-attempt1.zip"), tdx).unwrap();
    fs::write(root.join("raw/tdx-daily/20221010-attempt2.body"), failed).unwrap();
    fs::write(root.join("raw/szse-calendar-rerun/2022-10.body"), calendar).unwrap();
    fs::write(
        root.join("acceptance/szse-open-dates.json"),
        format!("[\"{START}\"]\n"),
    )
    .unwrap();
    write_tdx_logs(
        root,
        &[
            format!(
                "2022-10-10,1,2026-10-15T11:00:00Z,https://www.tdx.com.cn/products/data/data/g4day/20221010.zip,200,,{},{}",
                tdx.len(),
                tdx_sha
            ),
            format!(
                "2022-10-10,2,2026-10-15T11:01:00Z,https://www.tdx.com.cn/products/data/data/g4day/20221010.zip,403,access_denied,{},{}",
                failed.len(),
                failed_sha
            ),
        ],
        &[format!("2022-10-10,{tdx_sha},true,true,true,1,1,1,1,true")],
    );
    write_calendar_capture(root, calendar, calendar_sha);
}

fn write_tdx_logs(root: &Path, requests: &[String], validations: &[String]) {
    fs::create_dir_all(root.join("acceptance")).unwrap();
    let mut request_csv =
        String::from("session,attempt,observed_at,url,http_status,error,bytes,sha256\n");
    request_csv.push_str(&requests.join("\n"));
    request_csv.push('\n');
    fs::write(root.join("acceptance/tdx-daily-requests.csv"), request_csv).unwrap();
    let mut validation_csv = String::from(
        "session,sha256,zip_crc_pass,sz_cod_present,sz_md1_present,cod_records,md1_blocks,scope_codes,scope_records,structure_pass\n",
    );
    validation_csv.push_str(&validations.join("\n"));
    validation_csv.push('\n');
    fs::write(
        root.join("acceptance/tdx-daily-validation.csv"),
        validation_csv,
    )
    .unwrap();
}

fn write_calendar_capture(root: &Path, calendar: &[u8], calendar_sha: &str) {
    fs::create_dir_all(root.join("raw/szse-calendar-rerun")).unwrap();
    fs::create_dir_all(root.join("acceptance")).unwrap();
    fs::write(root.join("raw/szse-calendar-rerun/2022-10.body"), calendar).unwrap();
    fs::write(
        root.join("acceptance/szse-calendar-requests.csv"),
        format!(
            "month,url,observed_at,attempt,http_status,error,bytes,sha256\n2022-10,https://www.szse.cn/api/report/exchange/onepersistenthour/monthList?month=2022-10,2026-10-15T10:00:00Z,1,200,,{},{}\n",
            calendar.len(), calendar_sha
        ),
    )
    .unwrap();
}

fn calendar_month() -> Vec<u8> {
    let rows: Vec<_> = (1..=31)
        .map(|day| {
            serde_json::json!({
                "jyrq": format!("2022-10-{day:02}"),
                "jybz": if day == 10 { "1" } else { "0" }
            })
        })
        .collect();
    serde_json::to_vec(&serde_json::json!({ "data": rows })).unwrap()
}

fn tencent_response(symbol: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "code": 0,
        "data": {
            format!("sz{symbol}"): {
                "day": [[START, "1.000", "1.050", "1.100", "0.900", "100"]]
            }
        }
    }))
    .unwrap()
}

fn tencent_response_with_close(symbol: &str, close: &str) -> Vec<u8> {
    let mut response: Value = serde_json::from_slice(&tencent_response(symbol)).unwrap();
    response["data"][format!("sz{symbol}")]["day"][0][1] = Value::String(close.to_owned());
    serde_json::to_vec(&response).unwrap()
}

fn write_tencent_capture(root: &Path, symbol: &str, response: &[u8]) -> String {
    fs::create_dir_all(root.join("acceptance")).unwrap();
    fs::write(
        root.join("acceptance/tencent-fallback-plan.json"),
        serde_json::to_vec(&serde_json::json!([{
            "symbol": symbol,
            "session": START,
            "tdx_observation": "missing"
        }]))
        .unwrap(),
    )
    .unwrap();
    append_tencent_capture(root, symbol, response)
}

fn append_tencent_capture(root: &Path, symbol: &str, response: &[u8]) -> String {
    append_tencent_capture_with_network_path(
        root,
        symbol,
        response,
        "operator-confirmed direct domestic route",
    )
}

fn append_tencent_capture_with_network_path(
    root: &Path,
    symbol: &str,
    response: &[u8],
    network_path: &str,
) -> String {
    let digest = sha256(response);
    fs::create_dir_all(root.join("raw/tencent-responses")).unwrap();
    fs::write(
        root.join(format!("raw/tencent-responses/{digest}")),
        response,
    )
    .unwrap();
    let parameter = format!("sz{symbol},day,{START},{START},1,");
    let record = serde_json::json!({
        "url": format!(
            "https://web.ifzq.gtimg.cn/appstock/app/fqkline/get?param={}",
            parameter.replace(',', "%2C")
        ),
        "parameters": {"param": parameter},
        "observed_at": "2026-10-15T11:02:00Z",
        "http_status": 200,
        "byte_len": response.len(),
        "sha256": digest,
        "network_path": network_path,
        "terms_conclusion": "issue_172_owner_authorization_for_local_noncommercial_research",
        "error": null,
        "outcome": "success"
    });
    use std::io::Write as _;
    let mut log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("acceptance/tencent-requests.jsonl"))
        .unwrap();
    writeln!(log, "{}", serde_json::to_string(&record).unwrap()).unwrap();
    digest
}

fn tdx_package() -> Vec<u8> {
    tdx_package_for("221010")
}

fn tdx_package_for(day_key: &str) -> Vec<u8> {
    let mut code = vec![0_u8; 150];
    code[..6].copy_from_slice(b"159001");
    code[32..34].copy_from_slice(&0_u16.to_le_bytes());
    code[40..43].copy_from_slice(b"ETF");
    let mut bar = vec![0_u8; 512];
    for (offset, value) in [
        (4, 1.0_f64),
        (12, 1.0),
        (20, 1.1),
        (28, 0.9),
        (36, 1.05),
        (72, 100_000.0),
    ] {
        bar[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }
    bar[56..64].copy_from_slice(&100_000_u64.to_le_bytes());
    let mut writer = ZipWriter::new(std::io::Cursor::new(Vec::new()));
    writer
        .start_file(format!("sz{day_key}.cod"), SimpleFileOptions::default())
        .unwrap();
    writer.write_all(&code).unwrap();
    writer
        .start_file(format!("sz{day_key}.md1"), SimpleFileOptions::default())
        .unwrap();
    writer.write_all(&bar).unwrap();
    writer.finish().unwrap().into_inner()
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn manifest_paths(root: &Path) -> Vec<PathBuf> {
    let directory = root.join("manifests");
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect()
}

fn coverage_report(root: &Path) -> Value {
    let path = root.join("acceptance/mvp6-coverage.json");
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
