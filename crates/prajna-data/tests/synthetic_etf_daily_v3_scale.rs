use std::collections::BTreeSet;

use arrow_array::{Array, BooleanArray, Date32Array, Decimal128Array, RecordBatch, StringArray};
use chrono::NaiveDate;
use prajna_data::{
    NormalizerRegistry, RawStore, SourceKind, SourceRecordInput, publish_dataset, read_parquet,
};
use serde_json::{Value, json};

const FIXTURE: &[u8] =
    include_bytes!("../../../poc/poc0-benchmark/fixtures/b2-s2-scale-64x252-v2.json");

fn source_record() -> SourceRecordInput {
    SourceRecordInput {
        content_type: "application/json".into(),
        source_kind: SourceKind::Fixture,
        source_id: "poc0-b3-s2-scale-64x252-v2".into(),
        request: json!({}),
        observed_at: "synthetic".into(),
        ingested_by: "synthetic-etf-daily-v3-scale-test".into(),
    }
}

fn read_table(root: &std::path::Path, dsv: &str, table: &str) -> Vec<RecordBatch> {
    let hash = dsv.strip_prefix("dsv:sha256:").unwrap();
    read_parquet(
        root.join("normalized")
            .join(table)
            .join(hash)
            .join("part-00000.parquet"),
        table,
        "1",
    )
    .unwrap()
}

fn row_count(batches: &[RecordBatch]) -> usize {
    batches.iter().map(RecordBatch::num_rows).sum()
}

fn date32(value: &str) -> i32 {
    let date = NaiveDate::parse_from_str(value, "%Y-%m-%d").unwrap();
    i32::try_from((date - NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()).num_days()).unwrap()
}

fn published_open(batches: &[RecordBatch], symbol: &str, date: &str) -> i128 {
    let instrument_id = format!("{symbol}.SYNTH");
    let session_date = date32(date);
    for batch in batches {
        let ids = batch
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        let dates = batch
            .column(2)
            .as_any()
            .downcast_ref::<Date32Array>()
            .unwrap();
        let opens = batch
            .column(5)
            .as_any()
            .downcast_ref::<Decimal128Array>()
            .unwrap();
        for row in 0..batch.num_rows() {
            if ids.value(row) == instrument_id && dates.value(row) == session_date {
                return opens.value(row);
            }
        }
    }
    panic!("published bar not found for {instrument_id} on {date}");
}

fn formula_open_mantissa(instrument_index: usize, day: usize, instrument: &Value) -> i128 {
    let closes = instrument["closes"].as_array().unwrap();
    let close = if day == 0 {
        closes[day].as_f64().unwrap()
    } else {
        let previous_close = closes[day - 1].as_f64().unwrap();
        let adjustment = ((instrument_index * 7 + day * 3) % 11) as i32 - 5;
        (previous_close * (1.0 + f64::from(adjustment) * 0.001) * 100.0).round() / 100.0
    };
    (close * 100.0).round() as i128 * 10_i128.pow(16)
}

#[test]
fn v2_scale_fixture_publishes_through_normalizer_v3() {
    let fixture: Value = serde_json::from_slice(FIXTURE).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let store = RawStore::open(root).unwrap();
    let raw_hash = store.put(FIXTURE, source_record()).unwrap();
    let manifest = publish_dataset(
        root,
        &store,
        &NormalizerRegistry::with_builtins(),
        "synthetic-etf-daily",
        "3",
        &[raw_hash],
        "2026-10-02T00:00:00Z",
    )
    .unwrap();

    let tables = manifest
        .core
        .tables
        .iter()
        .map(|table| table.table.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        tables,
        BTreeSet::from(["bars", "execution_status", "instruments", "sessions"])
    );

    let instruments = read_table(root, &manifest.dsv, "instruments");
    let sessions = read_table(root, &manifest.dsv, "sessions");
    let bars = read_table(root, &manifest.dsv, "bars");
    let execution_status = read_table(root, &manifest.dsv, "execution_status");
    assert_eq!(row_count(&instruments), 64);
    assert_eq!(row_count(&sessions), 252);
    assert_eq!(row_count(&bars), 64 * 252 - 1);
    assert_eq!(row_count(&execution_status), 64 * 252);

    let missing_bars = fixture["missing_bars"].as_array().unwrap();
    assert_eq!(missing_bars.len(), 1);
    for missing in missing_bars {
        let symbol = missing["symbol"].as_str().unwrap();
        let date = date32(missing["date"].as_str().unwrap());
        assert!(bars.iter().all(|batch| {
            let ids = batch
                .column(0)
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let dates = batch
                .column(2)
                .as_any()
                .downcast_ref::<Date32Array>()
                .unwrap();
            (0..batch.num_rows())
                .all(|row| ids.value(row) != format!("{symbol}.SYNTH") || dates.value(row) != date)
        }));
    }

    let overrides = fixture["execution_status_overrides"].as_array().unwrap();
    assert_eq!(overrides.len(), 1);
    for status_override in overrides {
        let symbol = status_override["symbol"].as_str().unwrap();
        let date = date32(status_override["date"].as_str().unwrap());
        let mut found = false;
        for batch in &execution_status {
            let ids = batch
                .column(0)
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let dates = batch
                .column(1)
                .as_any()
                .downcast_ref::<Date32Array>()
                .unwrap();
            let statuses = batch
                .column(2)
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let tradable = batch
                .column(3)
                .as_any()
                .downcast_ref::<BooleanArray>()
                .unwrap();
            for row in 0..batch.num_rows() {
                if ids.value(row) == format!("{symbol}.SYNTH") && dates.value(row) == date {
                    found = true;
                    assert_eq!(statuses.value(row), "HALTED");
                    assert!(!tradable.value(row));
                }
            }
        }
        assert!(found, "execution status missing for {symbol} on {date}");
    }

    let calendar = fixture["calendar"].as_array().unwrap();
    let fixture_instruments = fixture["instruments"].as_array().unwrap();
    let samples = [(0, 0), (7, 10), (63, 251)];
    for (instrument_index, day) in samples {
        let instrument = &fixture_instruments[instrument_index];
        let symbol = instrument["symbol"].as_str().unwrap();
        let date = calendar[day].as_str().unwrap();
        assert_eq!(
            published_open(&bars, symbol, date),
            formula_open_mantissa(instrument_index, day, instrument),
            "open formula mismatch for {symbol} on {date}",
        );
    }

    let mut distinct_opens = BTreeSet::new();
    for batch in &bars {
        let opens = batch
            .column(5)
            .as_any()
            .downcast_ref::<Decimal128Array>()
            .unwrap();
        for row in 0..opens.len() {
            let open = opens.value(row);
            assert!(open > 0);
            assert_eq!(open % 10_i128.pow(16), 0);
            distinct_opens.insert(open);
        }
    }
    assert!(distinct_opens.len() >= 2);
}
