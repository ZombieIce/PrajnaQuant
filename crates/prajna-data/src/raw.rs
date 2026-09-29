use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);
static SOURCE_APPEND_LOCK: Mutex<()> = Mutex::new(());

/// The kind of source that produced a raw object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Fixture,
    VendorApi,
    FileImport,
    Generator,
}

/// Metadata supplied by the caller when adding a raw object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceRecordInput {
    pub content_type: String,
    pub source_kind: SourceKind,
    pub source_id: String,
    pub request: Value,
    /// Local observation time, or `"synthetic"` for generated fixtures.
    pub observed_at: String,
    pub ingested_by: String,
}

/// A persisted source record. Records are appended to JSONL and never rewritten.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceRecord {
    pub raw_sha256: String,
    pub content_type: String,
    pub source_kind: SourceKind,
    pub source_id: String,
    pub request: Value,
    pub observed_at: String,
    pub ingested_by: String,
}

/// Content-addressed storage for immutable raw bytes and append-only provenance.
#[derive(Debug, Clone)]
pub struct RawStore {
    root: PathBuf,
}

/// Errors reported by the raw store.
#[derive(Debug)]
pub enum RawStoreError {
    Io(io::Error),
    Json(serde_json::Error),
    InvalidHash(String),
    CorruptObject(String),
}

impl std::fmt::Display for RawStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "raw store I/O error: {error}"),
            Self::Json(error) => write!(f, "invalid raw source record: {error}"),
            Self::InvalidHash(hash) => write!(f, "invalid raw SHA-256: {hash}"),
            Self::CorruptObject(hash) => write!(f, "raw object is corrupt: {hash}"),
        }
    }
}

impl std::error::Error for RawStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::InvalidHash(_) | Self::CorruptObject(_) => None,
        }
    }
}

impl From<io::Error> for RawStoreError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for RawStoreError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl RawStore {
    /// Opens a raw store rooted at `root`, creating the root directory if needed.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, RawStoreError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    /// Stores bytes by SHA-256 and appends a sanitized provenance record.
    ///
    /// The returned identity is formatted as `sha256:<lowercase hex>`.
    pub fn put(
        &self,
        bytes: &[u8],
        source_record: SourceRecordInput,
    ) -> Result<String, RawStoreError> {
        let hash = format!("sha256:{:x}", Sha256::digest(bytes));
        let object_path = self.object_path(&hash)?;
        let parent = object_path.parent().expect("object path has a parent");
        fs::create_dir_all(parent)?;

        if object_path.exists() {
            self.verify_existing(&object_path, bytes, &hash)?;
        } else {
            self.install_object(&object_path, parent, bytes, &hash)?;
        }

        let record = SourceRecord {
            raw_sha256: hash.clone(),
            content_type: source_record.content_type,
            source_kind: source_record.source_kind,
            source_id: source_record.source_id,
            request: sanitize_request(source_record.request),
            observed_at: source_record.observed_at,
            ingested_by: source_record.ingested_by,
        };
        self.append_source(&hash, &record)?;
        Ok(hash)
    }

    /// Reads an object and verifies its content against the requested SHA-256.
    pub fn get(&self, hash: &str) -> Result<Vec<u8>, RawStoreError> {
        let path = self.object_path(hash)?;
        let bytes = fs::read(path)?;
        if format!("sha256:{:x}", Sha256::digest(&bytes)) != hash {
            return Err(RawStoreError::CorruptObject(hash.to_owned()));
        }
        Ok(bytes)
    }

    /// Returns every valid source record appended for an object.
    pub fn sources(&self, hash: &str) -> Result<Vec<SourceRecord>, RawStoreError> {
        let path = self.source_path(hash)?;
        let contents = match fs::read(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        contents
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| Ok(serde_json::from_slice(line)?))
            .collect()
    }

    fn object_path(&self, hash: &str) -> Result<PathBuf, RawStoreError> {
        let hex = parse_hash(hash)?;
        Ok(self.root.join("raw/sha256").join(&hex[..2]).join(hex))
    }

    fn source_path(&self, hash: &str) -> Result<PathBuf, RawStoreError> {
        let hex = parse_hash(hash)?;
        Ok(self
            .root
            .join("raw-sources/sha256")
            .join(&hex[..2])
            .join(format!("{hex}.jsonl")))
    }

    fn verify_existing(
        &self,
        path: &Path,
        expected: &[u8],
        hash: &str,
    ) -> Result<(), RawStoreError> {
        let stored = fs::read(path)?;
        if stored != expected || format!("sha256:{:x}", Sha256::digest(&stored)) != hash {
            return Err(RawStoreError::CorruptObject(hash.to_owned()));
        }
        Ok(())
    }

    fn install_object(
        &self,
        destination: &Path,
        parent: &Path,
        bytes: &[u8],
        hash: &str,
    ) -> Result<(), RawStoreError> {
        let (temp_path, mut file) = create_temp_file(parent)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);

        match fs::rename(&temp_path.0, destination) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists || destination.exists() => {
                self.verify_existing(destination, bytes, hash)?;
                fs::remove_file(&temp_path.0)?;
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }

    fn append_source(&self, hash: &str, record: &SourceRecord) -> Result<(), RawStoreError> {
        let path = self.source_path(hash)?;
        let parent = path.parent().expect("source path has a parent");
        fs::create_dir_all(parent)?;
        let _guard = SOURCE_APPEND_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        serde_json::to_writer(&mut file, record)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        Ok(())
    }
}

struct TempPath(PathBuf);

impl Drop for TempPath {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn create_temp_file(parent: &Path) -> Result<(TempPath, File), RawStoreError> {
    loop {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(".raw-{}-{id}.tmp", std::process::id()));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((TempPath(path), file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
}

fn parse_hash(hash: &str) -> Result<&str, RawStoreError> {
    let Some(hex) = hash.strip_prefix("sha256:") else {
        return Err(RawStoreError::InvalidHash(hash.to_owned()));
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(RawStoreError::InvalidHash(hash.to_owned()));
    }
    Ok(hex)
}

fn is_credential_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    ["token", "key", "secret", "signature", "password", "auth"]
        .iter()
        .any(|needle| key.contains(needle))
}

fn sanitize_request(mut request: Value) -> Value {
    sanitize_value(&mut request);
    request
}

fn sanitize_value(value: &mut Value) {
    match value {
        Value::Object(object) => {
            // Also support header lists such as [{"name":"Authorization","value":"..."}].
            let named_header_is_secret = object
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(is_credential_key);
            for (key, value) in object.iter_mut() {
                if is_credential_key(key)
                    || (named_header_is_secret && key.eq_ignore_ascii_case("value"))
                {
                    *value = Value::String("[REDACTED]".to_owned());
                } else if key.eq_ignore_ascii_case("url") {
                    if let Some(url) = value.as_str() {
                        *value = Value::String(sanitize_url(url));
                    } else {
                        sanitize_value(value);
                    }
                } else if key.eq_ignore_ascii_case("body") {
                    sanitize_body(value);
                } else {
                    sanitize_value(value);
                }
            }
        }
        Value::Array(values) => values.iter_mut().for_each(sanitize_value),
        _ => {}
    }
}

fn sanitize_body(value: &mut Value) {
    if let Value::String(body) = value {
        if let Ok(mut parsed) = serde_json::from_str::<Value>(body) {
            sanitize_value(&mut parsed);
            if let Ok(encoded) = serde_json::to_string(&parsed) {
                *body = encoded;
            }
        } else if body.contains('=') {
            *body = sanitize_query_parameters(body);
        }
    } else {
        sanitize_value(value);
    }
}

fn sanitize_url(url: &str) -> String {
    let Some(query_start) = url.find('?') else {
        return url.to_owned();
    };
    let fragment_start = url[query_start..].find('#').map(|i| query_start + i);
    let query_end = fragment_start.unwrap_or(url.len());
    let query = &url[query_start + 1..query_end];
    format!(
        "{}?{}{}",
        &url[..query_start],
        sanitize_query_parameters(query),
        &url[query_end..]
    )
}

fn sanitize_query_parameters(query: &str) -> String {
    query
        .split('&')
        .map(|part| {
            let (key, value) = part.split_once('=').unwrap_or((part, ""));
            if is_credential_key(&decode_query_key(key)) {
                format!("{key}=[REDACTED]")
            } else if part.contains('=') {
                format!("{key}={value}")
            } else {
                key.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn decode_query_key(key: &str) -> String {
    let bytes = key.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'+' {
            decoded.push(b' ');
            i += 1;
        } else if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2])) {
                decoded.push(high * 16 + low);
                i += 3;
            } else {
                decoded.push(bytes[i]);
                i += 1;
            }
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, thread};

    use serde_json::json;

    use super::*;

    fn input(source_id: &str, request: Value) -> SourceRecordInput {
        SourceRecordInput {
            content_type: "application/json".to_owned(),
            source_kind: SourceKind::Fixture,
            source_id: source_id.to_owned(),
            request,
            observed_at: "synthetic".to_owned(),
            ingested_by: "unit-test".to_owned(),
        }
    }

    #[test]
    fn duplicate_put_is_idempotent_and_appends_source_records() {
        let temp = tempfile::tempdir().unwrap();
        let store = RawStore::open(temp.path()).unwrap();
        let bytes = br#"{"value":1}"#;

        let first_hash = store.put(bytes, input("first", json!({}))).unwrap();
        let first_line = fs::read(store.source_path(&first_hash).unwrap()).unwrap();
        let second_hash = store.put(bytes, input("second", json!({}))).unwrap();

        assert_eq!(first_hash, second_hash);
        let source_path = store.source_path(&first_hash).unwrap();
        let all_lines = fs::read(&source_path).unwrap();
        assert!(all_lines.starts_with(&first_line));
        assert_eq!(store.get(&first_hash).unwrap(), bytes);
        let sources = store.sources(&first_hash).unwrap();
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].source_id, "first");
        assert_eq!(sources[1].source_id, "second");
    }

    #[test]
    fn corrupted_bytes_fail_on_get_and_put() {
        let temp = tempfile::tempdir().unwrap();
        let store = RawStore::open(temp.path()).unwrap();
        let bytes = b"original";
        let hash = store.put(bytes, input("source", json!({}))).unwrap();
        let object_path = store.object_path(&hash).unwrap();
        fs::write(object_path, b"tampered").unwrap();

        assert!(matches!(
            store.get(&hash),
            Err(RawStoreError::CorruptObject(_))
        ));
        assert!(matches!(
            store.put(bytes, input("source-2", json!({}))),
            Err(RawStoreError::CorruptObject(_))
        ));
    }

    #[test]
    fn concurrent_puts_succeed_without_temporary_files() {
        let temp = tempfile::tempdir().unwrap();
        let store = RawStore::open(temp.path()).unwrap();
        let bytes = b"same bytes";
        let left_store = store.clone();
        let right_store = store.clone();
        let left = thread::spawn(move || left_store.put(bytes, input("left", json!({}))));
        let right = thread::spawn(move || right_store.put(bytes, input("right", json!({}))));

        let left_hash = left.join().unwrap().unwrap();
        let right_hash = right.join().unwrap().unwrap();
        assert_eq!(left_hash, right_hash);
        assert_eq!(store.sources(&left_hash).unwrap().len(), 2);
        let raw_dir = temp.path().join("raw/sha256");
        for entry in fs::read_dir(raw_dir.join(&left_hash[7..9])).unwrap() {
            assert!(
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")
            );
        }
    }

    #[test]
    fn credentials_are_redacted_from_query_headers_and_body() {
        let temp = tempfile::tempdir().unwrap();
        let store = RawStore::open(temp.path()).unwrap();
        let request = json!({
            "url": "https://example.test/data?apiKey=abc&ok=yes&%61uth=xyz#frag",
            "headers": {"aUtHoRiZaTiOn": "Bearer secret", "X-Api-Key": "top-secret"},
            "body": {"clientSecret": "hidden", "nested": {"PASSWORD": "also-hidden"}, "value": 4}
        });
        let hash = store.put(b"payload", input("api", request)).unwrap();
        let stored = store.sources(&hash).unwrap().remove(0);

        assert_eq!(
            stored.request["url"],
            "https://example.test/data?apiKey=[REDACTED]&ok=yes&%61uth=[REDACTED]#frag"
        );
        assert_eq!(stored.request["headers"]["aUtHoRiZaTiOn"], "[REDACTED]");
        assert_eq!(stored.request["headers"]["X-Api-Key"], "[REDACTED]");
        assert_eq!(stored.request["body"]["clientSecret"], "[REDACTED]");
        assert_eq!(stored.request["body"]["nested"]["PASSWORD"], "[REDACTED]");
        assert_eq!(stored.request["body"]["value"], 4);

        let form_hash = store
            .put(
                b"form payload",
                input("form", json!({"body": "aUtH=body-secret&ordinary=visible"})),
            )
            .unwrap();
        let form = store.sources(&form_hash).unwrap().remove(0);
        assert_eq!(form.request["body"], "aUtH=[REDACTED]&ordinary=visible");
    }
}
