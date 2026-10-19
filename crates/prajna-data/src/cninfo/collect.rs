use super::{
    Announcement, FixedListResult, FundCandidate, FundIndex, IndexDataset, OrgIdResolution,
    PageSet, PdfText, Result, classify, extract_fields, scoped_code, should_download_pdf,
};
use crate::{RawStore, SourceKind, SourceRecordInput};
use chrono::{DateTime, Utc};
use chrono::{Duration as ChronoDuration, NaiveDate};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

const QUERY: &str = "https://www.cninfo.com.cn/new/hisAnnouncement/query";
const PDF: &str = "https://static.cninfo.com.cn/";

#[derive(Debug, Clone)]
pub struct Request {
    pub url: String,
    pub form: BTreeMap<String, String>,
}
pub struct HttpReply {
    pub status: u16,
    pub content_type: String,
    pub bytes: Vec<u8>,
}
/// Transport never follows redirects or uses a proxy. Mock implementations are
/// used by tests; the collector owns pacing, retries, evidence and access stops.
pub trait Transport {
    fn send(&mut self, request: &Request) -> Result<HttpReply>;
}
impl Transport for reqwest::blocking::Client {
    fn send(&mut self, request: &Request) -> Result<HttpReply> {
        let response = if request.form.is_empty() {
            self.get(&request.url)
        } else {
            self.post(&request.url).form(&request.form)
        }
        .send()?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_owned();
        let bytes = response.bytes()?.to_vec();
        Ok(HttpReply {
            status,
            content_type,
            bytes,
        })
    }
}

pub struct Collector<T> {
    transport: T,
    raw: RawStore,
    network_path: String,
    last_request: Option<Instant>,
    stopped: bool,
    captured: BTreeSet<String>,
}
impl Collector<reqwest::blocking::Client> {
    pub fn direct(raw: RawStore) -> Result<Self> {
        Self::direct_with_network_path(raw, "direct_no_proxy; egress region unverified")
    }

    /// Requires an operator-supplied description of the direct egress path.
    /// This is audit provenance only; it does not configure a proxy or route.
    pub fn direct_with_network_path(
        raw: RawStore,
        network_path: impl Into<String>,
    ) -> Result<Self> {
        let network_path = network_path.into();
        if network_path.trim().is_empty() {
            return Err("direct network path description is required".into());
        }
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(60))
            .user_agent("PrajnaQuant/0.1 (local noncommercial fund research)")
            .build()?;
        let mut collector = Self::new(client, raw);
        collector.network_path = network_path;
        Ok(collector)
    }
}
impl<T: Transport> Collector<T> {
    pub fn new(transport: T, raw: RawStore) -> Self {
        Self {
            transport,
            raw,
            network_path: "injected_transport".into(),
            last_request: None,
            stopped: false,
            captured: BTreeSet::new(),
        }
    }
    fn fetch(&mut self, request: Request) -> Result<(Vec<u8>, String)> {
        if self.stopped {
            return Err("cninfo source stopped: unresolved".into());
        }
        for attempt in 0..=3 {
            if let Some(last) = self.last_request {
                std::thread::sleep(Duration::from_secs(1).saturating_sub(last.elapsed()));
            }
            self.last_request = Some(Instant::now());
            let observed = Utc::now().to_rfc3339();
            let reply = self.transport.send(&request);
            let (status, content_type, bytes) = match reply {
                Ok(r) => (r.status, r.content_type, r.bytes),
                Err(_) => (0, "application/octet-stream".into(), Vec::new()),
            };
            let stored = self.raw.put(&bytes, SourceRecordInput {
                content_type, source_kind: SourceKind::VendorApi, source_id:"cninfo".into(),
                request:json!({"url":request.url,"params":request.form,"http_status":status,"attempt":attempt,"network_path":self.network_path.clone(),"terms_conclusion":"issue_172_owner_authorization_for_local_noncommercial_research","outcome":if status == 0 {"transport_error"} else {"http_response"}}),
                observed_at:observed, ingested_by:"prajna-cninfo/1".into(),
            });
            let hash = match stored {
                Ok(hash) => hash,
                Err(error) => {
                    self.stopped = true;
                    return Err(error.into());
                }
            };
            self.captured.insert(hash.clone());
            let text = String::from_utf8_lossy(&bytes).to_lowercase();
            // Conservative: stop on the first access denial rather than probe
            // around it. Empty/HTML login and captcha responses are not data.
            if matches!(status, 401 | 403 | 501)
                || text.contains("验证码")
                || text.contains("captcha")
                || (text.contains("<html") && text.contains("登录"))
            {
                self.stopped = true;
                return Err("cninfo access control: unresolved".into());
            }
            if status == 200 {
                return Ok((bytes, hash));
            }
            if attempt < 3 {
                std::thread::sleep(Duration::from_secs(1 << attempt));
            }
        }
        self.stopped = true;
        Err("cninfo request exhausted retries: unresolved".into())
    }
    fn pages(
        &mut self,
        window: &str,
        stock: &str,
    ) -> Result<(Vec<Announcement>, BTreeSet<String>)> {
        self.pages_for(window, stock, "")
    }

    /// Resolve and index an explicit fixed list without the old all-window
    /// discovery query. The existing `collect` contract remains unchanged.
    pub fn collect_fixed_list(
        &mut self,
        candidates: &[FundCandidate],
        explicit_pdf_categories: &BTreeSet<String>,
        converter: &mut impl PdfText,
    ) -> FixedListResult {
        let mut result = FixedListResult {
            resolutions: Vec::new(),
            announcements: BTreeMap::new(),
            pdf_raw_sha256_by_announcement: BTreeMap::new(),
            unresolved: Vec::new(),
        };
        for candidate in candidates {
            self.collect_fixed_candidate(
                candidate,
                explicit_pdf_categories,
                converter,
                &mut result,
            );
        }
        result.unresolved.sort();
        result.unresolved.dedup();
        result
    }

    fn collect_fixed_candidate(
        &mut self,
        candidate: &FundCandidate,
        explicit_pdf_categories: &BTreeSet<String>,
        converter: &mut impl PdfText,
        result: &mut FixedListResult,
    ) {
        if !scoped_code(&candidate.code) || candidate.active_start > candidate.active_end {
            result
                .unresolved
                .push(format!("{}:invalid_candidate", candidate.code));
            return;
        }

        let org_ids = self.resolve_fixed_org_ids(candidate).unwrap_or_default();
        let resolution = OrgIdResolution {
            code: candidate.code.clone(),
            status: if org_ids.is_empty() {
                "org_id_unresolved"
            } else {
                "resolved"
            }
            .into(),
            org_ids,
        };
        if resolution.org_ids.is_empty() {
            result
                .unresolved
                .push(format!("{}:org_id_unresolved", candidate.code));
        } else {
            for org_id in &resolution.org_ids {
                self.collect_fixed_org(
                    candidate,
                    org_id,
                    explicit_pdf_categories,
                    converter,
                    result,
                );
            }
        }
        result.resolutions.push(resolution);
    }

    fn collect_fixed_org(
        &mut self,
        candidate: &FundCandidate,
        org_id: &str,
        explicit_pdf_categories: &BTreeSet<String>,
        converter: &mut impl PdfText,
        result: &mut FixedListResult,
    ) {
        let items = self.fixed_org_announcements(candidate, org_id, result);
        let listing_notice_complete = self.collect_listing_notice_pdfs(&items, converter, result);
        self.download_primary_mvp6_pdfs(&items, result);
        self.download_supplementary_mvp6_pdfs(
            &items,
            listing_notice_complete,
            explicit_pdf_categories,
            result,
        );
        result
            .announcements
            .insert((candidate.code.clone(), org_id.to_owned()), items);
    }

    fn fixed_org_announcements(
        &mut self,
        candidate: &FundCandidate,
        org_id: &str,
        result: &mut FixedListResult,
    ) -> Vec<Announcement> {
        let mut by_id = BTreeMap::<String, Announcement>::new();
        let mut pagination_incomplete = false;
        for (start, end) in year_segments(candidate.active_start, candidate.active_end) {
            let window = format!("{start}~{end}");
            match self.pages_for(&window, &format!("{},{}", candidate.code, org_id), "") {
                Ok((items, _)) => {
                    for item in items {
                        if item.code != candidate.code || item.org_id.as_deref() != Some(org_id) {
                            pagination_incomplete = true;
                            continue;
                        }
                        by_id.insert(item.id.clone(), item);
                    }
                }
                Err(_) => pagination_incomplete = true,
            }
        }
        if pagination_incomplete {
            result
                .unresolved
                .push(format!("{}:{org_id}:pagination_incomplete", candidate.code));
        }

        let mut items = by_id.into_values().collect::<Vec<_>>();
        items.sort_by(|a, b| (a.announced_ms, &a.id).cmp(&(b.announced_ms, &b.id)));
        for item in &mut items {
            item.categories = classify_mvp6(&item.title);
        }
        items
    }

    fn collect_listing_notice_pdfs(
        &mut self,
        items: &[Announcement],
        converter: &mut impl PdfText,
        result: &mut FixedListResult,
    ) -> bool {
        let mut listing_notice_complete = false;
        for item in items
            .iter()
            .filter(|item| item.categories.iter().any(|c| c == "listing_notice"))
        {
            let Some((bytes, _)) = self.fetch_mvp6_pdf(item, result) else {
                continue;
            };
            match converter.text(&bytes) {
                Ok(text) => {
                    listing_notice_complete |= listing_notice_covers_metadata(&text);
                }
                Err(_) => result
                    .unresolved
                    .push(format!("{}:pdf_unavailable", item.id)),
            }
        }
        listing_notice_complete
    }

    fn download_primary_mvp6_pdfs(&mut self, items: &[Announcement], result: &mut FixedListResult) {
        for item in items.iter().filter(|item| {
            should_download_pdf(&item.categories, false, false)
                && !item
                    .categories
                    .iter()
                    .any(|c| c == "fund_contract" || c == "prospectus")
                && !item.categories.iter().any(|c| c == "listing_notice")
        }) {
            let _ = self.fetch_mvp6_pdf(item, result);
        }
    }

    fn download_supplementary_mvp6_pdfs(
        &mut self,
        items: &[Announcement],
        listing_notice_complete: bool,
        explicit_pdf_categories: &BTreeSet<String>,
        result: &mut FixedListResult,
    ) {
        for item in items {
            let explicitly_requested = item
                .categories
                .iter()
                .any(|category| explicit_pdf_categories.contains(category));
            let is_supplementary = item
                .categories
                .iter()
                .any(|category| category == "fund_contract" || category == "prospectus");
            let should_download = is_supplementary
                && should_download_pdf(
                    &item.categories,
                    listing_notice_complete,
                    explicitly_requested,
                )
                && !result.pdf_raw_sha256_by_announcement.contains_key(&item.id);
            if should_download {
                let _ = self.fetch_mvp6_pdf(item, result);
            }
        }
    }

    /// Shared PDF path validation, fetch, magic check, hash recording and
    /// `pdf_unavailable` reporting for both listing notices and other MVP-6
    /// PDF categories.
    fn fetch_mvp6_pdf(
        &mut self,
        item: &Announcement,
        result: &mut FixedListResult,
    ) -> Option<(Vec<u8>, String)> {
        let Some(path) = item.attachment.as_deref().filter(|p| valid_pdf_path(p)) else {
            result
                .unresolved
                .push(format!("{}:pdf_unavailable", item.id));
            return None;
        };
        match self.fetch(Request {
            url: format!("{PDF}{path}"),
            form: BTreeMap::new(),
        }) {
            Ok((bytes, hash)) if bytes.starts_with(b"%PDF-") => {
                result
                    .pdf_raw_sha256_by_announcement
                    .entry(item.id.clone())
                    .or_default()
                    .push(hash.clone());
                Some((bytes, hash))
            }
            _ => {
                result
                    .unresolved
                    .push(format!("{}:pdf_unavailable", item.id));
                None
            }
        }
    }

    fn resolve_fixed_org_ids(&mut self, candidate: &FundCandidate) -> Result<Vec<String>> {
        const TOP_SEARCH: &str = "https://www.cninfo.com.cn/new/information/topSearch/query";
        let request = Request {
            url: TOP_SEARCH.into(),
            form: BTreeMap::from([
                ("keyWord".into(), candidate.code.clone()),
                ("maxNum".into(), "10".into()),
                ("plate".into(), "fund".into()),
            ]),
        };
        let (bytes, _) = self.fetch(request)?;
        let value: Value = serde_json::from_slice(&bytes)?;
        let mut found = BTreeSet::new();
        collect_exact_suggestions(&value, &candidate.code, &mut found);
        if !found.is_empty() {
            return Ok(found.into_iter().collect());
        }
        if candidate.short_name.trim().is_empty() {
            return Ok(Vec::new());
        }
        for (start, end) in year_segments(candidate.active_start, candidate.active_end) {
            let window = format!("{start}~{end}");
            let (items, _) = self.pages_for(&window, "", &candidate.short_name)?;
            for item in items.into_iter().filter(|a| a.code == candidate.code) {
                if let Some(org) = item.org_id {
                    found.insert(org);
                }
            }
        }
        Ok(found.into_iter().collect())
    }

    fn pages_for(
        &mut self,
        window: &str,
        stock: &str,
        searchkey: &str,
    ) -> Result<(Vec<Announcement>, BTreeSet<String>)> {
        let mut pages = PageSet::default();
        for page in 1..=10000 {
            let form = BTreeMap::from([
                ("pageNum".into(), page.to_string()),
                ("pageSize".into(), "30".into()),
                ("column".into(), "fund".into()),
                ("tabName".into(), "fulltext".into()),
                ("seDate".into(), window.into()),
                ("stock".into(), stock.into()),
                ("searchkey".into(), searchkey.into()),
                ("sortName".into(), "time".into()),
                ("sortType".into(), "asc".into()),
                ("isHLtitle".into(), "false".into()),
            ]);
            let (bytes, _) = self.fetch(Request {
                url: QUERY.into(),
                form,
            })?;
            pages.push(page, &bytes)?;
            if !pages.needs_more() {
                return pages.finish();
            }
        }
        Err("pagination safety limit exceeded".into())
    }
    /// `codes` is the caller's historical candidate set, never inferred from
    /// search success. It keeps terminated/unmapped funds in the gap report.
    pub fn collect(
        &mut self,
        mut dataset: IndexDataset,
        codes: &[String],
        converter: &mut impl PdfText,
    ) -> IndexDataset {
        let window = format!("{}~{}", dataset.window_start, dataset.window_end);
        let discovery = self.pages(&window, "");
        let (discovered, raw) = match discovery {
            Ok(value) => value,
            Err(_) => {
                dataset
                    .unknown
                    .push("discovery_pagination_unresolved".into());
                (Vec::new(), BTreeSet::new())
            }
        };
        dataset.source_raw_sha256.extend(raw);
        let candidate_codes: BTreeSet<_> = codes
            .iter()
            .cloned()
            .chain(
                discovered
                    .iter()
                    .filter(|a| scoped_code(&a.code))
                    .map(|a| a.code.clone()),
            )
            .collect();
        for code in &candidate_codes {
            let mut fund = FundIndex::new(
                code.clone(),
                discovered
                    .iter()
                    .filter(|a| &a.code == code)
                    .cloned()
                    .collect(),
            );
            if !scoped_code(code) {
                fund.unknown.push("out_of_scope".into());
                dataset.funds.push(fund);
                continue;
            }
            let org_ids = fund.org_ids.clone();
            let mut all: BTreeMap<String, Announcement> = fund
                .announcements
                .iter()
                .map(|a| (a.id.clone(), a.clone()))
                .collect();
            for org in org_ids {
                match self.pages(&window, &format!("{code},{org}")) {
                    Ok((items, raw)) => {
                        dataset.source_raw_sha256.extend(raw);
                        let found: BTreeSet<_> = items.iter().map(|a| a.id.as_str()).collect();
                        if fund
                            .announcements
                            .iter()
                            .filter(|a| a.org_id.as_deref() == Some(&org))
                            .any(|a| !found.contains(a.id.as_str()))
                        {
                            fund.unknown
                                .push("fund_query_missing_discovered_announcement".into());
                        }
                        for item in items {
                            if item.code != *code || item.org_id.as_deref() != Some(&org) {
                                fund.unknown.push("fund_query_identity_mismatch".into());
                                continue;
                            }
                            if let Some(previous) = all.insert(item.id.clone(), item.clone()) {
                                let mut previous: Announcement = previous;
                                previous.source_raw_sha256 = item.source_raw_sha256.clone();
                                if previous != item {
                                    fund.unknown.push("conflicting_announcement".into());
                                }
                            }
                        }
                    }
                    Err(_) => fund.unknown.push("fund_pagination_unresolved".into()),
                }
            }
            let gaps: Vec<_> = fund
                .unknown
                .into_iter()
                .filter(|g| g != "legal_name_unknown")
                .collect();
            fund = FundIndex::new(code.clone(), all.into_values().collect());
            fund.unknown.extend(gaps);
            for a in &fund.announcements {
                let announced = DateTime::from_timestamp_millis(a.announced_ms)
                    .unwrap()
                    .with_timezone(&chrono_tz::Asia::Shanghai)
                    .date_naive();
                if announced < dataset.window_start || announced > dataset.window_end {
                    fund.unknown
                        .push(format!("{}:publication_out_of_window", a.id));
                    continue;
                }
                let categories = classify(&a.title);
                if categories.iter().any(|c| c == "underlying_index") {
                    fund.unknown
                        .push(format!("{}:name_change_requires_review", a.id));
                }
                if !categories.iter().any(|c| {
                    ["dividend", "liquidation", "suspension", "resumption"].contains(&c.as_str())
                }) {
                    continue;
                }
                let attachment = a.attachment.as_deref().filter(|p| {
                    p.starts_with("finalpage/")
                        && !p.contains("..")
                        && (p.ends_with(".PDF") || p.ends_with(".pdf"))
                });
                let event = attachment
                    .and_then(|p| {
                        self.fetch(Request {
                            url: format!("{PDF}{p}"),
                            form: BTreeMap::new(),
                        })
                        .ok()
                    })
                    .map(|(bytes, hash)| {
                        dataset.source_raw_sha256.insert(hash);
                        extract_fields(&a.title, &bytes, converter)
                    });
                if let Some(event) = event {
                    if event["unknown"].as_array().is_some_and(|v| !v.is_empty()) {
                        fund.unknown.push(format!("{}:pdf_fields_unknown", a.id));
                    }
                    fund.events.insert(a.id.clone(), event);
                } else {
                    fund.unknown.push(format!("{}:pdf_unavailable", a.id));
                }
            }
            fund.unknown.sort();
            fund.unknown.dedup();
            dataset.funds.push(fund);
        }
        dataset
            .source_raw_sha256
            .extend(self.captured.iter().cloned());
        dataset
    }
}

fn year_segments(start: NaiveDate, end: NaiveDate) -> Vec<(NaiveDate, NaiveDate)> {
    let mut segments = Vec::new();
    let mut cursor = start;
    while cursor <= end {
        let segment_end = (cursor + ChronoDuration::days(364)).min(end);
        segments.push((cursor, segment_end));
        cursor = segment_end + ChronoDuration::days(1);
    }
    segments
}

fn collect_exact_suggestions(value: &Value, code: &str, output: &mut BTreeSet<String>) {
    match value {
        Value::Array(rows) => {
            for row in rows {
                collect_exact_suggestions(row, code, output);
            }
        }
        Value::Object(object) => {
            let row_code = object.get("code").and_then(Value::as_str);
            if row_code == Some(code) {
                if let Some(org_id) = object
                    .get("orgId")
                    .and_then(Value::as_str)
                    .filter(|v| !v.trim().is_empty())
                {
                    output.insert(org_id.to_owned());
                }
            }
            for child in object.values() {
                collect_exact_suggestions(child, code, output);
            }
        }
        _ => {}
    }
}

fn classify_mvp6(title: &str) -> Vec<String> {
    let mut categories = classify(title);
    let extra = [
        (
            "conversion",
            &["份额拆分", "基金份额拆分", "拆分"] as &[&str],
        ),
        ("holders_meeting", &["计票结果", "表决结果", "表决情况"]),
        ("termination", &["终止上市", "终止基金合同"]),
        ("liquidation_report", &["清算报告"]),
        ("fund_contract", &["基金合同"]),
        ("prospectus", &["招募说明书"]),
    ];
    for (category, words) in extra {
        if words.iter().any(|word| title.contains(word))
            && !categories.iter().any(|c| c == category)
        {
            categories.push(category.into());
        }
    }
    categories.sort();
    categories.dedup();
    categories
}

fn valid_pdf_path(path: &str) -> bool {
    path.starts_with("finalpage/")
        && !path.contains("..")
        && (path.ends_with(".PDF") || path.ends_with(".pdf"))
}

fn listing_notice_covers_metadata(text: &str) -> bool {
    let has_index = text.contains("标的指数") || text.contains("跟踪指数");
    let has_category = text.contains("基金类别") || text.contains("基金类型");
    has_index && has_category
}
