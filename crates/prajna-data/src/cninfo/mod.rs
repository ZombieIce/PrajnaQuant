//! Local-only cninfo fund index. Pagination consistency is not source completeness.
mod acceptance;
mod citations;
mod collect;
mod crosscheck;
mod extract;
mod facts;
mod index;
mod mvp6;

pub use acceptance::{
    FactsAcceptance, FixedListAcceptance, V4Projection, collect_and_accept, load_v4_projection,
    write_facts_acceptance,
};
pub use citations::{Citation, CitationInput, read_citations_csv};
pub use collect::{Collector, HttpReply, Request, Transport};
pub use crosscheck::{CrosscheckMismatch, CrosscheckReport, V4DailyBar, crosscheck_v4};
pub use extract::{EXTRACTOR_VERSION, PdfText, Poppler, extract_fields};
pub use facts::validate_and_convert;
pub use index::{
    Announcement, FundIndex, IndexDataset, NameFact, PageSet, RULES_VERSION, classify, scoped_code,
};
pub use mvp6::{
    ActionFact, CatalogLifecycle, CorporateActionV2, EventFact, EvidenceLevel, FixedListResult,
    FundCandidate, IdentityFact, Mvp6Facts, Mvp6Session, OrgIdResolution, RiskEventV1,
    StructuredCorporateActionsV2, should_download_pdf,
};

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("sha256:{:x}", Sha256::digest(bytes))
}
