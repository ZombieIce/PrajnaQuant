use crate::{
    backtest::BacktestReport,
    core::ExperimentConfig,
    data::SnapshotManifest,
    factor::FactorReport,
    universe::{
        Capability, CoverageStatus, MembershipSnapshot, PitStatus, RESOLUTION_RULE_VERSION,
        SourceKind, UniverseDefinition,
    },
};
use anyhow::{Context, Result};
use chrono::{NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentResult {
    pub experiment_id: Uuid,
    pub created_at: String,
    pub engine_version: String,
    pub name: String,
    pub config: ExperimentConfig,
    /// Absent on historical result files; do not synthesize an identity for them.
    #[serde(default)]
    pub universe: Option<UniverseReportIdentity>,
    pub snapshot: SnapshotManifest,
    pub factor: FactorReport,
    pub backtest: BacktestReport,
    pub assumptions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UniverseReportIdentity {
    pub universe_id: uuid::Uuid,
    pub version_id: uuid::Uuid,
    pub name: String,
    pub source_kind: crate::universe::SourceKind,
    pub source_ref: String,
    pub asset_scope: crate::universe::AssetScope,
    pub content_hash: String,
    pub resolution_rule_version: String,
    #[serde(default)]
    pub membership_hashes: Vec<DailyMembershipHash>,
    #[serde(default)]
    pub empty_member_dates: Vec<NaiveDate>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub source_revision_hashes: Vec<String>,
    pub coverage: CoverageStatus,
    pub pit_status: PitStatus,
    pub capabilities: Vec<Capability>,
    pub snapshot_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyMembershipHash {
    pub as_of: NaiveDate,
    pub membership_hash: String,
}

impl UniverseReportIdentity {
    pub fn freeze(
        definition: &UniverseDefinition,
        snapshots: &[MembershipSnapshot],
        capabilities: Vec<Capability>,
        snapshot_hash: String,
    ) -> Result<Self> {
        let content_hash = definition.calculate_content_hash()?;
        let membership_hashes = snapshots
            .iter()
            .map(|snapshot| DailyMembershipHash {
                as_of: snapshot.as_of,
                membership_hash: snapshot.membership_hash.clone(),
            })
            .collect();
        let empty_member_dates: Vec<NaiveDate> = snapshots
            .iter()
            .filter(|snapshot| {
                snapshot.members.is_empty()
                    && (definition.source_kind == SourceKind::Manual
                        || (snapshot.coverage == CoverageStatus::Complete
                            && snapshot.pit_status == PitStatus::VerifiedPit))
            })
            .map(|snapshot| snapshot.as_of)
            .collect();
        let mut warnings: Vec<String> = snapshots
            .iter()
            .flat_map(|snapshot| snapshot.warnings.iter().cloned())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        if !empty_member_dates.is_empty() {
            warnings.push(format!(
                "resolved universe was empty on {} decision dates",
                empty_member_dates.len()
            ));
        }
        let source_revision_hashes = snapshots
            .iter()
            .flat_map(|snapshot| snapshot.source_revision_hashes.iter().cloned())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let coverage = snapshots
            .iter()
            .fold(CoverageStatus::Complete, |current, snapshot| {
                match (current, snapshot.coverage) {
                    (CoverageStatus::Gaps, _) | (_, CoverageStatus::Gaps) => CoverageStatus::Gaps,
                    (CoverageStatus::Unverified, _) | (_, CoverageStatus::Unverified) => {
                        CoverageStatus::Unverified
                    }
                    _ => CoverageStatus::Complete,
                }
            });
        let pit_status = snapshots
            .iter()
            .fold(PitStatus::VerifiedPit, |current, snapshot| {
                match (current, snapshot.pit_status) {
                    (PitStatus::Unknown, _) | (_, PitStatus::Unknown) => PitStatus::Unknown,
                    (PitStatus::RetrospectiveStatic, _) | (_, PitStatus::RetrospectiveStatic) => {
                        PitStatus::RetrospectiveStatic
                    }
                    _ => PitStatus::VerifiedPit,
                }
            });
        Ok(Self {
            universe_id: definition.universe_id,
            version_id: definition.version_id,
            name: definition.name.clone(),
            source_kind: definition.source_kind,
            source_ref: definition.source_ref.clone(),
            asset_scope: definition.asset_scope,
            content_hash,
            resolution_rule_version: RESOLUTION_RULE_VERSION.into(),
            membership_hashes,
            empty_member_dates,
            warnings,
            source_revision_hashes,
            coverage,
            pit_status,
            capabilities,
            snapshot_hash,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentSummary {
    pub experiment_id: Uuid,
    pub created_at: String,
    pub name: String,
    pub total_return: f64,
    pub max_drawdown: f64,
    pub mean_rank_ic: Option<f64>,
    pub annualized_return: f64,
    pub sharpe_ratio: f64,
    pub calmar_ratio: f64,
    pub turnover: f64,
    pub lookback_days: usize,
    pub score_mode: String,
    pub top_n: usize,
    pub rebalance_every: usize,
    /// Missing on older result files; these are explicitly surfaced as unknown.
    pub universe_id: Option<Uuid>,
    pub version_id: Option<Uuid>,
    pub universe_name: Option<String>,
    pub universe_status: String,
    pub universe_pit_status: Option<PitStatus>,
    pub universe_coverage: Option<CoverageStatus>,
}

impl ExperimentResult {
    pub fn new(
        config: ExperimentConfig,
        snapshot: SnapshotManifest,
        factor: FactorReport,
        backtest: BacktestReport,
    ) -> Self {
        let execution_assumption = match backtest.execution_status_mode.as_deref() {
            Some("status_gated") => {
                "execution uses the frozen snapshot's same-date status gate; historical status-source availability is unverified"
            }
            Some("legacy_bar_only") => {
                "legacy snapshot has no execution-status columns; fills use price bars only and do not verify tradability"
            }
            _ => "execution-status input mode is unknown",
        };
        Self{experiment_id:Uuid::new_v4(),created_at:Utc::now().to_rfc3339(),engine_version:env!("CARGO_PKG_VERSION").into(),name:config.name.clone(),config,universe:None,snapshot,factor,backtest,assumptions:vec![
            "signals are formed after the close and executed at the next available daily open".into(),
            "daily-bar execution applies configured slippage; it does not model intraday queue priority".into(),
            "if a held ETF that may need to be sold has no execution-day open, defer the whole rebalance with no partial trades and retry on the next market session".into(),
            "a later scheduled rebalance signal replaces the deferred target; deferred attempts and blocked symbols are recorded in the backtest report".into(),
            execution_assumption.into(),
            "cash earns no interest and ETF distributions are only reflected when present in source prices/data".into(),
        ]}
    }
    pub fn with_universe_identity(mut self, universe: UniverseReportIdentity) -> Self {
        self.universe = Some(universe);
        self
    }
    pub fn universe_status(&self) -> &'static str {
        if self.universe.is_some() {
            "known"
        } else {
            "legacy_universe_unknown"
        }
    }
    pub fn summary(&self) -> ExperimentSummary {
        ExperimentSummary {
            experiment_id: self.experiment_id,
            created_at: self.created_at.clone(),
            name: self.name.clone(),
            total_return: self.backtest.metrics.total_return,
            max_drawdown: self.backtest.metrics.max_drawdown,
            mean_rank_ic: self.factor.mean_rank_ic,
            annualized_return: self.backtest.metrics.annualized_return,
            sharpe_ratio: self.backtest.metrics.sharpe_ratio,
            calmar_ratio: self.backtest.metrics.calmar_ratio,
            turnover: self.backtest.metrics.turnover,
            lookback_days: self.config.strategy.lookback_days,
            score_mode: if self.config.strategy.uses_rotation_score() {
                "rotation_composite"
            } else {
                "single_momentum"
            }
            .into(),
            top_n: self.config.strategy.top_n,
            rebalance_every: self.config.strategy.rebalance_every,
            universe_id: self.universe.as_ref().map(|u| u.universe_id),
            version_id: self.universe.as_ref().map(|u| u.version_id),
            universe_name: self.universe.as_ref().map(|u| u.name.clone()),
            universe_status: self.universe_status().into(),
            universe_pit_status: self.universe.as_ref().map(|u| u.pit_status),
            universe_coverage: self.universe.as_ref().map(|u| u.coverage),
        }
    }
    pub fn save(&self, root: &Path) -> Result<PathBuf> {
        let dir = root
            .join("experiments")
            .join(self.experiment_id.to_string());
        fs::create_dir_all(&dir)?;
        let path = dir.join("experiment.json");
        fs::write(&path, serde_json::to_vec_pretty(self)?)
            .with_context(|| format!("write {}", path.display()))?;
        Ok(path)
    }
}

pub fn list_experiments(root: &Path) -> Result<Vec<ExperimentSummary>> {
    let dir = root.join("experiments");
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut values = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path().join("experiment.json");
        if path.is_file() {
            if let Ok(value) = serde_json::from_slice::<ExperimentResult>(&fs::read(path)?) {
                values.push(value.summary());
            }
        }
    }
    values.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(values)
}

pub fn load_experiment(root: &Path, id: &str) -> Result<ExperimentResult> {
    anyhow::ensure!(
        !id.is_empty() && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-'),
        "invalid experiment id"
    );
    let path = root.join("experiments").join(id).join("experiment.json");
    Ok(serde_json::from_slice(
        &fs::read(&path).with_context(|| format!("read {}", path.display()))?,
    )?)
}
