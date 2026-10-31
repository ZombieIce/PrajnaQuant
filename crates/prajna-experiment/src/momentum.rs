//! Versioned momentum research: Rust owns values, missing states and chart summaries.
use polars::prelude::*;
use prajna_data::{adjusted_series, dataset_preview};
use prajna_domain::VenueId;
use prajna_research::{
    StaticUniverse,
    cache::store::FactorCache,
    factor::{
        AvailabilityAssumption, Factor, FactorError, FactorKind, MomentumParams,
        TrendFilterStrictParams, VolatilityParams,
    },
    load_panel_for_venues,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

#[derive(Debug, Serialize)]
pub struct ResearchError {
    pub code: &'static str,
    pub message: String,
}
fn invalid(e: impl ToString) -> ResearchError {
    ResearchError {
        code: "invalid_momentum",
        message: e.to_string(),
    }
}
fn storage(e: impl ToString) -> ResearchError {
    ResearchError {
        code: "momentum_store_failed",
        message: e.to_string(),
    }
}
/// Additional research factors added by #299. Omitting `factor` keeps the
/// legacy Momentum request/report identity byte-for-byte unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FactorSelection {
    Volatility,
    TrendFilterStrict,
}
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationSelection {
    NextOpenToForwardOpenV1,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluation: Option<EvaluationSelection>,
    pub dsv: String,
    pub instrument: Option<String>,
    pub pool: Option<crate::pools::Reference>,
    pub adjusted: BTreeMap<String, String>,
    pub windows: Vec<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub factor: Option<FactorSelection>,
}
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Submit { request: Request },
    Read { id: String },
    List {},
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn checked_output_path(root: &Path, relative: &Path) -> std::io::Result<std::path::PathBuf> {
    let mut path = root.to_owned();
    for part in relative.components() {
        if !matches!(part, std::path::Component::Normal(_)) {
            return Err(std::io::Error::other("invalid output path"));
        }
        path.push(part);
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(std::io::Error::other("symlink output rejected"));
            }
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    Ok(path)
}
fn location(root: &Path, id: &str) -> Result<std::path::PathBuf, ResearchError> {
    if id.len() != 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid("invalid report id"));
    }
    checked_output_path(root, Path::new(&format!("momentum-research/{id}.json"))).map_err(storage)
}
pub fn command(root: &Path, cmd: Command) -> Result<Value, ResearchError> {
    let root = dataset_preview::open_dataset_root(root).map_err(invalid)?;
    let data = match cmd {
        Command::Submit { request } => submit(&root, request)?,
        Command::Read { id } => read(&root, &id)?,
        Command::List {} => {
            let directory =
                checked_output_path(&root, Path::new("momentum-research")).map_err(storage)?;
            let mut reports = Vec::new();
            if directory.exists() {
                for entry in fs::read_dir(directory).map_err(storage)? {
                    let entry = entry.map_err(storage)?;
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if let Some(id) = name.strip_suffix(".json") {
                        // Preserve a broken report as an observable entry; do not hide it.
                        reports.push(json!({"id":id}));
                    }
                }
            }
            reports.sort_by_key(|r| r["id"].as_str().unwrap_or("").to_owned());
            json!({"reports": reports})
        }
    };
    Ok(json!({"schema_version":1,"data":data}))
}
fn read(root: &Path, id: &str) -> Result<Value, ResearchError> {
    let bytes = fs::read(location(root, id)?).map_err(storage)?;
    let report: Value = serde_json::from_slice(&bytes).map_err(storage)?;
    let payload = &report["report"];
    if report["id"] != id || digest(&serde_json::to_vec(payload).map_err(storage)?) != id {
        return Err(storage("corrupt momentum report"));
    }
    Ok(report)
}
fn submit(root: &Path, mut request: Request) -> Result<Value, ResearchError> {
    if request.windows.is_empty() || request.windows.len() > 16 {
        return Err(invalid("require 1–16 windows"));
    }
    request.windows.sort_unstable();
    request.windows.dedup();
    let factors = request
        .windows
        .iter()
        .map(|n| selected_factor(request.factor, *n))
        .collect::<Result<Vec<_>, _>>()
        .map_err(invalid)?;
    let (semantics, direction) = match request.factor {
        None => ("momentum-research@2", "higher_is_stronger"),
        Some(FactorSelection::Volatility) => ("factor-research@1", "lower_is_stronger"),
        Some(FactorSelection::TrendFilterStrict) => ("factor-research@1", "binary_close_above_ma"),
    };
    let universe = match (&request.instrument, &request.pool) {
        (Some(id), None) => {
            StaticUniverse::new("momentum-single@1", vec![id.parse().map_err(invalid)?])
                .map_err(invalid)?
        }
        (None, Some(reference)) => {
            let frozen = crate::pools::resolve(root, reference).map_err(|e| invalid(e.message))?;
            StaticUniverse::load(root, &frozen.universe).map_err(invalid)?
        }
        _ => {
            return Err(invalid(
                "choose exactly one instrument or published pool version",
            ));
        }
    };
    let members = universe.members();
    if members.len() != request.adjusted.len() {
        return Err(invalid("bind exactly one adjusted version per member"));
    }
    let detail = dataset_preview::show(root, &request.dsv).map_err(invalid)?;
    for member in members {
        let found = detail["instruments"].as_array().and_then(|items| {
            items
                .iter()
                .find(|i| i["instrument_id"] == member.to_string())
        });
        if !found.is_some_and(|i| i["kind"] == "etf") {
            return Err(invalid(
                "momentum research requires ETF members present in Dataset",
            ));
        }
    }
    let venues = members
        .iter()
        .map(|i| i.venue().clone())
        .collect::<BTreeSet<VenueId>>()
        .into_iter()
        .collect::<Vec<_>>();
    let mut panel = load_panel_for_venues(root, &request.dsv, &venues).map_err(invalid)?;
    let mut sources = BTreeMap::new();
    let mut prices = BTreeMap::new();
    let mut opens = BTreeMap::new();
    for member in members {
        let version = request
            .adjusted
            .get(&member.to_string())
            .ok_or_else(|| invalid("adjusted version missing"))?;
        let input = adjusted_series::read(root, version).map_err(invalid)?;
        if input.raw_dsv != request.dsv
            || input.instrument_id != member.to_string()
            || input.rows.is_empty()
        {
            return Err(invalid("adjusted input mismatched or empty"));
        }
        for row in &input.rows {
            let close: f64 = row.close.parse().map_err(invalid)?;
            if !close.is_finite() || close <= 0.0 {
                return Err(invalid("invalid adjusted close"));
            }
            prices.insert((member.to_string(), row.trade_date), close);
            if request.evaluation.is_some() {
                let open: f64 = row.open.parse().map_err(invalid)?;
                opens.insert((member.to_string(), row.trade_date), open);
            }
        }
        let mut provenance = serde_json::to_value(&input).map_err(invalid)?;
        provenance.as_object_mut().unwrap().remove("rows");
        provenance["version"] = json!(version);
        provenance["historical_publication_time"] = json!("Unknown");
        sources.insert(member.to_string(), provenance);
    }
    let ids = panel
        .grid
        .column("instrument_id")
        .map_err(invalid)?
        .str()
        .map_err(invalid)?;
    let indices = panel
        .grid
        .column("session_index")
        .map_err(invalid)?
        .u32()
        .map_err(invalid)?;
    let closes = (0..panel.grid.height())
        .map(|i| {
            let id = ids.get(i).ok_or_else(|| invalid("null instrument"))?;
            let index = indices.get(i).ok_or_else(|| invalid("null session"))? as usize;
            Ok(prices
                .get(&(id.to_owned(), panel.sessions[index].session_date))
                .copied())
        })
        .collect::<Result<Vec<_>, ResearchError>>()?;
    let availability = panel
        .grid
        .column("session_index")
        .map_err(invalid)?
        .u32()
        .map_err(invalid)?
        .iter()
        .map(|i| i.map(|i| panel.sessions[i as usize].ts_close.as_unix_nanos()))
        .collect::<Vec<_>>();
    let has_bar = closes.iter().map(Option::is_some).collect::<Vec<_>>();
    panel
        .grid
        .with_column(Series::new("has_bar".into(), has_bar).into())
        .map_err(invalid)?;
    panel
        .grid
        .with_column(Series::new("close".into(), closes).into())
        .map_err(invalid)?;
    panel
        .grid
        .with_column(
            Series::new("close_available_at".into(), availability)
                .cast(&DataType::Datetime(
                    TimeUnit::Nanoseconds,
                    Some(TimeZone::UTC),
                ))
                .map_err(invalid)?
                .into(),
        )
        .map_err(invalid)?;
    // The cache input binding includes every adjusted version and the adapter semantic version.
    let binding =
        json!({"adapter":"momentum-adjusted@1","raw_dsv":request.dsv,"adjusted":request.adjusted});
    let input_id = format!(
        "research-input:sha256:{}",
        digest(&prajna_data::restricted_jcs(&binding).map_err(invalid)?)
    );
    checked_output_path(root, Path::new("features/.staging")).map_err(storage)?;
    let cache = FactorCache::open(root).map_err(storage)?;
    // Evaluation is downstream of the factor input adapter. Future opens are never
    // inserted into the factor Panel, registered as a Factor, or sent to a strategy.
    let session_dates = panel
        .sessions
        .iter()
        .map(|s| s.session_date)
        .collect::<Vec<_>>();
    let member_ids = members.iter().map(ToString::to_string).collect::<Vec<_>>();
    let labels = request
        .evaluation
        .map(|_| {
            prajna_research::evaluation::labels(&session_dates, &member_ids, &opens)
                .map_err(invalid)
        })
        .transpose()?;
    let mut outputs = Vec::new();
    for factor in factors {
        let key = prajna_research::cache::key::FactorKey::for_tree(
            &factor,
            &input_id,
            &universe.id().map_err(invalid)?,
            AvailabilityAssumption::None,
        );
        let hex = key
            .as_str()
            .strip_prefix("fv:sha256:")
            .ok_or_else(|| storage("invalid cache key"))?;
        for file in ["", "/values.parquet", "/meta.json"] {
            checked_output_path(root, Path::new(&format!("features/{hex}{file}")))
                .map_err(storage)?;
        }
        let frame = cache
            .get_or_compute(
                &panel,
                members,
                &universe.id().map_err(invalid)?,
                &input_id,
                &factor,
                AvailabilityAssumption::None,
            )
            .map_err(storage)?;
        let ids = frame
            .column("instrument_id")
            .map_err(invalid)?
            .str()
            .map_err(invalid)?;
        let dates = frame
            .column("session_date")
            .map_err(invalid)?
            .date()
            .map_err(invalid)?
            .physical();
        let values = frame
            .column("value")
            .map_err(invalid)?
            .f64()
            .map_err(invalid)?;
        let statuses = frame
            .column("status")
            .map_err(invalid)?
            .str()
            .map_err(invalid)?;
        let available = frame
            .column("available_at")
            .map_err(invalid)?
            .datetime()
            .map_err(invalid)?
            .physical();
        let mut counts = BTreeMap::<String, usize>::new();
        let mut rows = Vec::new();
        let mut valid = Vec::new();
        let mut values_by_key = BTreeMap::new();
        for i in 0..frame.height() {
            let instrument = ids.get(i).ok_or_else(|| invalid("null instrument"))?;
            let date = chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()
                + chrono::Duration::days(i64::from(
                    dates.get(i).ok_or_else(|| invalid("null date"))?,
                ));
            let status = statuses.get(i).ok_or_else(|| invalid("null status"))?;
            *counts.entry(status.to_owned()).or_default() += 1;
            if let Some(value) = values.get(i) {
                valid.push(value);
                values_by_key.insert((instrument.to_owned(), date), value);
            }
            rows.push(json!({"instrument":instrument,"date":date.to_string(),"value":values.get(i),"status":status,"available_at_ns":available.get(i).map(|v|v.to_string())}));
        }
        valid.sort_by(f64::total_cmp);
        // Histogram boundaries/counts are Rust report values, never browser calculations.
        let mut bins = Vec::new();
        if let (Some(min), Some(max)) = (valid.first(), valid.last()) {
            for i in 0..10 {
                let low = min + (max - min) * f64::from(i) / 10.0;
                let high = if i == 9 {
                    *max
                } else {
                    min + (max - min) * f64::from(i + 1) / 10.0
                };
                let count = valid
                    .iter()
                    .filter(|v| **v >= low && if i == 9 { **v <= high } else { **v < high })
                    .count();
                bins.push(json!({"low":low,"high":high,"count":count}));
            }
        }
        let mut output = json!({"definition":factor.canonical_json(),"rows":rows,"distribution":bins,"status_counts":counts});
        if let Some(labels) = &labels {
            output["relationships"] = relationships(&rows, labels);
            output["statistics"] = serde_json::to_value(
                prajna_research::evaluation::statistics(
                    &session_dates,
                    &member_ids,
                    &values_by_key,
                    labels,
                )
                .map_err(invalid)?,
            )
            .map_err(invalid)?;
        }
        outputs.push(output);
    }
    let execution =
        crate::ExecutionIdentity::capture(&input_id, std::env::current_dir().map_err(storage)?)
            .map_err(storage)?;
    let mut report = json!({"execution":execution,"schema_version":1,"semantics":semantics,"request":request,"universe":universe.id().map_err(invalid)?,"members":members.iter().map(ToString::to_string).collect::<Vec<_>>(),"sources":sources,"input_binding":binding,"direction":direction,"missing_rule":"preserve Session axis; no fill; Factor status propagation","sample_assumption":"Static Universe; survivor bias; supplier retrospective adjustments; not strict PIT","factors":outputs});
    if let Some(labels) = labels {
        report["semantics"] = json!("factor-evaluation@2");
        report["factor_semantics"] = json!(semantics);
        report["evaluation"] = json!({
            "version":prajna_research::evaluation::VERSION,
            "statistics_version":prajna_research::evaluation::STATISTICS_VERSION,
            "method":"next_open_to_forward_open", "horizons":[1,5,20],
            "calendar_basis":"Dataset exchange Sessions; strict venue alignment",
            "price_basis":"same bound supplier-adjusted version as factors; no transaction costs",
            "missing_rule":"null on missing endpoints or insufficient future Sessions; never skip dates",
            "usage":"evaluation only; overlapping labels are not a tradable NAV",
            "labels":labels
        });
    }
    let id = digest(&serde_json::to_vec(&report).map_err(storage)?);
    let path = location(root, &id)?;
    if path.exists() {
        return read(root, &id);
    }
    fs::create_dir_all(path.parent().unwrap()).map_err(storage)?;
    let result = json!({"id":id,"report":report});
    let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap()).map_err(storage)?;
    use std::io::Write;
    file.write_all(&serde_json::to_vec(&result).map_err(storage)?)
        .map_err(storage)?;
    file.as_file().sync_all().map_err(storage)?;
    match file.persist_noclobber(&path) {
        Ok(_) => {}
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(storage(e)),
    }
    fs::File::open(path.parent().unwrap())
        .and_then(|directory| directory.sync_all())
        .map_err(storage)?;
    read(root, &id)
}

/// One research window of the selected factor; `None` is the legacy Momentum flow.
fn selected_factor(selection: Option<FactorSelection>, n: u32) -> Result<Factor, FactorError> {
    match selection {
        None => Factor::new(FactorKind::Momentum, MomentumParams { n }),
        Some(FactorSelection::Volatility) => {
            Factor::new(FactorKind::Volatility, VolatilityParams { n })
        }
        Some(FactorSelection::TrendFilterStrict) => {
            Factor::new(FactorKind::TrendFilterStrict, TrendFilterStrictParams { n })
        }
    }
}

/// Pair only valid factor/label samples in Rust; the browser only plots these points.
fn relationships(rows: &[Value], labels: &[prajna_research::evaluation::Label]) -> Value {
    let values = rows
        .iter()
        .filter_map(|r| {
            Some((
                (r["instrument"].as_str()?, r["date"].as_str()?),
                r["value"].as_f64()?,
            ))
        })
        .collect::<BTreeMap<_, _>>();
    json!(prajna_research::evaluation::HORIZONS.iter().map(|horizon| {
        let mut points = Vec::new();
        let expected_count = labels.iter().filter(|l| l.horizon == *horizon).count();
        for label in labels.iter().filter(|l| l.horizon == *horizon) {
            if let (Some(factor_value), Some(forward_return)) = (
                values.get(&(label.instrument.as_str(), label.date.to_string().as_str())), label.value
            ) {
                points.push(json!({"instrument":label.instrument,"date":label.date,
                    "factor_value":factor_value,"forward_return":forward_return}));
            }
        }
        json!({"horizon":horizon,"expected_count":expected_count,
            "effective_count":points.len(),"missing_count":expected_count-points.len(),
            "missing_ratio":if expected_count == 0 {None} else {Some((expected_count-points.len()) as f64 / expected_count as f64)},
            "sample_assumption":"Static Universe; small samples do not establish predictive validity",
            "points":points})
    }).collect::<Vec<_>>())
}
