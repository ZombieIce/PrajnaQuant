use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
    path::Path,
};

use arrow_array::{
    Array, Date32Array, Decimal128Array, RecordBatch, StringArray, TimestampNanosecondArray,
};
use chrono::{Duration, NaiveDate};
use polars::prelude::{DataFrame, DataType, NamedFrom, Series, TimeUnit, TimeZone};
use prajna_data::{DataError, Manifest, ManifestError, read_manifest, read_parquet};
use prajna_domain::{InstrumentId, Price, Session, TimestampNs, VenueId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelSession {
    pub session_date: NaiveDate,
    pub ts_open: TimestampNs,
    pub ts_close: TimestampNs,
}

#[derive(Debug)]
pub struct Panel {
    pub sessions: Vec<PanelSession>,
    pub instruments: Vec<InstrumentId>,
    pub grid: DataFrame,
}

#[derive(Debug)]
pub enum PanelError {
    Manifest(ManifestError),
    Data(DataError),
    Polars(polars::error::PolarsError),
    InvalidManifest(String),
    InvalidTableData(String),
}

impl fmt::Display for PanelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manifest(error) => write!(formatter, "invalid DSV manifest: {error}"),
            Self::Data(error) => write!(formatter, "invalid D7 Parquet data: {error}"),
            Self::Polars(error) => write!(formatter, "could not build Polars panel: {error}"),
            Self::InvalidManifest(message) => {
                write!(formatter, "invalid panel manifest: {message}")
            }
            Self::InvalidTableData(message) => {
                write!(formatter, "invalid panel table data: {message}")
            }
        }
    }
}

impl Error for PanelError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Manifest(error) => Some(error),
            Self::Data(error) => Some(error),
            Self::Polars(error) => Some(error),
            Self::InvalidManifest(_) | Self::InvalidTableData(_) => None,
        }
    }
}

impl From<ManifestError> for PanelError {
    fn from(error: ManifestError) -> Self {
        Self::Manifest(error)
    }
}

impl From<DataError> for PanelError {
    fn from(error: DataError) -> Self {
        Self::Data(error)
    }
}

impl From<polars::error::PolarsError> for PanelError {
    fn from(error: polars::error::PolarsError) -> Self {
        Self::Polars(error)
    }
}

pub fn load_panel(
    lake_root: impl AsRef<Path>,
    dsv: &str,
    venue: &VenueId,
) -> Result<Panel, PanelError> {
    let lake_root = lake_root.as_ref();
    let manifest = read_manifest(lake_root, dsv)?;
    if manifest.core.manifest_version != 1 {
        return Err(PanelError::InvalidManifest(format!(
            "unsupported manifest version {}",
            manifest.core.manifest_version
        )));
    }

    let instruments_batches = read_table(lake_root, &manifest, "instruments")?;
    let sessions_batches = read_table(lake_root, &manifest, "sessions")?;
    let bars_batches = read_table(lake_root, &manifest, "bars")?;

    let instruments = read_instruments(&instruments_batches, venue)?;
    let sessions = read_sessions(&sessions_batches, venue)?;
    let bars = read_bars(&bars_batches, venue, &instruments, &sessions)?;
    let grid = build_grid(&instruments, &sessions, &bars)?;

    Ok(Panel {
        sessions,
        instruments,
        grid,
    })
}

fn read_table(
    lake_root: &Path,
    manifest: &Manifest,
    table_name: &'static str,
) -> Result<Vec<RecordBatch>, PanelError> {
    let table = manifest
        .core
        .tables
        .iter()
        .find(|table| table.table == table_name)
        .ok_or_else(|| {
            PanelError::InvalidManifest(format!("missing {table_name} table descriptor"))
        })?;
    if table.schema_version != 1 {
        return Err(PanelError::InvalidManifest(format!(
            "unsupported {table_name} schema version {}",
            table.schema_version
        )));
    }
    let dsv_hex = manifest
        .dsv
        .strip_prefix("dsv:sha256:")
        .ok_or_else(|| PanelError::InvalidManifest("manifest DSV has an invalid format".into()))?;
    let path = lake_root
        .join("normalized")
        .join(table_name)
        .join(dsv_hex)
        .join("part-00000.parquet");
    let batches = read_parquet(path, table_name, &table.schema_version.to_string())?;
    for batch in &batches {
        if batch
            .schema()
            .metadata()
            .get("prajna.dsv")
            .map(String::as_str)
            != Some(manifest.dsv.as_str())
        {
            return Err(PanelError::InvalidManifest(format!(
                "{table_name} Parquet DSV does not match the manifest"
            )));
        }
    }
    Ok(batches)
}

fn read_instruments(
    batches: &[RecordBatch],
    venue: &VenueId,
) -> Result<Vec<InstrumentId>, PanelError> {
    let mut instruments = BTreeSet::new();
    for batch in batches {
        let ids = column::<StringArray>(batch, 0, "instruments", "instrument_id")?;
        let venues = column::<StringArray>(batch, 1, "instruments", "venue_id")?;
        for row in 0..batch.num_rows() {
            let row_venue = required_string(venues, row, "instruments", "venue_id")?;
            if row_venue != venue.as_str() {
                continue;
            }
            let id = required_string(ids, row, "instruments", "instrument_id")?
                .parse::<InstrumentId>()
                .map_err(|error| invalid_table("instruments", error.to_string()))?;
            if id.venue() != venue {
                return Err(invalid_table(
                    "instruments",
                    format!("instrument {id} disagrees with venue_id {row_venue}"),
                ));
            }
            if !instruments.insert(id.clone()) {
                return Err(invalid_table(
                    "instruments",
                    format!("duplicate instrument_id {id}"),
                ));
            }
        }
    }
    Ok(instruments.into_iter().collect())
}

fn read_sessions(
    batches: &[RecordBatch],
    venue: &VenueId,
) -> Result<Vec<PanelSession>, PanelError> {
    let mut sessions = BTreeMap::new();
    for batch in batches {
        let venues = column::<StringArray>(batch, 0, "sessions", "venue_id")?;
        let dates = column::<Date32Array>(batch, 1, "sessions", "session_date")?;
        let opens = column::<TimestampNanosecondArray>(batch, 2, "sessions", "ts_open")?;
        let closes = column::<TimestampNanosecondArray>(batch, 3, "sessions", "ts_close")?;
        for row in 0..batch.num_rows() {
            if required_string(venues, row, "sessions", "venue_id")? != venue.as_str() {
                continue;
            }
            let session_date =
                date32_to_naive_date(required_date(dates, row, "sessions", "session_date")?)?;
            let ts_open = TimestampNs::from_unix_nanos(required_timestamp(
                opens, row, "sessions", "ts_open",
            )?);
            let ts_close = TimestampNs::from_unix_nanos(required_timestamp(
                closes, row, "sessions", "ts_close",
            )?);
            Session::new(venue.clone(), session_date, ts_open, ts_close).map_err(|error| {
                invalid_table(
                    "sessions",
                    format!("invalid session on {session_date}: {error}"),
                )
            })?;
            if sessions
                .insert(
                    session_date,
                    PanelSession {
                        session_date,
                        ts_open,
                        ts_close,
                    },
                )
                .is_some()
            {
                return Err(invalid_table(
                    "sessions",
                    format!("duplicate session_date {session_date} for venue {venue}"),
                ));
            }
        }
    }
    Ok(sessions.into_values().collect())
}

#[derive(Clone, Copy)]
struct BarValues {
    open: f64,
    close: f64,
    available_at: Option<i64>,
}

fn read_bars(
    batches: &[RecordBatch],
    venue: &VenueId,
    instruments: &[InstrumentId],
    sessions: &[PanelSession],
) -> Result<BTreeMap<(InstrumentId, NaiveDate), BarValues>, PanelError> {
    let instrument_ids = instruments.iter().cloned().collect::<BTreeSet<_>>();
    let session_dates = sessions
        .iter()
        .map(|session| session.session_date)
        .collect::<BTreeSet<_>>();
    let mut bars = BTreeMap::new();
    for batch in batches {
        let ids = column::<StringArray>(batch, 0, "bars", "instrument_id")?;
        let dates = column::<Date32Array>(batch, 2, "bars", "session_date")?;
        let opens = column::<Decimal128Array>(batch, 5, "bars", "open")?;
        let closes = column::<Decimal128Array>(batch, 8, "bars", "close")?;
        let available_at = column::<TimestampNanosecondArray>(batch, 11, "bars", "available_at")?;
        for row in 0..batch.num_rows() {
            let id = required_string(ids, row, "bars", "instrument_id")?
                .parse::<InstrumentId>()
                .map_err(|error| invalid_table("bars", error.to_string()))?;
            if id.venue() != venue {
                continue;
            }
            if !instrument_ids.contains(&id) {
                return Err(invalid_table(
                    "bars",
                    format!("bar references unknown instrument {id}"),
                ));
            }
            let session_date =
                date32_to_naive_date(required_date(dates, row, "bars", "session_date")?)?;
            if !session_dates.contains(&session_date) {
                return Err(invalid_table(
                    "bars",
                    format!("bar for {id} references unknown session {session_date}"),
                ));
            }
            let open = decimal_38_18_to_f64(required_decimal(opens, row, "bars", "open")?)?;
            let close = decimal_38_18_to_f64(required_decimal(closes, row, "bars", "close")?)?;
            let available_at = (!available_at.is_null(row)).then(|| available_at.value(row));
            if bars
                .insert(
                    (id.clone(), session_date),
                    BarValues {
                        open,
                        close,
                        available_at,
                    },
                )
                .is_some()
            {
                return Err(invalid_table(
                    "bars",
                    format!("multiple bars for {id} on {session_date}"),
                ));
            }
        }
    }
    Ok(bars)
}

fn build_grid(
    instruments: &[InstrumentId],
    sessions: &[PanelSession],
    bars: &BTreeMap<(InstrumentId, NaiveDate), BarValues>,
) -> Result<DataFrame, PanelError> {
    let rows = instruments
        .len()
        .checked_mul(sessions.len())
        .ok_or_else(|| PanelError::InvalidTableData("panel row count overflow".into()))?;
    let mut instrument_ids = Vec::with_capacity(rows);
    let mut session_dates = Vec::with_capacity(rows);
    let mut session_indices = Vec::with_capacity(rows);
    let mut opens = Vec::with_capacity(rows);
    let mut closes = Vec::with_capacity(rows);
    let mut has_bars = Vec::with_capacity(rows);
    let mut close_available_at = Vec::with_capacity(rows);

    for instrument in instruments {
        for (index, session) in sessions.iter().enumerate() {
            let index = u32::try_from(index)
                .map_err(|_| PanelError::InvalidTableData("session index exceeds u32".into()))?;
            let bar = bars.get(&(instrument.clone(), session.session_date));
            instrument_ids.push(instrument.to_string());
            session_dates.push(naive_date_to_date32(session.session_date)?);
            session_indices.push(index);
            opens.push(bar.map(|bar| bar.open));
            closes.push(bar.map(|bar| bar.close));
            has_bars.push(bar.is_some());
            close_available_at.push(bar.and_then(|bar| bar.available_at));
        }
    }

    let session_dates = Series::new("session_date".into(), session_dates).cast(&DataType::Date)?;
    let close_available_at = Series::new("close_available_at".into(), close_available_at).cast(
        &DataType::Datetime(TimeUnit::Nanoseconds, Some(TimeZone::UTC)),
    )?;
    Ok(DataFrame::new(
        rows,
        vec![
            Series::new("instrument_id".into(), instrument_ids).into(),
            session_dates.into(),
            Series::new("session_index".into(), session_indices).into(),
            Series::new("open".into(), opens).into(),
            Series::new("close".into(), closes).into(),
            Series::new("has_bar".into(), has_bars).into(),
            close_available_at.into(),
        ],
    )?)
}

fn column<'a, T: Array + 'static>(
    batch: &'a RecordBatch,
    index: usize,
    table: &str,
    name: &str,
) -> Result<&'a T, PanelError> {
    batch
        .column(index)
        .as_any()
        .downcast_ref::<T>()
        .ok_or_else(|| invalid_table(table, format!("unexpected type for {name}")))
}

fn required_string<'a>(
    values: &'a StringArray,
    row: usize,
    table: &str,
    name: &str,
) -> Result<&'a str, PanelError> {
    if values.is_null(row) {
        return Err(invalid_table(table, format!("null {name} at row {row}")));
    }
    Ok(values.value(row))
}

fn required_date(
    values: &Date32Array,
    row: usize,
    table: &str,
    name: &str,
) -> Result<i32, PanelError> {
    if values.is_null(row) {
        return Err(invalid_table(table, format!("null {name} at row {row}")));
    }
    Ok(values.value(row))
}

fn required_timestamp(
    values: &TimestampNanosecondArray,
    row: usize,
    table: &str,
    name: &str,
) -> Result<i64, PanelError> {
    if values.is_null(row) {
        return Err(invalid_table(table, format!("null {name} at row {row}")));
    }
    Ok(values.value(row))
}

fn required_decimal(
    values: &Decimal128Array,
    row: usize,
    table: &str,
    name: &str,
) -> Result<i128, PanelError> {
    if values.is_null(row) {
        return Err(invalid_table(table, format!("null {name} at row {row}")));
    }
    Ok(values.value(row))
}

fn date32_to_naive_date(days: i32) -> Result<NaiveDate, PanelError> {
    NaiveDate::from_ymd_opt(1970, 1, 1)
        .and_then(|epoch| epoch.checked_add_signed(Duration::days(i64::from(days))))
        .ok_or_else(|| invalid_table("sessions", format!("invalid Date32 value {days}")))
}

fn naive_date_to_date32(date: NaiveDate) -> Result<i32, PanelError> {
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).expect("Unix epoch is a valid date");
    i32::try_from((date - epoch).num_days())
        .map_err(|_| invalid_table("sessions", format!("date {date} exceeds Date32 range")))
}

fn decimal_38_18_to_f64(mantissa: i128) -> Result<f64, PanelError> {
    let price = Price::from_mantissa(mantissa)
        .map_err(|error| invalid_table("bars", format!("invalid Decimal(38,18): {error}")))?;
    Ok(price.to_f64())
}

fn invalid_table(table: &str, message: impl Into<String>) -> PanelError {
    PanelError::InvalidTableData(format!("{table}: {}", message.into()))
}

#[cfg(test)]
mod tests {
    use super::decimal_38_18_to_f64;

    #[test]
    fn explicitly_converts_decimal_38_18_mantissas_to_f64() {
        let mantissa = 12_345_000_000_000_000_000_i128;
        let value = decimal_38_18_to_f64(mantissa).unwrap();
        assert!((value - 12.345).abs() < f64::EPSILON);
    }
}
