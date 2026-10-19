//! Versioned fixed-list facts for MVP-6. Real source responses stay in the
//! local Raw Store; this module only exposes public facts and synthetic tests.
use super::{Result, scoped_code};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceLevel {
    Official,
    Secondary,
}

impl EvidenceLevel {
    fn parse(value: &str) -> Result<Self> {
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
pub struct RawCitation {
    pub announcement_id: String,
    pub page: u32,
    /// Field names supported by this locally frozen Raw page.
    pub evidenced_fields: BTreeSet<String>,
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
            identities: read_csv(identity, IDENTITY_HEADERS)?,
            actions: read_csv(actions, ACTION_HEADERS)?,
            events: read_csv(events, EVENT_HEADERS)?,
        })
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

/// Validate public fact tables against the local frozen Raw inventory and
/// frozen open-session calendar, then build versioned consumer inputs.
pub fn validate_and_convert(
    facts: &Mvp6Facts,
    raw: &[RawCitation],
    calendar: &BTreeSet<NaiveDate>,
) -> Result<Mvp6Session> {
    if calendar.is_empty() {
        return Err("frozen calendar is empty".into());
    }
    let raw = raw
        .iter()
        .map(|r| ((r.announcement_id.as_str(), r.page), r))
        .collect::<BTreeMap<_, _>>();
    let mut lifecycles = Vec::new();
    for row in &facts.identities {
        code(&row.code)?;
        required(&row.org_id, "org_id")?;
        required(&row.legal_name, "legal_name")?;
        required(&row.fund_category, "fund_category")?;
        let listing = parse_date(&row.listing_date, "listing_date")?;
        let termination = parse_optional_date(&row.termination_date, "termination_date")?;
        if termination.is_some_and(|date| date < listing) {
            return Err(format!("termination_date precedes listing_date for {}", row.code).into());
        }
        validate_date(listing, calendar, "listing_date")?;
        if let Some(date) = termination {
            validate_date(date, calendar, "termination_date")?;
        }
        let source = citation(
            &row.source_announcement_id,
            &row.source_page,
            &raw,
            &[
                "code",
                "org_id",
                "legal_name",
                "listing_date",
                "termination_date",
                "fund_category",
                "tracked_index",
            ],
        )?;
        require_evidence_if_populated(&row.termination_date, "termination_date", source)?;
        require_evidence_if_populated(&row.tracked_index, "tracked_index", source)?;
        lifecycles.push(CatalogLifecycle {
            code: row.code.clone(),
            org_id: row.org_id.clone(),
            legal_name: row.legal_name.clone(),
            listing_date: listing,
            termination_date: termination,
            fund_category: row.fund_category.clone(),
            tracked_index: row.tracked_index.clone(),
            evidence_level: EvidenceLevel::parse(&row.evidence_level)?,
            source_announcement_id: row.source_announcement_id.clone(),
            source_page: parse_page(&row.source_page)?,
        });
    }
    lifecycles.sort_by(|a, b| (&a.code, a.listing_date).cmp(&(&b.code, b.listing_date)));
    for pair in lifecycles.windows(2) {
        if pair[0].code == pair[1].code
            && pair[0]
                .termination_date
                .is_none_or(|end| pair[1].listing_date <= end)
        {
            return Err(format!("overlapping lifecycle for {}", pair[0].code).into());
        }
    }

    let mut converted_actions = Vec::new();
    for row in &facts.actions {
        code(&row.code)?;
        required(&row.kind, "kind")?;
        let announcement = parse_date(&row.announcement_date, "announcement_date")?;
        validate_date(announcement, calendar, "announcement_date")?;
        let record = parse_optional_date(&row.record_date, "record_date")?;
        let ex = parse_optional_date(&row.ex_date, "ex_date")?;
        let pay = parse_optional_date(&row.pay_date, "pay_date")?;
        for (name, date) in [("record_date", record), ("ex_date", ex), ("pay_date", pay)] {
            if let Some(date) = date {
                validate_date(date, calendar, name)?;
            }
        }
        let source = citation(
            &row.source_announcement_id,
            &row.source_page,
            &raw,
            &[
                "code",
                "kind",
                "announcement_date",
                "record_date",
                "ex_date",
                "pay_date",
                "cash_per_share",
                "split_ratio",
                "liquidation_per_share",
            ],
        )?;
        for (field, value) in [
            ("record_date", row.record_date.as_str()),
            ("ex_date", row.ex_date.as_str()),
            ("pay_date", row.pay_date.as_str()),
            ("cash_per_share", row.cash_per_share.as_str()),
            ("split_ratio", row.split_ratio.as_str()),
            ("liquidation_per_share", row.liquidation_per_share.as_str()),
        ] {
            require_evidence_if_populated(value, field, source)?;
        }
        validate_decimal(&row.cash_per_share, "cash_per_share", false)?;
        validate_decimal(&row.split_ratio, "split_ratio", true)?;
        validate_decimal(&row.liquidation_per_share, "liquidation_per_share", false)?;
        converted_actions.push(CorporateActionV2 {
            code: row.code.clone(),
            kind: row.kind.clone(),
            announcement_date: announcement,
            record_date: record,
            ex_date: ex,
            pay_date: pay,
            cash_per_share: empty_to_none(&row.cash_per_share),
            split_ratio: empty_to_none(&row.split_ratio),
            liquidation_per_share: empty_to_none(&row.liquidation_per_share),
            evidence_level: EvidenceLevel::parse(&row.evidence_level)?,
            source_announcement_id: row.source_announcement_id.clone(),
            source_page: parse_page(&row.source_page)?,
        });
    }

    let mut risk_events = Vec::new();
    for row in &facts.events {
        code(&row.code)?;
        required(&row.kind, "kind")?;
        if !matches!(row.session_part.as_str(), "open" | "morning" | "full_day") {
            return Err(format!("invalid session_part: {}", row.session_part).into());
        }
        let announcement = parse_date(&row.announcement_date, "announcement_date")?;
        let effective = parse_date(&row.effective_date, "effective_date")?;
        validate_date(announcement, calendar, "announcement_date")?;
        validate_date(effective, calendar, "effective_date")?;
        citation(
            &row.source_announcement_id,
            &row.source_page,
            &raw,
            &[
                "code",
                "kind",
                "announcement_date",
                "effective_date",
                "session_part",
            ],
        )?;
        risk_events.push(RiskEventV1 {
            code: row.code.clone(),
            kind: row.kind.clone(),
            announcement_date: announcement,
            effective_date: effective,
            session_part: row.session_part.clone(),
            evidence_level: EvidenceLevel::parse(&row.evidence_level)?,
            source_announcement_id: row.source_announcement_id.clone(),
            source_page: parse_page(&row.source_page)?,
        });
    }
    Ok(Mvp6Session {
        catalog_lifecycles: lifecycles,
        corporate_actions: StructuredCorporateActionsV2 {
            schema: "cninfo_corporateActions_structured_v2".into(),
            version: 2,
            actions: converted_actions,
        },
        risk_events,
    })
}

fn citation<'a>(
    id: &str,
    page: &str,
    raw: &'a BTreeMap<(&str, u32), &RawCitation>,
    fields: &[&str],
) -> Result<&'a RawCitation> {
    required(id, "source_announcement_id")?;
    let page = parse_page(page)?;
    let citation = raw
        .get(&(id, page))
        .ok_or_else(|| format!("missing Raw citation {id} page {page}"))?;
    let row = citation;
    let required_fields = fields.iter().copied().filter(|f| {
        !matches!(
            *f,
            "termination_date"
                | "tracked_index"
                | "record_date"
                | "ex_date"
                | "pay_date"
                | "cash_per_share"
                | "split_ratio"
                | "liquidation_per_share"
        )
    });
    for field in required_fields {
        if !row.evidenced_fields.contains(field) {
            return Err(format!("field {field} is not evidenced by {id} page {page}").into());
        }
    }
    Ok(row)
}

fn require_evidence_if_populated(value: &str, field: &str, raw: &RawCitation) -> Result<()> {
    if !value.trim().is_empty() && !raw.evidenced_fields.contains(field) {
        return Err(format!(
            "populated field {field} is not evidenced by {} page {}",
            raw.announcement_id, raw.page
        )
        .into());
    }
    Ok(())
}

fn code(value: &str) -> Result<()> {
    if scoped_code(value) {
        Ok(())
    } else {
        Err(format!("invalid fund code: {value}").into())
    }
}
fn validate_decimal(value: &str, field: &str, must_be_positive: bool) -> Result<()> {
    if value.trim().is_empty() {
        return Ok(());
    }
    let mut decimal_seen = false;
    let mut digits = 0usize;
    for character in value.chars() {
        if character == '.' && !decimal_seen {
            decimal_seen = true;
            continue;
        }
        if !character.is_ascii_digit() {
            return Err(format!("invalid decimal for {field}").into());
        }
        digits += 1;
    }
    if digits == 0 || value.starts_with('.') || value.ends_with('.') {
        return Err(format!("invalid decimal for {field}").into());
    }
    if must_be_positive && value.chars().all(|c| !c.is_ascii_digit() || c == '0') {
        return Err(format!("{field} must be positive").into());
    }
    Ok(())
}
fn required(value: &str, field: &str) -> Result<()> {
    if value.trim().is_empty() {
        Err(format!("required field {field} is empty").into())
    } else {
        Ok(())
    }
}
fn parse_page(value: &str) -> Result<u32> {
    let page = value
        .parse::<u32>()
        .map_err(|_| format!("invalid source_page: {value}"))?;
    if page == 0 {
        Err("source_page must be positive".into())
    } else {
        Ok(page)
    }
}
fn parse_date(value: &str, field: &str) -> Result<NaiveDate> {
    required(value, field)?;
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| format!("invalid {field}: {value}").into())
}
fn parse_optional_date(value: &str, field: &str) -> Result<Option<NaiveDate>> {
    if value.trim().is_empty() {
        Ok(None)
    } else {
        parse_date(value, field).map(Some)
    }
}
fn validate_date(date: NaiveDate, calendar: &BTreeSet<NaiveDate>, field: &str) -> Result<()> {
    if calendar.contains(&date) {
        Ok(())
    } else {
        Err(format!("{field} outside frozen calendar: {date}").into())
    }
}
fn empty_to_none(value: &str) -> Option<String> {
    (!value.trim().is_empty()).then(|| value.to_owned())
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
    pub announcements: BTreeMap<(String, String), Vec<super::Announcement>>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrosscheckMismatch {
    pub code: String,
    pub kind: String,
    pub expected_date: Option<NaiveDate>,
    pub observed_date: Option<NaiveDate>,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrosscheckReport {
    pub mismatches: Vec<CrosscheckMismatch>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct V4DailyBar {
    pub code: String,
    pub date: NaiveDate,
    pub close: Option<f64>,
    pub previous_close: Option<f64>,
    pub keyed_no_trade: bool,
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

/// Compare public facts with a synthetic or locally frozen v4 row set. The
/// function returns discrepancies only; it never rewrites either input.
pub fn crosscheck_v4(facts: &Mvp6Facts, bars: &[V4DailyBar]) -> CrosscheckReport {
    let mut mismatches = Vec::new();
    for identity in &facts.identities {
        let mut rows = bars
            .iter()
            .filter(|b| b.code == identity.code)
            .collect::<Vec<_>>();
        rows.sort_by_key(|b| b.date);
        let first = rows.first().map(|b| b.date);
        let last = rows.last().map(|b| b.date);
        if let Ok(expected) = NaiveDate::parse_from_str(&identity.listing_date, "%Y-%m-%d") {
            if first != Some(expected) {
                mismatches.push(mismatch(
                    &identity.code,
                    "listing_vs_first_bar",
                    Some(expected),
                    first,
                ));
            }
        }
        if let Ok(expected) = NaiveDate::parse_from_str(&identity.termination_date, "%Y-%m-%d") {
            if last != Some(expected) {
                mismatches.push(mismatch(
                    &identity.code,
                    "termination_vs_last_bar",
                    Some(expected),
                    last,
                ));
            }
        }
    }
    for action in &facts.actions {
        if !matches!(action.kind.as_str(), "dividend" | "split" | "conversion") {
            continue;
        }
        let Ok(expected) = NaiveDate::parse_from_str(&action.ex_date, "%Y-%m-%d") else {
            continue;
        };
        if let Some(bar) = bars
            .iter()
            .find(|b| b.code == action.code && b.date == expected)
        {
            let jump = bar
                .close
                .zip(bar.previous_close)
                .is_some_and(|(close, previous)| {
                    previous > 0.0 && (close - previous).abs() >= 0.001
                });
            if !jump {
                mismatches.push(mismatch(
                    &action.code,
                    "action_date_vs_previous_close_jump",
                    Some(expected),
                    Some(bar.date),
                ));
            }
        } else {
            mismatches.push(mismatch(
                &action.code,
                "action_date_missing_bar",
                Some(expected),
                None,
            ));
        }
    }
    for event in &facts.events {
        if event.kind != "suspension" {
            continue;
        }
        let Ok(expected) = NaiveDate::parse_from_str(&event.effective_date, "%Y-%m-%d") else {
            continue;
        };
        if let Some(bar) = bars
            .iter()
            .find(|b| b.code == event.code && b.date == expected)
        {
            if !bar.keyed_no_trade {
                mismatches.push(mismatch(
                    &event.code,
                    "suspension_vs_keyed_no_trade",
                    Some(expected),
                    Some(bar.date),
                ));
            }
        } else {
            mismatches.push(mismatch(
                &event.code,
                "suspension_missing_bar",
                Some(expected),
                None,
            ));
        }
    }
    for bar in bars.iter().filter(|bar| bar.keyed_no_trade) {
        let has_suspension = facts.events.iter().any(|event| {
            event.kind == "suspension"
                && event.code == bar.code
                && NaiveDate::parse_from_str(&event.effective_date, "%Y-%m-%d")
                    .is_ok_and(|date| date == bar.date)
        });
        if !has_suspension {
            mismatches.push(mismatch(
                &bar.code,
                "keyed_no_trade_vs_suspension",
                Some(bar.date),
                Some(bar.date),
            ));
        }
    }
    mismatches.sort_by(|a, b| {
        (&a.code, &a.kind, a.expected_date).cmp(&(&b.code, &b.kind, b.expected_date))
    });
    CrosscheckReport { mismatches }
}
fn mismatch(
    code: &str,
    kind: &str,
    expected_date: Option<NaiveDate>,
    observed_date: Option<NaiveDate>,
) -> CrosscheckMismatch {
    CrosscheckMismatch {
        code: code.into(),
        kind: kind.into(),
        expected_date,
        observed_date,
        detail: kind.into(),
    }
}
