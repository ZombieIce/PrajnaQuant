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
use chrono::DateTime;
use serde::Deserialize;
use std::collections::BTreeMap;

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
const CITED_FIELDS: &[&str] = &[
    "code",
    "org_id",
    "legal_name",
    "listing_date",
    "termination_date",
    "fund_category",
    "tracked_index",
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
        let citation = Self {
            fact_source: input.fact_source,
            field: input.field,
            announcement_id: input.announcement_id,
            page: input.page,
            raw_sha256: input.raw_sha256,
            excerpt: input.excerpt,
            verified_by: input.verified_by,
            verified_at: input.verified_at,
        };
        citation.verify(store, converter)?;
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
        for record in store.sources(&self.raw_sha256)? {
            let Ok(observed) = DateTime::parse_from_rfc3339(&record.observed_at) else {
                continue;
            };
            if verified_at < observed {
                return Err(format!(
                    "citation for {} was verified before the Raw object was captured",
                    self.announcement_id
                )
                .into());
            }
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
