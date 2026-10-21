//! Versioned fixed-list facts for MVP-6. Real source responses stay in the
//! local Raw Store; this module only exposes public facts and synthetic tests.
use super::{Announcement, Result, classify};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceLevel {
    Official,
    Secondary,
}

/// The only accepted non-empty `change_kind`: the row starts a new instrument
/// identity because the legal full name changed. The preceding same-code row
/// must be unterminated and carry a different legal name.
pub const LEGAL_NAME_CHANGE: &str = "legal_name_change";

impl EvidenceLevel {
    pub(super) fn parse(value: &str) -> Result<Self> {
        match value {
            "official" => Ok(Self::Official),
            "secondary" => Ok(Self::Secondary),
            _ => Err(format!("invalid evidence_level: {value}").into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityFact {
    pub code: String,
    pub org_id: String,
    pub legal_name: String,
    pub listing_date: String,
    pub termination_date: String,
    pub fund_category: String,
    pub tracked_index: String,
    pub evidence_level: String,
    pub source_announcement_id: String,
    pub source_page: String,
    /// Empty for an initial identity, or `legal_name_change` when this row
    /// starts a new instrument identity because the legal full name changed.
    /// #229 permits that one shape to overlap the preceding unterminated row.
    #[serde(default)]
    pub change_kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionFact {
    pub code: String,
    pub kind: String,
    pub announcement_date: String,
    pub record_date: String,
    pub ex_date: String,
    pub pay_date: String,
    pub cash_per_share: String,
    pub split_ratio: String,
    pub liquidation_per_share: String,
    pub evidence_level: String,
    pub source_announcement_id: String,
    pub source_page: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventFact {
    pub code: String,
    pub kind: String,
    pub announcement_date: String,
    pub effective_date: String,
    pub session_part: String,
    pub evidence_level: String,
    pub source_announcement_id: String,
    pub source_page: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mvp6Facts {
    pub identities: Vec<IdentityFact>,
    pub actions: Vec<ActionFact>,
    pub events: Vec<EventFact>,
}

impl Mvp6Facts {
    pub fn from_csv(identity: &str, actions: &str, events: &str) -> Result<Self> {
        Ok(Self {
            identities: read_identity_csv(identity)?,
            actions: read_csv(actions, ACTION_HEADERS)?,
            events: Self::events_from_csv(events)?,
        })
    }

    /// Parse only opening-event facts when a consumer does not need identity
    /// or corporate-action facts.
    pub fn events_from_csv(events: &str) -> Result<Vec<EventFact>> {
        read_csv(events, EVENT_HEADERS)
    }
}

const IDENTITY_HEADERS: &[&str] = &[
    "code",
    "org_id",
    "legal_name",
    "listing_date",
    "termination_date",
    "fund_category",
    "tracked_index",
    "evidence_level",
    "source_announcement_id",
    "source_page",
    "change_kind",
];
const IDENTITY_HEADERS_LEGACY: &[&str] = &[
    "code",
    "org_id",
    "legal_name",
    "listing_date",
    "termination_date",
    "fund_category",
    "tracked_index",
    "evidence_level",
    "source_announcement_id",
    "source_page",
];
const ACTION_HEADERS: &[&str] = &[
    "code",
    "kind",
    "announcement_date",
    "record_date",
    "ex_date",
    "pay_date",
    "cash_per_share",
    "split_ratio",
    "liquidation_per_share",
    "evidence_level",
    "source_announcement_id",
    "source_page",
];
const EVENT_HEADERS: &[&str] = &[
    "code",
    "kind",
    "announcement_date",
    "effective_date",
    "session_part",
    "evidence_level",
    "source_announcement_id",
    "source_page",
];

/// The `change_kind` column is an additive extension of the #229 header. A
/// local table written before the column existed still parses as an initial
/// identity; rows that need a legal-name change must add the column.
fn read_identity_csv(text: &str) -> Result<Vec<IdentityFact>> {
    let mut reader = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_reader(text.as_bytes());
    let headers = reader.headers()?.iter().collect::<Vec<_>>();
    if headers != IDENTITY_HEADERS && headers != IDENTITY_HEADERS_LEGACY {
        return Err(format!(
            "CSV header mismatch: expected {IDENTITY_HEADERS:?} or the legacy header without change_kind, got {headers:?}"
        )
        .into());
    }
    reader
        .deserialize()
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn read_csv<T: for<'de> Deserialize<'de>>(text: &str, expected: &[&str]) -> Result<Vec<T>> {
    let mut reader = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_reader(text.as_bytes());
    let headers = reader.headers()?.iter().collect::<Vec<_>>();
    if headers != expected {
        return Err(format!("CSV header mismatch: expected {expected:?}, got {headers:?}").into());
    }
    reader
        .deserialize()
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogLifecycle {
    pub code: String,
    pub org_id: String,
    pub legal_name: String,
    pub listing_date: NaiveDate,
    pub termination_date: Option<NaiveDate>,
    pub fund_category: String,
    pub tracked_index: String,
    pub evidence_level: EvidenceLevel,
    pub source_announcement_id: String,
    pub source_page: u32,
    /// Empty for an initial identity, or `legal_name_change`. Serialized only
    /// when set so existing facts-session JSON stays unchanged.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub change_kind: String,
    /// First Session on which a legal-name change is knowable (P6). The
    /// successor's `listing_date` remains the new name's effective Session;
    /// the physical range starts here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_from: Option<NaiveDate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructuredCorporateActionsV2 {
    pub schema: String,
    pub version: u32,
    pub actions: Vec<CorporateActionV2>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorporateActionV2 {
    pub code: String,
    pub kind: String,
    pub announcement_date: NaiveDate,
    pub record_date: Option<NaiveDate>,
    pub ex_date: Option<NaiveDate>,
    pub pay_date: Option<NaiveDate>,
    pub cash_per_share: Option<String>,
    pub split_ratio: Option<String>,
    pub liquidation_per_share: Option<String>,
    pub evidence_level: EvidenceLevel,
    pub source_announcement_id: String,
    pub source_page: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskEventV1 {
    pub code: String,
    pub kind: String,
    pub announcement_date: NaiveDate,
    pub effective_date: NaiveDate,
    pub session_part: String,
    pub evidence_level: EvidenceLevel,
    pub source_announcement_id: String,
    pub source_page: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mvp6Session {
    pub catalog_lifecycles: Vec<CatalogLifecycle>,
    pub corporate_actions: StructuredCorporateActionsV2,
    pub risk_events: Vec<RiskEventV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgIdResolution {
    pub code: String,
    pub org_ids: Vec<String>,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixedListResult {
    pub resolutions: Vec<OrgIdResolution>,
    pub announcements: BTreeMap<(String, String), Vec<Announcement>>,
    pub pdf_raw_sha256_by_announcement: BTreeMap<String, Vec<String>>,
    pub unresolved: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FundCandidate {
    pub code: String,
    pub short_name: String,
    pub active_start: NaiveDate,
    pub active_end: NaiveDate,
}

/// Keep the existing four-category collector behavior intact. New MVP-6
/// collection uses this explicit allowlist and skips periodic reports.
pub fn should_download_pdf(
    categories: &[String],
    listing_notice_complete: bool,
    explicit: bool,
) -> bool {
    if categories.iter().any(|c| c == "periodic_report") {
        return false;
    }
    let relevant = [
        "dividend",
        "liquidation",
        "suspension",
        "resumption",
        "listing_notice",
        "conversion",
        "holders_meeting",
        "termination",
        "liquidation_report",
    ];
    if categories.iter().any(|c| relevant.contains(&c.as_str())) {
        return true;
    }
    let supplementary = categories
        .iter()
        .any(|c| c == "fund_contract" || c == "prospectus");
    supplementary && (explicit || !listing_notice_complete)
}

/// Announcement categories used by the fixed-list collector. `kind` values in
/// the fact tables must be one of these categories for the frozen excerpt, so a
/// classification cannot be asserted without source text that supports it.
pub(super) fn classify_mvp6(title: &str) -> Vec<String> {
    let mut categories = classify(title);
    let extra = [
        (
            "conversion",
            &["份额拆分", "基金份额拆分", "拆分"] as &[&str],
        ),
        ("holders_meeting", &["计票结果", "表决结果", "表决情况"]),
        ("termination", &["终止上市", "终止基金合同"]),
        ("liquidation_report", &["清算报告"]),
        ("fund_contract", &["基金合同"]),
        ("prospectus", &["招募说明书"]),
    ];
    for (category, words) in extra {
        if words.iter().any(|word| title.contains(word))
            && !categories.iter().any(|c| c == category)
        {
            categories.push(category.into());
        }
    }
    categories.sort();
    categories.dedup();
    categories
}

pub(super) fn required(value: &str, field: &str) -> Result<()> {
    if value.trim().is_empty() {
        Err(format!("required field {field} is empty").into())
    } else {
        Ok(())
    }
}

pub(super) fn parse_page(value: &str) -> Result<u32> {
    let page = value
        .parse::<u32>()
        .map_err(|_| format!("invalid source_page: {value}"))?;
    if page == 0 {
        Err("source_page must be positive".into())
    } else {
        Ok(page)
    }
}

/// Removes all whitespace so source excerpts copied from wrapped PDF text can
/// be matched literally.
pub(super) fn strip_whitespace(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}
