//! Versioned universe identity and point-in-time membership resolution.
//!
//! This module resolves membership facts only. It never fetches index history,
//! infers membership from price bars, or crops the market-data panel.

use anyhow::{Result, bail, ensure};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use uuid::Uuid;

pub const RESOLUTION_RULE_VERSION: &str = "universe-pit-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetScope {
    Etf,
    Stock,
    Mixed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Manual,
    IndexHistory,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetType {
    Etf,
    Stock,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PitStatus {
    VerifiedPit,
    RetrospectiveStatic,
    Unknown,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageStatus {
    Complete,
    Gaps,
    Unverified,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityKind {
    MembershipReady,
    FactorResearchReady,
    StrategyBacktestReady,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    pub kind: CapabilityKind,
    pub ready: bool,
    pub blockers: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UniverseSelection {
    pub universe_id: Uuid,
    pub version_id: Uuid,
    pub asset_scope: AssetScope,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Instrument {
    pub instrument_id: String,
    pub exchange: String,
    pub code: String,
    pub name: String,
    pub asset_type: AssetType,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManualMember {
    pub instrument: Instrument,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
    pub source_ref: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MembershipAction {
    Add,
    Remove,
}
/// Separate add/remove facts preserve the announcement-time boundary for changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexMembershipEvent {
    pub instrument: Instrument,
    pub action: MembershipAction,
    pub effective_date: NaiveDate,
    pub published_at: DateTime<Utc>,
    pub source_ref: String,
    pub source_revision_hash: String,
    pub verified: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageSegment {
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub status: CoverageStatus,
    pub source_revision_hash: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UniverseDefinition {
    pub universe_id: Uuid,
    pub version_id: Uuid,
    pub name: String,
    pub description: String,
    pub asset_scope: AssetScope,
    pub source_kind: SourceKind,
    pub source_ref: String,
    pub content_hash: String,
    pub manual_members: Vec<ManualMember>,
    pub index_events: Vec<IndexMembershipEvent>,
    pub coverage: Vec<CoverageSegment>,
}

impl UniverseDefinition {
    /// SHA-256 of canonicalized array ordering and serde's stable struct field order.
    pub fn calculate_content_hash(&self) -> Result<String> {
        #[derive(Serialize)]
        struct Hashable<'a> {
            universe_id: Uuid,
            version_id: Uuid,
            name: &'a str,
            description: &'a str,
            asset_scope: AssetScope,
            source_kind: SourceKind,
            source_ref: &'a str,
            manual_members: &'a [ManualMember],
            index_events: &'a [IndexMembershipEvent],
            coverage: &'a [CoverageSegment],
        }
        let mut manual = self.manual_members.clone();
        manual.sort_by(|a, b| {
            (
                &a.instrument.instrument_id,
                a.effective_from,
                a.effective_to,
            )
                .cmp(&(
                    &b.instrument.instrument_id,
                    b.effective_from,
                    b.effective_to,
                ))
        });
        let mut events = self.index_events.clone();
        events.sort_by(|a, b| {
            (&a.instrument.instrument_id, a.effective_date, a.action).cmp(&(
                &b.instrument.instrument_id,
                b.effective_date,
                b.action,
            ))
        });
        let mut coverage = self.coverage.clone();
        coverage.sort_by_key(|v| (v.start, v.end));
        let bytes = serde_json::to_vec(&Hashable {
            universe_id: self.universe_id,
            version_id: self.version_id,
            name: &self.name,
            description: &self.description,
            asset_scope: self.asset_scope,
            source_kind: self.source_kind,
            source_ref: &self.source_ref,
            manual_members: &manual,
            index_events: &events,
            coverage: &coverage,
        })?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedMember {
    pub instrument: Instrument,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
    pub published_at: Option<DateTime<Utc>>,
    pub source_ref: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MembershipSnapshot {
    pub universe_id: Uuid,
    pub version_id: Uuid,
    pub as_of: NaiveDate,
    pub decision_cutoff: DateTime<Utc>,
    pub members: Vec<ResolvedMember>,
    pub pit_status: PitStatus,
    pub coverage: CoverageStatus,
    pub warnings: Vec<String>,
    pub membership_hash: String,
    pub source_revision_hashes: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UniverseCacheKey {
    pub universe_id: Uuid,
    pub version_id: Uuid,
    pub content_hash: String,
    pub resolution_rule_version: String,
    pub source_revision_hash: String,
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub decision_cutoff_rule: String,
}

pub fn strategy_capability(scope: AssetScope) -> Capability {
    match scope {
        AssetScope::Etf => Capability {
            kind: CapabilityKind::StrategyBacktestReady,
            ready: true,
            blockers: vec![],
        },
        AssetScope::Stock | AssetScope::Mixed => Capability {
            kind: CapabilityKind::StrategyBacktestReady,
            ready: false,
            blockers: vec![
                "capability_not_ready: stock ETF Rotation execution has not been validated".into(),
            ],
        },
    }
}

/// Resolve the close-time candidate set for one date. Price history is not filtered.
pub fn resolve_members(
    definition: &UniverseDefinition,
    as_of: NaiveDate,
    decision_cutoff: DateTime<Utc>,
    strict_pit: bool,
) -> Result<MembershipSnapshot> {
    ensure!(
        as_of <= decision_cutoff.date_naive(),
        "as_of is after decision cutoff"
    );
    let (coverage, covered, source_hashes) = coverage_on(definition, as_of);
    if strict_pit && !covered {
        bail!("coverage_gap: no verified complete universe coverage for {as_of}");
    }
    let mut warnings = Vec::new();
    let mut members = match definition.source_kind {
        SourceKind::Manual => {
            ensure!(
                definition.index_events.is_empty(),
                "manual universe cannot contain index events"
            );
            ensure!(
                definition
                    .manual_members
                    .iter()
                    .all(|m| m.effective_to.is_none_or(|e| m.effective_from < e)),
                "manual member interval must be non-empty"
            );
            let selected: Vec<ResolvedMember> = definition
                .manual_members
                .iter()
                .filter(|m| m.effective_from <= as_of && m.effective_to.is_none_or(|e| as_of < e))
                .map(|m| ResolvedMember {
                    instrument: m.instrument.clone(),
                    effective_from: m.effective_from,
                    effective_to: m.effective_to,
                    published_at: None,
                    source_ref: m.source_ref.clone(),
                })
                .collect();
            warnings.push("manual membership applied retrospectively; not historical PIT".into());
            selected
        }
        SourceKind::IndexHistory => {
            ensure!(
                definition.manual_members.is_empty(),
                "index-history universe cannot contain manual members"
            );
            let mut events: Vec<_> = definition
                .index_events
                .iter()
                .filter(|e| e.effective_date <= as_of && e.published_at <= decision_cutoff)
                .collect();
            events.sort_by(|a, b| {
                (
                    a.effective_date,
                    a.published_at,
                    &a.instrument.instrument_id,
                )
                    .cmp(&(
                        b.effective_date,
                        b.published_at,
                        &b.instrument.instrument_id,
                    ))
            });
            let mut active = BTreeMap::<String, ResolvedMember>::new();
            let mut all_verified = true;
            for event in events {
                all_verified &= event.verified && !event.source_revision_hash.is_empty();
                match event.action {
                    MembershipAction::Add => {
                        active.insert(
                            event.instrument.instrument_id.clone(),
                            ResolvedMember {
                                instrument: event.instrument.clone(),
                                effective_from: event.effective_date,
                                effective_to: None,
                                published_at: Some(event.published_at),
                                source_ref: event.source_ref.clone(),
                            },
                        );
                    }
                    MembershipAction::Remove => {
                        active.remove(&event.instrument.instrument_id);
                    }
                }
            }
            if !all_verified {
                warnings.push("one or more applicable membership events are unverified".into());
            }
            active.into_values().collect()
        }
    };
    members.sort_by(|a, b| a.instrument.instrument_id.cmp(&b.instrument.instrument_id));
    ensure!(
        members
            .windows(2)
            .all(|p| p[0].instrument.instrument_id != p[1].instrument.instrument_id),
        "duplicate instrument_id in resolved membership"
    );
    let pit_status = match definition.source_kind {
        SourceKind::Manual => PitStatus::RetrospectiveStatic,
        SourceKind::IndexHistory if covered && warnings.is_empty() => PitStatus::VerifiedPit,
        SourceKind::IndexHistory => PitStatus::Unknown,
    };
    if !covered {
        warnings.push(format!("membership coverage is {coverage:?} on {as_of}"));
    }
    let membership_hash = snapshot_hash(definition, as_of, &members, pit_status, &source_hashes)?;
    Ok(MembershipSnapshot {
        universe_id: definition.universe_id,
        version_id: definition.version_id,
        as_of,
        decision_cutoff,
        members,
        pit_status,
        coverage,
        warnings,
        membership_hash,
        source_revision_hashes: source_hashes,
    })
}

fn coverage_on(
    definition: &UniverseDefinition,
    date: NaiveDate,
) -> (CoverageStatus, bool, Vec<String>) {
    match definition
        .coverage
        .iter()
        .find(|s| s.start <= date && date <= s.end)
    {
        Some(s) => (
            s.status,
            s.status == CoverageStatus::Complete && !s.source_revision_hash.is_empty(),
            if s.source_revision_hash.is_empty() {
                vec![]
            } else {
                vec![s.source_revision_hash.clone()]
            },
        ),
        None => (CoverageStatus::Gaps, false, vec![]),
    }
}
fn snapshot_hash(
    definition: &UniverseDefinition,
    as_of: NaiveDate,
    members: &[ResolvedMember],
    pit_status: PitStatus,
    source_hashes: &[String],
) -> Result<String> {
    #[derive(Serialize)]
    struct Hashable<'a> {
        as_of: NaiveDate,
        instrument_ids: Vec<&'a str>,
        source_revision_hashes: &'a [String],
        pit_status: PitStatus,
        content_hash: &'a str,
    }
    let ids = members
        .iter()
        .map(|m| m.instrument.instrument_id.as_str())
        .collect();
    let bytes = serde_json::to_vec(&Hashable {
        as_of,
        instrument_ids: ids,
        source_revision_hashes: source_hashes,
        pit_status,
        content_hash: &definition.content_hash,
    })?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

/// Date/member scoring mask that leaves the complete input panel intact for warmup and holdings.
pub fn filter_scores_for_snapshot(
    scores: &HashMap<(NaiveDate, String), f64>,
    snapshot: &MembershipSnapshot,
) -> Result<HashMap<(NaiveDate, String), f64>> {
    let eligible: BTreeSet<String> = snapshot
        .members
        .iter()
        .map(|m| market_bar_symbol(&m.instrument.exchange, &m.instrument.code))
        .collect::<Result<_>>()?;
    Ok(scores
        .iter()
        .filter(|((date, symbol), _)| *date == snapshot.as_of && eligible.contains(symbol))
        .map(|(key, value)| (key.clone(), *value))
        .collect())
}

/// Convert a resolved security identity to the warehouse/Parquet symbol key.
pub fn market_bar_symbol(exchange: &str, code: &str) -> Result<String> {
    ensure!(
        code.len() == 6 && code.bytes().all(|byte| byte.is_ascii_digit()),
        "invalid six-digit instrument code"
    );
    let prefix = match exchange.to_ascii_uppercase().as_str() {
        "SH" | "XSHG" | "SSE" => "sh",
        "SZ" | "XSHE" | "SZSE" => "sz",
        "BJ" | "XBEI" | "BSE" => {
            bail!("capability_not_ready: Beijing market bars are not supported")
        }
        _ => bail!("unknown exchange {exchange}"),
    };
    Ok(format!("{prefix}{code}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};
    fn d(day: u32) -> NaiveDate {
        let n = day - 1;
        NaiveDate::from_ymd_opt(2026, 1, 5).unwrap() + Duration::days((n / 5 * 7 + n % 5) as i64)
    }
    fn cutoff(day: u32, hour: u32) -> DateTime<Utc> {
        // Fixture wall time is Asia/Shanghai (UTC+08:00).
        let utc_hour = (hour + 24 - 8) % 24;
        let utc_date = if hour < 8 {
            d(day) - Duration::days(1)
        } else {
            d(day)
        };
        Utc.from_utc_datetime(&utc_date.and_hms_opt(utc_hour, 0, 0).unwrap())
    }
    fn instrument(id: &str) -> Instrument {
        let code = match id {
            "A" => "600001",
            "B" => "600002",
            "C" => "600003",
            _ => "600000",
        };
        Instrument {
            instrument_id: id.into(),
            exchange: "XSHG".into(),
            code: code.into(),
            name: id.into(),
            asset_type: AssetType::Stock,
        }
    }
    fn event(
        id: &str,
        action: MembershipAction,
        effective: u32,
        published: u32,
        hour: u32,
    ) -> IndexMembershipEvent {
        IndexMembershipEvent {
            instrument: instrument(id),
            action,
            effective_date: d(effective),
            published_at: cutoff(published, hour),
            source_ref: format!("fixture:{id}:{effective}"),
            source_revision_hash: format!("sha256-{id}-{effective}"),
            verified: true,
        }
    }
    fn fixture() -> UniverseDefinition {
        UniverseDefinition {
            universe_id: Uuid::from_u128(1),
            version_id: Uuid::from_u128(2),
            name: "3 security PIT fixture".into(),
            description: "independently hand-calculated membership".into(),
            asset_scope: AssetScope::Stock,
            source_kind: SourceKind::IndexHistory,
            source_ref: "fixture".into(),
            content_hash: "fixture-content".into(),
            manual_members: vec![],
            index_events: vec![
                event("A", MembershipAction::Add, 1, 1, 14),
                event("C", MembershipAction::Add, 1, 1, 14),
                event("B", MembershipAction::Add, 4, 3, 14),
                event("C", MembershipAction::Remove, 6, 5, 16),
                event("A", MembershipAction::Remove, 8, 8, 16),
                event("B", MembershipAction::Remove, 10, 9, 14),
            ],
            coverage: vec![CoverageSegment {
                start: d(1),
                end: d(10),
                status: CoverageStatus::Complete,
                source_revision_hash: "sha256-d1-d10".into(),
            }],
        }
    }

    #[test]
    fn independently_hand_calculated_three_security_ten_day_pit_fixture() {
        let expected = [
            vec!["A", "C"],
            vec!["A", "C"],
            vec!["A", "C"],
            vec!["A", "B", "C"],
            vec!["A", "B", "C"],
            vec!["A", "B"],
            vec!["A", "B"],
            vec!["A", "B"],
            vec!["B"],
            vec![],
        ];
        for day in 1..=10 {
            let got = resolve_members(&fixture(), d(day), cutoff(day, 15), true).unwrap();
            let actual = got
                .members
                .iter()
                .map(|m| m.instrument.instrument_id.as_str())
                .collect::<Vec<_>>();
            assert_eq!(actual, expected[(day - 1) as usize], "D{day}");
            assert_eq!(got.pit_status, PitStatus::VerifiedPit);
        }
    }
    #[test]
    fn coverage_gap_unknown_and_strict_rejection_and_empty_pool() {
        let mut f = fixture();
        f.coverage[0].end = d(5);
        let got = resolve_members(&f, d(6), cutoff(6, 15), false).unwrap();
        assert_eq!(got.coverage, CoverageStatus::Gaps);
        assert_eq!(got.pit_status, PitStatus::Unknown);
        assert!(resolve_members(&f, d(6), cutoff(6, 15), true).is_err());
        let f = fixture();
        assert!(
            resolve_members(&f, d(10), cutoff(10, 15), true)
                .unwrap()
                .members
                .is_empty()
        );
    }
    #[test]
    fn score_filter_is_date_and_member_scoped_without_mutating_panel() {
        let snapshot = resolve_members(&fixture(), d(4), cutoff(4, 15), true).unwrap();
        let scores = HashMap::from([
            ((d(3), "sh600001".into()), 1.0),
            ((d(4), "sh600001".into()), 2.0),
            ((d(4), "sh600002".into()), 3.0),
            ((d(4), "outside".into()), 4.0),
        ]);
        let out = filter_scores_for_snapshot(&scores, &snapshot).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(scores.len(), 4);
    }
    #[test]
    fn cache_key_is_versioned_and_stock_strategy_is_blocked() {
        let a = UniverseCacheKey {
            universe_id: Uuid::from_u128(1),
            version_id: Uuid::from_u128(2),
            content_hash: "h".into(),
            resolution_rule_version: RESOLUTION_RULE_VERSION.into(),
            source_revision_hash: "s".into(),
            start: d(1),
            end: d(10),
            decision_cutoff_rule: "15:00 local".into(),
        };
        let mut b = a.clone();
        b.version_id = Uuid::from_u128(3);
        assert_ne!(a, b);
        assert!(!strategy_capability(AssetScope::Stock).ready);
        assert!(strategy_capability(AssetScope::Etf).ready);
    }
}
