//! Local-only cninfo fund index. Pagination consistency is not source completeness.
mod collect;
mod extract;
mod index;
mod mvp6;

pub use collect::{Collector, HttpReply, Request, Transport};
pub use extract::{EXTRACTOR_VERSION, PdfText, Poppler, extract_fields};
pub use index::{
    Announcement, FundIndex, IndexDataset, NameFact, PageSet, RULES_VERSION, classify, scoped_code,
};
pub use mvp6::{
    ActionFact, CatalogLifecycle, CorporateActionV2, CrosscheckMismatch, CrosscheckReport,
    EventFact, EvidenceLevel, FixedListResult, FundCandidate, IdentityFact, Mvp6Facts, Mvp6Session,
    OrgIdResolution, RawCitation, RiskEventV1, StructuredCorporateActionsV2, V4DailyBar,
    crosscheck_v4, should_download_pdf, validate_and_convert,
};

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("sha256:{:x}", Sha256::digest(bytes))
}
