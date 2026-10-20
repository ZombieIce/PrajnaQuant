//! Offline publisher for the MVP-6 corporate-action Dataset (#223).
//!
//! The command reads only frozen local inputs: the published
//! `szse-etf-frozen@4` market manifest and its Raw objects, the #229
//! `facts-session.json`, and optional Sina qfq/hfq Raw objects. It performs no
//! network access and writes the P21 result only to the local acceptance
//! report; raw prices, hashes and findings never leave the machine.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde_json::{Value, json};

use crate::{
    Manifest, NormalizerRegistry, RawStore, SourceKind, SourceRecordInput,
    lock_mvp6_coverage_report, normalizer::FactsSessionDocument, publish_dataset, read_manifest,
};

pub const CORPORATE_ACTION_NORMALIZER_VERSION: &str = "7";
pub const MARKET_NORMALIZER_VERSION: &str = "4";
const FACTS_CONTRACT: &str = "mvp-6-cninfo-facts-v1";

type AppResult<T> = Result<T, Box<dyn std::error::Error>>;

/// Result of one offline corporate-action publish and local report merge.
#[derive(Debug, Clone)]
pub struct CorporateActionPublish {
    pub manifest: Manifest,
    /// Updated local acceptance report. Contains real-data findings; never
    /// commit it to the public repository.
    pub report: Value,
}

/// Publishes `szse-etf-frozen@7` from the exact v4 market Raw inputs plus the
/// validated corporate-action facts and any locally frozen Sina pairs.
pub fn publish_corporate_actions(
    root: &Path,
    market_dsv: &str,
    facts_session: &Path,
    created_at: &str,
    observed_at: &str,
) -> AppResult<CorporateActionPublish> {
    let store = RawStore::open(root)?;
    let market = read_manifest(root, market_dsv)?;
    if market.core.normalizer.id != "szse-etf-frozen"
        || market.core.normalizer.version != MARKET_NORMALIZER_VERSION
    {
        return Err(invalid(format!(
            "corporate actions require szse-etf-frozen@{MARKET_NORMALIZER_VERSION}"
        )));
    }
    let market_inputs = market
        .core
        .inputs
        .iter()
        .map(|input| input.raw_sha256.clone())
        .collect::<Vec<_>>();

    let facts_bytes = fs::read(facts_session)?;
    let facts = FactsSessionDocument::parse(&facts_bytes, market_dsv)
        .map_err(|(_code, message)| invalid(message))?;
    let lifecycle_codes = facts
        .session()
        .catalog_lifecycles
        .iter()
        .map(|lifecycle| lifecycle.code.clone())
        .collect::<Vec<_>>();
    let facts_hash = store.put(
        &facts_bytes,
        SourceRecordInput {
            content_type: "application/json".into(),
            source_kind: SourceKind::FileImport,
            source_id: "cninfo_corporate_action_facts".into(),
            request: json!({
                "contract": FACTS_CONTRACT,
                "market_dsv": market_dsv,
                "file_name": facts_session
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("facts-session.json"),
            }),
            observed_at: observed_at.into(),
            ingested_by: "prajna-mvp6-publish-actions/1".into(),
        },
    )?;

    let (mut sina_hashes, sina_selection) = select_sina_inputs(root, &store, &lifecycle_codes)?;
    let mut inputs = market_inputs;
    inputs.push(facts_hash);
    inputs.append(&mut sina_hashes);
    inputs.sort();
    inputs.dedup();

    let registry = NormalizerRegistry::with_corporate_actions_v7(market_dsv).map_err(invalid)?;
    let manifest = publish_dataset(
        root,
        &store,
        &registry,
        "szse-etf-frozen",
        CORPORATE_ACTION_NORMALIZER_VERSION,
        &inputs,
        created_at,
    )?;
    let report = merge_coverage_report(root, market_dsv, &manifest, sina_selection)?;
    Ok(CorporateActionPublish { manifest, report })
}

/// Reads the local Sina collection report and keeps completed qfq/hfq pairs
/// for codes that also have a validated Catalog lifecycle. Missing or
/// unreadable pairs are reported as source gaps, never as `no event`.
fn select_sina_inputs(
    root: &Path,
    store: &RawStore,
    lifecycle_codes: &[String],
) -> AppResult<(Vec<String>, Value)> {
    let report_path = root.join("acceptance/sina/collection.json");
    let bytes = match fs::read(&report_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok((
                Vec::new(),
                json!({
                    "status": "not_present",
                    "reason": "acceptance/sina/collection.json does not exist",
                    "hashes": 0,
                    "gaps": [],
                }),
            ));
        }
        Err(error) => return Err(error.into()),
    };
    let report: Value = match serde_json::from_slice(&bytes) {
        Ok(report) => report,
        Err(_) => {
            return Ok((
                Vec::new(),
                json!({
                    "status": "source_gap",
                    "reason": "invalid acceptance/sina/collection.json",
                    "hashes": 0,
                    "gaps": [{"reason": "invalid_sina_collection_report"}],
                }),
            ));
        }
    };
    let Some(outcomes) = report["candidate_results"].as_array() else {
        return Ok((
            Vec::new(),
            json!({
                "status": "source_gap",
                "reason": "candidate_results is missing from the Sina report",
                "hashes": 0,
                "gaps": [{"reason": "sina_candidate_results_missing"}],
            }),
        ));
    };
    let codes = lifecycle_codes
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let mut hashes = Vec::new();
    let mut gaps = Vec::new();
    for outcome in outcomes {
        let Some(symbol) = outcome["symbol"].as_str() else {
            gaps.push(json!({"reason": "sina outcome has no symbol"}));
            continue;
        };
        let code = symbol.strip_prefix("sz").unwrap_or(symbol);
        if !codes.contains(code) {
            continue;
        }
        if outcome["state"] != "completed" {
            gaps.push(json!({
                "native_symbol": code,
                "reason": outcome["state"],
                "detail": outcome["reason"],
            }));
            continue;
        }
        for field in ["qfq_raw_sha256", "hfq_raw_sha256"] {
            match outcome[field].as_str() {
                Some(hash) if store.get(hash).is_ok() => hashes.push(hash.to_owned()),
                Some(_) => gaps.push(json!({
                    "native_symbol": code,
                    "reason": "sina_raw_missing_from_store",
                    "field": field,
                })),
                None => gaps.push(json!({
                    "native_symbol": code,
                    "reason": "sina_pair_field_missing",
                    "field": field,
                })),
            }
        }
    }
    hashes.sort();
    hashes.dedup();
    let hash_count = hashes.len();
    let status = if !gaps.is_empty() {
        "source_gap"
    } else if hash_count == 0 {
        "not_selected"
    } else {
        "selected"
    };
    Ok((
        hashes,
        json!({
            "status": status,
            "hashes": hash_count,
            "gaps": gaps,
        }),
    ))
}

fn merge_coverage_report(
    root: &Path,
    market_dsv: &str,
    manifest: &Manifest,
    sina_selection: Value,
) -> AppResult<Value> {
    let _lock = lock_mvp6_coverage_report(root)?;
    let path = root.join("acceptance/mvp6-coverage.json");
    let mut report: Value = serde_json::from_slice(&fs::read(&path)?)?;
    if report["dataset_published"] != true || report["dsv"] != market_dsv {
        return Err(invalid(
            "local coverage report does not match the requested v4 market DSV",
        ));
    }
    let coverage = &manifest.provenance.coverage;
    let p21 = coverage["p21"].clone();
    let p21_status = p21["status"].as_str().unwrap_or("source_gap").to_owned();
    let mismatch_count = coverage["p21_mismatches"]
        .as_array()
        .map(Vec::len)
        .unwrap_or(0);
    report["corporate_action_dataset"] = json!({
        "dsv": manifest.dsv,
        "normalizer": format!(
            "{}@{}",
            manifest.core.normalizer.id, manifest.core.normalizer.version
        ),
        "normalizer_config_sha256": manifest.core.normalizer.config_sha256,
        "market_dsv": market_dsv,
        "input_count": manifest.core.inputs.len(),
        "input_hashes": manifest
            .core
            .inputs
            .iter()
            .map(|input| input.raw_sha256.as_str())
            .collect::<Vec<_>>(),
        "p21_status": p21_status,
        "p21_mismatch_count": mismatch_count,
    });
    report["source_status"]["p21_crosscheck"] = json!(p21_status);
    report["source_status"]["p21_evaluated"] = json!(true);
    report["source_status"]["corporate_actions"] = json!(if p21_status == "evaluated" {
        "published_no_unknown"
    } else {
        "published_with_unknown"
    });
    report["p21"] = p21;
    report["p21_mismatches"] = coverage["p21_mismatches"].clone();
    report["corporate_actions_with_missing_fields"] =
        coverage["corporate_actions_with_missing_fields"].clone();
    report["excluded_etfs"] = coverage["excluded_etfs"].clone();
    let mut sina_crosscheck = coverage["sina_adjust_factor"].clone();
    if sina_crosscheck["status"] == "not_present" && sina_selection["status"] == "source_gap" {
        // The Dataset only sees Raw inputs, so an attempted collection whose
        // outcomes are all gaps would otherwise look absent. The local
        // acceptance report keeps that attempt as a source gap.
        sina_crosscheck["status"] = json!("source_gap");
        sina_crosscheck["selection_gaps"] = sina_selection["gaps"].clone();
    }
    report["sina_optional_crosscheck"] = sina_crosscheck;
    report["sina_input_selection"] = sina_selection;
    // This ticket evaluates P21 but does not complete the MVP-6 acceptance.
    report["mvp6_complete"] = json!(false);
    fs::write(&path, serde_json::to_vec_pretty(&report)?)?;
    Ok(report)
}

fn invalid(message: impl Into<String>) -> Box<dyn std::error::Error> {
    Box::new(io::Error::other(message.into()))
}

/// Default local root used by the command. Kept here so the binary and tests
/// agree on the only supported real-data location.
pub fn default_mvp6_root() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join("PrajnaData/mvp-6")
}
