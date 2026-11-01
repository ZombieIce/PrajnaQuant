//! Explicit local update plans. Attempts retain partial facts without advancing pool readiness.
use crate::pools;
use chrono::{NaiveDate, Utc};
use fs2::FileExt;
use prajna_data::{
    RawStore, SourceKind, SourceRecordInput, adjusted_series, corporate_facts,
    daily_publish::{self, DailyPublicationReceipt, DailyPublicationSelection},
    daily_raw::{self, DailyRawReport, DailyRawRequest, RawResponseOutcome, SecurityKind, Venue},
    dataset_preview, pool_publish,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub schema_version: u32,
    pub pool: pools::Reference,
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub members: BTreeMap<String, MemberConfig>,
    pub actions_file: Option<PathBuf>,
    pub actions_version: Option<String>,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MemberConfig {
    pub publication: DailyPublicationSelection,
    /// False explicitly consumes a prior local collector report, not a network success.
    pub collect: bool,
    pub adjusted_file: Option<PathBuf>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    config: Config,
    universe: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    config_name: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SupplyMapping {
    config: Option<String>,
    adjusted_files: BTreeMap<String, Option<PathBuf>>,
    actions_file: Option<PathBuf>,
    actions_version: Option<String>,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Supply {
    hash: Option<String>,
    error: Option<String>,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MemberResult {
    pub collection: String,
    pub publication: String,
    pub error: Option<String>,
    pub receipt: Option<DailyPublicationReceipt>,
    pub coverage: Option<Value>,
    pub succeeded_at: Option<String>,
    pub reused_from: Option<String>,
    pub adjusted: Option<String>,
    pub adjustment_error: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Attempt {
    pub id: String,
    pub plan: String,
    pub pool: String,
    pub pool_version: usize,
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub previous: Option<String>,
    pub created_at: String,
    pub finished_at: Option<String>,
    pub status: String,
    pub members: BTreeMap<String, MemberResult>,
    pub dataset: Option<DailyPublicationReceipt>,
    pub dataset_error: Option<String>,
    pub price_ready: bool,
    pub actions: Value,
    pub adjusted_supplies: BTreeMap<String, Supply>,
    pub report_supplies: BTreeMap<String, Supply>,
    pub action_supply: Supply,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supply_mapping: Option<SupplyMapping>,
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    List,
    Prepare {
        config: String,
    },
    Retry {
        attempt: String,
        config: Option<String>,
        #[serde(default)]
        interrupted: bool,
    },
    PoolUpdate {
        attempt: String,
    },
}

fn require(ok: bool, message: &'static str) -> Result<()> {
    if ok { Ok(()) } else { Err(message.into()) }
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn path(root: &Path, relative: &str) -> Result<PathBuf> {
    Ok(daily_raw::checked_output_path(
        root,
        Path::new(&format!("pool-updates/{relative}")),
    )?)
}
fn identifier(value: &str) -> Result<()> {
    require(
        !value.is_empty()
            && value.len() <= 80
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
        "invalid update identifier",
    )
}
fn read<T: serde::de::DeserializeOwned>(root: &Path, relative: &str) -> Result<T> {
    Ok(serde_json::from_slice(&fs::read(path(root, relative)?)?)?)
}
fn write<T: Serialize>(root: &Path, relative: &str, value: &T, immutable: bool) -> Result<()> {
    let path = path(root, relative)?;
    let bytes = serde_json::to_vec(value)?;
    if immutable && path.exists() {
        require(fs::read(path)? == bytes, "immutable update record conflict")?;
        return Ok(());
    }
    let directory = path.parent().ok_or("missing update directory")?;
    fs::create_dir_all(directory)?;
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    use std::io::Write;
    file.write_all(&bytes)?;
    file.as_file().sync_all()?;
    if immutable {
        file.persist_noclobber(path)?;
    } else {
        file.persist(path)?;
    }
    Ok(())
}
fn put(root: &Path, bytes: &[u8], source: &str) -> Result<String> {
    let digest = hash(bytes);
    for relative in [
        format!("raw/sha256/{}/{digest}", &digest[..2]),
        format!("raw-sources/sha256/{}/{digest}.jsonl", &digest[..2]),
    ] {
        daily_raw::checked_output_path(root, Path::new(&relative))?;
    }
    Ok(RawStore::open(root)?.put(
        bytes,
        SourceRecordInput {
            content_type: "application/json".into(),
            source_kind: SourceKind::FileImport,
            source_id: source.into(),
            request: json!({}),
            observed_at: Utc::now().to_rfc3339(),
            ingested_by: "prajna-pool-update/1".into(),
        },
    )?)
}
fn snapshot(root: &Path, file: Option<&Path>) -> Supply {
    let result = file
        .ok_or_else(|| std::io::Error::other("input file not configured"))
        .and_then(fs::read)
        .map_err(|e| e.to_string())
        .and_then(|bytes| put(root, &bytes, "pool-update-supply/1").map_err(|e| e.to_string()));
    match result {
        Ok(hash) => Supply {
            hash: Some(hash),
            error: None,
        },
        Err(error) => Supply {
            hash: None,
            error: Some(error),
        },
    }
}
fn supplied(root: &Path, supply: &Supply) -> Result<Vec<u8>> {
    let hash = supply.hash.as_ref().ok_or_else(|| {
        supply
            .error
            .clone()
            .unwrap_or_else(|| "missing frozen supply".into())
    })?;
    Ok(RawStore::open(root)?.get(hash)?)
}
fn load_plan(root: &Path, id: &str) -> Result<Plan> {
    identifier(id)?;
    let bytes = fs::read(path(root, &format!("plans/{id}.json"))?)?;
    require(hash(&bytes) == id, "update plan hash mismatch")?;
    Ok(serde_json::from_slice(&bytes)?)
}
pub fn get(root: &Path, id: &str) -> Result<Attempt> {
    identifier(id)?;
    let attempt: Attempt = read(root, &format!("attempts/{id}.json"))?;
    require(attempt.id == id, "update attempt identity mismatch")?;
    let frozen: Attempt = read(root, &format!("attempt-inputs/{id}.json"))?;
    let plan = load_plan(root, &attempt.plan)?;
    require(
        frozen.id == attempt.id
            && frozen.plan == attempt.plan
            && frozen.previous == attempt.previous
            && frozen.created_at == attempt.created_at
            && frozen.adjusted_supplies == attempt.adjusted_supplies
            && frozen.report_supplies == attempt.report_supplies
            && frozen.action_supply == attempt.action_supply
            && frozen.supply_mapping == attempt.supply_mapping
            && attempt.pool == plan.config.pool.id
            && attempt.pool_version == plan.config.pool.version
            && attempt.start == plan.config.start
            && attempt.end == plan.config.end
            && attempt.members.keys().eq(plan.config.members.keys()),
        "update attempt differs from frozen plan/inputs",
    )?;
    require(
        !attempt.price_ready
            || (attempt.status == "price_ready"
                && attempt.finished_at.is_some()
                && attempt.dataset.is_some()
                && attempt.members.values().all(|m| {
                    m.publication == "succeeded" && m.receipt.is_some() && m.adjusted.is_some()
                })),
        "invalid price-ready result",
    )?;
    Ok(attempt)
}
fn persist(root: &Path, attempt: &Attempt) -> Result<()> {
    write(
        root,
        &format!("attempts/{}.json", attempt.id),
        attempt,
        false,
    )
}
fn fixed_scope(config: &Config) -> Result<Value> {
    let mut value = serde_json::to_value(config)?;
    value
        .as_object_mut()
        .ok_or("invalid config")?
        .remove("actions_file");
    value
        .as_object_mut()
        .ok_or("invalid config")?
        .remove("actions_version");
    for member in value["members"]
        .as_object_mut()
        .ok_or("invalid members")?
        .values_mut()
    {
        member
            .as_object_mut()
            .ok_or("invalid member")?
            .remove("adjusted_file");
    }
    Ok(value)
}
fn make_attempt(
    root: &Path,
    plan_id: String,
    previous: Option<Attempt>,
    config_name: Option<&str>,
) -> Result<Attempt> {
    let plan = load_plan(root, &plan_id)?;
    let config_name = config_name
        .or_else(|| {
            previous
                .as_ref()
                .and_then(|a| a.supply_mapping.as_ref())
                .and_then(|m| m.config.as_deref())
        })
        .or(plan.config_name.as_deref());
    let replacement: Option<Config> = if previous.is_some() {
        config_name
            .map(|name| {
                identifier(name)?;
                read(root, &format!("configs/{name}.json"))
            })
            .transpose()?
    } else {
        None
    };
    let supply_config = replacement.as_ref().unwrap_or(&plan.config);
    require(
        fixed_scope(supply_config)? == fixed_scope(&plan.config)?,
        "retry configuration changes fixed plan scope",
    )?;
    require(
        supply_config.actions_file.is_none() || supply_config.actions_version.is_none(),
        "choose action file or fixed version, not both",
    )?;
    let mapping = SupplyMapping {
        config: config_name.map(str::to_owned),
        adjusted_files: supply_config
            .members
            .iter()
            .map(|(id, m)| (id.clone(), m.adjusted_file.clone()))
            .collect(),
        actions_file: supply_config.actions_file.clone(),
        actions_version: supply_config.actions_version.clone(),
    };
    let mut attempt = Attempt {
        id: format!(
            "{}-{}",
            Utc::now()
                .timestamp_nanos_opt()
                .ok_or("clock out of range")?,
            std::process::id()
        ),
        plan: plan_id,
        pool: plan.config.pool.id.clone(),
        pool_version: plan.config.pool.version,
        start: plan.config.start,
        end: plan.config.end,
        previous: previous.as_ref().map(|a| a.id.clone()),
        created_at: Utc::now().to_rfc3339(),
        finished_at: None,
        status: "queued".into(),
        members: BTreeMap::new(),
        dataset: None,
        dataset_error: None,
        price_ready: false,
        actions: json!({"state":"unknown","reason":"not attempted"}),
        adjusted_supplies: BTreeMap::new(),
        report_supplies: BTreeMap::new(),
        action_supply: snapshot(root, mapping.actions_file.as_deref()),
        supply_mapping: Some(mapping.clone()),
    };
    for (id, member) in &plan.config.members {
        let mut result = MemberResult {
            collection: "pending".into(),
            publication: "pending".into(),
            ..Default::default()
        };
        if let Some(old) = previous.as_ref().and_then(|p| p.members.get(id)) {
            if old.publication == "succeeded" {
                let receipt = old
                    .receipt
                    .as_ref()
                    .ok_or("successful member missing receipt")?;
                daily_publish::verify_selection_scope(
                    root,
                    receipt,
                    &member.publication,
                    id,
                    plan.config.start,
                    plan.config.end,
                )?;
                result = old.clone();
                result.reused_from = attempt.previous.clone();
                result.adjusted = None;
                result.adjustment_error = None;
            }
        }
        attempt.members.insert(id.clone(), result);
        attempt.adjusted_supplies.insert(
            id.clone(),
            snapshot(root, mapping.adjusted_files[id].as_deref()),
        );
        if !member.collect {
            attempt.report_supplies.insert(
                id.clone(),
                snapshot(root, Some(Path::new(&member.publication.report))),
            );
        }
    }
    write(
        root,
        &format!("attempt-inputs/{}.json", attempt.id),
        &attempt,
        true,
    )?;
    persist(root, &attempt)?;
    Ok(attempt)
}

pub fn prepare(root: &Path, config_name: &str) -> Result<Attempt> {
    identifier(config_name)?;
    let config: Config = read(root, &format!("configs/{config_name}.json"))?;
    require(
        config.schema_version == 1 && config.start <= config.end,
        "invalid update schema/date range",
    )?;
    require(
        config.actions_file.is_none() || config.actions_version.is_none(),
        "choose action file or fixed version, not both",
    )?;
    let pool = pools::resolve(root, &config.pool).map_err(|e| e.message)?;
    require(
        config.members.keys().cloned().collect::<Vec<_>>() == pool.members,
        "update inputs must match every fixed pool member",
    )?;
    for member in config.members.values() {
        let p = &member.publication;
        prajna_data::session_calendar::CalendarStore::open(&p.calendar_root)?.get(
            &prajna_data::session_calendar::CalendarVersionId::parse(&p.calendar_version)?,
        )?;
        prajna_data::SecurityEvidenceStore::open(&p.evidence_root)?.get_snapshot(
            &prajna_data::SecuritySnapshotId::parse(&p.security_snapshot)?,
        )?;
    }
    let plan = Plan {
        config,
        universe: pool.universe,
        config_name: Some(config_name.into()),
    };
    let id = hash(&serde_json::to_vec(&plan)?);
    write(root, &format!("plans/{id}.json"), &plan, true)?;
    make_attempt(root, id, None, None)
}
pub fn retry(root: &Path, id: &str, interrupted: bool, config: Option<&str>) -> Result<Attempt> {
    let mut previous = get(root, id)?;
    let lock = attempt_lock(root, id)?;
    lock.try_lock_exclusive()?;
    if interrupted && previous.finished_at.is_none() {
        previous.status = "interrupted".into();
        previous.finished_at = Some(Utc::now().to_rfc3339());
        persist(root, &previous)?;
    }
    require(previous.status != "queued", "cannot retry a queued attempt")?;
    make_attempt(root, previous.plan.clone(), Some(previous), config)
}
fn attempt_lock(root: &Path, id: &str) -> Result<fs::File> {
    identifier(id)?;
    let p = path(root, &format!("locks/{id}.lock"))?;
    fs::create_dir_all(p.parent().ok_or("invalid lock path")?)?;
    Ok(fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(p)?)
}

fn publish_member<F>(
    root: &Path,
    plan: &Plan,
    attempt: &mut Attempt,
    id: &str,
    collector: &mut F,
    source_stopped: &mut bool,
) -> Result<(DailyPublicationReceipt, Value, String, bool)>
where
    F: FnMut(
        &Path,
        DailyRawRequest,
    ) -> std::result::Result<daily_raw::CollectionRun, daily_raw::DailyRawError>,
{
    let config = &plan.config.members[id];
    let mut selection = config.publication.clone();
    let (report, report_path, collection) = if config.collect {
        require(
            !*source_stopped,
            "source stopped earlier in this update; no further network requests",
        )?;
        let instrument: prajna_domain::InstrumentId = id.parse()?;
        let run = collector(
            Path::new(&selection.raw_root),
            DailyRawRequest {
                symbol: instrument.symbol().into(),
                venue: match instrument.venue().as_str() {
                    "XSHG" => Venue::Xshg,
                    "XSHE" => Venue::Xshe,
                    _ => return Err("unsupported venue".into()),
                },
                kind: SecurityKind::Etf,
                start: plan.config.start,
                end: plan.config.end,
            },
        )?;
        *source_stopped = run.report.status == daily_raw::CollectionStatus::SourceStopped;
        (run.report, run.report_path, "collected")
    } else {
        let bytes = supplied(root, &attempt.report_supplies[id])?;
        let report: DailyRawReport = serde_json::from_slice(&bytes)?;
        let relative = format!("reports/{}-{id}.json", attempt.id);
        write(root, &relative, &report, true)?;
        (report, path(root, &relative)?, "explicit_report")
    };
    report.validate_consistency()?;
    let collection = match report.status {
        daily_raw::CollectionStatus::Complete => collection,
        daily_raw::CollectionStatus::SourceStopped => "source_stopped",
        _ => "partial_or_failed",
    };
    attempt
        .members
        .get_mut(id)
        .ok_or("missing member state")?
        .collection = collection.into();
    persist(root, attempt)?;
    require(
        report.instrument_id == id
            && report.requested_range.start == plan.config.start
            && report.requested_range.end == plan.config.end
            && report.security_kind == SecurityKind::Etf,
        "collection report does not match fixed update plan",
    )?;
    selection.report = report_path.to_string_lossy().into();
    selection.raw_sha256s = report
        .chunks
        .iter()
        .filter(|c| c.status == daily_raw::ChunkStatus::Complete)
        .flat_map(|c| {
            c.raw_objects
                .iter()
                .filter(|r| r.outcome == RawResponseOutcome::AcceptedResponse)
                .map(|r| r.raw_sha256.clone())
        })
        .collect();
    selection.raw_sha256s.sort();
    selection.raw_sha256s.dedup();
    require(
        !selection.raw_sha256s.is_empty(),
        "collection has no accepted nonempty response; previous success retained",
    )?;
    attempt
        .members
        .get_mut(id)
        .ok_or("missing member state")?
        .publication = "publishing".into();
    persist(root, attempt)?;
    let receipt_path = format!("pool-updates/receipts/{}-{id}.json", attempt.id);
    let manifest = daily_publish::publish_daily(root, selection, Path::new(&receipt_path))?;
    let receipt = serde_json::from_slice(&fs::read(root.join(receipt_path))?)?;
    let coverage =
        dataset_preview::show(root, &manifest.dsv)?["instruments"][0]["coverage"].clone();
    Ok((
        receipt,
        coverage,
        collection.into(),
        report.status == daily_raw::CollectionStatus::Complete,
    ))
}

/// A supplier payload without a project DSV is a new import, not an old object to rebind.
fn import_adjustment(
    root: &Path,
    supply: &Supply,
    dsv: &str,
    id: &str,
    start: NaiveDate,
    end: NaiveDate,
) -> Result<String> {
    let mut bytes = supplied(root, supply)?;
    let mut value: Value = serde_json::from_slice(&bytes)?;
    if value["schema_version"] == "supplier-adjusted/1" {
        let object = value
            .as_object_mut()
            .ok_or("supplier input must be an object")?;
        require(
            !object.contains_key("raw_dsv"),
            "unbound supplier schema cannot contain raw_dsv",
        )?;
        require(
            object.remove("price_unit") == Some(json!("CNY_per_fund_unit")),
            "supplier-adjusted/1 requires explicit CNY_per_fund_unit prices",
        )?;
        object.insert("schema_version".into(), json!(1));
        object.insert("raw_dsv".into(), json!(dsv));
        bytes = serde_json::to_vec(&value)?;
    }
    let input: adjusted_series::AdjustedInput = serde_json::from_value(value.clone())?;
    require(
        input.raw_dsv == dsv && input.instrument_id == id,
        "adjusted input DSV/instrument mismatch",
    )?;
    let version = adjusted_series::import(root, &bytes)?;
    let input = adjusted_series::read(root, &version)?;
    let manifest = prajna_data::read_manifest(root, dsv)?;
    let instrument: prajna_domain::InstrumentId = id.parse()?;
    let dates = input
        .rows
        .iter()
        .map(|r| r.trade_date)
        .collect::<std::collections::BTreeSet<_>>();
    let mut expected = Vec::new();
    for batch in prajna_data::read_dataset_table(root, &manifest, "sessions")? {
        expected.extend(
            prajna_data::sessions_from_record_batch(&batch)?
                .into_iter()
                .filter(|s| {
                    s.venue_id() == instrument.venue() && (start..=end).contains(&s.session_date())
                })
                .map(|s| s.session_date()),
        );
    }
    require(
        input.start <= start
            && input.end >= end
            && !expected.is_empty()
            && expected.iter().all(|d| dates.contains(d)),
        "adjusted input actual coverage is insufficient for the requested interval",
    )?;
    let record = json!({"schema_version":"supplier-adapter/1","source_raw":supply.hash,"dsv":dsv,"instrument":id,"version":version,"price_unit":"CNY_per_fund_unit"});
    let name = hash(&serde_json::to_vec(&record)?);
    write(root, &format!("imports/{name}.json"), &record, true)?;
    Ok(version)
}
fn action_coverage(root: &Path, plan: &Plan, attempt: &Attempt) -> Result<Value> {
    let fixed_version = attempt
        .supply_mapping
        .as_ref()
        .map(|m| &m.actions_version)
        .unwrap_or(&plan.config.actions_version);
    let version = if let Some(version) = fixed_version {
        corporate_facts::read(root, version)?;
        version.clone()
    } else {
        corporate_facts::import(root, &supplied(root, &attempt.action_supply)?)?
    };
    let mut scopes = BTreeMap::new();
    let mut complete = true;
    for id in plan.config.members.keys() {
        let scope = corporate_facts::query(root, &version, id, plan.config.start, plan.config.end)?;
        complete &= matches!(scope["state"].as_str(), Some("covered" | "no_events"));
        scopes.insert(id.clone(), scope);
    }
    let venues = plan
        .config
        .members
        .keys()
        .map(|id| id.rsplit('.').next())
        .collect::<std::collections::BTreeSet<_>>();
    let total_return = if !complete {
        "incomplete_facts"
    } else if venues.len() > 1 {
        "unsupported_mixed_venue"
    } else {
        "requires_strategy_preflight"
    };
    Ok(
        json!({"version":version,"complete":complete,"members":scopes,"total_return":total_return,
        "notice":"Fact coverage is not engine compatibility; mixed-Venue action binding restrictions remain."}),
    )
}
pub fn execute(root: &Path, id: &str) -> Result<Attempt> {
    execute_with_collection(root, id, |root, request| {
        std::thread::sleep(daily_raw::MIN_REQUEST_INTERVAL);
        daily_raw::collect_daily(root, request)
    })
}

pub fn execute_with_collection<F>(root: &Path, id: &str, mut collector: F) -> Result<Attempt>
where
    F: FnMut(
        &Path,
        DailyRawRequest,
    ) -> std::result::Result<daily_raw::CollectionRun, daily_raw::DailyRawError>,
{
    let lock = attempt_lock(root, id)?;
    lock.try_lock_exclusive()?;
    let mut attempt = get(root, id)?;
    if attempt.finished_at.is_some() {
        return Ok(attempt);
    }
    require(
        attempt.status == "queued",
        "interrupted attempt requires an explicit retry",
    )?;
    let frozen: Attempt = read(root, &format!("attempt-inputs/{id}.json"))?;
    require(
        serde_json::to_vec(&attempt)? == serde_json::to_vec(&frozen)?,
        "queued attempt input changed",
    )?;
    let plan = load_plan(root, &attempt.plan)?;
    for (id, member) in &attempt.members {
        if member.publication == "succeeded" {
            daily_publish::verify_selection_scope(
                root,
                member
                    .receipt
                    .as_ref()
                    .ok_or("reused member missing receipt")?,
                &plan
                    .config
                    .members
                    .get(id)
                    .ok_or("reused member absent from plan")?
                    .publication,
                id,
                plan.config.start,
                plan.config.end,
            )?;
        }
    }
    attempt.status = "running".into();
    persist(root, &attempt)?;
    let mut source_stopped = false;
    for id in plan.config.members.keys() {
        if attempt.members[id].publication == "succeeded" {
            continue;
        }
        attempt
            .members
            .get_mut(id)
            .ok_or("missing member state")?
            .collection = if plan.config.members[id].collect {
            "collecting"
        } else {
            "reading_report"
        }
        .into();
        persist(root, &attempt)?;
        let result = publish_member(
            root,
            &plan,
            &mut attempt,
            id,
            &mut collector,
            &mut source_stopped,
        );
        let target = attempt.members.get_mut(id).ok_or("missing member state")?;
        match result {
            Ok((receipt, coverage, collection, complete)) => {
                target.collection = collection;
                target.publication = if complete { "succeeded" } else { "partial" }.into();
                target.receipt = Some(receipt);
                target.coverage = Some(coverage);
                if complete {
                    target.succeeded_at = Some(Utc::now().to_rfc3339());
                } else {
                    target.error = Some(
                        "Collection incomplete; partial publication retained, pool not ready"
                            .into(),
                    );
                }
            }
            Err(error) => {
                if matches!(target.collection.as_str(), "collecting" | "reading_report") {
                    target.collection = "failed_or_incomplete".into();
                }
                target.publication = "not_ready".into();
                target.error = Some(error.to_string());
            }
        }
        persist(root, &attempt)?;
    }
    if attempt
        .members
        .values()
        .all(|m| m.publication == "succeeded")
    {
        let selection = pool_publish::PoolSelection {
            schema_version: 1,
            members: attempt
                .members
                .iter()
                .map(|(id, m)| {
                    Ok((
                        id.clone(),
                        m.receipt.clone().ok_or("missing member receipt")?,
                    ))
                })
                .collect::<Result<_>>()?,
        };
        match pool_publish::publish(
            root,
            &selection,
            Path::new(&format!("pool-updates/receipts/{}.json", attempt.id)),
        ) {
            Ok(dataset) => attempt.dataset = Some(dataset),
            Err(error) => attempt.dataset_error = Some(error.to_string()),
        }
    } else {
        attempt.dataset_error = Some(
            "Fixed pool incomplete; no member removed and no previous Dataset substituted".into(),
        );
    }
    persist(root, &attempt)?;
    if let Some(dataset) = &attempt.dataset {
        for id in plan.config.members.keys() {
            let result = import_adjustment(
                root,
                &attempt.adjusted_supplies[id],
                &dataset.dsv,
                id,
                plan.config.start,
                plan.config.end,
            );
            let member = attempt.members.get_mut(id).ok_or("missing member state")?;
            match result {
                Ok(version) => member.adjusted = Some(version),
                Err(error) => member.adjustment_error = Some(error.to_string()),
            }
        }
    }
    attempt.price_ready =
        attempt.dataset.is_some() && attempt.members.values().all(|m| m.adjusted.is_some());
    attempt.actions = match action_coverage(root, &plan, &attempt) {
        Ok(value) => value,
        Err(error) => {
            json!({"state":"unknown","complete":false,"total_return":"incomplete_facts","reason":error.to_string()})
        }
    };
    attempt.status = if attempt.price_ready {
        "price_ready"
    } else {
        "needs_input"
    }
    .into();
    attempt.finished_at = Some(Utc::now().to_rfc3339());
    persist(root, &attempt)?;
    Ok(attempt)
}
pub fn list(root: &Path) -> Result<Value> {
    let mut attempts = Vec::<Attempt>::new();
    let dir = path(root, "attempts")?;
    if dir.exists() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            if entry.path().extension().is_some_and(|v| v == "json") {
                let id = entry
                    .path()
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .ok_or("invalid attempt filename")?
                    .to_owned();
                attempts.push(get(root, &id)?);
            }
        }
    }
    attempts.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let mut configs = Vec::new();
    let dir = path(root, "configs")?;
    if dir.exists() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            if entry.path().extension().is_some_and(|v| v == "json") {
                let name = entry
                    .path()
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .ok_or("invalid config filename")?
                    .to_owned();
                identifier(&name)?;
                let config: Config = read(root, &format!("configs/{name}.json"))?;
                configs.push(
                    json!({"name":name,"pool":config.pool,"start":config.start,"end":config.end}),
                );
            }
        }
    }
    configs.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    let mut last_price_ready = BTreeMap::new();
    let mut last_member_success = BTreeMap::new();
    let mut price_success_times = BTreeMap::new();
    let mut member_success_times = BTreeMap::new();
    for attempt in &attempts {
        let scope = format!("{}:{}", attempt.pool, attempt.pool_version);
        if attempt.price_ready {
            let time = chrono::DateTime::parse_from_rfc3339(
                attempt
                    .finished_at
                    .as_deref()
                    .ok_or("ready attempt missing completion time")?,
            )?;
            if price_success_times
                .get(&scope)
                .is_none_or(|old| &time > old)
            {
                price_success_times.insert(scope.clone(), time);
                last_price_ready.insert(scope.clone(), json!({"attempt":attempt.id,"finished_at":attempt.finished_at,"dataset":attempt.dataset}));
            }
        }
        for (id, member) in &attempt.members {
            if member.publication == "succeeded" {
                let key = format!("{scope}:{id}");
                let time = chrono::DateTime::parse_from_rfc3339(
                    member
                        .succeeded_at
                        .as_deref()
                        .ok_or("successful member missing success time")?,
                )?;
                if member_success_times
                    .get(&key)
                    .is_none_or(|old| &time > old || (&time == old && member.reused_from.is_none()))
                {
                    member_success_times.insert(key.clone(), time);
                    last_member_success.insert(key, json!({"attempt":attempt.id,"succeeded_at":member.succeeded_at,"coverage":member.coverage}));
                }
            }
        }
    }
    Ok(
        json!({"configs":configs,"attempts":attempts,"last_price_ready":last_price_ready,"last_member_success":last_member_success}),
    )
}
pub fn command(root: &Path, command: Command) -> Result<Value> {
    let root = dataset_preview::open_dataset_root(root)?;
    let data = match command {
        Command::List => list(&root)?,
        Command::Prepare { config } => json!({"attempt":prepare(&root,&config)?}),
        Command::Retry {
            attempt,
            interrupted,
            config,
        } => json!({"attempt":retry(&root,&attempt,interrupted,config.as_deref())?}),
        Command::PoolUpdate { attempt } => json!({"attempt":execute(&root,&attempt)?}),
    };
    Ok(json!({"schema_version":1,"data":data}))
}
