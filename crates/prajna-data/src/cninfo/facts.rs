//! Validation and conversion for the MVP-6 public fact tables.
//!
//! Every populated field is matched against its own cited excerpt. The
//! citation itself is verified against the frozen Raw document by
//! [`super::citations`]; this module additionally requires the recorded value to
//! be recoverable from the cited excerpt, rejects citations that do not belong
//! to a populated field, and rejects a row whose `source_page` is not bound to
//! any of its field citations.

use super::mvp6::{LEGAL_NAME_CHANGE, classify_mvp6, parse_page, required, strip_whitespace};
use super::{
    CatalogLifecycle, Citation, CorporateActionV2, EvidenceLevel, Mvp6Facts, Mvp6Session, Result,
    RiskEventV1, StructuredCorporateActionsV2,
};
use chrono::{Datelike, NaiveDate};
use prajna_domain::Price;
use regex::Regex;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::OnceLock,
};

/// Validate public fact tables against the frozen open-session calendar and
/// the verified field citations, then build versioned consumer inputs.
pub fn validate_and_convert(
    facts: &Mvp6Facts,
    citations: &[Citation],
    calendar: &BTreeSet<NaiveDate>,
) -> Result<Mvp6Session> {
    if calendar.is_empty() {
        return Err("frozen calendar is empty".into());
    }
    let mut index = BTreeMap::<(String, String), &Citation>::new();
    for citation in citations {
        if index
            .insert(
                (
                    citation.fact_source().to_owned(),
                    citation.field().to_owned(),
                ),
                citation,
            )
            .is_some()
        {
            return Err(format!(
                "duplicate citation for {} field {}",
                citation.fact_source(),
                citation.field()
            )
            .into());
        }
    }
    let mut used = BTreeSet::<(String, String)>::new();

    let mut lifecycles = Vec::new();
    for row in &facts.identities {
        code(&row.code)?;
        required(&row.org_id, "org_id")?;
        required(&row.legal_name, "legal_name")?;
        required(&row.fund_category, "fund_category")?;
        let listing = parse_date(&row.listing_date, "listing_date")?;
        let termination = parse_optional_date(&row.termination_date, "termination_date")?;
        let change_kind = match row.change_kind.trim() {
            "" => String::new(),
            LEGAL_NAME_CHANGE => LEGAL_NAME_CHANGE.to_owned(),
            other => return Err(format!("invalid change_kind: {other}").into()),
        };
        if termination.is_some_and(|date| date < listing) {
            return Err(format!("termination_date precedes listing_date for {}", row.code).into());
        }
        validate_date(listing, calendar, "listing_date")?;
        if let Some(date) = termination {
            validate_date(date, calendar, "termination_date")?;
        }
        row_evidence(
            &index,
            &row.source_announcement_id,
            &row.source_page,
            &[
                ("code", row.code.as_str()),
                ("org_id", row.org_id.as_str()),
                ("legal_name", row.legal_name.as_str()),
                ("listing_date", row.listing_date.as_str()),
                ("fund_category", row.fund_category.as_str()),
                ("termination_date", row.termination_date.as_str()),
                ("tracked_index", row.tracked_index.as_str()),
            ],
            &mut used,
        )?;
        // The contract attachment is local evidence for the legal identity. It
        // has no identity.csv column, so #229 consumes it when present and
        // #221 requires it to populate `contract_raw_sha256`.
        if let Some(contract) =
            index.get(&(row.source_announcement_id.clone(), "contract".to_owned()))
        {
            field_supported("legal_name", &row.legal_name, contract.excerpt())?;
            used.insert((row.source_announcement_id.clone(), "contract".to_owned()));
        }
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
            change_kind,
        });
    }
    lifecycles.sort_by(|a, b| (&a.code, a.listing_date).cmp(&(&b.code, b.listing_date)));
    for pair in lifecycles.windows(2) {
        if pair[0].code != pair[1].code {
            continue;
        }
        let overlaps = pair[0]
            .termination_date
            .is_none_or(|end| pair[1].listing_date <= end);
        if !overlaps {
            continue;
        }
        // The one shared exception: an explicitly declared legal-name change
        // may start on a later date without terminating the preceding row.
        let legal_name_change = pair[1].change_kind == LEGAL_NAME_CHANGE
            && pair[0].termination_date.is_none()
            && pair[1].listing_date > pair[0].listing_date
            && pair[0].legal_name != pair[1].legal_name;
        if !legal_name_change {
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
        validate_decimal(&row.cash_per_share, "cash_per_share", false)?;
        validate_decimal(&row.split_ratio, "split_ratio", true)?;
        validate_decimal(&row.liquidation_per_share, "liquidation_per_share", false)?;
        row_evidence(
            &index,
            &row.source_announcement_id,
            &row.source_page,
            &[
                ("code", row.code.as_str()),
                ("kind", row.kind.as_str()),
                ("announcement_date", row.announcement_date.as_str()),
                ("record_date", row.record_date.as_str()),
                ("ex_date", row.ex_date.as_str()),
                ("pay_date", row.pay_date.as_str()),
                ("cash_per_share", row.cash_per_share.as_str()),
                ("split_ratio", row.split_ratio.as_str()),
                ("liquidation_per_share", row.liquidation_per_share.as_str()),
            ],
            &mut used,
        )?;
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
        row_evidence(
            &index,
            &row.source_announcement_id,
            &row.source_page,
            &[
                ("code", row.code.as_str()),
                ("kind", row.kind.as_str()),
                ("announcement_date", row.announcement_date.as_str()),
                ("effective_date", row.effective_date.as_str()),
                ("session_part", row.session_part.as_str()),
            ],
            &mut used,
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

    if let Some((fact_source, field)) = index.keys().find(|key| !used.contains(*key)) {
        return Err(format!(
            "citation for {fact_source} field {field} does not match a populated fact field"
        )
        .into());
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

/// Requires one verified citation per populated field, checks that the field
/// value is supported by the cited excerpt, and requires the row's own
/// `source_page` to appear among its citations.
fn row_evidence(
    index: &BTreeMap<(String, String), &Citation>,
    source_announcement_id: &str,
    source_page: &str,
    fields: &[(&str, &str)],
    used: &mut BTreeSet<(String, String)>,
) -> Result<()> {
    required(source_announcement_id, "source_announcement_id")?;
    let page = parse_page(source_page)?;
    let mut bound = false;
    for (field, value) in fields {
        if value.trim().is_empty() {
            continue;
        }
        let key = (source_announcement_id.to_owned(), (*field).to_owned());
        let citation = index.get(&key).ok_or_else(|| {
            format!("missing Raw citation for {source_announcement_id} field {field}")
        })?;
        field_supported(field, value, citation.excerpt())?;
        used.insert(key);
        if citation.announcement_id() == source_announcement_id && citation.page() == page {
            bound = true;
        }
    }
    if !bound {
        return Err(format!(
            "source_page {page} of {source_announcement_id} is not bound to any cited field"
        )
        .into());
    }
    Ok(())
}

/// Checks that the recorded value is recoverable from the cited excerpt.
/// Literal fields must occur in the excerpt; dates are accepted in the common
/// ISO and Chinese renderings; decimals must appear as an equal number. Coded
/// classification fields are attested by the named verifier instead.
fn field_supported(field: &str, value: &str, excerpt: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Ok(());
    }
    let excerpt = strip_whitespace(excerpt);
    let supported = if field == "kind" {
        kind_supported(value, &excerpt)
    } else if is_coded_field(field) {
        true
    } else if is_date_field(field) {
        let date = NaiveDate::parse_from_str(value, "%Y-%m-%d")
            .map_err(|_| format!("invalid {field}: {value}"))?;
        date_forms(date)
            .iter()
            .any(|form| excerpt.contains(form.as_str()))
    } else if is_decimal_field(field) {
        let expected =
            Price::parse(value).map_err(|_| format!("invalid decimal for {field}: {value}"))?;
        decimal_tokens(&excerpt).any(|token| Price::parse(token).ok() == Some(expected))
    } else {
        excerpt.contains(&strip_whitespace(value))
    };
    if supported {
        Ok(())
    } else {
        Err(format!("field {field} value {value} is not supported by its cited excerpt").into())
    }
}

/// The announcement category must be one the frozen excerpt classifies into.
/// `split` and `conversion` share the 拆分/折算 announcement category.
fn kind_supported(kind: &str, excerpt: &str) -> bool {
    let categories = classify_mvp6(excerpt);
    categories.iter().any(|category| category == kind)
        || (kind == "split" && categories.iter().any(|category| category == "conversion"))
}

/// Classification codes rather than literal source text. The citation still
/// requires a verbatim excerpt on the frozen page and a named verifier.
fn is_coded_field(field: &str) -> bool {
    matches!(field, "fund_category" | "session_part")
}

fn is_date_field(field: &str) -> bool {
    matches!(
        field,
        "listing_date"
            | "termination_date"
            | "announcement_date"
            | "record_date"
            | "ex_date"
            | "pay_date"
            | "effective_date"
    )
}

fn is_decimal_field(field: &str) -> bool {
    matches!(
        field,
        "cash_per_share" | "split_ratio" | "liquidation_per_share"
    )
}

fn date_forms(date: NaiveDate) -> [String; 5] {
    let (year, month, day) = (date.year(), date.month(), date.day());
    [
        format!("{year}-{month:02}-{day:02}"),
        format!("{year}/{month:02}/{day:02}"),
        format!("{year}.{month:02}.{day:02}"),
        format!("{year}年{month}月{day}日"),
        format!("{year}年{month:02}月{day:02}日"),
    ]
}

fn decimal_tokens(text: &str) -> impl Iterator<Item = &str> {
    static DECIMAL: OnceLock<Regex> = OnceLock::new();
    let pattern =
        DECIMAL.get_or_init(|| Regex::new(r"\d+(?:\.\d+)?").expect("static decimal regex"));
    pattern.find_iter(text).map(|value| value.as_str())
}

fn code(value: &str) -> Result<()> {
    if super::scoped_code(value) {
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
