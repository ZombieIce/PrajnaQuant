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

/// The declared observation window and the frozen sessions inside it.
///
/// Publication dates may fall anywhere inside the declared window, including
/// days that are not sessions; trade-related dates must be frozen sessions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactCalendar {
    pub sessions: BTreeSet<NaiveDate>,
    pub window_start: NaiveDate,
    pub window_end: NaiveDate,
}

impl FactCalendar {
    pub fn new(
        sessions: BTreeSet<NaiveDate>,
        window_start: NaiveDate,
        window_end: NaiveDate,
    ) -> Result<Self> {
        if sessions.is_empty() {
            return Err("frozen calendar is empty".into());
        }
        if window_start > window_end {
            return Err("fact window start is after its end".into());
        }
        Ok(Self {
            sessions,
            window_start,
            window_end,
        })
    }
}

/// Validate public fact tables against the frozen open-session calendar and
/// the verified field citations, then build versioned consumer inputs.
pub fn validate_and_convert(
    facts: &Mvp6Facts,
    citations: &[Citation],
    calendar: &FactCalendar,
) -> Result<Mvp6Session> {
    if calendar.sessions.is_empty() {
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
        validate_lifecycle_date(listing, calendar, "listing_date")?;
        if let Some(date) = termination {
            validate_lifecycle_date(date, calendar, "termination_date")?;
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
        validate_publication_date(announcement, calendar, "announcement_date")?;
        let record = parse_optional_date(&row.record_date, "record_date")?;
        let ex = parse_optional_date(&row.ex_date, "ex_date")?;
        let pay = parse_optional_date(&row.pay_date, "pay_date")?;
        for (name, date) in [("record_date", record), ("ex_date", ex), ("pay_date", pay)] {
            if let Some(date) = date {
                validate_date(date, &calendar.sessions, name)?;
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
        validate_publication_date(announcement, calendar, "announcement_date")?;
        validate_date(effective, &calendar.sessions, "effective_date")?;
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
    } else if field == "cash_per_share" {
        cash_per_share_supported(value, &excerpt)
    } else if field == "split_ratio" {
        split_ratio_supported(value, &excerpt)
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

/// A split ratio is the number of new shares per old share. Bind its direction
/// to the `old:new` pair on the page so a `2:1` reverse split cannot satisfy a
/// recorded `2`. Ratios without an explicit pair are not accepted.
fn split_ratio_supported(value: &str, excerpt: &str) -> bool {
    let Ok(expected) = Price::parse(value) else {
        return false;
    };
    static RATIO: OnceLock<Regex> = OnceLock::new();
    let pattern = RATIO.get_or_init(|| {
        Regex::new(r"(\d+(?:\.\d+)?):(\d+(?:\.\d+)?)").expect("static ratio regex")
    });
    pattern.captures_iter(excerpt).any(|captures| {
        let left = captures
            .get(1)
            .and_then(|value| Price::parse(value.as_str()).ok());
        let right = captures
            .get(2)
            .and_then(|value| Price::parse(value.as_str()).ok());
        matches!((left, right), (Some(left), Some(right))
            if left.checked_mul_decimal(expected).ok() == Some(right))
    })
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
    matches!(field, "split_ratio" | "liquidation_per_share")
}

/// cninfo distribution notices state the cash amount per ten fund shares
/// (`元/10份基金份额`), while the fact table records the amount per share.
/// Accept the cited number only when the excerpt binds it to that explicit
/// per-ten-shares unit and it equals exactly ten times the recorded value. No
/// other unit conversion is inferred.
fn cash_per_share_supported(value: &str, excerpt: &str) -> bool {
    let Ok(expected) = Price::parse(value) else {
        return false;
    };
    if decimal_tokens(excerpt).any(|token| Price::parse(token).ok() == Some(expected)) {
        return true;
    }
    static PER_TEN_SHARES: OnceLock<Regex> = OnceLock::new();
    let pattern = PER_TEN_SHARES.get_or_init(|| {
        Regex::new(r"元/10份[^0-9]{0,8}(\d+(?:\.\d+)?)").expect("static per-ten regex")
    });
    let Some(captured) = pattern
        .captures(excerpt)
        .and_then(|captures| captures.get(1))
    else {
        return false;
    };
    let Ok(per_ten_shares) = expected.checked_mul(10) else {
        return false;
    };
    Price::parse(captured.as_str()).ok() == Some(per_ten_shares)
}

fn date_forms(date: NaiveDate) -> [String; 6] {
    let (year, month, day) = (date.year(), date.month(), date.day());
    [
        format!("{year}-{month:02}-{day:02}"),
        format!("{year}/{month:02}/{day:02}"),
        format!("{year}.{month:02}.{day:02}"),
        format!("{year}年{month}月{day}日"),
        format!("{year}年{month:02}月{day:02}日"),
        // Official notices also sign and date themselves with Chinese numerals
        // (`二〇二六年一月五日`).
        format!(
            "{}年{}月{}日",
            chinese_year(year),
            chinese_number(month),
            chinese_number(day)
        ),
    ]
}

fn chinese_year(year: i32) -> String {
    year.to_string()
        .chars()
        .map(|digit| match digit {
            '0' => '〇',
            '1' => '一',
            '2' => '二',
            '3' => '三',
            '4' => '四',
            '5' => '五',
            '6' => '六',
            '7' => '七',
            '8' => '八',
            '9' => '九',
            other => other,
        })
        .collect()
}

/// Renders 1..=31 as a Chinese numeral. Values outside a calendar month or day
/// fall back to ASCII, which never matches a numeral-signed source date.
fn chinese_number(value: u32) -> String {
    const DIGITS: [&str; 10] = ["零", "一", "二", "三", "四", "五", "六", "七", "八", "九"];
    match value {
        0..=9 => DIGITS[value as usize].to_owned(),
        10 => "十".to_owned(),
        11..=19 => format!("十{}", DIGITS[(value - 10) as usize]),
        20 => "二十".to_owned(),
        21..=29 => format!("二十{}", DIGITS[(value - 20) as usize]),
        30 => "三十".to_owned(),
        31 => "三十一".to_owned(),
        other => other.to_string(),
    }
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

/// Publication dates are calendar dates, not exchange sessions: funds publish
/// notices on weekends and holidays. The date must still fall inside the
/// declared observation window, not merely between the first and last session.
fn validate_publication_date(date: NaiveDate, calendar: &FactCalendar, field: &str) -> Result<()> {
    if date >= calendar.window_start && date <= calendar.window_end {
        Ok(())
    } else {
        Err(format!("{field} outside declared window: {date}").into())
    }
}

fn validate_date(date: NaiveDate, sessions: &BTreeSet<NaiveDate>, field: &str) -> Result<()> {
    if sessions.contains(&date) {
        Ok(())
    } else {
        Err(format!("{field} outside frozen calendar: {date}").into())
    }
}

/// Lifecycle boundaries can predate the frozen market-data window: the MVP-6
/// fixed list includes funds listed years before 2022. Their evidence is the
/// cited official listing/termination notice, so the frozen sessions can only
/// confirm boundaries that fall inside the window. Trade-related action and
/// event dates stay strictly inside the frozen sessions; publication dates are
/// checked by [`validate_publication_date`].
fn validate_lifecycle_date(date: NaiveDate, calendar: &FactCalendar, field: &str) -> Result<()> {
    if calendar.sessions.contains(&date)
        || date < calendar.window_start
        || date > calendar.window_end
    {
        Ok(())
    } else {
        Err(format!("{field} outside frozen calendar: {date}").into())
    }
}

fn empty_to_none(value: &str) -> Option<String> {
    (!value.trim().is_empty()).then(|| value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_ten_share_amount_is_accepted_only_with_the_explicit_unit() {
        assert!(
            field_supported(
                "cash_per_share",
                "0.0453",
                "本次分红方案（单位：元/10 份基金份额）0.4530"
            )
            .is_ok()
        );
        assert!(
            field_supported(
                "cash_per_share",
                "0.0453",
                "本次分红方案（单位：元/10份基0.4530金份额）"
            )
            .is_ok()
        );
        assert!(field_supported("cash_per_share", "0.0453", "每份派发0.0453元").is_ok());
        assert!(field_supported("cash_per_share", "0.0453", "分红金额0.4530元").is_err());
        // The number must follow the explicit per-ten unit, not merely share a page.
        assert!(field_supported("cash_per_share", "1", "（单位：元/10份基金份额）").is_err());
        assert!(field_supported("cash_per_share", "0.0453", "0.4530元/10份").is_err());
        assert!(field_supported("cash_per_share", "0.0453", "0.4530元收益").is_err());
    }

    #[test]
    fn split_ratio_binds_direction_to_the_old_new_pair() {
        assert!(field_supported("split_ratio", "2", "本次拆分比例为1:2").is_ok());
        assert!(field_supported("split_ratio", "1.5", "本次拆分比例为2:3").is_ok());
        assert!(field_supported("split_ratio", "1.5", "本次拆分比例为3:2").is_err());
        assert!(field_supported("split_ratio", "2", "本次拆分比例为2:1").is_err());
        assert!(field_supported("split_ratio", "2", "每份基金份额拆分为2份").is_err());
    }

    fn window() -> FactCalendar {
        let sessions = ["2026-01-05", "2026-01-07"]
            .into_iter()
            .map(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d").unwrap())
            .collect::<BTreeSet<_>>();
        FactCalendar::new(
            sessions,
            NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
            NaiveDate::from_ymd_opt(2026, 1, 7).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn lifecycle_boundaries_may_predate_the_declared_window_but_not_gap_inside_it() {
        let calendar = window();
        assert!(
            validate_lifecycle_date(
                NaiveDate::from_ymd_opt(2012, 5, 28).unwrap(),
                &calendar,
                "listing_date"
            )
            .is_ok()
        );
        assert!(
            validate_lifecycle_date(
                NaiveDate::from_ymd_opt(2026, 1, 6).unwrap(),
                &calendar,
                "listing_date"
            )
            .is_err()
        );
        assert!(
            validate_lifecycle_date(
                NaiveDate::from_ymd_opt(2026, 1, 7).unwrap(),
                &calendar,
                "listing_date"
            )
            .is_ok()
        );
    }

    #[test]
    fn publication_dates_use_the_declared_window_not_the_session_span() {
        let calendar = window();
        let non_session = NaiveDate::from_ymd_opt(2026, 1, 6).unwrap();
        assert!(validate_publication_date(non_session, &calendar, "announcement_date").is_ok());
        assert!(validate_date(non_session, &calendar.sessions, "ex_date").is_err());
        assert!(
            validate_publication_date(
                NaiveDate::from_ymd_opt(2026, 1, 8).unwrap(),
                &calendar,
                "announcement_date"
            )
            .is_err()
        );

        // A window may open before the first frozen session (the MVP-6 window
        // starts on 2022-10-01 but the first session is 2022-10-10).
        let early = FactCalendar::new(
            calendar.sessions.clone(),
            NaiveDate::from_ymd_opt(2026, 1, 4).unwrap(),
            calendar.window_end,
        )
        .unwrap();
        assert!(
            validate_publication_date(
                NaiveDate::from_ymd_opt(2026, 1, 4).unwrap(),
                &early,
                "announcement_date"
            )
            .is_ok()
        );
        assert!(
            validate_publication_date(
                NaiveDate::from_ymd_opt(2026, 1, 3).unwrap(),
                &early,
                "announcement_date"
            )
            .is_err()
        );
    }

    #[test]
    fn dates_accept_chinese_numerals_from_official_signatures() {
        let date = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        assert!(date_forms(date).contains(&"二〇二六年一月五日".to_owned()));
        assert!(
            field_supported(
                "announcement_date",
                "2026-01-05",
                "公告日：二〇二六年一月五日"
            )
            .is_ok()
        );
        assert!(
            field_supported(
                "announcement_date",
                "2026-01-05",
                "本基金将于2026年1月5日开市起停牌"
            )
            .is_ok()
        );
        assert!(field_supported("announcement_date", "2026-01-05", "二〇二六年一月六日").is_err());
    }
}
