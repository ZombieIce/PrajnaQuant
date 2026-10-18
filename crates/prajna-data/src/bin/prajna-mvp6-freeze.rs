//! Imports previously captured MVP-6 source responses into the local Raw Store.
//!
//! This command deliberately performs no network access. It verifies each local
//! response against the capture log, retains failed response bodies and their
//! request metadata, then publishes the v4 dataset only when calendar and TDX
//! session coverage is closed for the requested window.

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    error::Error,
    fs::{self, File},
    io::{self, Cursor, Read},
    path::{Path, PathBuf},
};

use chrono::{Datelike, NaiveDate};
use fs2::FileExt;
use prajna_data::{
    NormalizerRegistry, PublishError, RawStore, SourceKind, SourceRecordInput,
    lock_mvp6_coverage_report, publish_dataset,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use zip::ZipArchive;

const SPEC_START: &str = "2022-10-01";
const SPEC_END: &str = "2026-09-30";
const TDX_SOURCE_ID: &str = "tdx_daily";
const CALENDAR_SOURCE_ID: &str = "szse_calendar";

type AppResult<T> = Result<T, Box<dyn Error>>;
type CsvRow = BTreeMap<String, String>;

struct CalendarCapture {
    raw_id: String,
    days: BTreeMap<NaiveDate, bool>,
}

struct TencentImportContext<'a> {
    root: &'a Path,
    start: NaiveDate,
    end: NaiveDate,
    store: &'a RawStore,
    tdx_sessions: &'a BTreeMap<NaiveDate, BTreeSet<String>>,
    open_dates: &'a BTreeSet<NaiveDate>,
}

fn main() {
    if run().is_err() {
        // Keep response metadata and errors in the user's local acceptance
        // directory; terminal and CI logs never receive real-data details.
        eprintln!("MVP-6 local import unresolved; consult local acceptance files");
        std::process::exit(1);
    }
}

fn run() -> AppResult<()> {
    let args = parse_args()?;
    let is_default_root = args.root.is_none();
    let is_test_root = args.test_root;
    let requested_root = args.root.unwrap_or_else(default_root);
    if !is_default_root && !is_test_root && requested_root != default_root() {
        return Err(invalid("MVP-6 data must use ~/PrajnaData/mvp-6"));
    }
    let temp_root = std::env::temp_dir().canonicalize()?;
    if is_test_root && !requested_root.canonicalize()?.starts_with(&temp_root) {
        return Err(invalid(
            "--test-root is limited to the system temp directory",
        ));
    }
    fs::create_dir_all(&requested_root)?;
    let root = requested_root.canonicalize()?;
    if (!is_test_root && root != requested_root) || (is_test_root && !root.starts_with(&temp_root))
    {
        return Err(invalid(
            "MVP-6 data must use ~/PrajnaData/mvp-6; --test-root is limited to the system temp directory",
        ));
    }
    let lock = OpenOptionsExt::lock(&root.join("mvp6-freeze.lock"))?;
    let _lock = lock;

    let mut report = base_report(args.start, args.end);
    let mut inventory = Vec::new();
    report["import_status"] = json!("running");
    let result = import_existing(&root, args.start, args.end, &mut report, &mut inventory);
    report["import_status"] = json!(if result.is_ok() {
        "complete"
    } else {
        "unresolved"
    });
    let coverage_lock = lock_mvp6_coverage_report(&root)?;
    let coverage_path = root.join("acceptance/mvp6-coverage.json");
    let latest_report = match fs::read(&coverage_path) {
        Ok(bytes) => Some(serde_json::from_slice::<Value>(&bytes)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if let Some(previous) = latest_report.as_ref() {
        if result.is_ok()
            && report.get("dsv").and_then(Value::as_str).is_some()
            && previous.get("dsv") == report.get("dsv")
        {
            preserve_collection_coverage(previous, &mut report);
        } else if result.is_err() && previous["dataset_published"] == true {
            preserve_collection_coverage(previous, &mut report);
            report["failed_import_window"] = report["window"].clone();
            for key in [
                "window",
                "dsv",
                "dataset_published",
                "spec_window_complete",
                "mvp6_complete",
                "input_hashes",
                "coverage",
            ] {
                if let Some(value) = previous.get(key) {
                    report[key] = value.clone();
                }
            }
        }
    }
    if report["historical_capture_status"] == "unresolved" {
        report["mvp6_complete"] = json!(false);
    }
    write_json(&coverage_path, &report)?;
    drop(coverage_lock);
    let inventory_path = root.join("acceptance/mvp6-raw-inventory.json");
    if result.is_err() {
        inventory = merge_previous_inventory(&inventory_path, inventory)?;
    }
    write_json(
        &inventory_path,
        &json!({
            "contract": "mvp-6/#206",
            "request_records": inventory,
        }),
    )?;
    result
}

/// A failed import may stop before re-listing earlier Raw objects; keep every
/// previously frozen request record and append only records not yet present.
fn merge_previous_inventory(path: &Path, current: Vec<Value>) -> AppResult<Vec<Value>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(current),
        Err(error) => return Err(error.into()),
    };
    let previous: Value = serde_json::from_slice(&bytes)?;
    let mut merged = previous
        .get("request_records")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| invalid("existing Raw inventory lacks request_records"))?;
    let mut seen: HashSet<String> = merged.iter().map(Value::to_string).collect();
    for record in current {
        if seen.insert(record.to_string()) {
            merged.push(record);
        }
    }
    Ok(merged)
}

fn preserve_collection_coverage(previous: &Value, current: &mut Value) {
    for key in [
        "missing_org_id_codes",
        "missing_legal_names",
        "sina_failures",
        "p21_mismatches",
        "unknown_rule_sessions",
        "cninfo_unknown_facts",
        "mvp6_complete",
    ] {
        if let Some(value) = previous.get(key) {
            current[key] = value.clone();
        }
    }
    for source in [
        "tencent_fallback",
        "sina_adjustment_factors",
        "cninfo",
        "p21_crosscheck",
        "unknown_rule_sessions",
    ] {
        if let Some(value) = previous
            .get("source_status")
            .and_then(|status| status.get(source))
        {
            current["source_status"][source] = value.clone();
        }
    }
}

fn import_tencent_fallback(
    context: TencentImportContext<'_>,
    inventory: &mut Vec<Value>,
    report: &mut Value,
) -> AppResult<BTreeSet<String>> {
    let TencentImportContext {
        root,
        start,
        end,
        store,
        tdx_sessions,
        open_dates,
    } = context;
    let plan_path = root.join("acceptance/tencent-fallback-plan.json");
    let log_path = root.join("acceptance/tencent-requests.jsonl");
    let plan_exists = plan_path.is_file();
    let log_exists = log_path.is_file();
    let mut issues = Vec::new();
    let mut planned = BTreeSet::new();

    if plan_exists {
        let plan: Value = serde_json::from_slice(&fs::read(&plan_path)?)?;
        let entries = plan
            .as_array()
            .ok_or_else(|| invalid("Tencent fallback plan must be a JSON array"))?;
        for entry in entries {
            let Some(object) = entry.as_object() else {
                issues.push(json!({"reason": "invalid_plan_entry"}));
                continue;
            };
            let symbol = object.get("symbol").and_then(Value::as_str);
            let session = object.get("session").and_then(Value::as_str);
            let tdx_observation = object.get("tdx_observation").and_then(Value::as_str);
            if object.len() != 3
                || !symbol.is_some_and(scoped_tencent_symbol)
                || tdx_observation != Some("missing")
            {
                issues.push(json!({"reason": "plan_must_name_only_missing_scoped_tdx_cells"}));
                continue;
            }
            let (Some(symbol), Some(session_text)) = (symbol, session) else {
                issues.push(json!({"reason": "invalid_plan_entry"}));
                continue;
            };
            let Ok(session) = date(session_text) else {
                issues.push(json!({"reason": "invalid_plan_session"}));
                continue;
            };
            if session < start || session > end || !open_dates.contains(&session) {
                issues.push(json!({"symbol": symbol, "session": session, "reason": "plan_outside_open_spec_window"}));
                continue;
            }
            if !planned.insert((session, symbol.to_owned())) {
                issues.push(
                    json!({"symbol": symbol, "session": session, "reason": "duplicate_plan_cell"}),
                );
            }
        }
    } else if log_exists {
        issues.push(json!({"reason": "request_log_without_explicit_missing_cell_plan"}));
    }

    let mut successful: BTreeMap<(NaiveDate, String), BTreeSet<String>> = BTreeMap::new();
    if log_exists {
        let text = fs::read_to_string(&log_path)?;
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let record: Value = serde_json::from_str(line)?;
            let url = record
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let parameters = record.get("parameters").cloned().unwrap_or(Value::Null);
            let parameter = parameters
                .get("param")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let Some((symbol, session)) = parse_tencent_cell(parameter) else {
                issues.push(json!({"reason": "request_parameters_not_one_exact_cell"}));
                continue;
            };
            if !planned.contains(&(session, symbol.clone())) {
                issues.push(json!({"symbol": symbol, "session": session, "reason": "request_not_in_explicit_plan"}));
            }
            if record.get("terms_conclusion").and_then(Value::as_str)
                != Some("issue_172_owner_authorization_for_local_noncommercial_research")
                || record
                    .get("network_path")
                    .and_then(Value::as_str)
                    .is_none_or(|path| path.trim().is_empty())
            {
                issues.push(json!({"symbol": symbol, "session": session, "reason": "request_provenance_incomplete"}));
            }
            let sha = match record
                .get("sha256")
                .and_then(Value::as_str)
                .and_then(|value| normalize_hash(value).ok())
            {
                Some(sha) => sha,
                None => {
                    issues.push(json!({"symbol": symbol, "session": session, "reason": "invalid_response_hash"}));
                    continue;
                }
            };
            let byte_len = record
                .get("byte_len")
                .and_then(Value::as_u64)
                .unwrap_or(u64::MAX);
            let observed_at = record
                .get("observed_at")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if validate_observed_at(observed_at).is_err() {
                issues.push(
                    json!({"symbol": symbol, "session": session, "reason": "invalid_observed_at"}),
                );
                continue;
            }
            inventory.push(json!({
                "source_id": "tencent_kline",
                "url": url,
                "params": parameters,
                "observed_at": observed_at,
                "http_status": record.get("http_status"),
                "error": record.get("error"),
                "byte_len": byte_len,
                "sha256": sha,
                "network_path": record.get("network_path"),
                "terms_conclusion": record.get("terms_conclusion")
            }));

            let hex = sha.strip_prefix("sha256:").unwrap_or(&sha);
            let body_path = root.join("raw/tencent-responses").join(hex);
            let body = match fs::read(body_path) {
                Ok(body) if body.len() as u64 == byte_len && sha256(&body) == hex => body,
                _ => {
                    issues.push(json!({"symbol": symbol, "session": session, "reason": "response_body_missing_or_hash_mismatch"}));
                    continue;
                }
            };
            let status = record
                .get("http_status")
                .and_then(Value::as_u64)
                .and_then(|value| u16::try_from(value).ok())
                .unwrap_or(0);
            let outcome = record
                .get("outcome")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let error = record.get("error").filter(|value| !value.is_null());
            let expected_param = format!("sz{symbol},day,{session},{session},1,");
            let expected_url = format!(
                "https://web.ifzq.gtimg.cn/appstock/app/fqkline/get?param={}",
                expected_param.replace(',', "%2C")
            );
            if url != expected_url || parameter != expected_param {
                issues.push(json!({"symbol": symbol, "session": session, "reason": "source_or_request_mismatch"}));
            }
            let stored = put_once(
                store,
                &body,
                SourceRecordInput {
                    content_type: "application/json".into(),
                    source_kind: SourceKind::FileImport,
                    source_id: "tencent_kline".into(),
                    request: json!({
                        "url": url,
                        "params": parameters,
                        "http_status": status,
                        "byte_len": body.len(),
                        "sha256": sha,
                        "error": error,
                        "outcome": outcome,
                        "network_path": record.get("network_path"),
                        "terms_conclusion": record.get("terms_conclusion")
                    }),
                    observed_at: observed_at.to_owned(),
                    ingested_by: "prajna-mvp6-freeze/1".into(),
                },
            )?;
            if outcome == "unresolved"
                && error.and_then(Value::as_str) == Some("access_restriction")
            {
                issues.push(json!({"symbol": symbol, "session": session, "reason": "source_access_restriction"}));
            }
            if outcome == "success" && error.is_none() && status == 200 {
                if !tencent_response_is_exact(&body, &symbol, session) {
                    issues.push(json!({"symbol": symbol, "session": session, "reason": "response_not_one_exact_unadjusted_cell"}));
                } else {
                    successful
                        .entry((session, symbol))
                        .or_default()
                        .insert(stored);
                }
            }
        }
    }

    if !plan_exists && !log_exists {
        report["tencent_fallback_plan_status"] = json!("no_plan_or_requests");
        report["source_status"]["tencent_fallback"] = json!("not_imported");
        report["tencent_fallback_issues"] = json!(issues);
        return Ok(BTreeSet::new());
    }
    for cell in &planned {
        match successful.get(cell) {
            None => issues.push(json!({"symbol": cell.1, "session": cell.0, "reason": "planned_cell_has_no_successful_response"})),
            Some(ids) if ids.len() > 1 => issues.push(json!({"symbol": cell.1, "session": cell.0, "reason": "planned_cell_has_conflicting_successful_response_revisions"})),
            Some(_) => {}
        }
        let Some(tdx_ids) = tdx_sessions.get(&cell.0).filter(|ids| ids.len() == 1) else {
            issues.push(json!({"symbol": cell.1, "session": cell.0, "reason": "tdx_session_package_missing_or_conflicting"}));
            continue;
        };
        let Some(tdx_id) = tdx_ids.iter().next() else {
            continue;
        };
        let tdx_bytes = store.get(tdx_id)?;
        match tdx_package_has_symbol(&tdx_bytes, cell.0, &cell.1) {
            Ok(true) => issues.push(json!({"symbol": cell.1, "session": cell.0, "reason": "tdx_keyed_observation_blocks_tencent_fallback"})),
            Ok(false) => {}
            Err(_) => issues.push(json!({"symbol": cell.1, "session": cell.0, "reason": "tdx_package_cell_could_not_be_verified"})),
        }
    }
    for cell in successful.keys() {
        if !planned.contains(cell) {
            issues.push(json!({"symbol": cell.1, "session": cell.0, "reason": "successful_cell_not_in_explicit_plan"}));
        }
    }
    let input_ids: BTreeSet<_> = successful
        .iter()
        .filter(|(cell, ids)| planned.contains(*cell) && ids.len() == 1)
        .flat_map(|(_, ids)| ids.iter().cloned())
        .collect();
    report["tencent_fallback_plan_status"] = json!(if issues.is_empty() {
        "validated"
    } else {
        "unresolved"
    });
    report["source_status"]["tencent_fallback"] = json!(if planned.is_empty() {
        "no_missing_cells_planned"
    } else if issues.is_empty() {
        "validated_local_fills"
    } else {
        "unresolved"
    });
    report["tencent_fallback_issues"] = json!(issues);
    Ok(input_ids)
}

fn scoped_tencent_symbol(symbol: &str) -> bool {
    symbol.len() == 6
        && symbol.bytes().all(|byte| byte.is_ascii_digit())
        && symbol
            .parse::<u32>()
            .is_ok_and(|code| (158000..=159999).contains(&code))
}

fn parse_tencent_cell(parameter: &str) -> Option<(String, NaiveDate)> {
    let parts: Vec<_> = parameter.split(',').collect();
    if parts.len() != 6
        || parts[1] != "day"
        || parts[2] != parts[3]
        || parts[4] != "1"
        || !parts[5].is_empty()
    {
        return None;
    }
    let symbol = parts[0].strip_prefix("sz")?.to_owned();
    if !scoped_tencent_symbol(&symbol) {
        return None;
    }
    Some((
        symbol,
        NaiveDate::parse_from_str(parts[2], "%Y-%m-%d").ok()?,
    ))
}

fn tencent_response_is_exact(bytes: &[u8], symbol: &str, session: NaiveDate) -> bool {
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return false;
    };
    let response_code = value.get("code").map(|code| match code {
        Value::String(value) => value.clone(),
        Value::Number(value) => value.to_string(),
        _ => String::new(),
    });
    let Some(data) = value.get("data").and_then(Value::as_object) else {
        return false;
    };
    if data.len() != 1 || !data.contains_key(format!("sz{symbol}").as_str()) {
        return false;
    }
    let Some(rows) = data
        .get(format!("sz{symbol}").as_str())
        .and_then(|node| node.get("day"))
        .and_then(Value::as_array)
    else {
        return false;
    };
    response_code.as_deref() == Some("0")
        && rows.len() == 1
        && rows[0]
            .as_array()
            .and_then(|row| row.first())
            .and_then(Value::as_str)
            == Some(session.to_string().as_str())
        && rows[0].as_array().is_some_and(|row| row.len() >= 6)
}

fn tdx_package_has_symbol(bytes: &[u8], session: NaiveDate, symbol: &str) -> AppResult<bool> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))?;
    let member_name = format!("sz{}.cod", session.format("%y%m%d"));
    let mut member = archive.by_name(&member_name)?;
    let mut contents = Vec::new();
    member.read_to_end(&mut contents)?;
    if contents.len() % 150 != 0 {
        return Err(invalid("TDX code table is not record aligned"));
    }
    Ok(contents
        .chunks_exact(150)
        .any(|record| record.get(..6) == Some(symbol.as_bytes())))
}

fn tdx_package_matches_session(bytes: &[u8], session: NaiveDate) -> bool {
    let Ok(archive) = ZipArchive::new(Cursor::new(bytes)) else {
        return false;
    };
    let day_key = session.format("%y%m%d").to_string();
    let expected_cod = format!("sz{day_key}.cod");
    let expected_md1 = format!("sz{day_key}.md1");
    let code_members = archive
        .file_names()
        .filter(|name| name.starts_with("sz") && name.ends_with(".cod"))
        .collect::<Vec<_>>();
    code_members.as_slice() == [expected_cod.as_str()]
        && archive.file_names().any(|name| name == expected_md1)
}

struct Args {
    root: Option<PathBuf>,
    test_root: bool,
    start: NaiveDate,
    end: NaiveDate,
}

fn parse_args() -> AppResult<Args> {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some("import-existing") {
        return Err(invalid(
            "usage: prajna-mvp6-freeze import-existing [--root DIR] [--start DATE] [--end DATE]",
        ));
    }
    let mut root = None;
    let mut test_root = false;
    let mut start = date(SPEC_START)?;
    let mut end = date(SPEC_END)?;
    while let Some(option) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| invalid("option requires a value"))?;
        match option.as_str() {
            "--root" => root = Some(PathBuf::from(value)),
            "--test-root" => {
                root = Some(PathBuf::from(value));
                test_root = true;
            }
            "--start" => start = date(&value)?,
            "--end" => end = date(&value)?,
            _ => return Err(invalid("unknown option")),
        }
    }
    if start < date(SPEC_START)? || end > date(SPEC_END)? || start > end {
        return Err(invalid(
            "window must be ordered and inside the MVP-6 specification range",
        ));
    }
    Ok(Args {
        root,
        test_root,
        start,
        end,
    })
}

fn default_root() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("PrajnaData/mvp-6")
}

fn import_existing(
    root: &Path,
    start: NaiveDate,
    end: NaiveDate,
    report: &mut Value,
    inventory: &mut Vec<Value>,
) -> AppResult<()> {
    report["import_stage"] = json!("read_capture_logs");
    let store = RawStore::open(root)?;
    let expected_months = months_between(start, end);
    let tdx_requests = read_csv(&root.join("acceptance/tdx-daily-requests.csv"))?;
    let tdx_validation = read_csv(&root.join("acceptance/tdx-daily-validation.csv"))?;
    let calendar_requests = read_csv(&root.join("acceptance/szse-calendar-requests.csv"))?;
    let tdx_response_files = collect_files(&root.join("raw/tdx-daily"))?;
    let calendar_response_files = collect_files(&root.join("raw/szse-calendar-rerun"))?;
    let validation = tdx_validation
        .iter()
        .filter_map(|row| {
            Some((
                (
                    date(required(row, "session").ok()?).ok()?,
                    normalize_hash(required(row, "sha256").ok()?).ok()?,
                ),
                row.clone(),
            ))
        })
        .collect::<BTreeMap<_, _>>();

    let mut tdx_valid: BTreeMap<NaiveDate, BTreeSet<String>> = BTreeMap::new();
    let mut tdx_seen_dates = BTreeSet::new();
    report["import_stage"] = json!("validate_tdx_captures");
    for row in &tdx_requests {
        let session = match required(row, "session").and_then(date) {
            Ok(session) => session,
            Err(_) => {
                report["invalid_tdx_requests"]
                    .as_array_mut()
                    .expect("array")
                    .push(json!({"reason": "invalid_or_missing_session"}));
                inventory.push(inventory_record_raw(TDX_SOURCE_ID, row));
                continue;
            }
        };
        if session < start || session > end {
            continue;
        }
        tdx_seen_dates.insert(session);
        inventory.push(inventory_record_raw(TDX_SOURCE_ID, row));
        if row.get("error").is_some_and(|error| !error.is_empty()) {
            import_failed_tdx_capture(
                &store,
                &tdx_response_files,
                session,
                row,
                &validation,
                report,
            )?;
            continue;
        }
        let status = match row
            .get("http_status")
            .filter(|value| !value.is_empty())
            .map(|value| parse_u16(value))
            .transpose()
        {
            Ok(Some(status)) => status,
            Ok(None) | Err(_) => {
                report["invalid_tdx_requests"].as_array_mut().expect("array")
                    .push(json!({"session": session.to_string(), "reason": "invalid_or_missing_http_status"}));
                continue;
            }
        };
        let url = match required(row, "url") {
            Ok(url) => url,
            Err(_) => {
                report["invalid_tdx_requests"]
                    .as_array_mut()
                    .expect("array")
                    .push(json!({"session": session.to_string(), "reason": "missing_url"}));
                continue;
            }
        };
        let sha = match required(row, "sha256").and_then(normalize_hash) {
            Ok(sha) => sha,
            Err(_) => {
                report["invalid_tdx_requests"].as_array_mut().expect("array")
                    .push(json!({"session": session.to_string(), "reason": "invalid_or_missing_sha256"}));
                continue;
            }
        };
        let bytes = match required(row, "bytes").and_then(parse_u64) {
            Ok(bytes) => bytes,
            Err(_) => {
                report["invalid_tdx_requests"].as_array_mut().expect("array")
                    .push(json!({"session": session.to_string(), "reason": "invalid_or_missing_byte_length"}));
                continue;
            }
        };
        let observed_at = match required(row, "observed_at").and_then(|value| {
            validate_observed_at(value)?;
            Ok(value)
        }) {
            Ok(observed_at) => observed_at,
            Err(_) => {
                report["invalid_tdx_requests"].as_array_mut().expect("array")
                    .push(json!({"session": session.to_string(), "reason": "invalid_or_missing_observed_at"}));
                continue;
            }
        };
        let _ = observed_at;
        let expected_url = format!(
            "https://www.tdx.com.cn/products/data/data/g4day/{}.zip",
            session.format("%Y%m%d")
        );
        if url != expected_url {
            report["invalid_tdx_requests"]
                .as_array_mut()
                .expect("array")
                .push(json!({"session": session.to_string(), "reason": "source_or_url_mismatch"}));
            continue;
        }
        if status != 200 && bytes == 0 {
            if sha != format!("sha256:{}", sha256(&[])) {
                report["invalid_tdx_requests"].as_array_mut().expect("array")
                    .push(json!({"session": session.to_string(), "reason": "empty_response_hash_mismatch"}));
                continue;
            }
            put_once(
                &store,
                &[],
                request_record(
                    TDX_SOURCE_ID,
                    row,
                    &sha,
                    Some(status),
                    "application/octet-stream",
                ),
            )?;
            continue;
        }
        let session_key = session.format("%Y%m%d").to_string();
        let Some(body) = find_logged_body(&tdx_response_files, &session_key, &sha)? else {
            report["missing_response_bodies"]
                .as_array_mut()
                .expect("array")
                .push(
                    json!({"source": TDX_SOURCE_ID, "session": session.to_string(), "sha256": sha}),
                );
            continue;
        };
        if body.len() as u64 != bytes {
            report["invalid_tdx_requests"]
                .as_array_mut()
                .expect("array")
                .push(json!({"session": session.to_string(), "reason": "byte_length_mismatch"}));
            continue;
        }
        let validation_passed = validation_passes(validation.get(&(session, sha.clone())));
        if status == 200 && validate_zip_archive(&body).is_err() {
            let mut source = request_record(
                TDX_SOURCE_ID,
                row,
                &sha,
                Some(status),
                "application/octet-stream",
            );
            source.request["outcome"] = json!("failed_response");
            source.request["validation_error"] = json!("invalid_zip_archive");
            put_once(&store, &body, source)?;
            if validation_passed {
                record_tdx_archive_integrity_mismatch(report, session);
            } else {
                historical_capture_issue(report, row, "invalid_zip_archive");
            }
            continue;
        }
        let id = put_once(
            &store,
            &body,
            request_record(
                TDX_SOURCE_ID,
                row,
                &sha,
                Some(status),
                if status == 200 {
                    "application/zip"
                } else {
                    "application/octet-stream"
                },
            ),
        )?;
        if status == 200 {
            let reason = if !validation_passed {
                Some("package_validation_missing_or_failed")
            } else if !tdx_package_matches_session(&body, session) {
                Some("package_session_members_missing_or_mismatched")
            } else {
                None
            };
            if let Some(reason) = reason {
                report["invalid_tdx_sessions"]
                    .as_array_mut()
                    .expect("array")
                    .push(json!({"session": session.to_string(), "reason": reason}));
            } else {
                tdx_valid.entry(session).or_default().insert(id);
            }
        }
    }
    report["historical_capture_status"] = json!(if report["historical_capture_issues"]
        .as_array()
        .is_some_and(Vec::is_empty)
    {
        "complete"
    } else {
        "unresolved"
    });

    report["import_stage"] = json!("validate_szse_calendar_captures");
    let calendar_rows = calendar_requests
        .iter()
        .filter(|row| {
            required(row, "month").ok().is_none_or(|month| {
                !valid_month(month) || expected_months.iter().any(|expected| expected == month)
            })
        })
        .collect::<Vec<_>>();
    let mut calendars: BTreeMap<String, CalendarCapture> = BTreeMap::new();
    let mut calendar_revision_conflicts = BTreeSet::new();
    for row in calendar_rows {
        let month = match required(row, "month") {
            Ok(month) => month,
            Err(_) => {
                report["invalid_calendar_months"]
                    .as_array_mut()
                    .expect("array")
                    .push(json!({"reason": "missing_month"}));
                inventory.push(inventory_record_raw(CALENDAR_SOURCE_ID, row));
                continue;
            }
        };
        inventory.push(inventory_record_raw(CALENDAR_SOURCE_ID, row));
        let url = match required(row, "url") {
            Ok(url) => url,
            Err(_) => {
                report["invalid_calendar_months"]
                    .as_array_mut()
                    .expect("array")
                    .push(json!({"month": month, "reason": "missing_url"}));
                continue;
            }
        };
        let status = match row
            .get("http_status")
            .filter(|value| !value.is_empty())
            .map(|value| parse_u16(value))
            .transpose()
        {
            Ok(Some(status)) => status,
            Ok(None) | Err(_) => {
                report["invalid_calendar_months"]
                    .as_array_mut()
                    .expect("array")
                    .push(json!({"month": month, "reason": "invalid_or_missing_http_status"}));
                continue;
            }
        };
        let sha = match required(row, "sha256").and_then(normalize_hash) {
            Ok(sha) => sha,
            Err(_) => {
                report["invalid_calendar_months"]
                    .as_array_mut()
                    .expect("array")
                    .push(json!({"month": month, "reason": "invalid_or_missing_sha256"}));
                continue;
            }
        };
        let bytes_len = match required(row, "bytes").and_then(parse_u64) {
            Ok(bytes) => bytes,
            Err(_) => {
                report["invalid_calendar_months"]
                    .as_array_mut()
                    .expect("array")
                    .push(json!({"month": month, "reason": "invalid_or_missing_byte_length"}));
                continue;
            }
        };
        let observed_at = match required(row, "observed_at").and_then(|value| {
            validate_observed_at(value)?;
            Ok(value)
        }) {
            Ok(observed_at) => observed_at,
            Err(_) => {
                report["invalid_calendar_months"]
                    .as_array_mut()
                    .expect("array")
                    .push(json!({"month": month, "reason": "invalid_or_missing_observed_at"}));
                continue;
            }
        };
        let _ = observed_at;
        let expected_url = format!(
            "https://www.szse.cn/api/report/exchange/onepersistenthour/monthList?month={month}"
        );
        if url != expected_url || !valid_month(month) {
            report["invalid_calendar_months"]
                .as_array_mut()
                .expect("array")
                .push(json!({"month": month, "reason": "source_or_url_mismatch"}));
            continue;
        }
        if status != 200 && bytes_len == 0 {
            if sha != format!("sha256:{}", sha256(&[])) {
                report["invalid_calendar_months"]
                    .as_array_mut()
                    .expect("array")
                    .push(json!({"month": month, "reason": "empty_response_hash_mismatch"}));
                continue;
            }
            put_once(
                &store,
                &[],
                request_record(
                    CALENDAR_SOURCE_ID,
                    row,
                    &sha,
                    Some(status),
                    "application/octet-stream",
                ),
            )?;
            continue;
        }
        let Some(body) = find_logged_body(&calendar_response_files, month, &sha)? else {
            report["missing_response_bodies"]
                .as_array_mut()
                .expect("array")
                .push(json!({"source": CALENDAR_SOURCE_ID, "month": month, "sha256": sha}));
            continue;
        };
        if body.len() as u64 != bytes_len {
            report["invalid_calendar_months"]
                .as_array_mut()
                .expect("array")
                .push(json!({"month": month, "reason": "byte_length_mismatch"}));
            continue;
        }
        let id = put_once(
            &store,
            &body,
            request_record(
                CALENDAR_SOURCE_ID,
                row,
                &sha,
                Some(status),
                "application/json",
            ),
        )?;
        if status != 200 {
            continue;
        }
        if calendar_revision_conflicts.contains(month) {
            continue;
        }
        match parse_calendar(&body, month) {
            Ok(days) => {
                if let Some(previous) = calendars.get(month)
                    && previous.raw_id != id
                {
                    report["invalid_calendar_months"]
                        .as_array_mut()
                        .expect("array")
                        .push(json!({"month": month, "reason": "calendar_revision_conflict"}));
                    calendar_revision_conflicts.insert(month.to_owned());
                    calendars.remove(month);
                } else {
                    calendars.insert(month.to_owned(), CalendarCapture { raw_id: id, days });
                }
            }
            Err(_) => report["invalid_calendar_months"]
                .as_array_mut()
                .expect("array")
                .push(json!({"month": month, "reason": "calendar_not_complete_natural_month"})),
        }
    }

    let missing_months: Vec<_> = expected_months
        .iter()
        .filter(|month| !calendars.contains_key(*month))
        .cloned()
        .collect();
    report["missing_calendar_months"] = json!(missing_months);
    let mut calendar_open_dates = BTreeSet::new();
    for capture in calendars.values() {
        calendar_open_dates.extend(
            capture
                .days
                .iter()
                .filter_map(|(day, open)| (*open && *day >= start && *day <= end).then_some(*day)),
        );
    }
    report["import_stage"] = json!("validate_open_dates");
    let expected_open_dates = match load_expected_open_dates(root, start, end) {
        Ok(dates) => Some(dates),
        Err(_) => {
            report["open_date_list_status"] = json!("missing_or_invalid");
            None
        }
    };
    let calendar_difference = expected_open_dates.as_ref().map(|expected| {
        calendar_open_dates
            .symmetric_difference(expected)
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    });
    if let Some(difference) = &calendar_difference {
        report["open_date_list_status"] = json!(if difference.is_empty() {
            "matched"
        } else {
            "mismatch"
        });
    }
    report["open_date_list_mismatches"] = json!(calendar_difference.unwrap_or_default());
    report["unknown_rule_sessions"] = json!(
        expected_open_dates
            .as_ref()
            .map(|dates| dates.iter().map(ToString::to_string).collect::<Vec<_>>())
            .unwrap_or_else(|| calendar_open_dates
                .iter()
                .map(ToString::to_string)
                .collect())
    );
    report["unknown_rule_sessions_status"] =
        json!("unresolved_no_verified_full_window_rule_evidence");

    report["import_stage"] = json!("validate_tencent_fallback");
    let tencent_inputs = import_tencent_fallback(
        TencentImportContext {
            root,
            start,
            end,
            store: &store,
            tdx_sessions: &tdx_valid,
            open_dates: &calendar_open_dates,
        },
        inventory,
        report,
    )?;

    let missing_tdx: Vec<_> = calendar_open_dates
        .iter()
        .filter(|day| tdx_valid.get(day).is_none_or(BTreeSet::is_empty))
        .map(ToString::to_string)
        .collect();
    report["missing_tdx_sessions"] = json!(missing_tdx);
    let tdx_conflicts: Vec<_> = tdx_valid
        .iter()
        .filter(|(_, hashes)| hashes.len() > 1)
        .map(|(day, _)| day.to_string())
        .collect();
    report["tdx_revision_conflict_sessions"] = json!(tdx_conflicts);
    report["request_log_sessions"] = json!(tdx_seen_dates.len());
    report["calendar_months_imported"] = json!(calendars.len());
    report["tdx_sessions_imported"] = json!(tdx_valid.len());
    let has_tdx_body_gaps = report["missing_response_bodies"]
        .as_array()
        .is_some_and(|items| items.iter().any(|item| item["source"] == TDX_SOURCE_ID));
    let tdx_closed = report["invalid_tdx_requests"]
        .as_array()
        .is_some_and(Vec::is_empty)
        && report["invalid_tdx_sessions"]
            .as_array()
            .is_some_and(Vec::is_empty)
        && report["missing_tdx_sessions"]
            .as_array()
            .is_some_and(Vec::is_empty)
        && report["tdx_revision_conflict_sessions"]
            .as_array()
            .is_some_and(Vec::is_empty)
        && !has_tdx_body_gaps;
    let has_calendar_body_gaps =
        report["missing_response_bodies"]
            .as_array()
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item["source"] == CALENDAR_SOURCE_ID)
            });
    let calendar_closed = report["missing_calendar_months"]
        .as_array()
        .is_some_and(Vec::is_empty)
        && report["invalid_calendar_months"]
            .as_array()
            .is_some_and(Vec::is_empty)
        && report["open_date_list_status"] == "matched"
        && report["open_date_list_mismatches"]
            .as_array()
            .is_some_and(Vec::is_empty)
        && !has_calendar_body_gaps;
    report["source_status"]["tdx"] = json!(if tdx_closed {
        "validated_local_captures"
    } else {
        "local_captures_unresolved"
    });
    report["source_status"]["szse_calendar"] = json!(if calendar_closed {
        "validated_local_captures"
    } else {
        "local_captures_unresolved"
    });

    let can_publish = report["missing_calendar_months"]
        .as_array()
        .is_some_and(Vec::is_empty)
        && report["invalid_calendar_months"]
            .as_array()
            .is_some_and(Vec::is_empty)
        && report["invalid_tdx_requests"]
            .as_array()
            .is_some_and(Vec::is_empty)
        && report["missing_tdx_sessions"]
            .as_array()
            .is_some_and(Vec::is_empty)
        && report["tdx_revision_conflict_sessions"]
            .as_array()
            .is_some_and(Vec::is_empty)
        && report["open_date_list_status"] == "matched"
        && report["open_date_list_mismatches"]
            .as_array()
            .is_some_and(Vec::is_empty)
        && report["invalid_tdx_sessions"]
            .as_array()
            .is_some_and(Vec::is_empty)
        && report["missing_response_bodies"]
            .as_array()
            .is_some_and(Vec::is_empty);
    let can_publish = can_publish
        && report["tencent_fallback_issues"]
            .as_array()
            .is_some_and(Vec::is_empty);
    if !can_publish {
        return Err(invalid("source coverage is not closed"));
    }

    report["import_stage"] = json!("publish_dataset");
    let mut inputs = BTreeSet::new();
    inputs.extend(calendars.values().map(|capture| capture.raw_id.clone()));
    inputs.extend(tdx_valid.values().flat_map(|hashes| hashes.iter().cloned()));
    inputs.extend(tencent_inputs);
    let input_hashes: Vec<_> = inputs.into_iter().collect();
    let registry = NormalizerRegistry::with_builtins();
    let created_at = chrono::Utc::now().to_rfc3339();
    let manifest = match publish_dataset(
        root,
        &store,
        &registry,
        "szse-etf-frozen",
        "4",
        &input_hashes,
        created_at,
    ) {
        Ok(manifest) => manifest,
        Err(error) => {
            report["publish_error_kind"] = json!(publish_error_kind(&error));
            return Err(Box::new(error));
        }
    };
    report["dataset_published"] = json!(true);
    report["mvp6_complete"] = json!(false);
    report["dsv"] = json!(manifest.dsv);
    report["input_hashes"] = json!(input_hashes);
    report["coverage"] = manifest.provenance.coverage;
    Ok(())
}

fn base_report(start: NaiveDate, end: NaiveDate) -> Value {
    json!({
        "contract": "mvp-6/#206",
        "window": {"start": start.to_string(), "end": end.to_string()},
        "spec_window_complete": start == NaiveDate::parse_from_str(SPEC_START, "%Y-%m-%d").expect("valid spec start")
            && end == NaiveDate::parse_from_str(SPEC_END, "%Y-%m-%d").expect("valid spec end"),
        "dataset_published": false,
        "mvp6_complete": false,
        "dsv": null,
        "source_status": {
            "tdx": "pending",
            "szse_calendar": "pending",
            "tencent_fallback": "not_imported",
            "sina_adjustment_factors": "not_collected",
            "cninfo": "not_collected",
            "p21_crosscheck": "not_evaluated",
            "unknown_rule_sessions": "not_evaluated"
        },
        "missing_calendar_months": [],
        "invalid_calendar_months": [],
        "invalid_tdx_requests": [],
        "invalid_tdx_sessions": [],
        "historical_capture_issues": [],
        "historical_capture_status": "not_checked",
        "missing_tdx_sessions": [],
        "tdx_revision_conflict_sessions": [],
        "missing_response_bodies": [],
        "tencent_fallback_issues": [],
        "tencent_fallback_plan_status": "not_checked",
        "open_date_list_status": "not_checked",
        "open_date_list_mismatches": [],
        "missing_org_id_codes": [],
        "missing_legal_names": [],
        "sina_failures": [],
        "p21_mismatches": [],
        "unknown_rule_sessions": [],
        "unknown_rule_sessions_status": "not_evaluated",
        "network_path": "unverified",
        "terms_conclusion": "See local collection-environment and issue #172 evidence; this command performs no network access."
    })
}

fn request_record(
    source_id: &str,
    row: &CsvRow,
    sha: &str,
    status: Option<u16>,
    content_type: &str,
) -> SourceRecordInput {
    let url = row.get("url").cloned().unwrap_or_default();
    let params = url.split_once('?').map(|(_, query)| query.to_owned());
    SourceRecordInput {
        content_type: content_type.to_owned(),
        source_kind: SourceKind::FileImport,
        source_id: source_id.to_owned(),
        request: json!({
            "method": "GET",
            "url": url,
            "params": params,
            "http_status": status,
            "logged_http_status": row.get("http_status"),
            "error": row.get("error").filter(|value| !value.is_empty()),
            "bytes": row.get("bytes"),
            "sha256": sha,
            "outcome": if status == Some(200) && row.get("error").is_none_or(String::is_empty) { "response" } else { "failed_response" },
            "network_path": "unverified_local_capture",
            "terms_conclusion": "issue_172_owner_authorization_for_local_noncommercial_research"
        }),
        observed_at: row.get("observed_at").cloned().unwrap_or_default(),
        ingested_by: "prajna-mvp6-freeze/1".to_owned(),
    }
}

fn historical_capture_issue(report: &mut Value, row: &CsvRow, reason: &'static str) {
    report["historical_capture_issues"]
        .as_array_mut()
        .expect("array")
        .push(json!({
            "source": TDX_SOURCE_ID,
            "session": row.get("session"),
            "attempt": row.get("attempt"),
            "reason": reason,
            "error": row.get("error"),
            "outcome": "unresolved"
        }));
}

fn record_tdx_archive_integrity_mismatch(report: &mut Value, session: NaiveDate) {
    report["invalid_tdx_sessions"]
        .as_array_mut()
        .expect("array")
        .push(json!({
            "session": session.to_string(),
            "reason": "package_archive_integrity_mismatch"
        }));
}

fn validate_zip_archive(bytes: &[u8]) -> AppResult<()> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))?;
    if archive.is_empty() {
        return Err(invalid("TDX archive is empty"));
    }
    for index in 0..archive.len() {
        io::copy(&mut archive.by_index(index)?, &mut io::sink())?;
    }
    Ok(())
}

fn import_failed_tdx_capture(
    store: &RawStore,
    files: &[PathBuf],
    session: NaiveDate,
    row: &CsvRow,
    validation: &BTreeMap<(NaiveDate, String), CsvRow>,
    report: &mut Value,
) -> AppResult<()> {
    let expected_url = format!(
        "https://www.tdx.com.cn/products/data/data/g4day/{}.zip",
        session.format("%Y%m%d")
    );
    if row.get("url") != Some(&expected_url) {
        historical_capture_issue(report, row, "source_or_url_mismatch");
        return Ok(());
    }
    if required(row, "observed_at")
        .and_then(validate_observed_at)
        .is_err()
    {
        historical_capture_issue(report, row, "invalid_or_missing_observed_at");
        return Ok(());
    }
    let status = match row.get("http_status").map(String::as_str) {
        None | Some("") => {
            historical_capture_issue(report, row, "missing_http_status");
            None
        }
        Some(value) => match parse_u16(value) {
            Ok(status) => Some(status),
            Err(_) => {
                historical_capture_issue(report, row, "invalid_http_status");
                None
            }
        },
    };
    let sha = match required(row, "sha256").and_then(normalize_hash) {
        Ok(sha) => sha,
        Err(_) => {
            historical_capture_issue(report, row, "invalid_or_missing_sha256");
            return Ok(());
        }
    };
    let bytes = match required(row, "bytes").and_then(parse_u64) {
        Ok(bytes) => bytes,
        Err(_) => {
            historical_capture_issue(report, row, "invalid_or_missing_byte_length");
            return Ok(());
        }
    };
    let body = if bytes == 0 {
        if sha != format!("sha256:{}", sha256(&[])) {
            historical_capture_issue(report, row, "empty_response_hash_mismatch");
            return Ok(());
        }
        Vec::new()
    } else if let Some(body) = find_logged_body(files, &session.format("%Y%m%d").to_string(), &sha)?
    {
        body
    } else {
        historical_capture_issue(report, row, "missing_response_body");
        return Ok(());
    };
    if body.len() as u64 != bytes {
        historical_capture_issue(report, row, "byte_length_mismatch");
        return Ok(());
    }
    if status == Some(200)
        && validation_passes(validation.get(&(session, sha.clone())))
        && validate_zip_archive(&body).is_err()
    {
        record_tdx_archive_integrity_mismatch(report, session);
    }
    put_once(
        store,
        &body,
        request_record(TDX_SOURCE_ID, row, &sha, status, "application/octet-stream"),
    )?;
    Ok(())
}

fn inventory_record_raw(source_id: &str, row: &CsvRow) -> Value {
    json!({
        "source_id": source_id,
        "capture_record": row,
        "url": row.get("url"),
        "params": row.get("url").and_then(|url| url.split_once('?').map(|(_, query)| query.to_owned())),
        "observed_at": row.get("observed_at"),
        "http_status": row.get("http_status"),
        "error": row.get("error"),
        "byte_len": row.get("bytes").and_then(|value| value.parse::<u64>().ok()),
        "sha256": row.get("sha256"),
        "network_path": "unverified_local_capture",
        "terms_conclusion": "issue_172_owner_authorization_for_local_noncommercial_research"
    })
}

fn read_csv(path: &Path) -> AppResult<Vec<CsvRow>> {
    let mut reader = csv::ReaderBuilder::new().flexible(false).from_path(path)?;
    let headers = reader.headers()?.clone();
    let mut rows = Vec::new();
    for result in reader.records() {
        let record = result?;
        let row = headers
            .iter()
            .zip(record.iter())
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect();
        rows.push(row);
    }
    Ok(rows)
}

fn collect_files(directory: &Path) -> AppResult<Vec<PathBuf>> {
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    let mut stack = vec![directory.to_path_buf()];
    while let Some(path) = stack.pop() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file()
                && path.extension().is_some_and(|extension| {
                    matches!(
                        extension.to_str(),
                        Some("zip" | "body" | "json" | "partial")
                    )
                })
            {
                paths.push(path);
            }
        }
    }
    Ok(paths)
}

fn find_logged_body(
    files: &[PathBuf],
    key: &str,
    expected_sha: &str,
) -> AppResult<Option<Vec<u8>>> {
    let expected_sha = expected_sha.strip_prefix("sha256:").unwrap_or(expected_sha);
    for path in files {
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        if name.contains(key) || name == expected_sha || name == format!("sha256-{expected_sha}") {
            let bytes = fs::read(path)?;
            if sha256(&bytes) == expected_sha {
                return Ok(Some(bytes));
            }
        }
    }
    Ok(None)
}

fn put_once(store: &RawStore, bytes: &[u8], input: SourceRecordInput) -> AppResult<String> {
    let hash = format!("sha256:{}", sha256(bytes));
    for existing in store.sources(&hash)? {
        if existing.source_id == input.source_id
            && existing.content_type == input.content_type
            && existing.source_kind == input.source_kind
            && existing.request == input.request
            && existing.observed_at == input.observed_at
            && existing.ingested_by == input.ingested_by
        {
            return Ok(hash);
        }
    }
    Ok(store.put(bytes, input)?)
}

fn publish_error_kind(error: &PublishError) -> &'static str {
    match error {
        PublishError::Raw(_) => "raw_store_error",
        PublishError::Manifest(_) => "manifest_error",
        PublishError::Validation(_) => "normalization_validation_error",
        PublishError::Io(_) => "publish_io_error",
        PublishError::NormalizerNotFound { .. } => "normalizer_not_found",
        PublishError::ExistingOutputConflict(_) => "existing_output_conflict",
        PublishError::Hash(_) => "manifest_hash_error",
    }
}

fn validation_passes(row: Option<&CsvRow>) -> bool {
    let Some(row) = row else { return false };
    [
        "zip_crc_pass",
        "sz_cod_present",
        "sz_md1_present",
        "structure_pass",
    ]
    .iter()
    .all(|field| {
        row.get(*field)
            .is_some_and(|value| value.eq_ignore_ascii_case("true") || value == "1")
    }) && parse_u64(row.get("cod_records").map(String::as_str).unwrap_or("")).ok()
        == parse_u64(row.get("md1_blocks").map(String::as_str).unwrap_or("")).ok()
        && parse_u64(row.get("scope_codes").map(String::as_str).unwrap_or("")).ok()
            == parse_u64(row.get("scope_records").map(String::as_str).unwrap_or("")).ok()
}

fn parse_calendar(bytes: &[u8], month: &str) -> AppResult<BTreeMap<NaiveDate, bool>> {
    let value: Value = serde_json::from_slice(bytes)?;
    let rows = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("calendar response has no data array"))?;
    let mut days = BTreeMap::new();
    for row in rows {
        let day = row
            .get("jyrq")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("calendar row date is missing"))?;
        let day = date(day)?;
        let flag = row
            .get("jybz")
            .map(value_text)
            .transpose()?
            .ok_or_else(|| invalid("calendar row flag is missing"))?;
        let open = match flag.as_str() {
            "0" => false,
            "1" => true,
            _ => return Err(invalid("calendar row flag is not 0 or 1")),
        };
        if day.format("%Y-%m").to_string() != month || days.insert(day, open).is_some() {
            return Err(invalid(
                "calendar month has a duplicate or out-of-month date",
            ));
        }
    }
    let first = NaiveDate::parse_from_str(&format!("{month}-01"), "%Y-%m-%d")?;
    let next_month = if first.month() == 12 {
        NaiveDate::from_ymd_opt(first.year() + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(first.year(), first.month() + 1, 1)
    }
    .ok_or_else(|| invalid("invalid calendar month"))?;
    let complete = days.len() == (next_month - first).num_days() as usize
        && days.keys().next() == Some(&first)
        && days.keys().next_back().and_then(|last| last.succ_opt()) == Some(next_month);
    if !complete {
        return Err(invalid(
            "calendar response is not one complete natural month",
        ));
    }
    Ok(days)
}

fn value_text(value: &Value) -> AppResult<String> {
    match value {
        Value::String(value) => Ok(value.clone()),
        Value::Number(value) => Ok(value.to_string()),
        _ => Err(invalid("calendar flag has unsupported JSON type")),
    }
}

fn load_expected_open_dates(
    root: &Path,
    start: NaiveDate,
    end: NaiveDate,
) -> AppResult<BTreeSet<NaiveDate>> {
    let path = root.join("acceptance/szse-open-dates.json");
    let dates: Vec<String> = serde_json::from_slice(&fs::read(path)?)?;
    dates
        .into_iter()
        .map(|text| date(&text))
        .filter_map(|date| match date {
            Ok(date) if date >= start && date <= end => Some(Ok(date)),
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .collect()
}

fn months_between(start: NaiveDate, end: NaiveDate) -> Vec<String> {
    let mut year = start.year();
    let mut month = start.month();
    let mut output = Vec::new();
    loop {
        output.push(format!("{year:04}-{month:02}"));
        if year == end.year() && month == end.month() {
            return output;
        }
        if month == 12 {
            year += 1;
            month = 1;
        } else {
            month += 1;
        }
    }
}

fn valid_month(month: &str) -> bool {
    NaiveDate::parse_from_str(&format!("{month}-01"), "%Y-%m-%d").is_ok() && month.len() == 7
}

fn required<'a>(row: &'a CsvRow, key: &str) -> AppResult<&'a str> {
    row.get(key)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid("capture log has a missing required field"))
}

fn normalize_hash(hash: &str) -> AppResult<String> {
    let hex = hash.strip_prefix("sha256:").unwrap_or(hash);
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid("capture log has an invalid SHA-256"));
    }
    Ok(format!("sha256:{}", hex.to_ascii_lowercase()))
}

fn parse_u16(value: &str) -> AppResult<u16> {
    Ok(value.parse()?)
}

fn parse_u64(value: &str) -> AppResult<u64> {
    Ok(value.parse()?)
}

fn validate_observed_at(value: &str) -> AppResult<()> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|_| ())
        .map_err(|_| invalid("capture log observation time is not RFC 3339"))
}

fn date(value: &str) -> AppResult<NaiveDate> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d").map_err(Into::into)
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn write_json(path: &Path, value: &Value) -> AppResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("local report has no parent"))?;
    fs::create_dir_all(parent)?;
    let bytes = serde_json::to_vec_pretty(value)?;
    let mut stage = tempfile::NamedTempFile::new_in(parent)?;
    use std::io::Write;
    stage.write_all(&bytes)?;
    stage.as_file().sync_all()?;
    stage.persist(path)?;
    Ok(())
}

struct OpenOptionsExt;

impl OpenOptionsExt {
    fn lock(path: &Path) -> AppResult<File> {
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)?;
        file.try_lock_exclusive()?;
        Ok(file)
    }
}

fn invalid(message: &'static str) -> Box<dyn Error> {
    Box::new(io::Error::new(io::ErrorKind::InvalidData, message))
}
