use super::{EXTRACTOR_VERSION, Result, hash};
use crate::{
    Manifest, ManifestCore, ManifestInput, ManifestProvenance, ManifestTable, NormalizerIdentity,
    logical_hash, restricted_jcs, schema_fingerprint,
};
use arrow_array::{RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use chrono::{DateTime, NaiveDate};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

pub const RULES_VERSION: &str = "cninfo_title_rules_v2";

pub fn scoped_code(code: &str) -> bool {
    code.len() == 6
        && code.bytes().all(|b| b.is_ascii_digit())
        && (code.starts_with("158") || code.starts_with("159"))
}

/// Multi-label: a holders meeting may also suspend trading on the vote day.
pub fn classify(title: &str) -> Vec<String> {
    let rules: &[(&str, &[&str])] = &[
        ("dividend", &["收益分配", "分红"]),
        ("conversion", &["份额折算", "折算"]),
        ("termination", &["终止上市", "合同终止", "终止基金合同"]),
        ("liquidation", &["清算", "剩余财产分配"]),
        ("suspension", &["停牌"]),
        ("resumption", &["复牌"]),
        ("holders_meeting", &["持有人大会"]),
        ("listing_notice", &["上市交易公告书"]),
        ("underlying_index", &["标的指数", "更名", "变更基金名称"]),
        (
            "periodic_report",
            &[
                "季度报告",
                "中期报告",
                "半年度报告",
                "年度报告",
                "年报",
                "季报",
            ],
        ),
    ];
    rules
        .iter()
        .filter(|(_, words)| words.iter().any(|w| title.contains(w)))
        .map(|(kind, _)| (*kind).to_owned())
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Announcement {
    pub id: String,
    pub code: String,
    pub org_id: Option<String>,
    pub title: String,
    pub categories: Vec<String>,
    pub announced_ms: i64,
    pub attachment: Option<String>,
    pub source_raw_sha256: String,
}

/// One query's complete page sequence. Changing totals, conflicting IDs and
/// missing pages fail closed; overlaps are deduplicated by announcement ID.
#[derive(Default)]
pub struct PageSet {
    total: Option<usize>,
    next: usize,
    declared_pages: Option<usize>,
    has_more: Option<bool>,
    empty_page: bool,
    items: BTreeMap<String, Announcement>,
    raw: BTreeSet<String>,
}
impl PageSet {
    pub fn push(&mut self, page: usize, bytes: &[u8]) -> Result<()> {
        if page != self.next + 1 {
            return Err("non-contiguous pages".into());
        }
        let body: Value = serde_json::from_slice(bytes)?;
        let total = body["totalAnnouncement"]
            .as_u64()
            .or_else(|| body["totalRecordNum"].as_u64())
            .ok_or("missing total")? as usize;
        if self.total.is_some_and(|n| n != total) {
            return Err("changing total".into());
        }
        let items = match &body["announcements"] {
            Value::Array(items) => items.as_slice(),
            Value::Null if total == 0 => &[],
            _ => return Err("missing announcements".into()),
        };
        let declared_pages = body
            .get("totalpages")
            .or_else(|| body.get("totalPages"))
            .map(|v| v.as_u64().ok_or("invalid total pages").map(|n| n as usize))
            .transpose()?;
        let has_more = body
            .get("hasMore")
            .map(|v| v.as_bool().ok_or("invalid hasMore"))
            .transpose()?;
        // cninfo's `totalpages` is not authoritative: it is 0 for a single page
        // whose total is below the page size, 1 even when a second page exists,
        // and may disagree with `hasMore`. Completeness is therefore decided by
        // the stable `totalAnnouncement` and the unique announcement IDs at
        // `finish`; `hasMore` only drives whether another page is requested.
        // Declared page counts are retained for the safety `needs_more` hint
        // but never reject a response whose unique rows can still reach the
        // declared total.
        if items.is_empty()
            && (has_more == Some(true)
                || declared_pages
                    .or(self.declared_pages)
                    .is_some_and(|n| page < n))
        {
            return Err("empty page declares remaining pages".into());
        }
        self.declared_pages = declared_pages.or(self.declared_pages);
        self.has_more = has_more;
        self.empty_page = items.is_empty();
        let raw = hash(bytes);
        for row in items {
            let text = |key| {
                row[key]
                    .as_str()
                    .filter(|s| !s.trim().is_empty())
                    .map(str::to_owned)
            };
            let title = text("announcementTitle")
                .ok_or("missing title")?
                .replace("<em>", "")
                .replace("</em>", "");
            let item = Announcement {
                id: text("announcementId").ok_or("missing announcementId")?,
                code: text("secCode").ok_or("missing secCode")?,
                org_id: text("orgId"),
                categories: classify(&title),
                title,
                announced_ms: row["announcementTime"]
                    .as_i64()
                    .ok_or("missing publication time")?,
                attachment: text("adjunctUrl"),
                source_raw_sha256: raw.clone(),
            };
            DateTime::from_timestamp_millis(item.announced_ms).ok_or("invalid publication time")?;
            if let Some(old) = self.items.get_mut(&item.id) {
                let mut comparable = item.clone();
                comparable.source_raw_sha256 = old.source_raw_sha256.clone();
                if *old != comparable {
                    return Err("conflicting announcement ID".into());
                }
                old.source_raw_sha256 = old.source_raw_sha256.clone().min(raw.clone());
            } else {
                self.items.insert(item.id.clone(), item);
            }
        }
        self.raw.insert(raw);
        self.next = page;
        self.total = Some(total);
        Ok(())
    }
    pub fn needs_more(&self) -> bool {
        if self.has_more == Some(false) || self.empty_page {
            return false;
        }
        self.has_more == Some(true)
            || self.declared_pages.is_some_and(|n| self.next < n)
            || self.total.is_some_and(|n| self.items.len() < n)
    }
    pub fn total(&self) -> Option<usize> {
        self.total
    }
    pub fn len(&self) -> usize {
        self.items.len()
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
    pub fn finish(self) -> Result<(Vec<Announcement>, BTreeSet<String>)> {
        if self.needs_more() || self.total != Some(self.items.len()) {
            return Err("unique IDs differ from declared total".into());
        }
        Ok((self.items.into_values().collect(), self.raw))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NameFact {
    pub name: String,
    /// Publication evidence only; consumers apply P6 calendar availability.
    pub announced_ms: i64,
    /// Calendar-derived P6 availability is unresolved at indexing time.
    pub available_at: Option<String>,
    pub announcement_id: String,
    pub source_raw_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FundIndex {
    pub code: String,
    pub org_ids: Vec<String>,
    pub announcements: Vec<Announcement>,
    pub full_names: Vec<NameFact>,
    pub events: BTreeMap<String, Value>,
    pub unknown: Vec<String>,
}
impl FundIndex {
    pub fn new(code: String, mut announcements: Vec<Announcement>) -> Self {
        announcements.sort_by(|a, b| (a.announced_ms, &a.id).cmp(&(b.announced_ms, &b.id)));
        let org_ids: Vec<_> = announcements
            .iter()
            .filter_map(|a| a.org_id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut full_names = Vec::new();
        for a in &announcements {
            if let Some(name) = legal_name(&a.title) {
                full_names.push(NameFact {
                    name,
                    announced_ms: a.announced_ms,
                    available_at: None,
                    announcement_id: a.id.clone(),
                    source_raw_sha256: a.source_raw_sha256.clone(),
                });
            }
        }
        let mut unknown = Vec::new();
        if announcements.iter().any(|a| a.org_id.is_none()) {
            unknown.push("announcement_orgId_unknown".into());
        }
        if full_names.windows(2).any(|pair| {
            pair[0].announced_ms == pair[1].announced_ms && pair[0].name != pair[1].name
        }) {
            unknown.push("conflicting_legal_names".into());
        }
        if org_ids.is_empty() {
            unknown.push("orgId_unknown".into());
        }
        if full_names.is_empty() {
            unknown.push("legal_name_unknown".into());
        }
        Self {
            code,
            org_ids,
            announcements,
            full_names,
            events: BTreeMap::new(),
            unknown,
        }
    }
    pub fn require_usable(&self) -> Result<()> {
        if !self.unknown.is_empty() {
            return Err("fund has unresolved coverage or fields".into());
        }
        Ok(())
    }
}

/// Accept only explicit full legal names at the beginning of titles. A rename
/// notice alone cannot establish its new name, so it creates a coverage gap.
fn legal_name(title: &str) -> Option<String> {
    let candidate = if title.contains("更名") || title.contains("变更基金名称") {
        let re = regex::Regex::new(r"(?:基金名称变更为|更名为)[“《]([^”》]+)[”》]").unwrap();
        re.captures(title)?.get(1)?.as_str()
    } else {
        title
    };
    let endings = [
        "交易型开放式指数证券投资基金",
        "交易型开放式指数发起式证券投资基金",
        "交易型开放式证券投资基金",
        "交易型开放式债券指数证券投资基金",
    ];
    let end = endings
        .iter()
        .find_map(|ending| candidate.find(ending).map(|i| i + ending.len()))?;
    let mut name = candidate[..end].trim().to_owned();
    for suffix in ["（QDII）", "(QDII)"] {
        if candidate[end..].starts_with(suffix) {
            name.push_str(suffix);
        }
    }
    if name.contains("关于") || name.is_empty() {
        return None;
    }
    Some(name)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexDataset {
    pub window_start: NaiveDate,
    pub window_end: NaiveDate,
    pub rules_version: String,
    pub extractor_version: String,
    pub source_raw_sha256: BTreeSet<String>,
    pub funds: Vec<FundIndex>,
    pub unknown: Vec<String>,
}
impl IndexDataset {
    pub fn new(start: NaiveDate, end: NaiveDate) -> Result<Self> {
        if start > end {
            return Err("reversed window".into());
        }
        Ok(Self {
            window_start: start,
            window_end: end,
            rules_version: RULES_VERSION.into(),
            extractor_version: EXTRACTOR_VERSION.into(),
            source_raw_sha256: BTreeSet::new(),
            funds: Vec::new(),
            unknown: Vec::new(),
        })
    }
    /// A versioned JSON payload table; uses the workspace's canonical Manifest
    /// and logical hash, so rules, extractor, gaps and PDF evidence affect DSV.
    pub fn manifest(&self, provenance: ManifestProvenance) -> Result<(Manifest, RecordBatch)> {
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Utf8, false),
            Field::new("payload", DataType::Utf8, false),
        ]));
        let payload = String::from_utf8(restricted_jcs(&serde_json::to_value(self)?)?)?;
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(StringArray::from(vec!["cninfo-index"])),
                Arc::new(StringArray::from(vec![payload])),
            ],
        )?;
        let config = json!({"rules": self.rules_version, "extractor": self.extractor_version, "actions": "cninfo_corporateActions_structured_v1", "record_date": "previous_session_before_ex_date_not_pdf", "scope": ["158xxx", "159xxx"]});
        let core = ManifestCore {
            manifest_version: 1,
            normalizer: NormalizerIdentity {
                id: "cninfo-fund-index".into(),
                version: "1".into(),
                config_sha256: hash(&restricted_jcs(&config)?),
            },
            inputs: self
                .source_raw_sha256
                .iter()
                .map(|h| ManifestInput {
                    raw_sha256: h.clone(),
                })
                .collect(),
            tables: vec![ManifestTable {
                table: "cninfo_index".into(),
                schema_version: 1,
                schema_fingerprint: schema_fingerprint(&schema)?,
                logical_hash: logical_hash(
                    "cninfo_index",
                    &schema,
                    std::slice::from_ref(&batch),
                    &["id"],
                )?,
                row_count: 1,
            }],
        };
        Ok((Manifest::new(core, provenance)?, batch))
    }
}
