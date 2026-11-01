//! Pool publication re-runs pinned production Normalizers, never copies Parquet tables.
use crate::{
    NormalizationIssue, NormalizationOutput, NormalizedTables, Normalizer, NormalizerRegistry,
    RawInput, RawStore, SecurityEvidenceSnapshot, SourceKind, bar_sources_from_record_batch,
    bar_sources_to_record_batch, bars_from_record_batch, bars_to_record_batch,
    daily_publish::{self, DailyPublicationReceipt},
    daily_raw::{checked_output_path, validate_raw_root},
    execution_status_from_record_batch, execution_status_to_record_batch,
    instruments_from_record_batch, instruments_to_record_batch, publish_dataset, read_manifest,
    rebuild_dataset, sessions_from_record_batch, sessions_to_record_batch,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    sync::Arc,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolSelection {
    pub schema_version: u32,
    pub members: BTreeMap<String, DailyPublicationReceipt>,
}

struct Member {
    normalizer: Arc<dyn Normalizer>,
    inputs: Vec<String>,
}
struct PoolNormalizer {
    recipe: String,
    calendar: String,
    security: String,
    inputs: BTreeSet<String>,
    members: BTreeMap<String, Member>,
}
fn require(ok: bool, message: &'static str) -> Result<()> {
    if ok { Ok(()) } else { Err(message.into()) }
}
fn freeze(raw: &RawStore, bytes: &[u8], source: &str) -> Result<String> {
    let hash = format!("sha256:{}", crate::sha256(bytes));
    match raw.get(&hash) {
        Ok(existing) => {
            require(existing == bytes, "frozen pool input mismatch")?;
            Ok(hash)
        }
        Err(crate::RawStoreError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            daily_publish::put(raw, bytes, source)
        }
        Err(error) => Err(error.into()),
    }
}

fn registry(
    root: &Path,
    selection: &PoolSelection,
) -> Result<(NormalizerRegistry, Vec<String>, String)> {
    require(
        selection.schema_version == 1 && !selection.members.is_empty(),
        "invalid pool publication selection",
    )?;
    let raw = RawStore::open(root)?;
    let mut inputs = BTreeSet::new();
    let mut members = BTreeMap::new();
    let mut calendar = None;
    let mut snapshot = SecurityEvidenceSnapshot {
        schema_version: 1,
        instruments: Vec::new(),
    };
    for (instrument, receipt) in &selection.members {
        let (recipe, selected) = daily_publish::verify_receipt(root, receipt)?;
        let evidence: SecurityEvidenceSnapshot =
            serde_json::from_slice(&raw.get(&recipe.security_snapshot)?)?;
        require(
            evidence.instruments.len() == 1
                && evidence.instruments[0].record.instrument_id.to_string() == *instrument,
            "pool member and daily publication identity mismatch",
        )?;
        require(
            calendar
                .as_ref()
                .is_none_or(|value| value == &recipe.calendar_version),
            "pool publications must select the same fixed calendar version",
        )?;
        calendar = Some(recipe.calendar_version.clone());
        snapshot.instruments.extend(evidence.instruments);
        let normalizer = daily_publish::registry(&recipe)?
            .get(&recipe.normalizer_id, &recipe.normalizer_version)
            .ok_or("missing daily normalizer")?;
        inputs.extend(selected.iter().cloned());
        inputs.extend([receipt.recipe_sha256.clone(), recipe.report_sha256]);
        members.insert(
            instrument.clone(),
            Member {
                normalizer,
                inputs: selected,
            },
        );
    }
    snapshot.validate()?;
    let security = freeze(
        &raw,
        &serde_json::to_vec(&snapshot)?,
        "pool-security-snapshot/1",
    )?;
    let recipe = freeze(
        &raw,
        &serde_json::to_vec(selection)?,
        "pool-publication-recipe/1",
    )?;
    inputs.extend([security.clone(), recipe.clone()]);
    let selected = inputs.iter().cloned().collect();
    let mut registry = NormalizerRegistry::new();
    registry.register(PoolNormalizer {
        recipe: recipe.clone(),
        calendar: calendar.ok_or("empty pool")?,
        security,
        inputs,
        members,
    });
    Ok((registry, selected, recipe))
}

impl PoolNormalizer {
    fn convert(&self, raw: &[RawInput]) -> Result<NormalizationOutput> {
        require(
            raw.len() == self.inputs.len()
                && raw
                    .iter()
                    .map(|r| r.raw_sha256.clone())
                    .collect::<BTreeSet<_>>()
                    == self.inputs,
            "pool Raw input set mismatch",
        )?;
        require(
            raw.iter()
                .all(|r| format!("sha256:{}", crate::sha256(&r.bytes)) == r.raw_sha256),
            "pool Raw bytes mismatch",
        )?;
        let mut instruments = Vec::new();
        let mut sessions = BTreeMap::new();
        let mut bars = Vec::new();
        let mut statuses = Vec::new();
        let mut sources = Vec::new();
        let mut coverage = BTreeMap::new();
        let mut assumptions = BTreeSet::new();
        let mut ignored = BTreeSet::new();
        for (id, member) in &self.members {
            let selected = raw
                .iter()
                .filter(|r| member.inputs.contains(&r.raw_sha256))
                .cloned()
                .collect::<Vec<_>>();
            let out = member
                .normalizer
                .normalize(&selected)
                .map_err(|e| format!("{e:?}"))?;
            let member_instruments = instruments_from_record_batch(&out.tables.instruments)?;
            require(
                member_instruments.len() == 1 && member_instruments[0].id().to_string() == *id,
                "normalized pool member mismatch",
            )?;
            let axis = sessions_from_record_batch(&out.tables.sessions)?;
            bars.extend(bars_from_record_batch(&out.tables.bars, &axis)?);
            for session in axis {
                let key = (session.venue_id().to_string(), session.session_date());
                if let Some(previous) = sessions.insert(key, session.clone()) {
                    require(previous == session, "conflicting pool Session")?;
                }
            }
            instruments.extend(member_instruments);
            if let Some(batch) = out.tables.execution_status {
                statuses.extend(execution_status_from_record_batch(&batch)?);
            }
            if let Some(batch) = out.tables.bar_sources {
                sources.extend(bar_sources_from_record_batch(&batch)?);
            }
            coverage.insert(id.clone(), out.coverage);
            assumptions.extend(out.synthetic_assumptions);
            ignored.extend(out.ignored_input_fields);
        }
        let venues = instruments
            .iter()
            .map(|i| i.id().venue().to_string())
            .collect::<BTreeSet<_>>();
        let mut common = None;
        for venue in venues {
            let axis = sessions
                .values()
                .filter(|s| s.venue_id().as_str() == venue)
                .map(|s| (s.session_date(), s.ts_open(), s.ts_close()))
                .collect::<Vec<_>>();
            require(
                !axis.is_empty() && common.as_ref().is_none_or(|previous| previous == &axis),
                "inconsistent_venue_calendar",
            )?;
            common = Some(axis);
        }
        Ok(NormalizationOutput {
            tables: NormalizedTables {
                instruments: instruments_to_record_batch(&instruments)?,
                sessions: sessions_to_record_batch(&sessions.into_values().collect::<Vec<_>>())?,
                bars: bars_to_record_batch(&bars)?,
                execution_status: if statuses.is_empty() {
                    None
                } else {
                    Some(execution_status_to_record_batch(&statuses)?)
                },
                bar_sources: Some(bar_sources_to_record_batch(&sources)?),
                source_crosscheck: None,
                announcements: None,
                corporate_actions: None,
                tdx_code_table: None,
            },
            coverage: json!({"normalizer_version":"3","source":"tencent_fqkline_daily","calendar_version":self.calendar,
                "security_snapshot":self.security,"members":coverage,"pool_recipe":self.recipe,
                "completeness_claim":"Only explicitly selected member reports; missing quotes and statuses remain unknown"}),
            synthetic_assumptions: assumptions.into_iter().collect(),
            ignored_input_fields: ignored.into_iter().collect(),
        })
    }
}
impl Normalizer for PoolNormalizer {
    fn id(&self) -> &str {
        "cn-stock-etf-daily"
    }
    fn version(&self) -> &str {
        "3"
    }
    fn config_sha256(&self) -> String {
        self.recipe.clone()
    }
    fn accepted_source_kinds(&self) -> &[SourceKind] {
        &[SourceKind::FileImport, SourceKind::VendorApi]
    }
    fn normalize(
        &self,
        raw: &[RawInput],
    ) -> std::result::Result<NormalizationOutput, Vec<NormalizationIssue>> {
        self.convert(raw).map_err(|e| {
            vec![NormalizationIssue {
                code: "invalid_pool_publication".into(),
                path: "$".into(),
                message: e.to_string(),
            }]
        })
    }
}

pub fn publish(
    root: &Path,
    selection: &PoolSelection,
    receipt: &Path,
) -> Result<DailyPublicationReceipt> {
    let root = validate_raw_root(root, &crate::default_mvp6_root())?;
    for name in ["raw", "raw-sources", "normalized", "manifests"] {
        daily_publish::check_tree(&checked_output_path(&root, Path::new(name))?)?;
    }
    let path = checked_output_path(&root, receipt)?;
    let (registry, inputs, recipe_sha256) = registry(&root, selection)?;
    let existing: Option<DailyPublicationReceipt> = if path.exists() {
        Some(serde_json::from_slice(&fs::read(&path)?)?)
    } else {
        None
    };
    require(
        existing
            .as_ref()
            .is_none_or(|v| v.recipe_sha256 == recipe_sha256),
        "pool receipt already names different inputs",
    )?;
    let manifest = publish_dataset(
        &root,
        &RawStore::open(&root)?,
        &registry,
        "cn-stock-etf-daily",
        "3",
        &inputs,
        chrono::Utc::now().to_rfc3339(),
    )
    .map_err(|e| format!("{e:?}"))?;
    if let Some(existing) = existing {
        require(existing.dsv == manifest.dsv, "pool receipt DSV mismatch")?;
        return Ok(existing);
    }
    let result = DailyPublicationReceipt {
        recipe_sha256,
        dsv: manifest.dsv,
    };
    fs::create_dir_all(path.parent().ok_or("missing receipt directory")?)?;
    let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    serde_json::to_writer(&mut file, &result)?;
    file.as_file().sync_all()?;
    file.persist_noclobber(path)?;
    Ok(result)
}

pub fn rebuild(root: &Path, receipt: &DailyPublicationReceipt) -> Result<()> {
    let raw = RawStore::open(root)?;
    let selection: PoolSelection = serde_json::from_slice(&raw.get(&receipt.recipe_sha256)?)?;
    let (registry, inputs, recipe) = registry(root, &selection)?;
    require(recipe == receipt.recipe_sha256, "pool recipe mismatch")?;
    let manifest = read_manifest(root, &receipt.dsv)?;
    require(
        manifest
            .core
            .inputs
            .iter()
            .map(|v| v.raw_sha256.clone())
            .collect::<BTreeSet<_>>()
            == inputs.into_iter().collect(),
        "pool manifest inputs mismatch",
    )?;
    rebuild_dataset(&raw, &registry, &manifest)?;
    Ok(())
}

pub fn rebuild_receipt(root: &Path, name: &Path) -> Result<()> {
    let root = validate_raw_root(root, &crate::default_mvp6_root())?;
    let receipt = serde_json::from_slice(&fs::read(checked_output_path(&root, name)?)?)?;
    rebuild(&root, &receipt)
}
