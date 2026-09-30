//! Arrow schemas and fixed-parameter Parquet transport for normalized domain rows.

use std::{collections::HashMap, fs::File, path::Path, sync::Arc};

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

/// Fixed settings applied to every normalized Parquet file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParquetWriteOptions {
    pub compression: &'static str,
    pub compression_level: i32,
    pub row_group_size: usize,
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
    NaiveDate::from_ymd_opt(1970, 1, 1)
        .unwrap()
        .checked_add_days(chrono::Days::new(value as u64))
        .or_else(|| {
            if value < 0 {
                NaiveDate::from_ymd_opt(1970, 1, 1)
                    .unwrap()
                    .checked_sub_days(chrono::Days::new(value.unsigned_abs() as u64))
            } else {
                None
            }
        })
        .ok_or_else(|| err("invalid Arrow Date32 value"))
}

pub fn instruments_to_record_batch(rows: &[InstrumentSpec]) -> Result<RecordBatch, DataError> {
    let mut ordered = rows.iter().collect::<Vec<_>>();
    ordered.sort_by(|a, b| a.id().cmp(b.id()));
    let rows = ordered;
    let schema = instruments_schema();
    let cols: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from_iter_values(
            rows.iter().map(|v| v.id().to_string()),
        )),
        Arc::new(StringArray::from_iter_values(
            rows.iter().map(|v| v.id().venue().as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            rows.iter().map(|v| format!("{:?}", v.kind())),
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
        let kind = match text(2)? {
            "Equity" => InstrumentKind::Equity,
            "Etf" => InstrumentKind::Etf,
            "Spot" => InstrumentKind::Spot,
            "Perpetual" => InstrumentKind::Perpetual,
            "Future" => InstrumentKind::Future,
            _ => return Err(err("invalid instrument kind")),
        };
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

pub fn bars_to_record_batch(rows: &[Bar]) -> Result<RecordBatch, DataError> {
    let mut ordered = rows.iter().collect::<Vec<_>>();
    ordered.sort_by(|a, b| {
        (a.instrument_id(), a.bar_spec().to_string(), a.ts_open()).cmp(&(
            b.instrument_id(),
            b.bar_spec().to_string(),
            b.ts_open(),
        ))
    });
    let rows = ordered;
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
pub fn bars_from_record_batch(batch: &RecordBatch) -> Result<Vec<Bar>, DataError> {
    check_batch(batch, "bars")?;
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
        let session = Session::new(id.venue().clone(), session_date, ts_open, ts_close)
            .map_err(|e| err(e.to_string()))?;
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
        out.push(Bar::new(data, Some(&session)).map_err(|e| err(e.to_string()))?);
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

fn check_batch(batch: &RecordBatch, table: &str) -> Result<(), DataError> {
    let schema = batch.schema();
    let m = schema.metadata();
    if m.get("prajna.table").map(String::as_str) != Some(table)
        || m.get("prajna.schema_version").map(String::as_str) != Some(SCHEMA_VERSION)
    {
        return Err(err("Arrow schema table or schema_version mismatch"));
    }
    let expected = match table {
        "instruments" => instruments_schema(),
        "bars" => bars_schema(),
        "sessions" => sessions_schema(),
        _ => return Err(err("unsupported normalized table")),
    };
    if schema.fields() != expected.fields() {
        return Err(err("Arrow schema fields do not match the table schema"));
    }
    Ok(())
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
    let expected = match table {
        "instruments" => instruments_schema(),
        "bars" => bars_schema(),
        "sessions" => sessions_schema(),
        _ => return Err(err("unsupported normalized table")),
    };
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

    fn fixture_bar() -> Bar {
        let venue = VenueId::new("SYNTH").unwrap();
        let id = InstrumentId::new("ETF001", venue.clone()).unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let open = TimestampNs::parse("2026-01-05T09:30:00+08:00").unwrap();
        let close = TimestampNs::parse("2026-01-05T15:00:00+08:00").unwrap();
        let session = Session::new(venue, date, open, close).unwrap();
        let data = BarData {
            instrument_id: id,
            bar_spec: "1d@session".parse().unwrap(),
            session_date: date,
            ts_open: open,
            ts_close: close,
            open: Price::parse("1.2").unwrap(),
            high: Price::parse("1.4").unwrap(),
            low: Price::parse("1.1").unwrap(),
            close: Price::parse("1.3").unwrap(),
            volume: Quantity::parse("10").unwrap(),
            amount: Some(Notional::parse("-2.5").unwrap()),
            available_at: None,
        };
        Bar::new(data, Some(&session)).unwrap()
    }

    #[test]
    fn d7_schema_snapshot_has_exact_fields_types_nullability_and_metadata() {
        let schemas = [instruments_schema(), bars_schema(), sessions_schema()];
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
        ];
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

    #[test]
    fn domain_arrow_parquet_domain_round_trip_preserves_nulls_and_negative_decimal() {
        let row = fixture_bar();
        let batch = bars_to_record_batch(std::slice::from_ref(&row)).unwrap();
        let path = tempfile::NamedTempFile::new().unwrap();
        assert_eq!(
            write_parquet(path.path(), &batch, "dsv:sha256:fixture").unwrap(),
            parquet_write_options()
        );
        let read = read_parquet(path.path(), "bars", "1").unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(
            read[0]
                .schema()
                .metadata()
                .get("prajna.dsv")
                .map(String::as_str),
            Some("dsv:sha256:fixture")
        );
        let restored = bars_from_record_batch(&read[0]).unwrap();
        assert_eq!(restored, vec![row]);
        assert_eq!(
            restored[0].amount().unwrap().mantissa(),
            Notional::parse("-2.5").unwrap().mantissa()
        );
        assert_eq!(restored[0].available_at(), None);
        assert!(read_parquet(path.path(), "sessions", "1").is_err());
        assert!(read_parquet(path.path(), "bars", "2").is_err());
    }

    #[test]
    fn instruments_and_sessions_convert_to_and_from_arrow() {
        let venue = VenueId::new("SYNTH").unwrap();
        let spec = InstrumentSpec::new(InstrumentSpecData {
            id: InstrumentId::new("ETF001", venue.clone()).unwrap(),
            kind: InstrumentKind::Etf,
            native_symbol: "ETF001".into(),
            market_segment: None,
            base_currency: None,
            quote_currency: None,
            settle_currency: None,
            is_inverse: false,
            multiplier: Some(Quantity::parse("-0.25").unwrap()),
            expiry: None,
            price_increment: Price::parse("0.01").unwrap(),
            size_increment: Quantity::parse("1").unwrap(),
            lot_size: None,
            session_timezone: Some("Asia/Shanghai".into()),
        })
        .unwrap();
        let instruments = instruments_to_record_batch(std::slice::from_ref(&spec)).unwrap();
        assert_eq!(
            instruments_from_record_batch(&instruments).unwrap(),
            vec![spec]
        );
        let bar = fixture_bar();
        let session = Session::new(
            bar.instrument_id().venue().clone(),
            bar.session_date(),
            bar.ts_open(),
            bar.ts_close(),
        )
        .unwrap();
        let sessions = sessions_to_record_batch(std::slice::from_ref(&session)).unwrap();
        assert_eq!(
            sessions_from_record_batch(&sessions).unwrap(),
            vec![session]
        );
    }
}
