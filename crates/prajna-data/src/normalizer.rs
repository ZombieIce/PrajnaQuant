use std::{collections::BTreeMap, sync::Arc};

use arrow_array::RecordBatch;
use serde_json::Value;
use sha2::{Digest, Sha256};

mod synthetic_etf_daily;

/// Raw bytes and their content identity supplied to a Normalizer.
#[derive(Debug, Clone)]
pub struct RawInput {
    pub raw_sha256: String,
    pub bytes: Vec<u8>,
}

impl RawInput {
    pub fn from_bytes(bytes: impl Into<Vec<u8>>) -> Self {
        let bytes = bytes.into();
        let raw_sha256 = format!("sha256:{:x}", Sha256::digest(&bytes));
        Self { raw_sha256, bytes }
    }

    fn has_valid_identity(&self) -> bool {
        self.raw_sha256 == format!("sha256:{:x}", Sha256::digest(&self.bytes))
    }
}

/// One machine-readable normalization problem. `path` uses fixture JSON paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizationIssue {
    pub code: String,
    pub path: String,
    pub message: String,
}

impl NormalizationIssue {
    pub(crate) fn new(
        code: impl Into<String>,
        path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            path: path.into(),
            message: message.into(),
        }
    }
}

/// Logical tables and audit facts produced by a Normalizer.
#[derive(Debug, Clone)]
pub struct NormalizationOutput {
    pub tables: NormalizedTables,
    pub coverage: Value,
    pub synthetic_assumptions: Vec<String>,
    pub ignored_input_fields: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct NormalizedTables {
    pub instruments: RecordBatch,
    pub sessions: RecordBatch,
    pub bars: RecordBatch,
    pub execution_status: Option<RecordBatch>,
}

/// A versioned conversion from immutable raw bytes to the normalized contract.
pub trait Normalizer: Send + Sync {
    fn id(&self) -> &str;
    fn version(&self) -> &str;
    fn config_sha256(&self) -> String;
    fn normalize(
        &self,
        raw_inputs: &[RawInput],
    ) -> Result<NormalizationOutput, Vec<NormalizationIssue>>;
}

/// Resolves Normalizers by their full `(id, version)` identity.
#[derive(Clone, Default)]
pub struct NormalizerRegistry {
    normalizers: BTreeMap<(String, String), Arc<dyn Normalizer>>,
}

impl NormalizerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_builtins() -> Self {
        let mut registry = Self::new();
        registry.register(synthetic_etf_daily::SyntheticEtfDaily::v1());
        registry.register(synthetic_etf_daily::SyntheticEtfDaily::v2());
        registry.register(synthetic_etf_daily::SyntheticEtfDaily::v3());
        registry
    }

    pub fn register(&mut self, normalizer: impl Normalizer + 'static) -> bool {
        let key = (normalizer.id().to_owned(), normalizer.version().to_owned());
        if self.normalizers.contains_key(&key) {
            return false;
        }
        self.normalizers.insert(key, Arc::new(normalizer));
        true
    }

    pub fn contains(&self, id: &str, version: &str) -> bool {
        self.normalizers
            .contains_key(&(id.to_owned(), version.to_owned()))
    }

    pub fn get(&self, id: &str, version: &str) -> Option<Arc<dyn Normalizer>> {
        self.normalizers
            .get(&(id.to_owned(), version.to_owned()))
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::NormalizerRegistry;

    #[test]
    fn builtin_registry_resolves_exact_normalizer_identity() {
        let registry = NormalizerRegistry::with_builtins();

        assert!(registry.contains("synthetic-etf-daily", "1"));
        assert!(registry.contains("synthetic-etf-daily", "2"));
        assert!(registry.contains("synthetic-etf-daily", "3"));
        assert!(!registry.contains("other", "1"));
    }
}
