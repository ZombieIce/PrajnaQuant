//! Builds the local Tencent fallback plan from a published, complete MVP-6 v4 DSV.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fs,
    io::{self, Cursor, Read},
    path::{Path, PathBuf},
};

use chrono::NaiveDate;
use prajna_data::{
    BarSourceRow, Manifest, RawStore, TdxCodeTableRow, bar_sources_from_record_batch, read_parquet,
    sessions_from_record_batch, tdx_code_table_from_record_batch,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use zip::ZipArchive;

const SPEC_START: &str = "2022-10-01";
const SPEC_END: &str = "2026-09-30";

type AppResult<T> = Result<T, Box<dyn Error>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TdxObservation {
    Priced,
    NoTrade,
    Missing,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("MVP-6 Tencent fallback plan unresolved: {error}");
        std::process::exit(1);
    }
}

fn run() -> AppResult<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let is_test_root = args.first().is_some_and(|arg| arg == "--test-root");
    let root = match (is_test_root, args.as_slice()) {
        (true, [_, path]) => PathBuf::from(path),
        (false, []) => default_root(),
        (false, [path]) => PathBuf::from(path),
        _ => {
            return Err(invalid(
                "usage: prajna-mvp6-plan-tencent-fallback [MVP6_ROOT | --test-root TEMP_DIR]",
            )
            .into());
        }
    };
    let canonical_root = root.canonicalize()?;
    let temp_root = std::env::temp_dir().canonicalize()?;
    if (!is_test_root && (canonical_root != root || root != default_root()))
        || (is_test_root && !canonical_root.starts_with(&temp_root))
    {
        return Err(invalid("MVP-6 plan root must be ~/PrajnaData/mvp-6; --test-root is limited to the system temp directory").into());
    }

    let coverage: Value =
        serde_json::from_slice(&fs::read(root.join("acceptance/mvp6-coverage.json"))?)?;
    if !spec_window_complete(&coverage) {
        return Err(invalid("Tencent fallback plan requires a published v4 DSV with the complete MVP-6 specification window").into());
    }
    let dsv = coverage["dsv"]
        .as_str()
        .ok_or_else(|| invalid("local coverage report has no published DSV"))?;
    if coverage["dataset_published"] != true {
        return Err(invalid("local v4 DSV is not published").into());
    }
    let manifest: Manifest = serde_json::from_slice(&fs::read(manifest_path(&root, dsv)?)?)?;
    manifest.validate()?;
    if manifest.dsv != dsv
        || manifest.core.normalizer.id != "szse-etf-frozen"
        || manifest.core.normalizer.version != "4"
    {
        return Err(invalid(
            "Tencent fallback plan requires a published szse-etf-frozen v4 Dataset",
        )
        .into());
    }
    for table in ["sessions", "bars", "bar_sources", "tdx_code_table"] {
        if !manifest.core.tables.iter().any(|item| item.table == table) {
            return Err(invalid("published v4 manifest is missing a required table").into());
        }
    }

    let session_batches = read_table(&root, &manifest, dsv, "sessions", "1")?;
    let sessions = session_batches
        .iter()
        .map(sessions_from_record_batch)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let session_dates = sessions
        .iter()
        .map(|session| session.session_date())
        .collect::<BTreeSet<_>>();
    let code_batches = read_table(&root, &manifest, dsv, "tdx_code_table", "1")?;
    let code_rows = code_batches
        .iter()
        .map(tdx_code_table_from_record_batch)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let manifest_inputs = manifest
        .core
        .inputs
        .iter()
        .map(|input| input.raw_sha256.as_str())
        .collect::<BTreeSet<_>>();
    if code_rows
        .iter()
        .any(|row| !manifest_inputs.contains(raw_id(&row.source_raw_sha256).as_str()))
    {
        return Err(
            invalid("v4 TDX code table references a Raw object outside its manifest").into(),
        );
    }
    let source_batches = read_table(&root, &manifest, dsv, "bar_sources", "1")?;
    let bar_sources = source_batches
        .iter()
        .map(bar_sources_from_record_batch)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();

    let (plan, report) = build_plan(&root, dsv, &session_dates, &code_rows, &bar_sources)?;
    let acceptance = root.join("acceptance");
    fs::create_dir_all(&acceptance)?;
    write_deterministic_json(&acceptance.join("tencent-fallback-plan.json"), &plan)?;
    write_deterministic_json(
        &acceptance.join("tencent-fallback-plan-report.json"),
        &report,
    )?;
    Ok(())
}

fn build_plan(
    root: &Path,
    dsv: &str,
    session_dates: &BTreeSet<NaiveDate>,
    code_rows: &[TdxCodeTableRow],
    bar_sources: &[BarSourceRow],
) -> AppResult<(Vec<Value>, Value)> {
    if code_rows.len() != session_dates.len() {
        return Err(invalid("v4 TDX code table does not cover every published session").into());
    }
    let rows = code_rows
        .iter()
        .map(|row| (row.session_date, row))
        .collect::<BTreeMap<_, _>>();
    if rows.len() != code_rows.len()
        || rows.keys().copied().collect::<BTreeSet<_>>() != *session_dates
    {
        return Err(invalid(
            "v4 TDX code table sessions do not match the published session window",
        )
        .into());
    }
    let tdx_priced = bar_sources
        .iter()
        .filter(|source| source.source == "tdx")
        .map(|source| {
            (
                source.session_date,
                source.instrument_id.symbol().to_owned(),
            )
        })
        .collect::<BTreeSet<_>>();
    let raw_store = RawStore::open(root)?;
    let mut observations = BTreeMap::<NaiveDate, BTreeMap<String, TdxObservation>>::new();
    for row in code_rows {
        let bytes = raw_store.get(&raw_id(&row.source_raw_sha256))?;
        observations.insert(
            row.session_date,
            decode_tdx_observations(&bytes, row.session_date)?,
        );
    }

    let all_symbols = code_rows
        .iter()
        .flat_map(|row| row.native_symbols.iter().cloned())
        .filter(|symbol| {
            symbol
                .parse::<u32>()
                .is_ok_and(|code| (158000..=159999).contains(&code))
        })
        .collect::<BTreeSet<_>>();
    let ordered_sessions = session_dates.iter().copied().collect::<Vec<_>>();
    let mut plan_cells = BTreeSet::<(NaiveDate, String)>::new();
    let mut outside_lifecycle_gaps = 0_u64;
    let mut code_table_absences = 0_u64;
    let mut no_trade_excluded = 0_u64;
    let mut priced_excluded = 0_u64;
    let mut lifecycle_count = 0_u64;

    for symbol in all_symbols {
        let present = ordered_sessions
            .iter()
            .map(|date| rows[date].native_symbols.binary_search(&symbol).is_ok())
            .collect::<Vec<_>>();
        let mut start = 0;
        while start < present.len() {
            if !present[start] {
                start += 1;
                continue;
            }
            let end = (start + 1..present.len())
                .find(|index| !present[*index])
                .unwrap_or(present.len());
            lifecycle_count += 1;
            let priced = (start..end)
                .filter(|index| tdx_priced.contains(&(ordered_sessions[*index], symbol.clone())))
                .collect::<Vec<_>>();
            if let (Some(first), Some(last)) = (priced.first(), priced.last()) {
                for (index, date) in ordered_sessions.iter().enumerate().take(end).skip(start) {
                    let date = *date;
                    let observation = observations[&date]
                        .get(&symbol)
                        .copied()
                        .unwrap_or(TdxObservation::Missing);
                    let has_priced_bar = tdx_priced.contains(&(date, symbol.clone()));
                    match observation {
                        TdxObservation::NoTrade if has_priced_bar => return Err(invalid("TDX no-trade observation conflicts with a published TDX Bar").into()),
                        TdxObservation::NoTrade => no_trade_excluded += 1,
                        TdxObservation::Priced if has_priced_bar => priced_excluded += 1,
                        TdxObservation::Priced => return Err(invalid("TDX package contains a priced observation absent from the published v4 source table").into()),
                        TdxObservation::Missing if has_priced_bar => return Err(invalid("published TDX Bar is missing from its source Raw package").into()),
                        TdxObservation::Missing if index > *first && index < *last => {
                            plan_cells.insert((date, symbol.clone()));
                        }
                        TdxObservation::Missing => outside_lifecycle_gaps += 1,
                    }
                }
            } else {
                outside_lifecycle_gaps += (start..end)
                    .filter(|index| !observations[&ordered_sessions[*index]].contains_key(&symbol))
                    .count() as u64;
            }
            start = end;
        }
        code_table_absences += present.iter().filter(|is_present| !**is_present).count() as u64;
    }

    let plan = plan_cells
        .into_iter()
        .map(|(date, symbol)| {
            json!({
                "symbol": symbol,
                "session": date.to_string(),
                "tdx_observation": "missing"
            })
        })
        .collect::<Vec<_>>();
    let report = json!({
        "dataset_version": "szse-etf-frozen@4",
        "dsv": dsv,
        "window": {"start": SPEC_START, "end": SPEC_END},
        "plan_cells": plan.len(),
        "code_table_absence_cells": code_table_absences,
        "outside_lifecycle_missing_observations": outside_lifecycle_gaps,
        "keyed_no_trade_excluded": no_trade_excluded,
        "priced_tdx_excluded": priced_excluded,
        "code_table_lifecycle_intervals": lifecycle_count,
    });
    Ok((plan, report))
}

fn decode_tdx_observations(
    bytes: &[u8],
    session: NaiveDate,
) -> AppResult<BTreeMap<String, TdxObservation>> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))?;
    let day_key = session.format("%y%m%d").to_string();
    let cod_name = format!("sz{day_key}.cod");
    let md1_name = format!("sz{day_key}.md1");
    let mut cod = Vec::new();
    archive.by_name(&cod_name)?.read_to_end(&mut cod)?;
    let mut md1 = Vec::new();
    archive.by_name(&md1_name)?.read_to_end(&mut md1)?;
    if cod.len() % 150 != 0 || md1.len() % 512 != 0 || cod.len() / 150 != md1.len() / 512 {
        return Err(invalid("TDX package code table and price blocks are not aligned").into());
    }
    let mut observations = BTreeMap::new();
    for record in cod.chunks_exact(150) {
        let code = &record[..6];
        let Some(first) = code.iter().position(|byte| *byte != 0 && *byte != b' ') else {
            return Err(invalid("TDX package contains an empty code").into());
        };
        let last = code
            .iter()
            .rposition(|byte| *byte != 0 && *byte != b' ')
            .expect("non-empty code has a last non-padding byte");
        let symbol = std::str::from_utf8(&code[first..=last])?;
        if symbol.len() != 6 || !symbol.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(invalid("TDX package contains an invalid six-digit code").into());
        }
        let sequence = usize::from(u16::from_le_bytes([record[32], record[33]]));
        let block = md1
            .get(sequence * 512..(sequence + 1) * 512)
            .ok_or_else(|| invalid("TDX code points outside its price blocks"))?;
        let read = |offset: usize| {
            f64::from_le_bytes(block[offset..offset + 8].try_into().expect("8-byte field"))
        };
        let [open, high, low, close] = [12, 20, 28, 36].map(read);
        let volume = u64::from_le_bytes(block[56..64].try_into().expect("8-byte volume"));
        let observation =
            if close <= 0.0 || [open, high, low].iter().all(|value| *value == 0.0) || volume == 0 {
                TdxObservation::NoTrade
            } else {
                TdxObservation::Priced
            };
        if observations
            .insert(symbol.to_owned(), observation)
            .is_some()
        {
            return Err(invalid("TDX package contains a duplicate code").into());
        }
    }
    Ok(observations)
}

fn spec_window_complete(coverage: &Value) -> bool {
    coverage["spec_window_complete"] == true
        && coverage["window"]["start"] == SPEC_START
        && coverage["window"]["end"] == SPEC_END
        && coverage["dataset_published"] == true
}

fn read_table(
    root: &Path,
    manifest: &Manifest,
    dsv: &str,
    table: &str,
    version: &str,
) -> AppResult<Vec<arrow_array::RecordBatch>> {
    let hex = dsv
        .strip_prefix("dsv:sha256:")
        .ok_or_else(|| invalid("invalid DSV"))?;
    let relative_path = format!("normalized/{table}/{hex}/part-00000.parquet");
    let file = manifest
        .provenance
        .files
        .iter()
        .find(|file| file.path == relative_path)
        .ok_or_else(|| invalid("published manifest is missing a required Parquet file"))?;
    let path = root.join(&file.path);
    let bytes = fs::read(&path)?;
    if bytes.len() as u64 != file.byte_len
        || file.sha256 != format!("sha256:{:x}", Sha256::digest(&bytes))
    {
        return Err(invalid("published Parquet file does not match its manifest hash").into());
    }
    Ok(read_parquet(path, table, version)?)
}

fn manifest_path(root: &Path, dsv: &str) -> io::Result<PathBuf> {
    let hex = dsv
        .strip_prefix("dsv:sha256:")
        .ok_or_else(|| invalid("invalid DSV"))?;
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid("invalid DSV"));
    }
    Ok(root.join("manifests").join(format!("{hex}.json")))
}

fn raw_id(value: &str) -> String {
    if value.starts_with("sha256:") {
        value.to_owned()
    } else {
        format!("sha256:{value}")
    }
}

fn write_deterministic_json(path: &Path, value: &impl serde::Serialize) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    bytes.push(b'\n');
    fs::write(path, bytes)
}

fn default_root() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("PrajnaData/mvp-6")
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use chrono::NaiveDate;
    use prajna_data::{BarSourceRow, RawStore, SourceKind, SourceRecordInput, TdxCodeTableRow};
    use prajna_domain::{InstrumentId, VenueId};
    use serde_json::json;
    use zip::{ZipWriter, write::SimpleFileOptions};

    use super::{build_plan, spec_window_complete};

    #[test]
    fn requires_a_published_complete_spec_window() {
        assert!(spec_window_complete(&json!({
            "dataset_published": true,
            "spec_window_complete": true,
            "window": {"start": "2022-10-01", "end": "2026-09-30"}
        })));
        for coverage in [
            json!({"dataset_published": false, "spec_window_complete": true, "window": {"start": "2022-10-01", "end": "2026-09-30"}}),
            json!({"dataset_published": true, "spec_window_complete": false, "window": {"start": "2022-10-01", "end": "2026-09-30"}}),
            json!({"dataset_published": true, "spec_window_complete": true, "window": {"start": "2022-10-02", "end": "2026-09-30"}}),
        ] {
            assert!(!spec_window_complete(&coverage));
        }
    }

    #[test]
    fn plans_only_interior_missing_cells_and_splits_code_table_lifecycles() {
        let root = tempfile::tempdir().unwrap();
        let raw_store = RawStore::open(root.path()).unwrap();
        let dates = (1..=5)
            .map(|day| NaiveDate::from_ymd_opt(2024, 1, day).unwrap())
            .collect::<Vec<_>>();
        let symbols = ["158001", "159001", "159002"];
        let mut code_rows = Vec::new();
        let mut tdx_sources = Vec::new();
        for (index, date) in dates.iter().enumerate() {
            let row_symbols = symbols
                .iter()
                .copied()
                .filter(|symbol| *symbol != "159002" || index != 2)
                .collect::<Vec<_>>();
            let mut records = Vec::new();
            for symbol in &row_symbols {
                let priced = match (*symbol, index) {
                    ("158001", 0 | 4) | ("159001", 1 | 3) | ("159002", 0 | 1 | 3 | 4) => Some(true),
                    ("158001", 1 | 3) => Some(false),
                    _ => None,
                };
                if let Some(priced) = priced {
                    records.push((*symbol, priced));
                    if priced {
                        tdx_sources.push(source_row(symbol, *date));
                    }
                }
            }
            let bytes = tdx_package(*date, &records);
            let hash = raw_store
                .put(
                    &bytes,
                    SourceRecordInput {
                        content_type: "application/zip".into(),
                        source_kind: SourceKind::Fixture,
                        source_id: "synthetic-tdx.zip".into(),
                        request: json!({}),
                        observed_at: "synthetic".into(),
                        ingested_by: "prajna-data-tests".into(),
                    },
                )
                .unwrap();
            code_rows.push(TdxCodeTableRow {
                session_date: *date,
                native_symbols: row_symbols.into_iter().map(str::to_owned).collect(),
                code_ranges: vec![(158000, 159999)],
                source_raw_sha256: hash,
            });
        }

        let (plan, report) = build_plan(
            root.path(),
            "dsv:sha256:0000000000000000000000000000000000000000000000000000000000000000",
            &dates.iter().copied().collect(),
            &code_rows,
            &tdx_sources,
        )
        .unwrap();
        assert_eq!(
            plan,
            vec![
                json!({"symbol": "158001", "session": "2024-01-03", "tdx_observation": "missing"}),
                json!({"symbol": "159001", "session": "2024-01-03", "tdx_observation": "missing"}),
            ]
        );
        assert_eq!(report["keyed_no_trade_excluded"], 2);
        assert_eq!(report["outside_lifecycle_missing_observations"], 2);
        assert_eq!(report["code_table_absence_cells"], 1);
        assert_eq!(report["code_table_lifecycle_intervals"], 4);
        let first = serde_json::to_vec(&plan).unwrap();
        let second = serde_json::to_vec(&plan).unwrap();
        assert_eq!(first, second);
        for cell in plan {
            assert_eq!(
                cell.as_object()
                    .unwrap()
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>(),
                vec!["session", "symbol", "tdx_observation"]
            );
            assert_eq!(cell["tdx_observation"], "missing");
        }
    }

    fn source_row(symbol: &str, session_date: NaiveDate) -> BarSourceRow {
        BarSourceRow {
            instrument_id: InstrumentId::new(symbol, VenueId::new("SZSE").unwrap()).unwrap(),
            session_date,
            source: "tdx".into(),
            source_raw_sha256: "synthetic".into(),
            reference_price: None,
        }
    }

    fn tdx_package(session: NaiveDate, records: &[(&str, bool)]) -> Vec<u8> {
        let day_key = session.format("%y%m%d");
        let mut cod = Vec::new();
        let mut md1 = Vec::new();
        for (sequence, (symbol, priced)) in records.iter().enumerate() {
            let mut record = [0_u8; 150];
            record[..6].copy_from_slice(symbol.as_bytes());
            record[32..34].copy_from_slice(&(sequence as u16).to_le_bytes());
            cod.extend_from_slice(&record);
            let mut block = [0_u8; 512];
            if *priced {
                for (offset, value) in [(12, 1.0_f64), (20, 1.1_f64), (28, 0.9_f64), (36, 1.0_f64)]
                {
                    block[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
                }
                block[56..64].copy_from_slice(&100_u64.to_le_bytes());
            }
            md1.extend_from_slice(&block);
        }
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default();
        writer
            .start_file(format!("sz{day_key}.cod"), options)
            .unwrap();
        writer.write_all(&cod).unwrap();
        writer
            .start_file(format!("sz{day_key}.md1"), options)
            .unwrap();
        writer.write_all(&md1).unwrap();
        writer.finish().unwrap().into_inner()
    }
}
