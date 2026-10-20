//! Field-level evidence citations for MVP-6 public facts.
//!
//! A citation binds one fact field to a verbatim excerpt on a specific page of
//! a frozen Raw document. The excerpt must occur on that page after whitespace
//! normalization, and the document must reference the cited announcement. Each
//! citation also names the operator who verified it and when; that verification
//! cannot predate the local capture of the document.
//!
//! Coded classification fields (`kind`, `fund_category`, `session_part`) are not
//! literal source text. For those fields the verbatim excerpt plus the named,
//! timestamped verifier is the checked human verification: the verifier
//! attests the mapping, and the excerpt must still be on the cited page.

use super::mvp6::{required, strip_whitespace};
use super::{PdfText, Result};
use crate::RawStore;
use chrono::{DateTime, FixedOffset, NaiveDate};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

const CITATION_HEADERS: &[&str] = &[
    "fact_source",
    "field",
    "announcement_id",
    "page",
    "raw_sha256",
    "excerpt",
    "verified_by",
    "verified_at",
];

/// Every fact-table field that must carry its own cited evidence. Provenance
/// ratings (`evidence_level`) are not source facts and are not cited.
/// `contract` is the fund-contract attachment that proves the legal identity;
/// it is attached to an identity row but has no column of its own.
const CITED_FIELDS: &[&str] = &[
    "code",
    "org_id",
    "legal_name",
    "listing_date",
    "termination_date",
    "fund_category",
    "tracked_index",
    "contract",
    "kind",
    "announcement_date",
    "record_date",
    "ex_date",
    "pay_date",
    "cash_per_share",
    "split_ratio",
    "liquidation_per_share",
    "effective_date",
    "session_part",
];

/// Caller-supplied data for one citation. Build the citation with
/// [`Citation::from_store`], which verifies the document, page, excerpt and
/// verifier before the citation can be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CitationInput {
    /// `source_announcement_id` of the fact row that uses this field.
    pub fact_source: String,
    pub field: String,
    /// Frozen document that states the field. It may differ from `fact_source`
    /// when a row's fields come from different documents.
    pub announcement_id: String,
    pub page: u32,
    pub raw_sha256: String,
    /// Verbatim text from the cited page.
    pub excerpt: String,
    pub verified_by: String,
    pub verified_at: String,
}

/// A citation that has been checked against the local Raw Store. Fields are
/// private so a citation can only come from [`Citation::from_store`] or
/// [`read_citations_csv`], never from an unchecked struct literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Citation {
    fact_source: String,
    field: String,
    announcement_id: String,
    page: u32,
    raw_sha256: String,
    excerpt: String,
    verified_by: String,
    verified_at: String,
    /// Historical announcement date parsed from the frozen Raw's cninfo
    /// `finalpage/YYYY-MM-DD/` path or matching `announcementTime`. Local
    /// capture time is never substituted; `None` means the Raw carries no
    /// unambiguous date.
    publication_date: Option<NaiveDate>,
}

impl Citation {
    /// Reads the frozen document from `store`, proves that it references the
    /// cited announcement, and checks the page, excerpt and verifier. PDF pages
    /// are counted from the converter output, so a page beyond the document or
    /// an excerpt on another page is rejected.
    pub fn from_store(
        store: &RawStore,
        converter: &mut impl PdfText,
        input: CitationInput,
    ) -> Result<Self> {
        let mut citation = Self {
            fact_source: input.fact_source,
            field: input.field,
            announcement_id: input.announcement_id,
            page: input.page,
            raw_sha256: input.raw_sha256,
            excerpt: input.excerpt,
            verified_by: input.verified_by,
            verified_at: input.verified_at,
            publication_date: None,
        };
        citation.verify(store, converter)?;
        citation.publication_date =
            publication_from_store(store, &citation.raw_sha256, &citation.announcement_id)?;
        Ok(citation)
    }

    pub fn fact_source(&self) -> &str {
        &self.fact_source
    }

    pub fn field(&self) -> &str {
        &self.field
    }

    pub fn announcement_id(&self) -> &str {
        &self.announcement_id
    }

    pub fn page(&self) -> u32 {
        self.page
    }

    pub fn raw_sha256(&self) -> &str {
        &self.raw_sha256
    }

    pub fn excerpt(&self) -> &str {
        &self.excerpt
    }

    pub fn verified_by(&self) -> &str {
        &self.verified_by
    }

    pub fn verified_at(&self) -> &str {
        &self.verified_at
    }

    /// Historical announcement date of the cited Raw, or `None` when the Raw
    /// carries no unambiguous cninfo date.
    pub fn publication_date(&self) -> Option<NaiveDate> {
        self.publication_date
    }

    /// A rename's effective date and new legal name must be evidenced by
    /// the same frozen announcement. Separate pages of that document are valid.
    pub fn legal_name_change_publication(&self, listing: &Self) -> Result<NaiveDate> {
        if self.field != "legal_name"
            || listing.field != "listing_date"
            || self.fact_source != listing.fact_source
            || self.announcement_id != listing.announcement_id
            || self.raw_sha256 != listing.raw_sha256
            || self.publication_date != listing.publication_date
        {
            return Err("legal_name_change listing_date and legal_name must cite the same rename announcement".into());
        }
        self.publication_date
            .ok_or_else(|| "legal_name_change has no announcement publication date".into())
    }

    fn verify(&self, store: &RawStore, converter: &mut impl PdfText) -> Result<()> {
        required(&self.fact_source, "fact_source")?;
        required(&self.announcement_id, "announcement_id")?;
        if !CITED_FIELDS.contains(&self.field.as_str()) {
            return Err(format!("unknown cited field: {}", self.field).into());
        }
        if self.page == 0 {
            return Err("cited page must be positive".into());
        }
        let verified_at = DateTime::parse_from_rfc3339(&self.verified_at)
            .map_err(|_| format!("invalid verified_at: {}", self.verified_at))?;
        validate_verifier(&self.verified_by)?;
        if strip_whitespace(&self.excerpt).is_empty() {
            return Err("cited excerpt is empty".into());
        }

        let bytes = store.get(&self.raw_sha256).map_err(|error| {
            format!(
                "cited Raw {} for announcement {} is unavailable: {error}",
                self.raw_sha256, self.announcement_id
            )
        })?;
        if !self.document_references_announcement(store, &bytes)? {
            return Err(format!(
                "cited Raw {} does not reference announcement {}",
                self.raw_sha256, self.announcement_id
            )
            .into());
        }
        // A later re-capture of identical bytes does not move the freeze time;
        // the citation only has to postdate the first observed capture of the
        // object it cites.
        let earliest_capture = store
            .sources(&self.raw_sha256)?
            .iter()
            .filter_map(|record| DateTime::parse_from_rfc3339(&record.observed_at).ok())
            .min();
        if earliest_capture.is_some_and(|observed| verified_at < observed) {
            return Err(format!(
                "citation for {} was verified before the Raw object was captured",
                self.announcement_id
            )
            .into());
        }

        let page_text = self.page_text(&bytes, converter)?;
        if !strip_whitespace(&page_text).contains(&strip_whitespace(&self.excerpt)) {
            return Err(format!(
                "cited excerpt is not present on page {} of {}",
                self.page, self.raw_sha256
            )
            .into());
        }
        Ok(())
    }

    fn document_references_announcement(&self, store: &RawStore, bytes: &[u8]) -> Result<bool> {
        let id = self.announcement_id.as_bytes();
        if !id.is_empty() && bytes.windows(id.len()).any(|window| window == id) {
            return Ok(true);
        }
        Ok(store
            .sources(&self.raw_sha256)?
            .iter()
            .filter_map(|record| serde_json::to_string(&record.request).ok())
            .any(|request| request.contains(&self.announcement_id)))
    }

    /// Returns the cited page. A PDF is split on the form-feed separators that
    /// text converters emit; other Raws are a single page.
    fn page_text(&self, bytes: &[u8], converter: &mut impl PdfText) -> Result<String> {
        if bytes.starts_with(b"%PDF-") {
            let text = converter.text(bytes).map_err(|error| {
                format!("cited PDF {} text is unavailable: {error}", self.raw_sha256)
            })?;
            let pages = text.split('\u{c}').collect::<Vec<_>>();
            pages
                .get(self.page as usize - 1)
                .map(|page| (*page).to_owned())
                .ok_or_else(|| {
                    format!(
                        "cited page {} exceeds the {} frozen page(s) of {}",
                        self.page,
                        pages.len(),
                        self.raw_sha256
                    )
                    .into()
                })
        } else if self.page == 1 {
            Ok(String::from_utf8_lossy(bytes).into_owned())
        } else {
            Err(format!(
                "non-PDF Raw {} can only be cited on page 1",
                self.raw_sha256
            )
            .into())
        }
    }
}

/// Reads the local citation manifest. Every row is verified against the Raw
/// Store; duplicate `(fact_source, field)` keys fail closed.
pub fn read_citations_csv(
    text: &str,
    store: &RawStore,
    converter: &mut impl PdfText,
) -> Result<Vec<Citation>> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct CitationRow {
        fact_source: String,
        field: String,
        announcement_id: String,
        page: String,
        raw_sha256: String,
        excerpt: String,
        verified_by: String,
        verified_at: String,
    }

    let mut reader = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_reader(text.as_bytes());
    let headers = reader.headers()?.iter().collect::<Vec<_>>();
    if headers != CITATION_HEADERS {
        return Err(format!(
            "citation CSV header mismatch: expected {CITATION_HEADERS:?}, got {headers:?}"
        )
        .into());
    }
    let mut citations = BTreeMap::<(String, String), Citation>::new();
    for row in reader.deserialize::<CitationRow>() {
        let row = row?;
        let page = row
            .page
            .parse::<u32>()
            .map_err(|_| format!("invalid citation page: {}", row.page))?;
        let citation = Citation::from_store(
            store,
            converter,
            CitationInput {
                fact_source: row.fact_source,
                field: row.field,
                announcement_id: row.announcement_id,
                page,
                raw_sha256: row.raw_sha256,
                excerpt: row.excerpt,
                verified_by: row.verified_by,
                verified_at: row.verified_at,
            },
        )?;
        let key = (citation.fact_source.clone(), citation.field.clone());
        if citations.insert(key.clone(), citation).is_some() {
            return Err(format!("duplicate citation for {} field {}", key.0, key.1).into());
        }
    }
    Ok(citations.into_values().collect())
}

/// `verified_by` identifies the operator who checked the excerpt against the
/// page; it must be a usable identifier rather than a placeholder.
fn validate_verifier(value: &str) -> Result<()> {
    let value = value.trim();
    if !(3..=64).contains(&value.len()) {
        return Err(format!("invalid verified_by length: {value}").into());
    }
    if !value.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':' | b'@')
    }) {
        return Err(format!("invalid verified_by: {value}").into());
    }
    if matches!(
        value.to_ascii_lowercase().as_str(),
        "unknown" | "anonymous" | "tbd" | "todo" | "n/a" | "none" | "test"
    ) {
        return Err(format!("verified_by must name the verifier: {value}").into());
    }
    Ok(())
}

/// Derives the announcement date from frozen Raw provenance, never from the
/// local capture time or the fact's effective date. An ambiguous Raw fails
/// closed instead of choosing a date.
fn publication_from_store(
    store: &RawStore,
    raw_sha256: &str,
    announcement_id: &str,
) -> Result<Option<NaiveDate>> {
    let bytes = store
        .get(raw_sha256)
        .map_err(|error| format!("cited Raw {raw_sha256} is unavailable: {error}"))?;
    let mut dates = BTreeSet::new();
    for record in store
        .sources(raw_sha256)
        .map_err(|error| error.to_string())?
    {
        if let Some(url) = record.request.get("url").and_then(Value::as_str)
            && let Some(date) = cninfo_adjunct_date(url)
        {
            dates.insert(date);
        }
    }
    if !bytes.starts_with(b"%PDF-")
        && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
    {
        collect_announcement_dates(&value, announcement_id, &mut dates);
    }
    match dates.len() {
        0 => Ok(None),
        1 => Ok(dates.first().copied()),
        _ => Err(format!("ambiguous announcement dates for {announcement_id}").into()),
    }
}

fn cninfo_adjunct_date(url: &str) -> Option<NaiveDate> {
    let (_, rest) = url.split_once("finalpage/")?;
    NaiveDate::parse_from_str(rest.get(..10)?, "%Y-%m-%d").ok()
}

fn collect_announcement_dates(
    value: &Value,
    announcement_id: &str,
    dates: &mut BTreeSet<NaiveDate>,
) {
    match value {
        Value::Object(object) => {
            let numeric_id = announcement_id.parse::<i64>().ok();
            if object.get("announcementId").is_some_and(|id| {
                id.as_str() == Some(announcement_id)
                    || numeric_id.is_some_and(|value| id.as_i64() == Some(value))
            }) && let Some(millis) = object.get("announcementTime").and_then(Value::as_i64)
                && let Some(date) = shanghai_date(millis)
            {
                dates.insert(date);
            }
            for value in object.values() {
                collect_announcement_dates(value, announcement_id, dates);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_announcement_dates(item, announcement_id, dates);
            }
        }
        _ => {}
    }
}

fn shanghai_date(millis: i64) -> Option<NaiveDate> {
    let offset = FixedOffset::east_opt(8 * 3600)?;
    Some(
        DateTime::from_timestamp_millis(millis)?
            .with_timezone(&offset)
            .date_naive(),
    )
}
