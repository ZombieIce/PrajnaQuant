//! Local-only acceptance workflow for MVP-6 public facts. It reads the
//! published v4 Dataset, validates facts against the local Raw Store, and
//! writes the discrepancy report under the local acceptance directory. No
//! network access, real facts, or hashes are committed to the repository.

use super::{
    Citation, Collector, CrosscheckReport, FactCalendar, FundCandidate, Mvp6Facts, PdfText, Result,
    Transport, V4DailyBar, crosscheck_v4, read_citations_csv, validate_and_convert,
};
use crate::{
    Manifest, RawStore, bar_sources_from_record_batch, bars_from_record_batch, read_manifest,
    read_parquet, sessions_from_record_batch, tdx_code_table_from_record_batch,
};
use arrow_array::RecordBatch;
use chrono::NaiveDate;
use prajna_domain::{Price, Session};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

/// The local v4 row projection used by the cross-check. Only positive Bars and
/// keyed no-trade observations for the fact identities are retained.
#[derive(Debug, Clone, PartialEq)]
pub struct V4Projection {
    pub dsv: String,
    pub calendar: BTreeSet<NaiveDate>,
    pub bars: Vec<V4DailyBar>,
}

/// Paths and findings written by the local facts acceptance step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactsAcceptance {
    pub directory: PathBuf,
    pub facts_session_path: PathBuf,
    pub crosscheck_path: PathBuf,
    pub dsv: String,
    pub report: CrosscheckReport,
}

/// One local fixed-list run: the collected announcements and, when the local
/// `facts/` directory exists, the validated facts and v4 cross-check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixedListAcceptance {
    pub directory: PathBuf,
    pub collection_path: PathBuf,
    pub facts: Option<FactsAcceptance>,
    pub unresolved: Vec<String>,
}

/// Loads the published `szse-etf-frozen@4` projection for the fact codes.
/// A code-table observation inside an identity lifecycle with no Bar is a
/// keyed no-trade fact; dates outside the lifecycle are ignored.
pub fn load_v4_projection(lake_root: &Path, dsv: &str, facts: &Mvp6Facts) -> Result<V4Projection> {
    let manifest = read_manifest(lake_root, dsv)?;
    if manifest.core.normalizer.id != "szse-etf-frozen" || manifest.core.normalizer.version != "4" {
        return Err(format!(
            "cross-check requires szse-etf-frozen@4, found {}@{}",
            manifest.core.normalizer.id, manifest.core.normalizer.version
        )
        .into());
    }

    let mut sessions = Vec::new();
    for batch in table_batches(lake_root, &manifest, "sessions")? {
        sessions.extend(sessions_from_record_batch(&batch)?);
    }
    let calendar = sessions
        .iter()
        .map(Session::session_date)
        .collect::<BTreeSet<_>>();
    if calendar.is_empty() {
        return Err("v4 Dataset has no sessions".into());
    }

    let mut windows = BTreeMap::<String, Vec<(NaiveDate, Option<NaiveDate>)>>::new();
    for identity in &facts.identities {
        let listing = NaiveDate::parse_from_str(&identity.listing_date, "%Y-%m-%d")
            .map_err(|_| format!("invalid listing_date for {}", identity.code))?;
        let termination = if identity.termination_date.trim().is_empty() {
            None
        } else {
            Some(
                NaiveDate::parse_from_str(&identity.termination_date, "%Y-%m-%d")
                    .map_err(|_| format!("invalid termination_date for {}", identity.code))?,
            )
        };
        windows
            .entry(identity.code.clone())
            .or_default()
            .push((listing, termination));
    }

    let mut reference_prices = BTreeMap::<(String, NaiveDate), Price>::new();
    for batch in table_batches(lake_root, &manifest, "bar_sources")? {
        for row in bar_sources_from_record_batch(&batch)? {
            if let Some(reference) = row.reference_price {
                reference_prices.insert(
                    (row.instrument_id.symbol().to_owned(), row.session_date),
                    reference,
                );
            }
        }
    }

    let mut bars = Vec::new();
    let mut traded = BTreeSet::new();
    for batch in table_batches(lake_root, &manifest, "bars")? {
        for bar in bars_from_record_batch(&batch, &sessions)? {
            let code = bar.instrument_id().symbol().to_owned();
            if !windows.contains_key(&code) {
                continue;
            }
            let key = (code.clone(), bar.session_date());
            traded.insert(key.clone());
            bars.push(V4DailyBar {
                code,
                date: bar.session_date(),
                close: Some(bar.close()),
                reference_price: reference_prices.get(&key).copied(),
                keyed_no_trade: false,
            });
        }
    }
    bars.sort_by(|left, right| (&left.code, left.date).cmp(&(&right.code, right.date)));

    let mut keyed = BTreeSet::new();
    for batch in table_batches(lake_root, &manifest, "tdx_code_table")? {
        for table in tdx_code_table_from_record_batch(&batch)? {
            if !calendar.contains(&table.session_date) {
                return Err(format!(
                    "TDX code table session {} is outside the v4 calendar",
                    table.session_date
                )
                .into());
            }
            for code in table
                .native_symbols
                .iter()
                .filter(|code| windows.contains_key(code.as_str()))
            {
                let Some(ranges) = windows.get(code.as_str()) else {
                    continue;
                };
                let in_lifecycle = ranges.iter().any(|(listing, termination)| {
                    table.session_date >= *listing
                        && termination.is_none_or(|end| table.session_date <= end)
                });
                if !in_lifecycle {
                    continue;
                }
                if traded.contains(&(code.clone(), table.session_date)) {
                    continue;
                }
                if keyed.insert((code.clone(), table.session_date)) {
                    bars.push(V4DailyBar {
                        code: code.clone(),
                        date: table.session_date,
                        close: None,
                        reference_price: None,
                        keyed_no_trade: true,
                    });
                }
            }
        }
    }
    bars.sort_by(|left, right| (&left.code, left.date).cmp(&(&right.code, right.date)));
    Ok(V4Projection {
        dsv: manifest.dsv,
        calendar,
        bars,
    })
}

/// Validates the local fact tables and writes both the converted session and
/// the local-only cross-check report into `directory`.
pub fn write_facts_acceptance(
    directory: &Path,
    lake_root: &Path,
    dsv: &str,
    window_start: NaiveDate,
    window_end: NaiveDate,
    facts: &Mvp6Facts,
    citations: &[Citation],
) -> Result<FactsAcceptance> {
    let projection = load_v4_projection(lake_root, dsv, facts)?;
    let calendar = FactCalendar::new(projection.calendar, window_start, window_end)?;
    let session = validate_and_convert(facts, citations, &calendar)?;
    let report = crosscheck_v4(facts, &projection.bars);
    fs::create_dir_all(directory)?;

    let facts_session_path = directory.join("facts-session.json");
    write_json(
        &facts_session_path,
        &json!({
            "contract": "mvp-6-cninfo-facts-v1",
            "dsv": projection.dsv,
            "session": session,
        }),
    )?;
    let crosscheck_path = directory.join("crosscheck.json");
    write_json(
        &crosscheck_path,
        &json!({
            "contract": "mvp-6-cninfo-crosscheck-v1",
            "dsv": projection.dsv,
            "facts_rows": {
                "identities": facts.identities.len(),
                "actions": facts.actions.len(),
                "events": facts.events.len(),
            },
            "mismatch_count": report.mismatches.len(),
            "mismatches": report.mismatches,
        }),
    )?;
    Ok(FactsAcceptance {
        directory: directory.to_path_buf(),
        facts_session_path,
        crosscheck_path,
        dsv: projection.dsv,
        report,
    })
}

/// Runs the local-only fixed-list acceptance workflow. Collection output is
/// always written; facts validation and the v4 cross-check run only when the
/// operator created `root/facts/`. The caller decides whether unresolved
/// collection gaps are fatal.
pub fn collect_and_accept<T: Transport>(
    root: &Path,
    store: &RawStore,
    collector: &mut Collector<T>,
    candidates: &[FundCandidate],
    explicit_pdf_categories: &BTreeSet<String>,
    converter: &mut impl PdfText,
) -> Result<FixedListAcceptance> {
    let result = collector.collect_fixed_list(candidates, explicit_pdf_categories, converter);
    let announcements = result
        .announcements
        .iter()
        .map(|((code, org_id), rows)| {
            json!({
                "code": code, "org_id": org_id, "announcements": rows,
            })
        })
        .collect::<Vec<_>>();
    let report = json!({
        "contract": "mvp-6-fixed-cninfo-v1",
        "resolutions": result.resolutions,
        "announcements": announcements,
        "pdf_raw_sha256_by_announcement": result.pdf_raw_sha256_by_announcement,
        "unresolved": result.unresolved,
    });
    let directory = root
        .join("acceptance/cninfo-fixed")
        .join(chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ").to_string());
    fs::create_dir_all(&directory)?;
    let collection_path = directory.join("collection.json");
    write_json(&collection_path, &report)?;

    let facts_dir = root.join("facts");
    let facts = if facts_dir.is_dir() {
        let facts = Mvp6Facts::from_csv(
            &fs::read_to_string(facts_dir.join("identity.csv"))?,
            &fs::read_to_string(facts_dir.join("actions.csv"))?,
            &fs::read_to_string(facts_dir.join("events.csv"))?,
        )?;
        let citations = read_citations_csv(
            &fs::read_to_string(facts_dir.join("citations.csv"))?,
            store,
            converter,
        )?;
        let (dsv, window_start, window_end) = published_local_dataset(root)?;
        Some(write_facts_acceptance(
            &directory,
            root,
            &dsv,
            window_start,
            window_end,
            &facts,
            &citations,
        )?)
    } else {
        None
    };
    Ok(FixedListAcceptance {
        directory,
        collection_path,
        facts,
        unresolved: result.unresolved,
    })
}

fn published_local_dataset(root: &Path) -> Result<(String, NaiveDate, NaiveDate)> {
    let coverage: Value =
        serde_json::from_slice(&fs::read(root.join("acceptance/mvp6-coverage.json"))?)?;
    if coverage["dataset_published"] != true {
        return Err("local v4 dataset is not published".into());
    }
    let dsv = coverage["dsv"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "local coverage report has no published DSV".to_owned())?;
    let start = coverage["window"]["start"]
        .as_str()
        .ok_or("local coverage report has no window start")?
        .parse::<NaiveDate>()?;
    let end = coverage["window"]["end"]
        .as_str()
        .ok_or("local coverage report has no window end")?
        .parse::<NaiveDate>()?;
    Ok((dsv, start, end))
}

fn table_batches(
    lake_root: &Path,
    manifest: &Manifest,
    table_name: &str,
) -> Result<Vec<RecordBatch>> {
    let table = manifest
        .core
        .tables
        .iter()
        .find(|table| table.table == table_name)
        .ok_or_else(|| format!("v4 manifest is missing the {table_name} table"))?;
    if table.schema_version != 1 {
        return Err(format!(
            "unsupported {table_name} schema version {}",
            table.schema_version
        )
        .into());
    }
    let hex = manifest
        .dsv
        .strip_prefix("dsv:sha256:")
        .ok_or("invalid v4 manifest DSV")?;
    let path = lake_root
        .join("normalized")
        .join(table_name)
        .join(hex)
        .join("part-00000.parquet");
    let batches = read_parquet(&path, table_name, "1")?;
    for batch in &batches {
        if batch
            .schema()
            .metadata()
            .get("prajna.dsv")
            .map(String::as_str)
            != Some(manifest.dsv.as_str())
        {
            return Err(format!("{table_name} Parquet DSV does not match the manifest").into());
        }
    }
    Ok(batches)
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(&bytes)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}
