//! POC-0 Parquet transport. Row groups are symbol/date blocks so the reader can
//! skip irrelevant blocks before decoding and project only B1 input columns.
use super::*;
use polars_io::parquet::{
    read::ParquetReader,
    write::{ParquetCompression, ParquetWriter},
};
use polars_io::prelude::{ParallelStrategy, SerReader};
use std::{fs::File, io::Read};

const FORMAT: &str = "poc0-b1-parquet.v1";
const GROUP_DAYS: usize = 5;
const FILLER_GROUP_ROWS: usize = 4096;
const COLUMNS: [&str; 7] = [
    "date",
    "symbol",
    "close",
    "unused_open",
    "unused_high",
    "unused_low",
    "unused_volume",
];

#[derive(Serialize, Deserialize)]
struct Group {
    symbol: String,
    first: NaiveDate,
    last: NaiveDate,
    offset: usize,
    rows: usize,
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    format: String,
    dataset_content_sha256: String,
    fixture_content_sha256: String,
    filler_rows: usize,
    parquet_sha256: String,
    parquet_bytes: u64,
    columns: Vec<String>,
    groups: Vec<Group>,
}

fn file_sha256(path: &Path) -> Result<String> {
    let mut input = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 65536];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn frame(bars: &[&Bar]) -> Result<DataFrame> {
    let dates = bars
        .iter()
        .map(|b| b.trade_date.to_string())
        .collect::<Vec<_>>();
    let symbols = bars.iter().map(|b| b.symbol.as_str()).collect::<Vec<_>>();
    let closes = bars.iter().map(|b| b.close).collect::<Vec<_>>();
    Ok(DataFrame::new(
        bars.len(),
        vec![
            Series::new("date".into(), dates).into(),
            Series::new("symbol".into(), symbols).into(),
            Series::new("close".into(), closes).into(),
            Series::new(
                "unused_open".into(),
                bars.iter().map(|b| b.open).collect::<Vec<_>>(),
            )
            .into(),
            Series::new(
                "unused_high".into(),
                bars.iter().map(|b| b.high).collect::<Vec<_>>(),
            )
            .into(),
            Series::new(
                "unused_low".into(),
                bars.iter().map(|b| b.low).collect::<Vec<_>>(),
            )
            .into(),
            Series::new(
                "unused_volume".into(),
                bars.iter().map(|b| b.volume).collect::<Vec<_>>(),
            )
            .into(),
        ],
    )?)
}

fn write_fixture(
    prepared: &PreparedDataset,
    path: &Path,
    filler_rows: usize,
) -> Result<(Manifest, u128)> {
    let estimated_bytes = (filler_rows as u64)
        .checked_mul(160)
        .and_then(|v| v.checked_add(4 * 1024 * 1024))
        .context("Parquet size estimate overflow")?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let free_bytes = fs2::available_space(parent)?;
    ensure!(
        free_bytes >= 10 * 1024_u64.pow(3) + estimated_bytes,
        "space gate: free={free_bytes} estimated_output={estimated_bytes} reserve=10737418240; reduce --filler-rows or free space"
    );
    let started = Instant::now();
    let mut groups = Vec::new();
    let mut offset = 0;
    let mut writer = None;
    let mut symbols = prepared
        .spec
        .instruments
        .iter()
        .map(|i| i.symbol.as_str())
        .collect::<Vec<_>>();
    symbols.sort_unstable();
    for symbol in symbols {
        for days in prepared.spec.calendar.chunks(GROUP_DAYS) {
            let bars = prepared
                .bars
                .iter()
                .filter(|bar| bar.symbol == symbol && days.contains(&bar.trade_date))
                .collect::<Vec<_>>();
            if bars.is_empty() {
                continue;
            }
            let batch = frame(&bars)?;
            if writer.is_none() {
                writer = Some(
                    ParquetWriter::new(File::create(path)?)
                        .with_compression(ParquetCompression::Uncompressed)
                        .set_parallel(false)
                        .batched(batch.schema())?,
                );
            }
            writer
                .as_mut()
                .expect("writer initialized")
                .write_batch(&batch)?;
            groups.push(Group {
                symbol: symbol.to_owned(),
                first: *days.first().expect("nonempty"),
                last: *days.last().expect("nonempty"),
                offset,
                rows: bars.len(),
            });
            offset += bars.len();
        }
    }
    let mut remaining = filler_rows;
    let filler_date = prepared.spec.calendar[0];
    while remaining > 0 {
        let count = remaining.min(FILLER_GROUP_ROWS);
        let filler = (0..count)
            .map(|i| {
                let ordinal = (filler_rows - remaining + i) as u64;
                let mut hash = ordinal.wrapping_add(0x9e3779b97f4a7c15);
                hash = (hash ^ (hash >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
                hash = (hash ^ (hash >> 27)).wrapping_mul(0x94d049bb133111eb);
                let random = (hash ^ (hash >> 31)) as f64 / u64::MAX as f64;
                let close = 100.0 + random;
                Bar {
                    symbol: "__FILLER__".into(),
                    name: "__FILLER__".into(),
                    trade_date: filler_date,
                    open: 90.0 + random * 10.0,
                    high: 110.0 + random * 10.0,
                    low: 80.0 + random * 10.0,
                    close,
                    volume: random * 1_000_000.0,
                    amount: None,
                }
            })
            .collect::<Vec<_>>();
        let refs = filler.iter().collect::<Vec<_>>();
        writer
            .as_mut()
            .expect("fixture writer initialized")
            .write_batch(&frame(&refs)?)?;
        groups.push(Group {
            symbol: "__FILLER__".into(),
            first: filler_date,
            last: filler_date,
            offset,
            rows: count,
        });
        offset += count;
        remaining -= count;
    }
    ensure!(offset > 0, "empty Parquet dataset");
    writer.expect("nonempty writer").finish()?;
    let manifest = Manifest {
        format: FORMAT.into(),
        dataset_content_sha256: sha256(
            format!("{}:{filler_rows}:{FORMAT}", prepared.content_sha256).as_bytes(),
        ),
        fixture_content_sha256: prepared.content_sha256.clone(),
        filler_rows,
        parquet_sha256: file_sha256(path)?,
        parquet_bytes: fs::metadata(path)?.len(),
        columns: COLUMNS.iter().map(|s| (*s).into()).collect(),
        groups,
    };
    let manifest_path = path.with_extension("manifest.json");
    fs::write(manifest_path, serde_json::to_vec_pretty(&manifest)?)?;
    Ok((manifest, started.elapsed().as_nanos()))
}

fn read_selected(
    path: &Path,
    manifest: &Manifest,
    symbols: &[String],
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
) -> Result<(Vec<Bar>, usize, u128, u128)> {
    let mut bars = Vec::new();
    let mut scanned_groups = 0;
    let mut decode_ns = 0;
    let mut conversion_ns = 0;
    for group in &manifest.groups {
        if !symbols.is_empty() && !symbols.contains(&group.symbol) {
            continue;
        }
        if start.is_some_and(|date| group.last < date) || end.is_some_and(|date| group.first > date)
        {
            continue;
        }
        let started = Instant::now();
        let df = ParquetReader::new(File::open(path)?)
            .with_slice(Some((group.offset, group.rows)))
            .with_columns(Some(vec!["date".into(), "symbol".into(), "close".into()]))
            .set_low_memory(true)
            .read_parallel(ParallelStrategy::RowGroups)
            .finish()?;
        decode_ns += started.elapsed().as_nanos();
        scanned_groups += 1;
        let started = Instant::now();
        let dates = df.column("date")?.str()?;
        let names = df.column("symbol")?.str()?;
        let closes = df.column("close")?.f64()?;
        for i in 0..df.height() {
            let date = NaiveDate::parse_from_str(dates.get(i).context("null date")?, "%Y-%m-%d")?;
            if start.is_some_and(|first| date < first) || end.is_some_and(|last| date > last) {
                continue;
            }
            let symbol = names.get(i).context("null symbol")?;
            ensure!(symbol == group.symbol, "row-group symbol mismatch");
            let close = closes.get(i).context("null close")?;
            ensure!(close.is_finite() && close > 0.0, "invalid close");
            bars.push(Bar {
                symbol: symbol.into(),
                name: symbol.into(),
                trade_date: date,
                open: close,
                high: close,
                low: close,
                close,
                volume: 0.0,
                amount: None,
            });
        }
        conversion_ns += started.elapsed().as_nanos();
    }
    Ok((bars, scanned_groups, decode_ns, conversion_ns))
}

pub struct ParquetOptions<'a> {
    pub symbols: &'a [String],
    pub start: Option<NaiveDate>,
    pub end: Option<NaiveDate>,
    pub reuse: bool,
    pub filler_rows: usize,
}

pub fn run_parquet(
    dataset_path: &Path,
    expected_path: &Path,
    parquet_path: &Path,
    options: ParquetOptions<'_>,
) -> Result<Value> {
    let ParquetOptions {
        symbols,
        start,
        end,
        reuse,
        filler_rows,
    } = options;
    ensure!(
        start.zip(end).is_none_or(|(a, b)| a <= b),
        "start must be <= end"
    );
    let total_started = Instant::now();
    let golden = run(dataset_path, expected_path, "soa")?;
    ensure!(
        golden.correctness_status() == "passed",
        "fixed fixture correctness gate failed"
    );
    let spec: DatasetInput = serde_json::from_slice(&fs::read(dataset_path)?)?;
    let prepared = prepare_dataset(spec)?;
    for symbol in symbols {
        ensure!(
            prepared
                .spec
                .instruments
                .iter()
                .any(|i| &i.symbol == symbol),
            "unknown symbol {symbol}"
        );
    }
    let (manifest, generation_ns) = if reuse {
        let bytes = fs::read(parquet_path.with_extension("manifest.json"))?;
        (serde_json::from_slice::<Manifest>(&bytes)?, 0)
    } else {
        match write_fixture(&prepared, parquet_path, filler_rows) {
            Ok(result) => result,
            Err(error) if error.to_string().starts_with("space gate:") => {
                return Ok(serde_json::json!({"format": FORMAT, "status": "unresolved",
                    "reason": error.to_string(), "filler_rows": filler_rows,
                    "required_reserve_bytes": 10 * 1024_u64.pow(3)}));
            }
            Err(error) => return Err(error),
        }
    };
    ensure!(manifest.format == FORMAT, "unexpected Parquet format");
    ensure!(
        manifest.fixture_content_sha256 == prepared.content_sha256,
        "Parquet dataset identity mismatch"
    );
    ensure!(
        manifest.parquet_bytes == fs::metadata(parquet_path)?.len(),
        "Parquet file size mismatch"
    );
    ensure!(
        manifest.parquet_sha256 == file_sha256(parquet_path)?,
        "Parquet checksum mismatch"
    );
    ensure!(
        manifest.filler_rows == 0 || !symbols.is_empty(),
        "large source requires explicit --symbol filter"
    );
    let read_end = if let Some(last) = end {
        let index = prepared
            .spec
            .calendar
            .iter()
            .rposition(|date| *date <= last)
            .context("end precedes calendar")?;
        prepared
            .spec
            .calendar
            .get(index + 1)
            .copied()
            .or(Some(last))
    } else {
        None
    };
    let (bars, scanned_groups, scan_decode_ns, scan_conversion_ns) =
        read_selected(parquet_path, &manifest, symbols, None, read_end)?;
    ensure!(!bars.is_empty(), "selection contains no bars");
    let selected_days = prepared
        .spec
        .calendar
        .iter()
        .copied()
        .filter(|date| read_end.is_none_or(|b| *date <= b))
        .collect::<Vec<_>>();
    ensure!(
        !selected_days.is_empty(),
        "selection contains no calendar dates"
    );
    let selected_symbols = prepared
        .spec
        .instruments
        .iter()
        .filter(|i| symbols.is_empty() || symbols.contains(&i.symbol))
        .map(|i| i.symbol.clone())
        .collect::<Vec<_>>();
    let mut selected = prepared;
    selected.spec.calendar = selected_days;
    selected
        .spec
        .instruments
        .retain(|i| selected_symbols.contains(&i.symbol));
    selected.bars = bars;
    let baseline_spec: DatasetInput = serde_json::from_slice(&fs::read(dataset_path)?)?;
    let baseline = prepare_dataset(baseline_spec)?;
    let mut actual_rows = selected
        .bars
        .iter()
        .map(|b| (&b.symbol, b.trade_date, b.close.to_bits()))
        .collect::<Vec<_>>();
    let mut baseline_rows = baseline
        .bars
        .iter()
        .filter(|b| {
            selected_symbols.contains(&b.symbol) && read_end.is_none_or(|last| b.trade_date <= last)
        })
        .map(|b| (&b.symbol, b.trade_date, b.close.to_bits()))
        .collect::<Vec<_>>();
    actual_rows.sort_unstable();
    baseline_rows.sort_unstable();
    ensure!(
        actual_rows == baseline_rows,
        "Parquet selection differs from fixture contents"
    );
    let config = experiment_config(&selected.spec);
    let compute_started = Instant::now();
    let (soa, soa_samples, soa_phases) = run_soa(&selected, &config, true);
    let (arrow, arrow_samples, arrow_phases, arrow_conversion) =
        run_arrow(&selected, &config, true);
    let (polars, polars_samples, polars_phases, polars_conversion) =
        run_polars(&selected, &config, true)?;
    let compute_ns = compute_started.elapsed().as_nanos();
    let requested = |projection: &B1SoaProjection| B1SoaProjection {
        rankings: projection
            .rankings
            .iter()
            .filter(|row| start.is_none_or(|d| row.date >= d) && end.is_none_or(|d| row.date <= d))
            .cloned()
            .collect(),
        targets: projection
            .targets
            .iter()
            .filter(|row| start.is_none_or(|d| row.date >= d) && end.is_none_or(|d| row.date <= d))
            .cloned()
            .collect(),
        target_weights: projection
            .target_weights
            .iter()
            .filter(|row| start.is_none_or(|d| row.date >= d) && end.is_none_or(|d| row.date <= d))
            .cloned()
            .collect(),
        portfolio_returns: projection
            .portfolio_returns
            .iter()
            .filter(|row| start.is_none_or(|d| row.from >= d) && end.is_none_or(|d| row.from <= d))
            .cloned()
            .collect(),
    };
    let soa_requested = requested(&soa);
    ensure!(
        !soa_requested.rankings.is_empty(),
        "selection has no ranking dates"
    );
    let same = projections_match(&soa_requested, &requested(&arrow))
        && projections_match(&soa_requested, &requested(&polars));
    let full_selection = symbols.is_empty() && start.is_none() && end.is_none();
    if full_selection {
        let original_spec: DatasetInput = serde_json::from_slice(&fs::read(dataset_path)?)?;
        let original = prepare_dataset(original_spec)?;
        let mut actual = selected
            .bars
            .iter()
            .map(|b| (&b.symbol, b.trade_date, b.close.to_bits()))
            .collect::<Vec<_>>();
        let mut expected = original
            .bars
            .iter()
            .map(|b| (&b.symbol, b.trade_date, b.close.to_bits()))
            .collect::<Vec<_>>();
        actual.sort_unstable();
        expected.sort_unstable();
        ensure!(actual == expected, "Parquet content differs from fixture");
        ensure!(
            projections_match(&soa, &run_soa(&original, &config, false).0),
            "Parquet B1 differs from fixture"
        );
    }
    let serialize_started = Instant::now();
    let projection_sha256 = sha256(&serde_json::to_vec(&soa_requested)?);
    let serialization_ns = serialize_started.elapsed().as_nanos();
    let report = serde_json::json!({
        "format": FORMAT, "status": if same { "passed" } else { "failed" },
        "dataset_content_sha256": manifest.dataset_content_sha256,
        "parquet_sha256": manifest.parquet_sha256,
        "parquet_bytes": manifest.parquet_bytes,
        "filler_rows": manifest.filler_rows,
        "projection_sha256": projection_sha256,
        "filter": {"symbols": selected_symbols, "start": start, "end": end,
            "read_start": selected.spec.calendar.first(), "read_end": read_end,
            "history_included_for_rolling_windows": true},
        "pruning": {"source_groups": manifest.groups.len(), "scanned_groups": scanned_groups,
            "source_columns": manifest.columns.len(), "decoded_columns": 3,
            "source_rows": manifest.groups.iter().map(|g| g.rows).sum::<usize>(),
            "selected_bars": selected.bars.len()},
        "timing_ns": {"generation": generation_ns, "scan_decode": scan_decode_ns,
            "scan_conversion": scan_conversion_ns, "compute_all_candidates": compute_ns,
            "serialization": serialization_ns, "total": total_started.elapsed().as_nanos()},
        "candidates": {"soa": {"samples_ns": soa_samples, "phases": soa_phases},
            "arrow": {"samples_ns": arrow_samples, "phases": arrow_phases, "conversion_ns": arrow_conversion},
            "polars": {"samples_ns": polars_samples, "phases": polars_phases, "conversion_ns": polars_conversion}},
        "cache_condition": if reuse { "not controlled; reads existing file" } else { "not controlled; first read follows in-process generation" },
        "result_boundary": "B1 close-to-close evaluation projection; not an executable account ledger",
    });
    Ok(report)
}
