//! Test-only publisher: a real DSV with deliberately delayed/unknown availability.
use std::{error::Error, fs, sync::Arc};

use arrow_array::{Array, Date32Array, RecordBatch, StringArray, TimestampNanosecondArray};
use chrono::NaiveDate;
use prajna_data::{
    NormalizationIssue, NormalizationOutput, Normalizer, NormalizerRegistry, RawInput, RawStore,
    SourceKind, SourceRecordInput, publish_dataset,
};
use serde_json::Value;

struct FixtureNormalizer;

impl Normalizer for FixtureNormalizer {
    fn id(&self) -> &str {
        "python-strategy-test"
    }

    fn version(&self) -> &str {
        "1"
    }

    fn config_sha256(&self) -> String {
        // The full override content is already part of the Raw input identity.
        format!("sha256:{}", "0".repeat(64))
    }

    fn normalize(
        &self,
        inputs: &[RawInput],
    ) -> Result<NormalizationOutput, Vec<NormalizationIssue>> {
        let mut output = NormalizerRegistry::with_builtins()
            .get("synthetic-etf-daily", "3")
            .expect("registered fixture normalizer")
            .normalize(inputs)?;
        let mut adjust = || -> Result<(), Box<dyn Error>> {
            let fixture: Value = serde_json::from_slice(&inputs[0].bytes)?;
            let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).expect("epoch");
            for (name, batch) in [
                ("bars", &mut output.tables.bars),
                (
                    "execution_status",
                    output
                        .tables
                        .execution_status
                        .as_mut()
                        .ok_or("missing execution_status")?,
                ),
            ] {
                let Some(overrides) = fixture.get(format!("test_{name}_availability")) else {
                    continue;
                };
                let ids = batch
                    .column_by_name("instrument_id")
                    .ok_or("missing instrument_id")?
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .ok_or("invalid instrument_id")?;
                let dates = batch
                    .column_by_name("session_date")
                    .ok_or("missing session_date")?
                    .as_any()
                    .downcast_ref::<Date32Array>()
                    .ok_or("invalid session_date")?;
                let availability = batch
                    .column_by_name("available_at")
                    .ok_or("missing available_at")?
                    .as_any()
                    .downcast_ref::<TimestampNanosecondArray>()
                    .ok_or("invalid available_at")?;
                let mut times = availability.iter().collect::<Vec<_>>();
                for item in overrides.as_array().ok_or("overrides must be an array")? {
                    let id = item["instrument_id"]
                        .as_str()
                        .ok_or("missing override ID")?;
                    let date = NaiveDate::parse_from_str(
                        item["session_date"]
                            .as_str()
                            .ok_or("missing override date")?,
                        "%Y-%m-%d",
                    )?;
                    for (row, time) in times.iter_mut().enumerate() {
                        if ids.value(row) == id
                            && i64::from(dates.value(row)) == (date - epoch).num_days()
                        {
                            *time = if item["available_at"].is_null() {
                                None
                            } else {
                                Some(
                                    item["available_at"]
                                        .as_i64()
                                        .ok_or("invalid override time")?,
                                )
                            };
                        }
                    }
                }
                let mut columns = batch.columns().to_vec();
                let index = batch.schema().index_of("available_at")?;
                columns[index] =
                    Arc::new(TimestampNanosecondArray::from(times).with_timezone("UTC"));
                *batch = RecordBatch::try_new(batch.schema(), columns)?;
            }
            Ok(())
        };
        adjust().map_err(|error| {
            vec![NormalizationIssue {
                code: "invalid_test_fixture".into(),
                path: "$".into(),
                message: error.to_string(),
            }]
        })?;
        Ok(output)
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 2 {
        return Err("usage: strategy_fixture <lake> <fixture.json>".into());
    }
    let store = RawStore::open(&args[0])?;
    let hash = store.put(
        &fs::read(&args[1])?,
        SourceRecordInput {
            content_type: "application/json".into(),
            source_kind: SourceKind::Fixture,
            source_id: "python-strategy-test".into(),
            request: Default::default(),
            observed_at: "2026-10-13T00:00:00Z".into(),
            ingested_by: "strategy_fixture".into(),
        },
    )?;
    let mut registry = NormalizerRegistry::new();
    registry.register(FixtureNormalizer);
    let manifest = publish_dataset(
        &args[0],
        &store,
        &registry,
        "python-strategy-test",
        "1",
        &[hash],
        "2026-10-13T00:00:00Z",
    )?;
    println!("{}", manifest.dsv);
    Ok(())
}
