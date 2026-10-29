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

fn check_row_date(
    date: NaiveDate,
    start: NaiveDate,
    end: NaiveDate,
    sessions: &BTreeSet<NaiveDate>,
    raw_dates: &BTreeSet<&str>,
) -> Result<()> {
    if date < start || date > end {
        return Err(invalid(
            "adjusted row is outside the explicitly declared date range",
        ));
    }
    if !sessions.contains(&date) {
        return Err(invalid(
            "adjusted row date is not a Session in the raw Dataset",
        ));
    }
    if !raw_dates.contains(date.to_string().as_str()) {
        return Err(invalid(
            "adjusted row has no raw Bar in the raw Dataset; no price is invented",
        ));
    }
    Ok(())
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
    let manifest = match crate::read_manifest(root, &input.raw_dsv) {
        Ok(manifest) => manifest,
        Err(crate::ManifestError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(PreviewError::NotFound(format!(
                "raw Dataset Version {} does not exist",
                input.raw_dsv
            )));
        }
        Err(error) => {
            return Err(PreviewError::Manifest {
                dsv: input.raw_dsv.clone(),
                message: error.to_string(),
            });
        }
    };
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
        if !dates.insert(row.trade_date) {
            return Err(invalid("adjusted row date is duplicated"));
        }
        check_row_date(
            row.trade_date,
            input.start,
            input.end,
            &sessions,
            &raw_dates,
        )?;
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
    let missing_without_raw_bar = missing
        .iter()
        .filter(|date| !raw_dates.contains(date.as_str()))
        .count();
    let missing_with_raw_bar = missing.len() - missing_without_raw_bar;
    Ok(
        json!({"rows": dates.len(), "first": dates.first(), "last": dates.last(),
        "missing_sessions": missing.len(),
        "missing_sessions_with_raw_bar": missing_with_raw_bar,
        "missing_sessions_without_raw_bar": missing_without_raw_bar,
        "session_gaps": missing.iter().take(dataset_preview::MAX_GAP_DATES).collect::<Vec<_>>(),
        "session_gaps_truncated": missing.len() > dataset_preview::MAX_GAP_DATES}),
    )
}

/// Import failures caused by the referenced raw Dataset are invalid input (400);
/// integrity failures keep their I/O/table status.
fn import_error(error: PreviewError) -> PreviewError {
    match &error {
        PreviewError::NotFound(_) | PreviewError::Manifest { .. } => invalid(&error.to_string()),
        _ => error,
    }
}

/// A stored adjusted object whose frozen raw dependency no longer validates is an
/// unreadable adjusted object, not an input error. Keep the underlying reason visible.
fn read_error(error: PreviewError) -> PreviewError {
    match &error {
        PreviewError::InvalidInput(_)
        | PreviewError::NotFound(_)
        | PreviewError::Manifest { .. } => PreviewError::Table {
            table: "adjusted_series".into(),
            message: format!("frozen raw Dataset dependency failed: {error}"),
        },
        _ => error,
    }
}

/// Imports one explicit supplier file, atomically and without overwriting old versions.
/// Revisions to any identity-bearing field create a distinct version. No implicit latest.
pub fn import(root: &Path, bytes: &[u8]) -> Result<String> {
    let root = dataset_preview::open_dataset_root(root)?;
    let mut input: AdjustedInput =
        serde_json::from_slice(bytes).map_err(|_| invalid("invalid adjusted input JSON"))?;
    validate(&root, &mut input).map_err(import_error)?;
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

fn load(root: &Path, id: &str) -> Result<AdjustedInput> {
    let bytes = fs::read(path(root, id)?).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            PreviewError::NotFound("adjusted version does not exist".into())
        } else {
            io(error)
        }
    })?;
    if version(&bytes) != id {
        return Err(corrupt());
    }
    serde_json::from_slice(&bytes).map_err(|_| corrupt())
}

/// Research consumers must freeze both this version and its raw DSV; reads verify both.
/// Returns supplier OHLC only. There is no execution, dividend or account adapter.
pub fn read(root: &Path, id: &str) -> Result<AdjustedInput> {
    let root = dataset_preview::open_dataset_root(root)?;
    let mut input = load(&root, id)?;
    validate(&root, &mut input).map_err(read_error)?;
    Ok(input)
}

fn metadata(input: &AdjustedInput, id: &str, coverage: Value) -> Result<Value> {
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
/// The raw DSV accepts the same full or bare 64-hex form as the preview reader.
pub fn catalog(root: &Path, dsv: &str) -> Result<Value> {
    let root = dataset_preview::open_dataset_root(root)?;
    let dsv = dataset_preview::normalize_dsv(dsv)?;
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
            let invalid = |error: PreviewError| {
                json!({
                    "version": id,
                    "error": format!("unreadable adjusted version; not selectable: {error}")
                })
            };
            match load(&root, &id) {
                Ok(mut input) if input.raw_dsv == dsv => match validate(&root, &mut input) {
                    Ok(coverage) => versions.push(metadata(&input, &id, coverage)?),
                    Err(error) => invalid_versions.push(invalid(error)),
                },
                Ok(_) => {}
                Err(error) => invalid_versions.push(invalid(error)),
            }
        }
    }
    versions.sort_by_key(|v| v["version"].as_str().unwrap_or_default().to_owned());
    invalid_versions.sort_by_key(|v| v["version"].as_str().unwrap_or_default().to_owned());
    Ok(json!({"versions": versions, "invalid_versions": invalid_versions}))
}

/// Applies the preview's date/limit semantics to one frozen adjusted version.
pub fn bars(root: &Path, request: &BarsRequest, id: &str) -> Result<Value> {
    if request.limit == 0 {
        return Err(invalid("limit must be positive"));
    }
    if let (Some(start), Some(end)) = (request.start, request.end) {
        if start > end {
            return Err(invalid("start must not be after end"));
        }
    }
    let root = dataset_preview::open_dataset_root(root)?;
    // The reader accepts the documented full or bare 64-hex raw DSV; compare the
    // normalized form with the object's stored canonical DSV.
    let dsv = dataset_preview::normalize_dsv(&request.dsv)?;
    let mut input = load(&root, id)?;
    let coverage = validate(&root, &mut input).map_err(read_error)?;
    if input.raw_dsv != dsv || input.instrument_id != request.instrument_id {
        return Err(invalid(
            "adjusted version belongs to a different raw DSV or instrument",
        ));
    }
    let info = metadata(&input, id, coverage)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn date(month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, month, day).unwrap()
    }

    #[test]
    fn row_dates_are_checked_against_range_session_and_raw_bar_separately() {
        let start = date(1, 5);
        let end = date(1, 9);
        let sessions = [date(1, 5), date(1, 8)]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let raw_dates = ["2026-01-05"].into_iter().collect::<BTreeSet<_>>();
        let message = |value: Result<()>| value.unwrap_err().to_string();
        assert!(
            message(check_row_date(
                date(1, 4),
                start,
                end,
                &sessions,
                &raw_dates
            ))
            .contains("outside the explicitly declared date range")
        );
        assert!(
            message(check_row_date(
                date(1, 6),
                start,
                end,
                &sessions,
                &raw_dates
            ))
            .contains("not a Session")
        );
        assert!(
            message(check_row_date(
                date(1, 8),
                start,
                end,
                &sessions,
                &raw_dates
            ))
            .contains("no raw Bar")
        );
        check_row_date(date(1, 5), start, end, &sessions, &raw_dates).unwrap();
    }
}
