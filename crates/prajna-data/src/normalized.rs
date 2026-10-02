//! Arrow schemas and fixed-parameter Parquet transport for normalized domain rows.

use std::{cmp::Ordering, collections::HashMap, fs::File, path::Path, sync::Arc};

use arrow_array::{
    Array, ArrayRef, BooleanArray, Date32Array, Decimal128Array, RecordBatch, StringArray,
    TimestampNanosecondArray,
};
use arrow_schema::{DataType, Field, Schema, SchemaRef, TimeUnit};
use chrono::{Datelike, NaiveDate};
use parquet::{
    arrow::{ArrowWriter, arrow_reader::ParquetRecordBatchReaderBuilder},
    basic::{Compression, ZstdLevel},
    file::properties::WriterProperties,
};
use prajna_domain::{
    Bar, BarData, BarSpec, Currency, InstrumentId, InstrumentKind, InstrumentSpec,
    InstrumentSpecData, Notional, Price, Quantity, Session, TimestampNs, VenueId,
};

const SCHEMA_VERSION: &str = "1";
const ZSTD_LEVEL: i32 = 3;
const ROW_GROUP_SIZE: usize = 65_536;

fn kind_code(kind: InstrumentKind) -> &'static str {
    match kind {
        InstrumentKind::Equity => "Equity",
        InstrumentKind::Etf => "Etf",
        InstrumentKind::Spot => "Spot",
        InstrumentKind::Perpetual => "Perpetual",
        InstrumentKind::Future => "Future",
    }
}

fn parse_kind(code: &str) -> Result<InstrumentKind, DataError> {
    [
        InstrumentKind::Equity,
        InstrumentKind::Etf,
        InstrumentKind::Spot,
        InstrumentKind::Perpetual,
        InstrumentKind::Future,
    ]
    .into_iter()
    .find(|kind| kind_code(*kind) == code)
    .ok_or_else(|| err("invalid instrument kind"))
}

/// Fixed settings applied to every normalized Parquet file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParquetWriteOptions {
    pub compression: &'static str,
    pub compression_level: i32,
    pub row_group_size: usize,
}

/// Normalized tradability status for one instrument and venue session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionStatusRow {
    pub instrument_id: InstrumentId,
    pub session_date: NaiveDate,
    pub trade_status: String,
    pub is_tradable: bool,
    pub available_at: Option<TimestampNs>,
    pub source: String,
}

pub const fn parquet_write_options() -> ParquetWriteOptions {
    ParquetWriteOptions {
        compression: "ZSTD",
        compression_level: ZSTD_LEVEL,
        row_group_size: ROW_GROUP_SIZE,
    }
}

#[derive(Debug)]
pub struct DataError(String);

impl std::fmt::Display for DataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for DataError {}
impl From<arrow_schema::ArrowError> for DataError {
    fn from(value: arrow_schema::ArrowError) -> Self {
        Self(value.to_string())
    }
}
impl From<parquet::errors::ParquetError> for DataError {
    fn from(value: parquet::errors::ParquetError) -> Self {
        Self(value.to_string())
    }
}
impl From<std::io::Error> for DataError {
    fn from(value: std::io::Error) -> Self {
        Self(value.to_string())
    }
}
fn err(message: impl Into<String>) -> DataError {
    DataError(message.into())
}

fn field(name: &str, data_type: DataType, nullable: bool) -> Field {
    let mut metadata = HashMap::new();
    if matches!(data_type, DataType::Decimal128(38, 18)) {
        metadata.insert("prajna.decimal_scale".into(), "18".into());
    }
    match name {
        "ts_open" | "ts_close" | "available_at" | "session_date" => {
            metadata.insert("prajna.time_role".into(), name.into());
        }
        _ => {}
    }
    Field::new(name, data_type, nullable).with_metadata(metadata)
}

fn schema(table: &str, fields: Vec<Field>) -> SchemaRef {
    let metadata = HashMap::from([
        ("prajna.table".into(), table.into()),
        ("prajna.schema_version".into(), SCHEMA_VERSION.into()),
    ]);
    Arc::new(Schema::new_with_metadata(fields, metadata))
}
fn utf8(name: &str, nullable: bool) -> Field {
    field(name, DataType::Utf8, nullable)
}
fn decimal(name: &str, nullable: bool) -> Field {
    field(name, DataType::Decimal128(38, 18), nullable)
}
fn timestamp(name: &str, nullable: bool) -> Field {
    field(
        name,
        DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())),
        nullable,
    )
}
fn date(name: &str) -> Field {
    field(name, DataType::Date32, false)
}

pub fn instruments_schema() -> SchemaRef {
    schema(
        "instruments",
        vec![
            utf8("instrument_id", false),
            utf8("venue_id", false),
            utf8("kind", false),
            utf8("native_symbol", false),
            utf8("market_segment", true),
            utf8("base_currency", true),
            utf8("quote_currency", true),
            utf8("settle_currency", true),
            field("is_inverse", DataType::Boolean, false),
            decimal("multiplier", true),
            field("expiry", DataType::Date32, true),
            decimal("price_increment", false),
            decimal("size_increment", false),
            decimal("lot_size", true),
            utf8("session_timezone", true),
        ],
    )
}
pub fn bars_schema() -> SchemaRef {
    schema(
        "bars",
        vec![
            utf8("instrument_id", false),
            utf8("bar_spec", false),
            date("session_date"),
            timestamp("ts_open", false),
            timestamp("ts_close", false),
            decimal("open", false),
            decimal("high", false),
            decimal("low", false),
            decimal("close", false),
            decimal("volume", false),
            decimal("amount", true),
            timestamp("available_at", true),
        ],
    )
}
pub fn sessions_schema() -> SchemaRef {
    schema(
        "sessions",
        vec![
            utf8("venue_id", false),
            date("session_date"),
            timestamp("ts_open", false),
            timestamp("ts_close", false),
        ],
    )
}
pub fn execution_status_schema() -> SchemaRef {
    schema(
        "execution_status",
        vec![
            utf8("instrument_id", false),
            date("session_date"),
            utf8("trade_status", false),
            field("is_tradable", DataType::Boolean, false),
            timestamp("available_at", true),
            utf8("source", false),
        ],
    )
}

fn date32(value: NaiveDate) -> Result<i32, DataError> {
    let days = i64::from(value.num_days_from_ce())
        - i64::from(
            NaiveDate::from_ymd_opt(1970, 1, 1)
                .unwrap()
                .num_days_from_ce(),
        );
    i32::try_from(days).map_err(|_| err("date is outside Arrow Date32 range"))
}
fn naive_date(value: i32) -> Result<NaiveDate, DataError> {
    let days = i64::from(value)
        + i64::from(
            NaiveDate::from_ymd_opt(1970, 1, 1)
                .unwrap()
                .num_days_from_ce(),
        );
    let days = i32::try_from(days).map_err(|_| err("invalid Arrow Date32 value"))?;
    NaiveDate::from_num_days_from_ce_opt(days).ok_or_else(|| err("invalid Arrow Date32 value"))
}

pub fn instruments_to_record_batch(rows: &[InstrumentSpec]) -> Result<RecordBatch, DataError> {
    let mut ordered = rows
        .iter()
        .map(|row| (row.id().to_string(), row))
        .collect::<Vec<_>>();
    ordered.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let rows = ordered.iter().map(|(_, row)| *row).collect::<Vec<_>>();
    let schema = instruments_schema();
    let cols: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from_iter_values(
            rows.iter().map(|v| v.id().to_string()),
        )),
        Arc::new(StringArray::from_iter_values(
            rows.iter().map(|v| v.id().venue().as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            rows.iter().map(|v| kind_code(v.kind())),
        )),
        Arc::new(StringArray::from_iter_values(
            rows.iter().map(|v| v.native_symbol()),
        )),
        Arc::new(StringArray::from(
            rows.iter().map(|v| v.market_segment()).collect::<Vec<_>>(),
        )),
        Arc::new(StringArray::from(
            rows.iter()
                .map(|v| v.base_currency().map(|c| c.as_str()))
                .collect::<Vec<_>>(),
        )),
        Arc::new(StringArray::from(
            rows.iter()
                .map(|v| v.quote_currency().map(|c| c.as_str()))
                .collect::<Vec<_>>(),
        )),
        Arc::new(StringArray::from(
            rows.iter()
                .map(|v| v.settle_currency().map(|c| c.as_str()))
                .collect::<Vec<_>>(),
        )),
        Arc::new(BooleanArray::from(
            rows.iter().map(|v| v.is_inverse()).collect::<Vec<_>>(),
        )),
        decimal_array(
            rows.iter()
                .map(|v| v.multiplier().map(|n| n.mantissa()))
                .collect(),
        )?,
        Arc::new(Date32Array::from(
            rows.iter()
                .map(|v| v.expiry().map(date32).transpose())
                .collect::<Result<Vec<_>, _>>()?,
        )),
        decimal_array(
            rows.iter()
                .map(|v| Some(v.price_increment().mantissa()))
                .collect(),
        )?,
        decimal_array(
            rows.iter()
                .map(|v| Some(v.size_increment().mantissa()))
                .collect(),
        )?,
        decimal_array(
            rows.iter()
                .map(|v| v.lot_size().map(|n| n.mantissa()))
                .collect(),
        )?,
        Arc::new(StringArray::from(
            rows.iter()
                .map(|v| v.session_timezone())
                .collect::<Vec<_>>(),
        )),
    ];
    RecordBatch::try_new(schema, cols).map_err(Into::into)
}

pub fn instruments_from_record_batch(
    batch: &RecordBatch,
) -> Result<Vec<InstrumentSpec>, DataError> {
    check_batch(batch, "instruments")?;
    let mut rows = Vec::with_capacity(batch.num_rows());
    for i in 0..batch.num_rows() {
        let text = |column: usize| -> Result<&str, DataError> {
            Ok(batch
                .column(column)
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or_else(|| err("expected UTF-8 column"))?
                .value(i))
        };
        let optional_text = |column: usize| -> Result<Option<&str>, DataError> {
            let a = batch
                .column(column)
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or_else(|| err("expected UTF-8 column"))?;
            Ok((!a.is_null(i)).then(|| a.value(i)))
        };
        let id: InstrumentId = text(0)?
            .parse()
            .map_err(|e: prajna_domain::InstrumentIdError| err(e.to_string()))?;
        if id.venue().as_str() != text(1)? {
            return Err(err("venue_id does not match instrument_id"));
        }
        let kind = parse_kind(text(2)?)?;
        let currency = |column| -> Result<Option<Currency>, DataError> {
            optional_text(column)?
                .map(|v| Currency::new(v).map_err(|e| err(e.to_string())))
                .transpose()
        };
        let date_col = batch
            .column(10)
            .as_any()
            .downcast_ref::<Date32Array>()
            .ok_or_else(|| err("expected Date32 expiry"))?;
        let bool_col = batch
            .column(8)
            .as_any()
            .downcast_ref::<BooleanArray>()
            .ok_or_else(|| err("expected Boolean is_inverse"))?;
        let price = required_decimal(batch, 11, i)?;
        let size = required_decimal(batch, 12, i)?;
        let spec = InstrumentSpec::new(InstrumentSpecData {
            id,
            kind,
            native_symbol: text(3)?.into(),
            market_segment: optional_text(4)?.map(str::to_owned),
            base_currency: currency(5)?,
            quote_currency: currency(6)?,
            settle_currency: currency(7)?,
            is_inverse: bool_col.value(i),
            multiplier: optional_decimal(batch, 9, i)?
                .map(Quantity::from_mantissa)
                .transpose()
                .map_err(|e| err(e.to_string()))?,
            expiry: if date_col.is_null(i) {
                None
            } else {
                Some(naive_date(date_col.value(i))?)
            },
            price_increment: Price::from_mantissa(price).map_err(|e| err(e.to_string()))?,
            size_increment: Quantity::from_mantissa(size).map_err(|e| err(e.to_string()))?,
            lot_size: optional_decimal(batch, 13, i)?
                .map(Quantity::from_mantissa)
                .transpose()
                .map_err(|e| err(e.to_string()))?,
            session_timezone: optional_text(14)?.map(str::to_owned),
        })
        .map_err(|e| err(e.to_string()))?;
        rows.push(spec);
    }
    Ok(rows)
}

pub fn sessions_to_record_batch(rows: &[Session]) -> Result<RecordBatch, DataError> {
    let mut ordered = rows.iter().collect::<Vec<_>>();
    ordered.sort_by(|a, b| (a.venue_id(), a.session_date()).cmp(&(b.venue_id(), b.session_date())));
    let rows = ordered;
    let schema = sessions_schema();
    let cols: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from_iter_values(
            rows.iter().map(|v| v.venue_id().as_str()),
        )),
        Arc::new(Date32Array::from(
            rows.iter()
                .map(|v| date32(v.session_date()))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        timestamps(rows.iter().map(|v| Some(v.ts_open())).collect()),
        timestamps(rows.iter().map(|v| Some(v.ts_close())).collect()),
    ];
    RecordBatch::try_new(schema, cols).map_err(Into::into)
}
pub fn sessions_from_record_batch(batch: &RecordBatch) -> Result<Vec<Session>, DataError> {
    check_batch(batch, "sessions")?;
    let mut out = Vec::with_capacity(batch.num_rows());
    for i in 0..batch.num_rows() {
        let venue = string_at(batch, 0, i)?
            .parse::<VenueId>()
            .map_err(|e| err(e.to_string()))?;
        let days = batch
            .column(1)
            .as_any()
            .downcast_ref::<Date32Array>()
            .ok_or_else(|| err("expected Date32"))?;
        out.push(
            Session::new(
                venue,
                naive_date(days.value(i))?,
                TimestampNs::from_unix_nanos(timestamp_at(batch, 2, i)?),
                TimestampNs::from_unix_nanos(timestamp_at(batch, 3, i)?),
            )
            .map_err(|e| err(e.to_string()))?,
        );
    }
    Ok(out)
}

pub fn execution_status_to_record_batch(
    rows: &[ExecutionStatusRow],
) -> Result<RecordBatch, DataError> {
    let mut ordered = rows
        .iter()
        .map(|row| (row.instrument_id.to_string(), row.session_date, row))
        .collect::<Vec<_>>();
    ordered.sort_by(|left, right| (&left.0, left.1).cmp(&(&right.0, right.1)));
    if ordered
        .windows(2)
        .any(|pair| pair[0].0 == pair[1].0 && pair[0].1 == pair[1].1)
    {
        return Err(err("duplicate execution status primary key"));
    }

    let rows = ordered.iter().map(|(_, _, row)| *row).collect::<Vec<_>>();
    let schema = execution_status_schema();
    let cols: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from_iter_values(
            rows.iter().map(|row| row.instrument_id.to_string()),
        )),
        Arc::new(Date32Array::from(
            rows.iter()
                .map(|row| date32(row.session_date))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        Arc::new(StringArray::from_iter_values(
            rows.iter().map(|row| row.trade_status.as_str()),
        )),
        Arc::new(BooleanArray::from(
            rows.iter().map(|row| row.is_tradable).collect::<Vec<_>>(),
        )),
        timestamps(rows.iter().map(|row| row.available_at).collect()),
        Arc::new(StringArray::from_iter_values(
            rows.iter().map(|row| row.source.as_str()),
        )),
    ];
    RecordBatch::try_new(schema, cols).map_err(Into::into)
}

pub fn execution_status_from_record_batch(
    batch: &RecordBatch,
) -> Result<Vec<ExecutionStatusRow>, DataError> {
    check_batch(batch, "execution_status")?;
    let dates = batch
        .column(1)
        .as_any()
        .downcast_ref::<Date32Array>()
        .ok_or_else(|| err("expected Date32"))?;
    let tradability = batch
        .column(3)
        .as_any()
        .downcast_ref::<BooleanArray>()
        .ok_or_else(|| err("expected Boolean column"))?;
    let mut rows = Vec::with_capacity(batch.num_rows());
    for index in 0..batch.num_rows() {
        if dates.is_null(index) {
            return Err(err("unexpected null session date"));
        }
        if tradability.is_null(index) {
            return Err(err("unexpected null tradability value"));
        }
        rows.push(ExecutionStatusRow {
            instrument_id: string_at(batch, 0, index)?
                .parse::<InstrumentId>()
                .map_err(|error| err(error.to_string()))?,
            session_date: naive_date(dates.value(index))?,
            trade_status: string_at(batch, 2, index)?.to_owned(),
            is_tradable: tradability.value(index),
            available_at: optional_timestamp_at(batch, 4, index)?,
            source: string_at(batch, 5, index)?.to_owned(),
        });
    }
    Ok(rows)
}

pub fn bars_to_record_batch(rows: &[Bar]) -> Result<RecordBatch, DataError> {
    let mut ordered = rows
        .iter()
        .map(|row| {
            (
                row.instrument_id().to_string(),
                row.bar_spec().to_string(),
                row.ts_open(),
                row,
            )
        })
        .collect::<Vec<_>>();
    ordered.sort_by(|a, b| (&a.0, &a.1, a.2).cmp(&(&b.0, &b.1, b.2)));
    let rows = ordered
        .iter()
        .map(|(_, _, _, row)| *row)
        .collect::<Vec<_>>();
    let schema = bars_schema();
    let cols: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from_iter_values(
            rows.iter().map(|v| v.instrument_id().to_string()),
        )),
        Arc::new(StringArray::from_iter_values(
            rows.iter().map(|v| v.bar_spec().to_string()),
        )),
        Arc::new(Date32Array::from(
            rows.iter()
                .map(|v| date32(v.session_date()))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        timestamps(rows.iter().map(|v| Some(v.ts_open())).collect()),
        timestamps(rows.iter().map(|v| Some(v.ts_close())).collect()),
        decimal_array(rows.iter().map(|v| Some(v.open().mantissa())).collect())?,
        decimal_array(rows.iter().map(|v| Some(v.high().mantissa())).collect())?,
        decimal_array(rows.iter().map(|v| Some(v.low().mantissa())).collect())?,
        decimal_array(rows.iter().map(|v| Some(v.close().mantissa())).collect())?,
        decimal_array(rows.iter().map(|v| Some(v.volume().mantissa())).collect())?,
        decimal_array(
            rows.iter()
                .map(|v| v.amount().map(|n| n.mantissa()))
                .collect(),
        )?,
        timestamps(rows.iter().map(|v| v.available_at()).collect()),
    ];
    RecordBatch::try_new(schema, cols).map_err(Into::into)
}
/// Decode bars against authoritative session bounds for session-anchored bar specs.
pub fn bars_from_record_batch(
    batch: &RecordBatch,
    sessions: &[Session],
) -> Result<Vec<Bar>, DataError> {
    check_batch(batch, "bars")?;
    let mut sessions_by_key = HashMap::new();
    for session in sessions {
        let key = (session.venue_id().clone(), session.session_date());
        if sessions_by_key.insert(key, session).is_some() {
            return Err(err("duplicate session key"));
        }
    }
    let mut out = Vec::with_capacity(batch.num_rows());
    for i in 0..batch.num_rows() {
        let id = string_at(batch, 0, i)?
            .parse::<InstrumentId>()
            .map_err(|e| err(e.to_string()))?;
        let spec = string_at(batch, 1, i)?
            .parse::<BarSpec>()
            .map_err(|e| err(e.to_string()))?;
        let dates = batch
            .column(2)
            .as_any()
            .downcast_ref::<Date32Array>()
            .ok_or_else(|| err("expected Date32"))?;
        let session_date = naive_date(dates.value(i))?;
        let ts_open = TimestampNs::from_unix_nanos(timestamp_at(batch, 3, i)?);
        let ts_close = TimestampNs::from_unix_nanos(timestamp_at(batch, 4, i)?);
        let session = sessions_by_key
            .get(&(id.venue().clone(), session_date))
            .copied();
        let amount = optional_decimal(batch, 10, i)?
            .map(Notional::from_mantissa)
            .transpose()
            .map_err(|e| err(e.to_string()))?;
        let available_at = optional_timestamp_at(batch, 11, i)?;
        let data = BarData {
            instrument_id: id,
            bar_spec: spec,
            session_date,
            ts_open,
            ts_close,
            open: Price::from_mantissa(required_decimal(batch, 5, i)?)
                .map_err(|e| err(e.to_string()))?,
            high: Price::from_mantissa(required_decimal(batch, 6, i)?)
                .map_err(|e| err(e.to_string()))?,
            low: Price::from_mantissa(required_decimal(batch, 7, i)?)
                .map_err(|e| err(e.to_string()))?,
            close: Price::from_mantissa(required_decimal(batch, 8, i)?)
                .map_err(|e| err(e.to_string()))?,
            volume: Quantity::from_mantissa(required_decimal(batch, 9, i)?)
                .map_err(|e| err(e.to_string()))?,
            amount,
            available_at,
        };
        out.push(Bar::new(data, session).map_err(|e| err(e.to_string()))?);
    }
    Ok(out)
}

fn decimal_array(values: Vec<Option<i128>>) -> Result<ArrayRef, DataError> {
    Ok(Arc::new(
        Decimal128Array::from(values).with_precision_and_scale(38, 18)?,
    ))
}
fn timestamps(values: Vec<Option<TimestampNs>>) -> ArrayRef {
    Arc::new(
        TimestampNanosecondArray::from(
            values
                .iter()
                .map(|v| v.map(TimestampNs::as_unix_nanos))
                .collect::<Vec<_>>(),
        )
        .with_timezone("UTC"),
    )
}
fn string_at(batch: &RecordBatch, col: usize, row: usize) -> Result<&str, DataError> {
    let a = batch
        .column(col)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| err("expected UTF-8 column"))?;
    if a.is_null(row) {
        Err(err("unexpected null UTF-8 value"))
    } else {
        Ok(a.value(row))
    }
}
fn required_decimal(batch: &RecordBatch, col: usize, row: usize) -> Result<i128, DataError> {
    optional_decimal(batch, col, row)?.ok_or_else(|| err("unexpected null decimal value"))
}
fn optional_decimal(
    batch: &RecordBatch,
    col: usize,
    row: usize,
) -> Result<Option<i128>, DataError> {
    let a = batch
        .column(col)
        .as_any()
        .downcast_ref::<Decimal128Array>()
        .ok_or_else(|| err("expected Decimal128 column"))?;
    Ok((!a.is_null(row)).then(|| a.value(row)))
}
fn timestamp_at(batch: &RecordBatch, col: usize, row: usize) -> Result<i64, DataError> {
    optional_timestamp_at(batch, col, row)?
        .map(TimestampNs::as_unix_nanos)
        .ok_or_else(|| err("unexpected null timestamp"))
}
fn optional_timestamp_at(
    batch: &RecordBatch,
    col: usize,
    row: usize,
) -> Result<Option<TimestampNs>, DataError> {
    let a = batch
        .column(col)
        .as_any()
        .downcast_ref::<TimestampNanosecondArray>()
        .ok_or_else(|| err("expected Timestamp(ns, UTC) column"))?;
    Ok((!a.is_null(row)).then(|| TimestampNs::from_unix_nanos(a.value(row))))
}

fn expected_schema(table: &str) -> Result<SchemaRef, DataError> {
    match table {
        "instruments" => Ok(instruments_schema()),
        "bars" => Ok(bars_schema()),
        "sessions" => Ok(sessions_schema()),
        "execution_status" => Ok(execution_status_schema()),
        _ => Err(err("unsupported normalized table")),
    }
}

fn check_batch(batch: &RecordBatch, table: &str) -> Result<(), DataError> {
    let schema = batch.schema();
    let m = schema.metadata();
    if m.get("prajna.table").map(String::as_str) != Some(table)
        || m.get("prajna.schema_version").map(String::as_str) != Some(SCHEMA_VERSION)
    {
        return Err(err("Arrow schema table or schema_version mismatch"));
    }
    let expected = expected_schema(table)?;
    if schema.fields() != expected.fields() {
        return Err(err("Arrow schema fields do not match the table schema"));
    }
    Ok(())
}

fn check_primary_key_order(batch: &RecordBatch, table: &str) -> Result<(), DataError> {
    let key_columns: &[usize] = match table {
        "instruments" => &[0],
        "bars" => &[0, 1, 3],
        "sessions" => &[0, 1],
        "execution_status" => &[0, 1],
        _ => return Err(err("unsupported normalized table")),
    };
    let schema = batch.schema();
    let mut previous: Option<Vec<super::Cell>> = None;
    for row in 0..batch.num_rows() {
        let key = key_columns
            .iter()
            .map(|&column| {
                super::read_cell(batch.column(column), schema.field(column), row)
                    .map_err(|error| err(error.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if key.iter().any(|cell| matches!(cell, super::Cell::Null)) {
            return Err(err("primary key contains null"));
        }
        if let Some(previous) = &previous {
            let ordering = previous
                .iter()
                .zip(&key)
                .map(|(left, right)| super::compare_cell(left, right))
                .find(|ordering| *ordering != Ordering::Equal)
                .unwrap_or(Ordering::Equal);
            if ordering != Ordering::Less {
                return Err(err("primary keys must be unique and sorted"));
            }
        }
        previous = Some(key);
    }
    Ok(())
}

fn valid_dsv(dsv: &str) -> bool {
    let Some(digest) = dsv.strip_prefix("dsv:sha256:") else {
        return false;
    };
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Write a single table batch using the registered DSV and fixed writer settings.
pub fn write_parquet(
    path: impl AsRef<Path>,
    batch: &RecordBatch,
    dsv: &str,
) -> Result<ParquetWriteOptions, DataError> {
    let table = batch
        .schema()
        .metadata()
        .get("prajna.table")
        .cloned()
        .ok_or_else(|| err("missing prajna.table"))?;
    check_batch(batch, &table)?;
    check_primary_key_order(batch, &table)?;
    if !valid_dsv(dsv) {
        return Err(err("prajna.dsv must be dsv:sha256:<64 lowercase hex>"));
    }
    let mut metadata = batch.schema().metadata().clone();
    metadata.insert("prajna.dsv".into(), dsv.into());
    let schema = Arc::new(Schema::new_with_metadata(
        batch.schema().fields().clone(),
        metadata,
    ));
    let batch = RecordBatch::try_new(schema.clone(), batch.columns().to_vec())?;
    let options = parquet_write_options();
    let properties = WriterProperties::builder()
        .set_compression(Compression::ZSTD(
            ZstdLevel::try_new(ZSTD_LEVEL).map_err(|e| err(e.to_string()))?,
        ))
        .set_max_row_group_row_count(Some(ROW_GROUP_SIZE))
        .build();
    let mut writer = ArrowWriter::try_new(File::create(path)?, schema, Some(properties))?;
    writer.write(&batch)?;
    writer.close()?;
    Ok(options)
}

/// Read all batches and reject files whose table identity or schema version differs.
pub fn read_parquet(
    path: impl AsRef<Path>,
    table: &str,
    schema_version: &str,
) -> Result<Vec<RecordBatch>, DataError> {
    let builder = ParquetRecordBatchReaderBuilder::try_new(File::open(path)?)?;
    let metadata = builder.schema().metadata();
    if metadata.get("prajna.table").map(String::as_str) != Some(table)
        || metadata.get("prajna.schema_version").map(String::as_str) != Some(schema_version)
    {
        return Err(err("Parquet table or schema_version mismatch"));
    }
    let schema = builder.schema().clone();
    let expected = expected_schema(table)?;
    if schema.fields() != expected.fields() {
        return Err(err("Parquet fields do not match the table schema"));
    }
    builder
        .build()?
        .map(|batch| {
            let batch = batch?;
            RecordBatch::try_new(schema.clone(), batch.columns().to_vec()).map_err(Into::into)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow_schema::DataType;
    use parquet::file::reader::{FileReader, SerializedFileReader};

    const TEST_DSV: &str =
        "dsv:sha256:0000000000000000000000000000000000000000000000000000000000000000";

    fn fixture_session(venue: &str) -> Session {
        let venue = VenueId::new(venue).unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let open = TimestampNs::parse("2026-01-05T09:30:00+08:00").unwrap();
        let close = TimestampNs::parse("2026-01-05T15:00:00+08:00").unwrap();
        Session::new(venue, date, open, close).unwrap()
    }

    fn fixture_bar_for(
        symbol: &str,
        venue: &str,
        amount: Option<Notional>,
        available_at: Option<TimestampNs>,
    ) -> Bar {
        let session = fixture_session(venue);
        let data = BarData {
            instrument_id: InstrumentId::new(symbol, session.venue_id().clone()).unwrap(),
            bar_spec: "1d@session".parse().unwrap(),
            session_date: session.session_date(),
            ts_open: session.ts_open(),
            ts_close: session.ts_close(),
            open: Price::parse("1.2").unwrap(),
            high: Price::parse("1.4").unwrap(),
            low: Price::parse("1.1").unwrap(),
            close: Price::parse("1.3").unwrap(),
            volume: Quantity::parse("10").unwrap(),
            amount,
            available_at,
        };
        Bar::new(data, Some(&session)).unwrap()
    }

    fn fixture_bar() -> Bar {
        fixture_bar_for(
            "ETF001",
            "SYNTH",
            Some(Notional::parse("-2.5").unwrap()),
            None,
        )
    }

    fn fixture_spec(symbol: &str, venue: &str) -> InstrumentSpec {
        let perpetual = symbol.ends_with("-PERP");
        InstrumentSpec::new(InstrumentSpecData {
            id: InstrumentId::new(symbol, VenueId::new(venue).unwrap()).unwrap(),
            kind: if venue == "BINANCE" {
                if perpetual {
                    InstrumentKind::Perpetual
                } else {
                    InstrumentKind::Spot
                }
            } else {
                InstrumentKind::Etf
            },
            native_symbol: symbol.into(),
            market_segment: None,
            base_currency: None,
            quote_currency: None,
            settle_currency: perpetual.then(|| Currency::new("USDT").unwrap()),
            is_inverse: false,
            multiplier: Some(Quantity::parse("-0.25").unwrap()),
            expiry: None,
            price_increment: Price::parse("0.01").unwrap(),
            size_increment: Quantity::parse("1").unwrap(),
            lot_size: None,
            session_timezone: Some("Asia/Shanghai".into()),
        })
        .unwrap()
    }

    #[test]
    fn d7_schema_snapshot_has_exact_fields_types_nullability_and_metadata() {
        let schemas = [
            instruments_schema(),
            bars_schema(),
            sessions_schema(),
            execution_status_schema(),
        ];
        let expected = [
            (
                "instruments",
                vec![
                    ("instrument_id", "Utf8", false),
                    ("venue_id", "Utf8", false),
                    ("kind", "Utf8", false),
                    ("native_symbol", "Utf8", false),
                    ("market_segment", "Utf8", true),
                    ("base_currency", "Utf8", true),
                    ("quote_currency", "Utf8", true),
                    ("settle_currency", "Utf8", true),
                    ("is_inverse", "Boolean", false),
                    ("multiplier", "Decimal128(38, 18)", true),
                    ("expiry", "Date32", true),
                    ("price_increment", "Decimal128(38, 18)", false),
                    ("size_increment", "Decimal128(38, 18)", false),
                    ("lot_size", "Decimal128(38, 18)", true),
                    ("session_timezone", "Utf8", true),
                ],
            ),
            (
                "bars",
                vec![
                    ("instrument_id", "Utf8", false),
                    ("bar_spec", "Utf8", false),
                    ("session_date", "Date32", false),
                    ("ts_open", "Timestamp(Nanosecond, Some(\"UTC\"))", false),
                    ("ts_close", "Timestamp(Nanosecond, Some(\"UTC\"))", false),
                    ("open", "Decimal128(38, 18)", false),
                    ("high", "Decimal128(38, 18)", false),
                    ("low", "Decimal128(38, 18)", false),
                    ("close", "Decimal128(38, 18)", false),
                    ("volume", "Decimal128(38, 18)", false),
                    ("amount", "Decimal128(38, 18)", true),
                    ("available_at", "Timestamp(Nanosecond, Some(\"UTC\"))", true),
                ],
            ),
            (
                "sessions",
                vec![
                    ("venue_id", "Utf8", false),
                    ("session_date", "Date32", false),
                    ("ts_open", "Timestamp(Nanosecond, Some(\"UTC\"))", false),
                    ("ts_close", "Timestamp(Nanosecond, Some(\"UTC\"))", false),
                ],
            ),
            (
                "execution_status",
                vec![
                    ("instrument_id", "Utf8", false),
                    ("session_date", "Date32", false),
                    ("trade_status", "Utf8", false),
                    ("is_tradable", "Boolean", false),
                    ("available_at", "Timestamp(Nanosecond, Some(\"UTC\"))", true),
                    ("source", "Utf8", false),
                ],
            ),
        ];
        assert_eq!(schemas.len(), expected.len());
        for (schema, (table, fields)) in schemas.iter().zip(expected) {
            assert_eq!(
                schema.metadata().get("prajna.table").map(String::as_str),
                Some(table)
            );
            assert_eq!(
                schema
                    .metadata()
                    .get("prajna.schema_version")
                    .map(String::as_str),
                Some("1")
            );
            assert_eq!(schema.fields().len(), fields.len());
            for (field, (name, dtype, nullable)) in schema.fields().iter().zip(fields) {
                assert_eq!(field.name(), name);
                assert_eq!(format!("{:?}", field.data_type()), dtype);
                assert_eq!(field.is_nullable(), nullable);
                let mut expected_metadata = HashMap::new();
                if matches!(field.data_type(), DataType::Decimal128(38, 18)) {
                    expected_metadata.insert("prajna.decimal_scale".to_owned(), "18".to_owned());
                }
                if ["session_date", "ts_open", "ts_close", "available_at"]
                    .contains(&field.name().as_str())
                {
                    expected_metadata.insert("prajna.time_role".to_owned(), field.name().clone());
                }
                assert_eq!(field.metadata(), &expected_metadata);
            }
        }
        assert_eq!(
            parquet_write_options(),
            ParquetWriteOptions {
                compression: "ZSTD",
                compression_level: 3,
                row_group_size: 65_536
            }
        );
    }

    fn fixture_execution_status(
        symbol: &str,
        session_date: NaiveDate,
        trade_status: &str,
        is_tradable: bool,
        available_at: Option<TimestampNs>,
    ) -> ExecutionStatusRow {
        ExecutionStatusRow {
            instrument_id: InstrumentId::new(symbol, VenueId::new("SYNTH").unwrap()).unwrap(),
            session_date,
            trade_status: trade_status.into(),
            is_tradable,
            available_at,
            source: "fixture".into(),
        }
    }

    #[test]
    fn execution_status_round_trip_sorts_rows_and_rejects_duplicate_primary_keys() {
        let jan_5 = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let first = fixture_execution_status(
            "ETF001",
            jan_5,
            "TRADABLE",
            true,
            Some(TimestampNs::parse("2026-01-05T16:00:00+08:00").unwrap()),
        );
        let second = fixture_execution_status(
            "ETF001",
            NaiveDate::from_ymd_opt(2026, 1, 6).unwrap(),
            "UNKNOWN",
            false,
            None,
        );
        let third = fixture_execution_status("ETF002", jan_5, "HALTED", false, None);
        let batch =
            execution_status_to_record_batch(&[third.clone(), second.clone(), first.clone()])
                .unwrap();

        let schema = execution_status_schema();
        assert_eq!(crate::schema_fingerprint(&schema).unwrap().len(), 64);
        assert!(
            crate::logical_hash(
                "execution_status",
                &schema,
                std::slice::from_ref(&batch),
                &["instrument_id", "session_date"],
            )
            .is_ok()
        );
        assert!(execution_status_to_record_batch(&[first.clone(), first.clone()]).is_err());

        let path = tempfile::NamedTempFile::new().unwrap();
        write_parquet(path.path(), &batch, TEST_DSV).unwrap();
        let read = read_parquet(path.path(), "execution_status", "1").unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(
            execution_status_from_record_batch(&read[0]).unwrap(),
            vec![first, second, third]
        );
        assert!(execution_status_from_record_batch(&batch).is_ok());
        assert!(
            execution_status_from_record_batch(
                &sessions_to_record_batch(&[fixture_session("SYNTH")]).unwrap()
            )
            .is_err()
        );
    }

    #[test]
    fn domain_arrow_parquet_domain_round_trip_preserves_nulls_and_negative_decimal() {
        let negative_amount = fixture_bar();
        let null_amount = fixture_bar_for(
            "ETF002",
            "SYNTH",
            None,
            Some(TimestampNs::parse("2026-01-05T16:00:00+08:00").unwrap()),
        );
        let batch = bars_to_record_batch(&[null_amount.clone(), negative_amount.clone()]).unwrap();
        let path = tempfile::NamedTempFile::new().unwrap();
        assert_eq!(
            write_parquet(path.path(), &batch, TEST_DSV).unwrap(),
            parquet_write_options()
        );
        let read = read_parquet(path.path(), "bars", "1").unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].num_rows(), 2);
        assert_eq!(
            read[0]
                .schema()
                .metadata()
                .get("prajna.dsv")
                .map(String::as_str),
            Some(TEST_DSV)
        );
        let session = fixture_session("SYNTH");
        let restored = bars_from_record_batch(&read[0], std::slice::from_ref(&session)).unwrap();
        assert_eq!(restored, vec![negative_amount, null_amount]);
        assert_eq!(
            restored[0].amount().unwrap().mantissa(),
            Notional::parse("-2.5").unwrap().mantissa()
        );
        assert_eq!(restored[0].available_at(), None);
        assert_eq!(restored[1].amount(), None);
        assert_eq!(
            restored[1].available_at(),
            Some(TimestampNs::parse("2026-01-05T16:00:00+08:00").unwrap())
        );
        assert!(bars_from_record_batch(&read[0], &[]).is_err());
        let wrong_session = Session::new(
            session.venue_id().clone(),
            session.session_date(),
            TimestampNs::from_unix_nanos(session.ts_open().as_unix_nanos() + 1),
            session.ts_close(),
        )
        .unwrap();
        assert!(bars_from_record_batch(&read[0], &[wrong_session]).is_err());
        assert!(read_parquet(path.path(), "sessions", "1").is_err());
        assert!(read_parquet(path.path(), "bars", "2").is_err());
    }

    #[test]
    fn instruments_and_sessions_round_trip_through_parquet() {
        let spec = fixture_spec("ETF001", "SYNTH");
        let instruments = instruments_to_record_batch(std::slice::from_ref(&spec)).unwrap();
        assert_eq!(
            instruments_from_record_batch(&instruments).unwrap(),
            vec![spec.clone()]
        );
        let instrument_path = tempfile::NamedTempFile::new().unwrap();
        write_parquet(instrument_path.path(), &instruments, TEST_DSV).unwrap();
        let restored = read_parquet(instrument_path.path(), "instruments", "1").unwrap();
        assert_eq!(
            instruments_from_record_batch(&restored[0]).unwrap(),
            vec![spec]
        );

        let session = fixture_session("SYNTH");
        let sessions = sessions_to_record_batch(std::slice::from_ref(&session)).unwrap();
        assert_eq!(
            sessions_from_record_batch(&sessions).unwrap(),
            vec![session.clone()]
        );
        let session_path = tempfile::NamedTempFile::new().unwrap();
        write_parquet(session_path.path(), &sessions, TEST_DSV).unwrap();
        let restored = read_parquet(session_path.path(), "sessions", "1").unwrap();
        assert_eq!(
            sessions_from_record_batch(&restored[0]).unwrap(),
            vec![session]
        );
    }

    #[test]
    fn instrument_and_bar_order_uses_encoded_primary_key_bytes() {
        let spot = fixture_spec("BTC-USDT", "BINANCE");
        let perpetual = fixture_spec("BTC-USDT-PERP", "BINANCE");
        let instruments = instruments_to_record_batch(&[spot, perpetual]).unwrap();
        let ids = instruments
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(ids.value(0), "BTC-USDT-PERP.BINANCE");
        assert_eq!(ids.value(1), "BTC-USDT.BINANCE");

        let crypto_bar = |symbol: &str| {
            let session_date = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
            let bar_spec: BarSpec = "1d@+08:00".parse().unwrap();
            let (ts_open, ts_close) = bar_spec.bounds(session_date, None).unwrap();
            Bar::new(
                BarData {
                    instrument_id: InstrumentId::new(symbol, VenueId::new("BINANCE").unwrap())
                        .unwrap(),
                    bar_spec,
                    session_date,
                    ts_open,
                    ts_close,
                    open: Price::parse("1").unwrap(),
                    high: Price::parse("1").unwrap(),
                    low: Price::parse("1").unwrap(),
                    close: Price::parse("1").unwrap(),
                    volume: Quantity::parse("1").unwrap(),
                    amount: None,
                    available_at: None,
                },
                None,
            )
            .unwrap()
        };
        let bars =
            bars_to_record_batch(&[crypto_bar("BTC-USDT"), crypto_bar("BTC-USDT-PERP")]).unwrap();
        let ids = bars
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(ids.value(0), "BTC-USDT-PERP.BINANCE");
        assert_eq!(ids.value(1), "BTC-USDT.BINANCE");
        let path = tempfile::NamedTempFile::new().unwrap();
        write_parquet(path.path(), &bars, TEST_DSV).unwrap();
        let read = read_parquet(path.path(), "bars", "1").unwrap();
        assert_eq!(bars_from_record_batch(&read[0], &[]).unwrap().len(), 2);
    }

    #[test]
    fn writer_rejects_unsorted_primary_keys_and_invalid_dsv_before_file_creation() {
        let batch = RecordBatch::try_new(
            sessions_schema(),
            vec![
                Arc::new(StringArray::from(vec!["SYNTH", "SYNTH"])),
                Arc::new(Date32Array::from(vec![1, 0])),
                timestamps(vec![
                    Some(TimestampNs::from_unix_nanos(10)),
                    Some(TimestampNs::from_unix_nanos(20)),
                ]),
                timestamps(vec![
                    Some(TimestampNs::from_unix_nanos(11)),
                    Some(TimestampNs::from_unix_nanos(21)),
                ]),
            ],
        )
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let unsorted_path = directory.path().join("unsorted.parquet");
        assert!(write_parquet(&unsorted_path, &batch, TEST_DSV).is_err());
        assert!(!unsorted_path.exists());

        let sorted = sessions_to_record_batch(&[fixture_session("SYNTH")]).unwrap();
        let empty_dsv_path = directory.path().join("empty-dsv.parquet");
        assert!(write_parquet(&empty_dsv_path, &sorted, " ").is_err());
        assert!(!empty_dsv_path.exists());
        let malformed_dsv_path = directory.path().join("malformed-dsv.parquet");
        assert!(write_parquet(&malformed_dsv_path, &sorted, "dsv:sha256:fixture").is_err());
        assert!(!malformed_dsv_path.exists());
    }

    #[test]
    fn writer_respects_fixed_row_group_limit() {
        let venue = VenueId::new("SYNTH").unwrap();
        let start_date = NaiveDate::from_ymd_opt(2000, 1, 1).unwrap();
        let start_ns = TimestampNs::parse("2000-01-01T00:00:00Z")
            .unwrap()
            .as_unix_nanos();
        let rows = (0..70_000_u64)
            .map(|index| {
                let date = start_date
                    .checked_add_days(chrono::Days::new(index))
                    .unwrap();
                let open = start_ns + index as i64 * 86_400_000_000_000;
                Session::new(
                    venue.clone(),
                    date,
                    TimestampNs::from_unix_nanos(open),
                    TimestampNs::from_unix_nanos(open + 1_000_000_000),
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        let batch = sessions_to_record_batch(&rows).unwrap();
        let path = tempfile::NamedTempFile::new().unwrap();
        write_parquet(path.path(), &batch, TEST_DSV).unwrap();
        let file = SerializedFileReader::new(File::open(path.path()).unwrap()).unwrap();
        let groups = file
            .metadata()
            .row_groups()
            .iter()
            .map(|group| group.num_rows())
            .collect::<Vec<_>>();
        assert_eq!(groups, vec![65_536, 4_464]);
        assert!(
            file.metadata()
                .row_groups()
                .iter()
                .all(|group| matches!(group.column(0).compression(), Compression::ZSTD(_)))
        );
    }
}
