//! Arrow and Parquet data layer for Prajna Quant.

mod manifest;
mod normalized;
mod normalizer;
mod publish;
mod raw;

pub use normalizer::{
    NormalizationIssue, NormalizationOutput, NormalizedTables, Normalizer, NormalizerRegistry,
    RawInput,
};
pub use publish::{PublishError, RebuildError, publish_dataset, rebuild_dataset};

pub use manifest::{
    Manifest, ManifestCore, ManifestError, ManifestFile, ManifestInput, ManifestProvenance,
    ManifestTable, NormalizerIdentity, read_manifest, write_manifest,
};
pub use normalized::{
    DataError, ParquetWriteOptions, bars_from_record_batch, bars_schema, bars_to_record_batch,
    instruments_from_record_batch, instruments_schema, instruments_to_record_batch,
    parquet_write_options, read_parquet, sessions_from_record_batch, sessions_schema,
    sessions_to_record_batch, write_parquet,
};
pub use raw::{RawStore, RawStoreError, SourceKind, SourceRecord, SourceRecordInput};

use std::{cmp::Ordering, fmt};

use arrow_array::{Array, ArrayRef, RecordBatch};
use arrow_cast::cast;
use arrow_schema::{DataType, Field, Schema, TimeUnit};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Errors returned by canonical encoders and logical hashing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalError(String);

impl CanonicalError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for CanonicalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CanonicalError {}

/// Serialize the supported JSON subset using RFC 8785 canonical ordering.
///
/// Floating point values and integers outside JavaScript's exact integer range are
/// rejected so the result remains stable across implementations.
pub fn restricted_jcs(value: &Value) -> Result<Vec<u8>, CanonicalError> {
    let mut output = String::new();
    write_jcs(value, &mut output)?;
    Ok(output.into_bytes())
}

fn write_jcs(value: &Value, output: &mut String) -> Result<(), CanonicalError> {
    match value {
        Value::Null => output.push_str("null"),
        Value::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                if value.unsigned_abs() > (1_u64 << 53) - 1 {
                    return Err(CanonicalError::new("JCS integer exceeds 2^53 - 1"));
                }
                output.push_str(&value.to_string());
            } else if let Some(value) = number.as_u64() {
                if value > (1_u64 << 53) - 1 {
                    return Err(CanonicalError::new("JCS integer exceeds 2^53 - 1"));
                }
                output.push_str(&value.to_string());
            } else {
                return Err(CanonicalError::new(
                    "JCS floating point values are forbidden",
                ));
            }
        }
        Value::String(value) => output.push_str(
            &serde_json::to_string(value)
                .map_err(|error| CanonicalError::new(format!("encode JCS string: {error}")))?,
        ),
        Value::Array(values) => {
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    output.push(',');
                }
                write_jcs(value, output)?;
            }
            output.push(']');
        }
        Value::Object(values) => {
            let mut entries: Vec<_> = values.iter().collect();
            entries.sort_by(|(left, _), (right, _)| utf16_cmp(left, right));
            output.push('{');
            for (index, (key, value)) in entries.into_iter().enumerate() {
                if index != 0 {
                    output.push(',');
                }
                output.push_str(&serde_json::to_string(key).map_err(|error| {
                    CanonicalError::new(format!("encode JCS object key: {error}"))
                })?);
                output.push(':');
                write_jcs(value, output)?;
            }
            output.push('}');
        }
    }
    Ok(())
}

fn utf16_cmp(left: &str, right: &str) -> Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}

/// Calculate the stable schema fingerprint specified by POC-0 Appendix B.
pub fn schema_fingerprint(schema: &Schema) -> Result<String, CanonicalError> {
    let fields = schema
        .fields()
        .iter()
        .map(|field| {
            serde_json::json!({
                "name": field.name(),
                "type": canonical_type(field.data_type()),
                "nullable": field.is_nullable(),
                "metadata": metadata_json(field.metadata(), None),
            })
        })
        .collect::<Vec<_>>();
    let canonical = serde_json::json!({
        "fields": fields,
        "metadata": metadata_json(schema.metadata(), Some("prajna.dsv")),
    });
    Ok(sha256(&restricted_jcs(&canonical)?))
}

fn metadata_json(
    metadata: &std::collections::HashMap<String, String>,
    excluded_key: Option<&str>,
) -> Value {
    Value::Object(
        metadata
            .iter()
            .filter(|(key, _)| Some(key.as_str()) != excluded_key)
            .map(|(key, value)| (key.clone(), Value::String(value.clone())))
            .collect(),
    )
}

fn canonical_type(data_type: &DataType) -> String {
    match data_type {
        // Dictionary encoding is a storage detail; decoded value type is logical.
        DataType::Dictionary(_, value_type) => canonical_type(value_type),
        DataType::Utf8 => "utf8".into(),
        DataType::LargeUtf8 => "large_utf8".into(),
        DataType::Int8 => "int8".into(),
        DataType::Int16 => "int16".into(),
        DataType::Int32 => "int32".into(),
        DataType::Int64 => "int64".into(),
        DataType::UInt8 => "uint8".into(),
        DataType::UInt16 => "uint16".into(),
        DataType::UInt32 => "uint32".into(),
        DataType::UInt64 => "uint64".into(),
        DataType::Float16 => "float16".into(),
        DataType::Float32 => "float32".into(),
        DataType::Float64 => "float64".into(),
        DataType::Boolean => "bool".into(),
        DataType::Date32 => "date32".into(),
        DataType::Date64 => "date64".into(),
        DataType::Decimal128(precision, scale) => format!("decimal128({precision},{scale})"),
        DataType::Decimal256(precision, scale) => format!("decimal256({precision},{scale})"),
        DataType::Timestamp(TimeUnit::Second, zone) => timestamp_type("s", zone.as_deref()),
        DataType::Timestamp(TimeUnit::Millisecond, zone) => timestamp_type("ms", zone.as_deref()),
        DataType::Timestamp(TimeUnit::Microsecond, zone) => timestamp_type("us", zone.as_deref()),
        DataType::Timestamp(TimeUnit::Nanosecond, zone) => timestamp_type("ns", zone.as_deref()),
        _ => format!("{data_type:?}"),
    }
}

fn timestamp_type(unit: &str, timezone: Option<&str>) -> String {
    match timezone {
        Some(zone) => format!("timestamp({unit},{zone})"),
        None => format!("timestamp({unit})"),
    }
}

/// Calculate the content hash of Arrow batches using Appendix A's row encoding.
///
/// Rows are sorted by the named primary-key columns before encoding. Dictionary
/// arrays are decoded before their values are encoded. Every batch must share the
/// supplied schema, and duplicate primary keys or unsupported physical types fail.
pub fn logical_hash(
    table_name: &str,
    schema: &Schema,
    batches: &[RecordBatch],
    primary_key: &[&str],
) -> Result<String, CanonicalError> {
    if table_name.is_empty() {
        return Err(CanonicalError::new("table name must not be empty"));
    }
    if primary_key.is_empty() {
        return Err(CanonicalError::new("primary key must not be empty"));
    }
    for field in schema.fields() {
        validate_logical_type(field.data_type())?;
    }

    let key_indices = primary_key
        .iter()
        .map(|name| {
            schema
                .index_of(name)
                .map_err(|_| CanonicalError::new(format!("primary key field not found: {name}")))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut rows = Vec::new();
    for batch in batches {
        if batch.schema().as_ref() != schema {
            return Err(CanonicalError::new(
                "record batch schema does not match schema",
            ));
        }
        for row_index in 0..batch.num_rows() {
            let mut row = Vec::with_capacity(schema.fields().len());
            for (column_index, field) in schema.fields().iter().enumerate() {
                row.push(read_cell(batch.column(column_index), field, row_index)?);
            }
            rows.push(row);
        }
    }
    rows.sort_by(|left, right| compare_keys(left, right, &key_indices));
    for pair in rows.windows(2) {
        if compare_keys(&pair[0], &pair[1], &key_indices) == Ordering::Equal {
            return Err(CanonicalError::new("duplicate primary key"));
        }
    }

    Ok(sha256(&encode_logical_rows(table_name, schema, &rows)?))
}

fn validate_logical_type(data_type: &DataType) -> Result<(), CanonicalError> {
    match data_type {
        DataType::Dictionary(_, value_type) => validate_logical_type(value_type),
        DataType::Utf8
        | DataType::Int64
        | DataType::Decimal128(38, 18)
        | DataType::Date32
        | DataType::Boolean => Ok(()),
        DataType::Timestamp(TimeUnit::Nanosecond, Some(timezone)) if timezone.as_ref() == "UTC" => {
            Ok(())
        }
        other => Err(CanonicalError::new(format!(
            "unsupported logical hash type: {}",
            canonical_type(other)
        ))),
    }
}

fn encode_logical_rows(
    table_name: &str,
    schema: &Schema,
    rows: &[Vec<Cell>],
) -> Result<Vec<u8>, CanonicalError> {
    let schema_hash = decode_sha256(&schema_fingerprint(schema)?)?;
    let mut bytes = b"PRAJNA-LH\x01".to_vec();
    put_len(&mut bytes, table_name.len())?;
    bytes.extend_from_slice(table_name.as_bytes());
    bytes.extend_from_slice(&schema_hash);
    let row_count =
        u64::try_from(rows.len()).map_err(|_| CanonicalError::new("row count exceeds u64"))?;
    bytes.extend_from_slice(&row_count.to_le_bytes());
    for row in rows {
        for cell in row {
            cell.write_to(&mut bytes)?;
        }
    }
    Ok(bytes)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Cell {
    Null,
    Utf8(String),
    Int64(i64),
    Decimal128(i128),
    Timestamp(i64),
    Date32(i32),
    Boolean(bool),
}

impl Cell {
    fn write_to(&self, output: &mut Vec<u8>) -> Result<(), CanonicalError> {
        match self {
            Cell::Null => output.push(0x00),
            Cell::Utf8(value) => {
                output.push(0x01);
                put_len(output, value.len())?;
                output.extend_from_slice(value.as_bytes());
            }
            Cell::Int64(value) => {
                output.push(0x02);
                output.extend_from_slice(&value.to_le_bytes());
            }
            Cell::Decimal128(value) => {
                output.push(0x03);
                output.extend_from_slice(&value.to_le_bytes());
            }
            Cell::Timestamp(value) => {
                output.push(0x04);
                output.extend_from_slice(&value.to_le_bytes());
            }
            Cell::Date32(value) => {
                output.push(0x05);
                output.extend_from_slice(&value.to_le_bytes());
            }
            Cell::Boolean(value) => {
                output.push(0x06);
                output.push(u8::from(*value));
            }
        }
        Ok(())
    }
}

fn read_cell(array: &ArrayRef, field: &Field, row: usize) -> Result<Cell, CanonicalError> {
    if array.is_null(row) {
        return Ok(Cell::Null);
    }
    let data_type = match field.data_type() {
        DataType::Dictionary(_, value_type) => value_type.as_ref(),
        data_type => data_type,
    };
    let decoded;
    let array = if matches!(field.data_type(), DataType::Dictionary(_, _)) {
        decoded = cast(array.as_ref(), data_type)
            .map_err(|error| CanonicalError::new(format!("decode dictionary: {error}")))?;
        &decoded
    } else {
        array.as_ref()
    };
    if array.is_null(row) {
        return Ok(Cell::Null);
    }
    match data_type {
        DataType::Utf8 => Ok(Cell::Utf8(
            array
                .as_any()
                .downcast_ref::<arrow_array::StringArray>()
                .ok_or_else(|| CanonicalError::new("invalid Utf8 array"))?
                .value(row)
                .to_owned(),
        )),
        DataType::Int64 => Ok(Cell::Int64(
            array
                .as_any()
                .downcast_ref::<arrow_array::Int64Array>()
                .ok_or_else(|| CanonicalError::new("invalid Int64 array"))?
                .value(row),
        )),
        DataType::Decimal128(38, 18) => Ok(Cell::Decimal128(
            array
                .as_any()
                .downcast_ref::<arrow_array::Decimal128Array>()
                .ok_or_else(|| CanonicalError::new("invalid Decimal128 array"))?
                .value(row),
        )),
        DataType::Timestamp(TimeUnit::Nanosecond, timezone)
            if timezone.as_deref() == Some("UTC") =>
        {
            Ok(Cell::Timestamp(
                array
                    .as_any()
                    .downcast_ref::<arrow_array::TimestampNanosecondArray>()
                    .ok_or_else(|| CanonicalError::new("invalid Timestamp(ns, UTC) array"))?
                    .value(row),
            ))
        }
        DataType::Date32 => Ok(Cell::Date32(
            array
                .as_any()
                .downcast_ref::<arrow_array::Date32Array>()
                .ok_or_else(|| CanonicalError::new("invalid Date32 array"))?
                .value(row),
        )),
        DataType::Boolean => Ok(Cell::Boolean(
            array
                .as_any()
                .downcast_ref::<arrow_array::BooleanArray>()
                .ok_or_else(|| CanonicalError::new("invalid Boolean array"))?
                .value(row),
        )),
        other => Err(CanonicalError::new(format!(
            "unsupported logical hash type: {}",
            canonical_type(other)
        ))),
    }
}

fn compare_keys(left: &[Cell], right: &[Cell], key_indices: &[usize]) -> Ordering {
    for index in key_indices {
        let ordering = compare_cell(&left[*index], &right[*index]);
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    Ordering::Equal
}

fn compare_cell(left: &Cell, right: &Cell) -> Ordering {
    match (left, right) {
        (Cell::Null, Cell::Null) => Ordering::Equal,
        (Cell::Null, _) => Ordering::Less,
        (_, Cell::Null) => Ordering::Greater,
        (Cell::Utf8(left), Cell::Utf8(right)) => left.as_bytes().cmp(right.as_bytes()),
        (Cell::Int64(left), Cell::Int64(right)) => left.cmp(right),
        (Cell::Decimal128(left), Cell::Decimal128(right)) => left.cmp(right),
        (Cell::Timestamp(left), Cell::Timestamp(right)) => left.cmp(right),
        (Cell::Date32(left), Cell::Date32(right)) => left.cmp(right),
        (Cell::Boolean(left), Cell::Boolean(right)) => left.cmp(right),
        _ => Ordering::Equal,
    }
}

fn put_len(output: &mut Vec<u8>, length: usize) -> Result<(), CanonicalError> {
    let length =
        u32::try_from(length).map_err(|_| CanonicalError::new("value exceeds u32 length"))?;
    output.extend_from_slice(&length.to_le_bytes());
    Ok(())
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn decode_sha256(hex: &str) -> Result<[u8; 32], CanonicalError> {
    let mut bytes = [0; 32];
    for (index, chunk) in hex.as_bytes().chunks_exact(2).enumerate() {
        bytes[index] = u8::from_str_radix(std::str::from_utf8(chunk).expect("hex is ASCII"), 16)
            .map_err(|error| CanonicalError::new(format!("invalid schema fingerprint: {error}")))?;
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, fs, path::PathBuf, sync::Arc};

    use arrow_array::{
        ArrayRef, Date32Array, Decimal128Array, RecordBatch, StringArray, TimestampNanosecondArray,
    };
    use arrow_schema::{DataType, Field, Schema, TimeUnit};
    use serde_json::json;

    use super::{encode_logical_rows, logical_hash, read_cell, restricted_jcs, schema_fingerprint};

    #[test]
    fn crate_compiles_with_arrow_and_parquet_dependencies() {
        let schema = arrow_schema::Schema::empty();
        let _batch = arrow_array::RecordBatch::new_empty(Arc::new(schema));
        let _writer_properties = parquet::file::properties::WriterProperties::builder().build();
        let _domain_type = std::any::type_name::<prajna_domain::Price>();
    }

    fn sessions_schema() -> Schema {
        let mut metadata = HashMap::new();
        metadata.insert("prajna.table".into(), "sessions".into());
        metadata.insert("prajna.schema_version".into(), "1".into());
        metadata.insert("prajna.dsv".into(), "first".into());
        Schema::new_with_metadata(
            vec![
                Field::new("venue_id", DataType::Utf8, false),
                Field::new("session_date", DataType::Date32, false),
                Field::new(
                    "ts_open",
                    DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())),
                    false,
                ),
                Field::new(
                    "ts_close",
                    DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())),
                    false,
                ),
                Field::new("settlement", DataType::Decimal128(38, 18), true),
                Field::new("note", DataType::Utf8, true),
            ],
            metadata,
        )
    }

    fn sessions_batch(
        schema: &Schema,
        venues: Vec<Option<&str>>,
        dates: Vec<Option<i32>>,
        opens: Vec<Option<i64>>,
        closes: Vec<Option<i64>>,
        settlements: Vec<Option<i128>>,
        notes: Vec<Option<&str>>,
    ) -> RecordBatch {
        let columns: Vec<ArrayRef> = vec![
            std::sync::Arc::new(StringArray::from(venues)),
            std::sync::Arc::new(Date32Array::from(dates)),
            std::sync::Arc::new(TimestampNanosecondArray::from(opens).with_timezone("UTC")),
            std::sync::Arc::new(TimestampNanosecondArray::from(closes).with_timezone("UTC")),
            std::sync::Arc::new(
                Decimal128Array::from(settlements)
                    .with_precision_and_scale(38, 18)
                    .unwrap(),
            ),
            std::sync::Arc::new(StringArray::from(notes)),
        ];
        RecordBatch::try_new(std::sync::Arc::new(schema.clone()), columns).unwrap()
    }

    #[test]
    fn canonical_test_vector_matches_external_hex_fixture() {
        let schema = sessions_schema();
        let batch = sessions_batch(
            &schema,
            vec![Some("SYNTH"), Some("交易所😀")],
            vec![Some(20_454), Some(20_455)],
            vec![
                Some(1_767_225_000_000_000_000),
                Some(1_767_311_400_000_000_000),
            ],
            vec![
                Some(1_767_244_800_000_000_000),
                Some(1_767_331_200_000_000_000),
            ],
            vec![Some(-1_230_000_000_000_000_000), None],
            vec![None, Some("开市")],
        );
        let rows = (0..batch.num_rows())
            .map(|row_index| {
                schema
                    .fields()
                    .iter()
                    .enumerate()
                    .map(|(column_index, field)| {
                        read_cell(batch.column(column_index), field, row_index).unwrap()
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let bytes = encode_logical_rows("sessions", &schema, &rows).unwrap();
        let hash =
            logical_hash("sessions", &schema, &[batch], &["venue_id", "session_date"]).unwrap();
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/sessions-logical-hash.hex");
        let expected_bytes = fs::read_to_string(fixture).unwrap();
        let expected_hex = expected_bytes.split_whitespace().collect::<String>();
        let actual_hex = bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(actual_hex, expected_hex);
        assert_eq!(
            hash,
            "2c627a1813303394238717869776a089234b08da095a9c1bae0198ca26c2b58c"
        );
    }

    #[test]
    fn row_order_batching_and_dictionary_encoding_do_not_change_hash() {
        let schema = sessions_schema();
        let first = sessions_batch(
            &schema,
            vec![Some("交易所😀")],
            vec![Some(20_455)],
            vec![Some(1_767_311_400_000_000_000)],
            vec![Some(1_767_331_200_000_000_000)],
            vec![None],
            vec![Some("开市")],
        );
        let second = sessions_batch(
            &schema,
            vec![Some("SYNTH")],
            vec![Some(20_454)],
            vec![Some(1_767_225_000_000_000_000)],
            vec![Some(1_767_244_800_000_000_000)],
            vec![Some(-1_230_000_000_000_000_000)],
            vec![None],
        );
        let split = logical_hash(
            "sessions",
            &schema,
            &[first.clone(), second.clone()],
            &["venue_id", "session_date"],
        )
        .unwrap();
        let combined = logical_hash(
            "sessions",
            &schema,
            &[second.clone(), first.clone()],
            &["venue_id", "session_date"],
        )
        .unwrap();
        assert_eq!(split, combined);

        let plain_schema = Schema::new(vec![Field::new("key", DataType::Utf8, false)]);
        let plain_batch = RecordBatch::try_from_iter(vec![(
            "key",
            Arc::new(StringArray::from(vec!["b", "a"])) as ArrayRef,
        )])
        .unwrap();
        let dictionary_schema = Schema::new(vec![Field::new(
            "key",
            DataType::Dictionary(Box::new(DataType::Int8), Box::new(DataType::Utf8)),
            false,
        )]);
        let dictionary: arrow_array::DictionaryArray<arrow_array::types::Int8Type> =
            arrow_array::DictionaryArray::try_new(
                arrow_array::Int8Array::from(vec![Some(0), Some(1)]),
                Arc::new(StringArray::from(vec!["b", "a"])),
            )
            .unwrap();
        let dictionary_batch = RecordBatch::try_new(
            Arc::new(dictionary_schema.clone()),
            vec![Arc::new(dictionary)],
        )
        .unwrap();
        assert_eq!(
            logical_hash("t", &plain_schema, &[plain_batch], &["key"]),
            logical_hash("t", &dictionary_schema, &[dictionary_batch], &["key"])
        );
    }

    #[test]
    fn schema_dsv_is_excluded_but_field_metadata_and_types_are_included() {
        let base = sessions_schema();
        let mut changed_dsv = base.metadata().clone();
        changed_dsv.insert("prajna.dsv".into(), "second".into());
        let same = Schema::new_with_metadata(base.fields().clone(), changed_dsv);
        assert_eq!(schema_fingerprint(&base), schema_fingerprint(&same));

        let field_metadata = HashMap::from([("prajna.time_role".to_owned(), "ts_open".to_owned())]);
        let changed_field = Field::new("ts_open", base.field(2).data_type().clone(), false)
            .with_metadata(field_metadata);
        let mut fields = base.fields().iter().cloned().collect::<Vec<_>>();
        fields[2] = Arc::new(changed_field);
        assert_ne!(
            schema_fingerprint(&base),
            schema_fingerprint(&Schema::new_with_metadata(
                fields.clone(),
                base.metadata().clone(),
            ))
        );
        fields[2] = Arc::new(Field::new("ts_open", DataType::Int64, false));
        assert_ne!(
            schema_fingerprint(&base),
            schema_fingerprint(&Schema::new_with_metadata(fields, base.metadata().clone()))
        );
    }

    #[test]
    fn content_changes_null_changes_and_field_order_change_hash() {
        let schema = sessions_schema();
        let original = sessions_batch(
            &schema,
            vec![Some("SYNTH"), Some("交易所😀")],
            vec![Some(20_454), Some(20_455)],
            vec![
                Some(1_767_225_000_000_000_000),
                Some(1_767_311_400_000_000_000),
            ],
            vec![
                Some(1_767_244_800_000_000_000),
                Some(1_767_331_200_000_000_000),
            ],
            vec![Some(-1_230_000_000_000_000_000), None],
            vec![None, Some("开市")],
        );
        let changed = sessions_batch(
            &schema,
            vec![Some("SYNTH"), Some("交易所😀")],
            vec![Some(20_454), Some(20_455)],
            vec![
                Some(1_767_225_000_000_000_000),
                Some(1_767_311_400_000_000_000),
            ],
            vec![
                Some(1_767_244_800_000_000_000),
                Some(1_767_331_200_000_000_000),
            ],
            vec![Some(-1_230_000_000_000_000_000), None],
            vec![Some("复市"), Some("开市")],
        );
        assert_ne!(
            logical_hash(
                "sessions",
                &schema,
                std::slice::from_ref(&original),
                &["venue_id", "session_date"]
            ),
            logical_hash(
                "sessions",
                &schema,
                &[changed],
                &["venue_id", "session_date"]
            )
        );
        let ordered_schema = Schema::new(vec![
            Field::new("a", DataType::Utf8, false),
            Field::new("b", DataType::Int64, false),
        ]);
        let ordered_batch = RecordBatch::try_from_iter(vec![
            ("a", Arc::new(StringArray::from(vec!["x"])) as ArrayRef),
            (
                "b",
                Arc::new(arrow_array::Int64Array::from(vec![7])) as ArrayRef,
            ),
        ])
        .unwrap();
        let reversed_schema = Schema::new(vec![
            Field::new("b", DataType::Int64, false),
            Field::new("a", DataType::Utf8, false),
        ]);
        let reversed_batch = RecordBatch::try_from_iter(vec![
            (
                "b",
                Arc::new(arrow_array::Int64Array::from(vec![7])) as ArrayRef,
            ),
            ("a", Arc::new(StringArray::from(vec!["x"])) as ArrayRef),
        ])
        .unwrap();
        assert_ne!(
            logical_hash("t", &ordered_schema, &[ordered_batch], &["a"]),
            logical_hash("t", &reversed_schema, &[reversed_batch], &["a"])
        );
    }

    #[test]
    fn duplicate_keys_unsupported_types_and_bad_batch_schema_are_rejected() {
        let schema = Schema::new(vec![Field::new("key", DataType::Utf8, false)]);
        let duplicate = RecordBatch::try_from_iter(vec![(
            "key",
            Arc::new(StringArray::from(vec!["x", "x"])) as ArrayRef,
        )])
        .unwrap();
        assert!(logical_hash("t", &schema, &[duplicate], &["key"]).is_err());

        let float_schema = Schema::new(vec![
            Field::new("key", DataType::Utf8, false),
            Field::new("x", DataType::Float64, false),
        ]);
        let float_batch = RecordBatch::try_from_iter(vec![
            ("key", Arc::new(StringArray::from(vec!["a"])) as ArrayRef),
            (
                "x",
                Arc::new(arrow_array::Float64Array::from(vec![1.0])) as ArrayRef,
            ),
        ])
        .unwrap();
        assert!(logical_hash("t", &float_schema, &[float_batch], &["key"]).is_err());
        assert!(logical_hash("t", &float_schema, &[], &["key"]).is_err());
    }

    #[test]
    fn restricted_jcs_sorts_keys_by_utf16_and_rejects_floats_and_large_integers() {
        assert_eq!(
            restricted_jcs(&json!({"z": 1, "a": true})).unwrap(),
            br#"{"a":true,"z":1}"#
        );
        let utf16_keys = json!({"\u{e000}": 2, "\u{10000}": 1});
        assert_eq!(
            restricted_jcs(&utf16_keys).unwrap(),
            "{\"𐀀\":1,\"\":2}".as_bytes()
        );
        assert!(restricted_jcs(&json!(1.25)).is_err());
        assert!(restricted_jcs(&json!(9_007_199_254_740_992_u64)).is_err());
    }
}
