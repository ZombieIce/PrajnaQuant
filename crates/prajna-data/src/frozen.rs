use serde_json::{Value, json};

use crate::{RawStore, RawStoreError, SourceKind, SourceRecordInput};

/// Provenance recorded with a frozen vendor file. None of it enters the Raw hash.
#[derive(Debug, Clone, PartialEq)]
pub struct FrozenSourceMeta {
    /// Media type of the vendor bytes as received, e.g. `application/zip`.
    pub content_type: String,
    pub source_id: String,
    pub endpoint: String,
    /// Local observation time (RFC 3339); not a historical publication time.
    pub observed_at: String,
    /// Owner's terms-of-use conclusion for this use, e.g. `unresolved`.
    pub terms_conclusion: String,
    pub coverage: Value,
    pub gaps: Value,
    pub ingested_by: String,
}

/// Freezes already-downloaded bytes into the Raw Store as a `file_import`.
///
/// Rejects blank provenance so a Raw object never lacks media type, source,
/// endpoint, observation time or terms conclusion. A revised vendor file has
/// different bytes and therefore becomes a new Raw object; nothing is overwritten.
pub fn freeze_raw(
    store: &RawStore,
    bytes: &[u8],
    meta: FrozenSourceMeta,
) -> Result<String, RawStoreError> {
    let blank = [
        &meta.content_type,
        &meta.source_id,
        &meta.endpoint,
        &meta.observed_at,
        &meta.terms_conclusion,
        &meta.ingested_by,
    ]
    .iter()
    .any(|field| field.trim().is_empty());
    if blank || meta.observed_at == "synthetic" {
        return Err(RawStoreError::InvalidProvenance);
    }
    store.put(
        bytes,
        SourceRecordInput {
            content_type: meta.content_type,
            source_kind: SourceKind::FileImport,
            source_id: meta.source_id,
            request: json!({
                "endpoint": meta.endpoint,
                "terms_conclusion": meta.terms_conclusion,
                "coverage": meta.coverage,
                "gaps": meta.gaps,
            }),
            observed_at: meta.observed_at,
            ingested_by: meta.ingested_by,
        },
    )
}
