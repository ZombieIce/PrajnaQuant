//! Immutable supplier-adjusted research prices. Never execution prices or cash facts.
use crate::{
    daily_raw::checked_output_path,
    dataset_preview::{self, BarsRequest, PreviewError},
};
use chrono::{DateTime, NaiveDate};
use prajna_domain::Price;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

type Result<T> = std::result::Result<T, PreviewError>;
fn invalid(message: &str) -> PreviewError {
    PreviewError::InvalidInput(message.into())
}
fn io(error: impl ToString) -> PreviewError {
    PreviewError::Io(error.to_string())
}
fn corrupt() -> PreviewError {
    PreviewError::Table {
        table: "adjusted_series".into(),
        message: "invalid or corrupt adjusted version".into(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdjustedRow {
    pub trade_date: NaiveDate,
    pub open: String,
    pub high: String,
    pub low: String,
    pub close: String,
}

/// All fields, including source/method/assumptions and raw DSV, participate in identity.
/// `observed_at` is a local observation, never historical publication time.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdjustedInput {
    pub schema_version: u32,
    pub raw_dsv: String,
    pub instrument_id: String,
    pub method: String,
    pub method_version: String,
    pub source: crate::session_calendar::CalendarSource,
    pub observed_at: String,
    pub retrospective_assumption: String,
    pub close_availability_assumption: String,
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub rows: Vec<AdjustedRow>,
}

/// Uses the same protected-root and non-symlink destination policy as daily publishing.
fn path(root: &Path, version: &str) -> Result<PathBuf> {
    let hex = version
        .strip_prefix("adj:sha256:")
        .ok_or_else(|| invalid("invalid adjusted version"))?;
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid("invalid adjusted version"));
    }
    checked_output_path(root, Path::new(&format!("adjusted-series/{hex}.json"))).map_err(io)
}

fn version(bytes: &[u8]) -> String {
    format!("adj:sha256:{}", crate::sha256(bytes))
}

fn validate(root: &Path, input: &mut AdjustedInput) -> Result<Value> {
    if input.schema_version != 1
        || !matches!(input.method.as_str(), "forward" | "backward")
        || input.start > input.end
        || DateTime::parse_from_rfc3339(&input.observed_at).is_err()
        || [
            &input.method_version,
            &input.source.source_id,
            &input.source.source_version,
            &input.source.source_ref,
            &input.retrospective_assumption,
            &input.close_availability_assumption,
        ]
        .iter()
        .any(|v| v.trim().is_empty())
    {
        return Err(invalid(
            "schema, method, source, date range and explicit assumptions are required",
        ));
    }
    let manifest = crate::read_manifest(root, &input.raw_dsv)
        .map_err(|_| invalid("raw Dataset is unavailable or invalid"))?;
    if manifest.dsv != input.raw_dsv || manifest.core.normalizer.id != "cn-stock-etf-daily" {
        return Err(invalid(
            "adjusted series requires an exact cn-stock-etf-daily raw DSV",
        ));
    }
    let raw = dataset_preview::bars(
        root,
        &BarsRequest {
            dsv: input.raw_dsv.clone(),
            instrument_id: input.instrument_id.clone(),
            start: None,
            end: None,
            limit: usize::MAX,
            offset: 0,
        },
    )?;
    let instrument: prajna_domain::InstrumentId = input
        .instrument_id
        .parse()
        .map_err(|_| invalid("invalid instrument"))?;
    let mut sessions = BTreeSet::new();
    for batch in crate::read_dataset_table(root, &manifest, "sessions").map_err(|_| corrupt())? {
        for session in crate::sessions_from_record_batch(&batch).map_err(|_| corrupt())? {
            if session.venue_id() == instrument.venue() {
                sessions.insert(session.session_date());
            }
        }
    }
    if sessions.first().is_none_or(|d| input.start < *d)
        || sessions.last().is_none_or(|d| input.end > *d)
    {
        return Err(invalid(
            "requested adjustment range exceeds the raw Dataset Session axis",
        ));
    }
    let raw_dates = raw["items"]
        .as_array()
        .ok_or_else(corrupt)?
        .iter()
        .filter_map(|row| row["trade_date"].as_str())
        .collect::<BTreeSet<_>>();
    input.rows.sort_by_key(|row| row.trade_date);
    let mut dates = BTreeSet::new();
    for row in &mut input.rows {
        if row.trade_date < input.start
            || row.trade_date > input.end
            || !sessions.contains(&row.trade_date)
            || !raw_dates.contains(row.trade_date.to_string().as_str())
            || !dates.insert(row.trade_date)
        {
            return Err(invalid(
                "adjusted row is duplicate, outside range, or has no raw Bar/Session",
            ));
        }
        let mut prices = Vec::new();
        for value in [&mut row.open, &mut row.high, &mut row.low, &mut row.close] {
            let price = Price::parse(value).map_err(|_| invalid("invalid exact adjusted price"))?;
            if price <= Price::ZERO {
                return Err(invalid("adjusted prices must be positive"));
            }
            *value = price.to_string();
            prices.push(price);
        }
        let [open, high, low, close] = prices.as_slice() else {
            unreachable!()
        };
        if low > open || low > close || high < open || high < close || low > high {
            return Err(invalid("adjusted OHLC ordering is invalid"));
        }
    }
    let missing = sessions
        .iter()
        .filter(|d| **d >= input.start && **d <= input.end && !dates.contains(d))
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    Ok(
        json!({"rows": dates.len(), "first": dates.first(), "last": dates.last(),
        "missing_sessions": missing.len(), "session_gaps": missing.iter().take(dataset_preview::MAX_GAP_DATES).collect::<Vec<_>>(),
        "session_gaps_truncated": missing.len() > dataset_preview::MAX_GAP_DATES}),
    )
}

/// Imports one explicit supplier file, atomically and without overwriting old versions.
/// Revisions to any identity-bearing field create a distinct version. No implicit latest.
pub fn import(root: &Path, bytes: &[u8]) -> Result<String> {
    let root = dataset_preview::open_dataset_root(root)?;
    let mut input: AdjustedInput =
        serde_json::from_slice(bytes).map_err(|_| invalid("invalid adjusted input JSON"))?;
    validate(&root, &mut input)?;
    // Preserve the exact imported source file; formatting changes also form a new version.
    let bytes = bytes.to_vec();
    let id = version(&bytes);
    let target = path(&root, &id)?;
    let parent = target.parent().expect("adjusted object parent");
    fs::create_dir_all(parent).map_err(io)?;
    let target = path(&root, &id)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(io)?;
    temp.write_all(&bytes).map_err(io)?;
    temp.as_file().sync_all().map_err(io)?;
    match temp.persist_noclobber(&target) {
        Ok(_) => {}
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            if fs::read(target).map_err(io)? != bytes {
                return Err(corrupt());
            }
        }
        Err(error) => return Err(io(error.error)),
    }
    Ok(id)
}

/// Research consumers must freeze both this version and its raw DSV; reads verify both.
/// Returns supplier OHLC only. There is no execution, dividend or account adapter.
pub fn read(root: &Path, id: &str) -> Result<AdjustedInput> {
    let root = dataset_preview::open_dataset_root(root)?;
    let bytes = fs::read(path(&root, id)?).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            PreviewError::NotFound("adjusted version does not exist".into())
        } else {
            io(error)
        }
    })?;
    if version(&bytes) != id {
        return Err(corrupt());
    }
    let mut input: AdjustedInput = serde_json::from_slice(&bytes).map_err(|_| corrupt())?;
    validate(&root, &mut input).map_err(|_| corrupt())?;
    Ok(input)
}

fn metadata(root: &Path, input: &mut AdjustedInput, id: &str) -> Result<Value> {
    let coverage = validate(root, input)?;
    let mut value = serde_json::to_value(input).map_err(io)?;
    let obj = value.as_object_mut().expect("adjusted input object");
    obj.remove("rows");
    obj.insert("version".into(), json!(id));
    obj.insert("coverage".into(), coverage);
    obj.insert("notice".into(), json!("Supplier retrospective adjustment; not strict PIT or sequential TRI. Research prices only; raw prices remain execution/valuation authority. No dividend cash flow is derived."));
    obj.insert("available_at".into(), Value::Null);
    Ok(value)
}

/// Lists versions for one raw DSV without choosing a latest; corrupt objects stay visible.
pub fn catalog(root: &Path, dsv: &str) -> Result<Value> {
    let root = dataset_preview::open_dataset_root(root)?;
    let directory = checked_output_path(&root, Path::new("adjusted-series")).map_err(io)?;
    let mut versions = Vec::new();
    let mut invalid_versions = Vec::new();
    if directory.exists() {
        for entry in fs::read_dir(directory).map_err(io)? {
            let file = entry.map_err(io)?.path();
            if file.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let id = format!(
                "adj:sha256:{}",
                file.file_stem().and_then(|s| s.to_str()).unwrap_or("")
            );
            match read(&root, &id) {
                Ok(mut input) if input.raw_dsv == dsv => {
                    versions.push(metadata(&root, &mut input, &id)?)
                }
                Ok(_) => {}
                Err(_) => invalid_versions.push(
                    json!({"version": id, "error": "unreadable adjusted version; not selectable"}),
                ),
            }
        }
    }
    versions.sort_by_key(|v| v["version"].as_str().unwrap_or_default().to_owned());
    invalid_versions.sort_by_key(|v| v["version"].as_str().unwrap_or_default().to_owned());
    Ok(json!({"versions": versions, "invalid_versions": invalid_versions}))
}

/// Uses the raw preview's range/pagination validation, while keeping adjusted gaps empty.
pub fn bars(root: &Path, request: &BarsRequest, id: &str) -> Result<Value> {
    // Validate the original request even when the imported series is empty.
    dataset_preview::bars(root, request)?;
    let mut input = read(root, id)?;
    if input.raw_dsv != request.dsv || input.instrument_id != request.instrument_id {
        return Err(invalid(
            "adjusted version belongs to a different raw DSV or instrument",
        ));
    }
    let info = metadata(root, &mut input, id)?;
    let rows = input
        .rows
        .iter()
        .filter(|row| {
            request.start.is_none_or(|d| row.trade_date >= d)
                && request.end.is_none_or(|d| row.trade_date <= d)
        })
        .collect::<Vec<_>>();
    if request.offset > rows.len() {
        return Err(invalid("offset exceeds the matching adjusted row count"));
    }
    let items = rows.iter().skip(request.offset).take(request.limit).map(|row| json!({
        "trade_date": row.trade_date, "open": row.open, "high": row.high, "low": row.low, "close": row.close,
        "available_at": null, "volume": null, "amount": null, "source": input.source.source_id
    })).collect::<Vec<_>>();
    Ok(
        json!({"schema_version": 1, "dsv": input.raw_dsv, "adjusted": info,
        "price_adjustment": input.method, "price_unit": "CNY per share/fund unit; supplier adjusted research price",
        "volume_unit": "not part of adjusted price series", "amount_unit": "not part of adjusted price series",
        "total": rows.len(), "offset": request.offset,
        "next_offset": (request.offset + items.len() < rows.len()).then_some(request.offset + items.len()),
        "items": items, "warnings": ["Historical publication time is Unknown; local observed_at is not historical availability."]}),
    )
}
