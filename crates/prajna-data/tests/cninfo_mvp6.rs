//! Synthetic-only tests for the public MVP-6 fixed-list contract.
use arrow_array::RecordBatch;
use chrono::NaiveDate;
use prajna_data::{
    BarSourceRow, Manifest, ManifestCore, ManifestProvenance, ManifestTable, RawStore, SourceKind,
    SourceRecordInput, TdxCodeTableRow, bar_sources_to_record_batch, bars_to_record_batch,
    cninfo::*, logical_hash, schema_fingerprint, sessions_to_record_batch,
    tdx_code_table_to_record_batch, write_manifest, write_parquet,
};
use prajna_domain::{Bar, BarData, InstrumentId, Price, Quantity, Session, TimestampNs, VenueId};
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, VecDeque},
    fs,
    path::Path,
    sync::{Arc, Mutex},
    time::Instant,
};

fn reply(body: impl Into<Vec<u8>>) -> HttpReply {
    HttpReply {
        status: 200,
        content_type: "application/json".into(),
        bytes: body.into(),
    }
}
fn json_reply(value: Value) -> HttpReply {
    reply(serde_json::to_vec(&value).unwrap())
}
fn ann(id: &str, title: &str) -> Value {
    json!({"announcementId":id,"secCode":"159001","orgId":"synthetic-org","announcementTitle":title,
        "announcementTime":1767571200000_i64,"adjunctUrl":format!("finalpage/2026-01-05/{id}.PDF")})
}
fn page(total: usize, rows: Vec<Value>) -> HttpReply {
    json_reply(json!({"totalAnnouncement":total,"announcements":rows}))
}
fn candidate() -> FundCandidate {
    FundCandidate {
        code: "159001".into(),
        short_name: "合成ETF".into(),
        active_start: "2026-01-01".parse().unwrap(),
        active_end: "2026-12-31".parse().unwrap(),
    }
}

struct Mock {
    replies: VecDeque<HttpReply>,
    requests: Arc<Mutex<Vec<Request>>>,
    times: Arc<Mutex<Vec<Instant>>>,
}
impl Transport for Mock {
    fn send(&mut self, request: &Request) -> Result<HttpReply> {
        self.requests.lock().unwrap().push(request.clone());
        self.times.lock().unwrap().push(Instant::now());
        self.replies
            .pop_front()
            .ok_or_else(|| "unexpected synthetic request".into())
    }
}
fn make_collector(
    replies: Vec<HttpReply>,
    root: &std::path::Path,
) -> (Collector<Mock>, Arc<Mutex<Vec<Request>>>) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let times = Arc::new(Mutex::new(Vec::new()));
    (
        Collector::new(
            Mock {
                replies: replies.into(),
                requests: requests.clone(),
                times,
            },
            RawStore::open(root).unwrap(),
        ),
        requests,
    )
}
fn top(rows: Value) -> HttpReply {
    json_reply(json!({"data":rows}))
}
struct NoopPdf;
impl PdfText for NoopPdf {
    fn text(&mut self, _: &[u8]) -> Result<String> {
        Ok("synthetic PDF text".into())
    }
}
struct CompleteListingPdf;
impl PdfText for CompleteListingPdf {
    fn text(&mut self, _: &[u8]) -> Result<String> {
        Ok("基金类型：指数基金；标的指数：合成指数".into())
    }
}
struct UnreadablePdf;
impl PdfText for UnreadablePdf {
    fn text(&mut self, _: &[u8]) -> Result<String> {
        Err("synthetic PDF parse failure".into())
    }
}

#[test]
fn top_search_keeps_only_exact_code_matches_and_preserves_multiple_org_ids() {
    let dir = tempfile::tempdir().unwrap();
    let (mut collector, requests) = make_collector(
        vec![
            top(
                json!([{"code":"1590010","orgId":"reject"},{"code":"159001","orgId":"org-a"},{"code":"159001","orgId":"org-b"}]),
            ),
            page(0, vec![]),
            page(0, vec![]),
        ],
        dir.path(),
    );
    let result = collector.collect_fixed_list(&[candidate()], &BTreeSet::new(), &mut NoopPdf);
    assert_eq!(result.resolutions[0].org_ids, vec!["org-a", "org-b"]);
    let requests = requests.lock().unwrap();
    assert_eq!(
        requests[0].url,
        "https://www.cninfo.com.cn/new/information/topSearch/query"
    );
    assert_eq!(requests[0].form["keyWord"], "159001");
    assert_eq!(requests[0].form["maxNum"], "10");
    assert_eq!(requests[0].form["plate"], "fund");
    assert!(
        requests
            .iter()
            .any(|r| r.form.get("stock").is_some_and(|v| v == "159001,org-a"))
    );
    assert!(
        requests
            .iter()
            .any(|r| r.form.get("stock").is_some_and(|v| v == "159001,org-b"))
    );
}

#[test]
fn empty_top_search_falls_back_to_exact_short_name_and_records_unresolved_when_empty() {
    let dir = tempfile::tempdir().unwrap();
    let (mut collector, requests) = make_collector(
        vec![
            top(json!([])),
            page(
                1,
                vec![{
                    let mut row = ann("fallback", "合成ETF公告");
                    row["orgId"] = json!("fallback-org");
                    row
                }],
            ),
            page(0, vec![]),
        ],
        dir.path(),
    );
    let result = collector.collect_fixed_list(&[candidate()], &BTreeSet::new(), &mut NoopPdf);
    assert_eq!(result.resolutions[0].org_ids, vec!["fallback-org"]);
    assert_eq!(requests.lock().unwrap()[1].form["searchkey"], "合成ETF");

    let dir = tempfile::tempdir().unwrap();
    let (mut collector, requests) =
        make_collector(vec![top(json!([])), page(0, vec![])], dir.path());
    let result = collector.collect_fixed_list(&[candidate()], &BTreeSet::new(), &mut NoopPdf);
    assert_eq!(result.resolutions[0].status, "org_id_unresolved");
    assert!(
        result
            .unresolved
            .contains(&"159001:org_id_unresolved".into())
    );
    assert_eq!(requests.lock().unwrap().len(), 2);
}

#[test]
fn fixed_index_flags_incomplete_totals_and_downloads_new_pdf_categories_only() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = HttpReply {
        status: 200,
        content_type: "application/pdf".into(),
        bytes: b"%PDF-1.4 synthetic".to_vec(),
    };
    let (mut collector, requests) = make_collector(
        vec![
            top(json!([{"code":"159001","orgId":"synthetic-org"}])),
            page(
                2,
                vec![
                    ann("split", "基金份额拆分公告"),
                    ann("report", "2026年第一季度报告"),
                ],
            ),
            pdf,
        ],
        dir.path(),
    );
    let result = collector.collect_fixed_list(&[candidate()], &BTreeSet::new(), &mut NoopPdf);
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests[2].url.ends_with("/split.PDF"));
    assert!(!requests.iter().any(|r| r.url.ends_with("/report.PDF")));
    assert_eq!(result.pdf_raw_sha256_by_announcement["split"].len(), 1);
    assert!(
        !result
            .unresolved
            .iter()
            .any(|s| s.contains("pagination_incomplete"))
    );

    let dir = tempfile::tempdir().unwrap();
    let (mut collector, _) = make_collector(
        vec![
            top(json!([{"code":"159001","orgId":"synthetic-org"}])),
            json_reply(
                json!({"totalAnnouncement":2,"hasMore":true,"announcements":[ann("one", "收益分配公告")]}),
            ),
            json_reply(
                json!({"totalAnnouncement":2,"hasMore":false,"announcements":[ann("one", "收益分配公告")]}),
            ),
        ],
        dir.path(),
    );
    let result = collector.collect_fixed_list(&[candidate()], &BTreeSet::new(), &mut NoopPdf);
    assert!(
        result
            .unresolved
            .contains(&"159001:synthetic-org:pagination_incomplete".into())
    );
}

#[test]
fn fixed_list_access_denial_stops_source_before_short_name_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let denied = HttpReply {
        status: 403,
        content_type: "text/html".into(),
        bytes: b"synthetic access denied".to_vec(),
    };
    let (mut collector, requests) = make_collector(vec![denied], dir.path());
    let result = collector.collect_fixed_list(&[candidate()], &BTreeSet::new(), &mut NoopPdf);
    assert_eq!(requests.lock().unwrap().len(), 1);
    assert_eq!(result.resolutions[0].status, "org_id_unresolved");
}

#[test]
fn fixed_queries_split_long_lifetimes_into_non_overlapping_max_365_day_windows() {
    let dir = tempfile::tempdir().unwrap();
    let (mut collector, requests) = make_collector(
        vec![
            top(json!([{"code":"159001","orgId":"synthetic-org"}])),
            page(0, vec![]),
            page(0, vec![]),
            page(0, vec![]),
        ],
        dir.path(),
    );
    let mut long = candidate();
    long.active_start = "2024-01-01".parse().unwrap();
    long.active_end = "2026-01-01".parse().unwrap();
    collector.collect_fixed_list(&[long], &BTreeSet::new(), &mut NoopPdf);
    let requests = requests.lock().unwrap();
    let windows = requests
        .iter()
        .skip(1)
        .map(|r| r.form["seDate"].clone())
        .collect::<Vec<_>>();
    assert_eq!(windows.len(), 3);
    let parsed = windows
        .iter()
        .map(|window| {
            let (start, end) = window.split_once('~').unwrap();
            (date(start), date(end))
        })
        .collect::<Vec<_>>();
    for (start, end) in &parsed {
        assert!((*end - *start).num_days() < 365);
    }
    for pair in parsed.windows(2) {
        assert_eq!(pair[0].1.succ_opt().unwrap(), pair[1].0);
    }
}

fn facts() -> Mvp6Facts {
    Mvp6Facts {
        identities: vec![IdentityFact {
            code: "159001".into(),
            org_id: "org".into(),
            legal_name: "合成ETF".into(),
            listing_date: "2026-01-05".into(),
            termination_date: "".into(),
            fund_category: "index".into(),
            tracked_index: "".into(),
            evidence_level: "official".into(),
            source_announcement_id: "identity".into(),
            source_page: "1".into(),
        }],
        actions: vec![ActionFact {
            code: "159001".into(),
            kind: "dividend".into(),
            announcement_date: "2026-01-05".into(),
            record_date: "".into(),
            ex_date: "2026-01-06".into(),
            pay_date: "".into(),
            cash_per_share: "".into(),
            split_ratio: "".into(),
            liquidation_per_share: "".into(),
            evidence_level: "official".into(),
            source_announcement_id: "action".into(),
            source_page: "1".into(),
        }],
        events: vec![EventFact {
            code: "159001".into(),
            kind: "suspension".into(),
            announcement_date: "2026-01-05".into(),
            effective_date: "2026-01-07".into(),
            session_part: "full_day".into(),
            evidence_level: "official".into(),
            source_announcement_id: "event".into(),
            source_page: "1".into(),
        }],
    }
}
fn evidence_fields(fields: &[&str]) -> BTreeSet<String> {
    fields.iter().map(|field| (*field).to_owned()).collect()
}
fn source_record(id: &str) -> SourceRecordInput {
    SourceRecordInput {
        content_type: "application/json".into(),
        source_kind: SourceKind::Fixture,
        source_id: "cninfo".into(),
        request: json!({"url": format!("https://example.test/{id}")}),
        observed_at: "synthetic".into(),
        ingested_by: "cninfo-mvp6-test".into(),
    }
}
fn citation_fixture(store: &RawStore) -> Vec<RawCitation> {
    let mut citations = Vec::new();
    for (id, fields) in [
        (
            "identity",
            &[
                "code",
                "org_id",
                "legal_name",
                "listing_date",
                "fund_category",
            ][..],
        ),
        (
            "action",
            &["code", "kind", "announcement_date", "ex_date"][..],
        ),
        (
            "event",
            &[
                "code",
                "kind",
                "announcement_date",
                "effective_date",
                "session_part",
            ][..],
        ),
    ] {
        let bytes = serde_json::to_vec(&json!({"announcementId": id})).unwrap();
        let hash = store.put(&bytes, source_record(id)).unwrap();
        citations
            .push(RawCitation::from_store(store, id, 1, hash, evidence_fields(fields)).unwrap());
    }
    citations
}
fn raw_fixture() -> (tempfile::TempDir, RawStore, Vec<RawCitation>) {
    let dir = tempfile::tempdir().unwrap();
    let store = RawStore::open(dir.path()).unwrap();
    let citations = citation_fixture(&store);
    (dir, store, citations)
}
fn calendar() -> BTreeSet<NaiveDate> {
    ["2026-01-05", "2026-01-06", "2026-01-07"]
        .into_iter()
        .map(|s| s.parse().unwrap())
        .collect()
}
fn date(value: &str) -> NaiveDate {
    value.parse().unwrap()
}
fn price(value: &str) -> Price {
    Price::parse(value).unwrap()
}

#[test]
fn public_facts_validate_raw_calendar_lifecycle_required_fields_and_explicit_evidence() {
    let (_dir, store, evidence) = raw_fixture();
    let f = facts();
    let converted = validate_and_convert(&f, &store, &evidence, &calendar()).unwrap();
    assert_eq!(
        converted.corporate_actions.schema,
        "cninfo_corporateActions_structured_v2"
    );
    assert_eq!(converted.risk_events[0].session_part, "full_day");
    assert!(
        validate_and_convert(&f, &store, &evidence[1..], &calendar())
            .unwrap_err()
            .to_string()
            .contains("missing Raw citation")
    );
    let mut missing_raw = evidence.clone();
    missing_raw[0].raw_sha256 = format!("sha256:{}", "0".repeat(64));
    assert!(
        validate_and_convert(&f, &store, &missing_raw, &calendar())
            .unwrap_err()
            .to_string()
            .contains("unavailable")
    );
    let mut fabricated = evidence.clone();
    fabricated[2].announcement_id = "invented".into();
    assert!(
        validate_and_convert(&f, &store, &fabricated, &calendar())
            .unwrap_err()
            .to_string()
            .contains("does not reference")
    );
    let mut outside = f.clone();
    outside.events[0].effective_date = "2026-01-08".into();
    assert!(
        validate_and_convert(&outside, &store, &evidence, &calendar())
            .unwrap_err()
            .to_string()
            .contains("outside frozen calendar")
    );
    let mut overlap = f.clone();
    let mut second = overlap.identities[0].clone();
    second.org_id = "another".into();
    second.source_announcement_id = "identity".into();
    overlap.identities.push(second);
    assert!(
        validate_and_convert(&overlap, &store, &evidence, &calendar())
            .unwrap_err()
            .to_string()
            .contains("overlapping lifecycle")
    );
    let mut missing = f.clone();
    missing.identities[0].legal_name.clear();
    assert!(
        validate_and_convert(&missing, &store, &evidence, &calendar())
            .unwrap_err()
            .to_string()
            .contains("legal_name")
    );
    let mut inferred = f;
    inferred.identities[0].tracked_index = "synthetic-index".into();
    assert!(
        validate_and_convert(&inferred, &store, &evidence, &calendar())
            .unwrap_err()
            .to_string()
            .contains("not evidenced")
    );
    let mut reversed = facts();
    reversed.identities[0].termination_date = "2025-12-31".into();
    let mut reversed_evidence = evidence.clone();
    reversed_evidence[0]
        .evidenced_fields
        .insert("termination_date".into());
    assert!(
        validate_and_convert(&reversed, &store, &reversed_evidence, &calendar())
            .unwrap_err()
            .to_string()
            .contains("precedes")
    );
    let mut malformed = facts();
    malformed.actions[0].cash_per_share = "1e-3".into();
    let mut decimal_evidence = evidence.clone();
    decimal_evidence[1]
        .evidenced_fields
        .insert("cash_per_share".into());
    assert!(
        validate_and_convert(&malformed, &store, &decimal_evidence, &calendar())
            .unwrap_err()
            .to_string()
            .contains("invalid decimal")
    );
    let mut bad_code = facts();
    bad_code.identities[0].code = "160001".into();
    assert!(
        validate_and_convert(&bad_code, &store, &evidence, &calendar())
            .unwrap_err()
            .to_string()
            .contains("invalid fund code")
    );
}

#[test]
fn citation_csv_binds_each_row_to_a_frozen_raw_object_and_rejects_duplicates() {
    let (_dir, store, evidence) = raw_fixture();
    let csv = format!(
        "announcement_id,page,raw_sha256,evidenced_fields\n\
         identity,1,{},code;org_id;legal_name;listing_date;fund_category\n\
         action,1,{},code;kind;announcement_date;ex_date\n\
         event,1,{},code;kind;announcement_date;effective_date;session_part\n",
        evidence[0].raw_sha256, evidence[1].raw_sha256, evidence[2].raw_sha256
    );
    let citations = read_citations_csv(&csv, &store).unwrap();
    assert_eq!(citations.len(), 3);
    let action = citations
        .iter()
        .find(|citation| citation.announcement_id == "action")
        .unwrap();
    assert!(action.evidenced_fields.contains("ex_date"));

    let missing = csv.replace(
        &evidence[0].raw_sha256,
        &format!("sha256:{}", "0".repeat(64)),
    );
    assert!(
        read_citations_csv(&missing, &store)
            .unwrap_err()
            .to_string()
            .contains("unavailable")
    );
    let duplicate = format!(
        "{csv}identity,1,{},code;org_id;legal_name;listing_date;fund_category\n",
        evidence[0].raw_sha256
    );
    assert!(
        read_citations_csv(&duplicate, &store)
            .unwrap_err()
            .to_string()
            .contains("duplicate citation")
    );
}

#[test]
fn crosscheck_detects_listing_action_and_suspension_disagreements() {
    let mut f = facts();
    f.identities[0].listing_date = "2026-01-06".into();
    f.identities[0].termination_date = "2026-01-06".into();
    let bars = vec![
        V4DailyBar {
            code: "159001".into(),
            date: date("2026-01-05"),
            close: Some(price("1.0")),
            previous_close: Some(price("1.0")),
            reference_price: Some(price("1.0")),
            keyed_no_trade: false,
        },
        V4DailyBar {
            code: "159001".into(),
            date: date("2026-01-06"),
            close: Some(price("1.0")),
            previous_close: Some(price("1.0")),
            reference_price: Some(price("1.0")),
            keyed_no_trade: false,
        },
        V4DailyBar {
            code: "159001".into(),
            date: date("2026-01-07"),
            close: Some(price("1.0")),
            previous_close: Some(price("1.0")),
            reference_price: Some(price("1.0")),
            keyed_no_trade: false,
        },
    ];
    let report = crosscheck_v4(&f, &bars);
    let kinds = report
        .mismatches
        .iter()
        .map(|m| m.kind.as_str())
        .collect::<BTreeSet<_>>();
    assert!(kinds.contains("listing_vs_first_bar"));
    assert!(kinds.contains("termination_vs_last_bar"));
    assert!(kinds.contains("action_date_vs_reference_price_jump"));
    assert!(kinds.contains("suspension_vs_keyed_no_trade"));
    let (_dir, store, evidence) = raw_fixture();
    let converted = validate_and_convert(&facts(), &store, &evidence, &calendar()).unwrap();
    let manual = Mvp6Session {
        catalog_lifecycles: vec![CatalogLifecycle {
            code: "159001".into(),
            org_id: "org".into(),
            legal_name: "合成ETF".into(),
            listing_date: date("2026-01-05"),
            termination_date: None,
            fund_category: "index".into(),
            tracked_index: "".into(),
            evidence_level: EvidenceLevel::Official,
            source_announcement_id: "identity".into(),
            source_page: 1,
        }],
        corporate_actions: StructuredCorporateActionsV2 {
            schema: "cninfo_corporateActions_structured_v2".into(),
            version: 2,
            actions: vec![CorporateActionV2 {
                code: "159001".into(),
                kind: "dividend".into(),
                announcement_date: date("2026-01-05"),
                record_date: None,
                ex_date: Some(date("2026-01-06")),
                pay_date: None,
                cash_per_share: None,
                split_ratio: None,
                liquidation_per_share: None,
                evidence_level: EvidenceLevel::Official,
                source_announcement_id: "action".into(),
                source_page: 1,
            }],
        },
        risk_events: vec![RiskEventV1 {
            code: "159001".into(),
            kind: "suspension".into(),
            announcement_date: date("2026-01-05"),
            effective_date: date("2026-01-07"),
            session_part: "full_day".into(),
            evidence_level: EvidenceLevel::Official,
            source_announcement_id: "event".into(),
            source_page: 1,
        }],
    };
    assert_eq!(converted, manual);
}

#[test]
fn csv_parser_enforces_headers_and_accepts_empty_templates_without_inventing_rows() {
    let identity = include_str!("../../../configs/mvp-6/identity.csv");
    let actions = include_str!("../../../configs/mvp-6/actions.csv");
    let events = include_str!("../../../configs/mvp-6/events.csv");
    let empty = Mvp6Facts::from_csv(identity, actions, events).unwrap();
    assert!(empty.identities.is_empty() && empty.actions.is_empty() && empty.events.is_empty());
    let invalid = "code,org_id\n159001,org\n";
    assert!(
        Mvp6Facts::from_csv(invalid, actions, events)
            .unwrap_err()
            .to_string()
            .contains("header mismatch")
    );
}

#[test]
fn contract_and_prospectus_download_is_gated_and_periodic_reports_never_download() {
    assert!(should_download_pdf(&["fund_contract".into()], false, false));
    assert!(!should_download_pdf(&["fund_contract".into()], true, false));
    assert!(should_download_pdf(&["fund_contract".into()], true, true));
    assert!(!should_download_pdf(
        &["periodic_report".into(), "conversion".into()],
        false,
        true
    ));
}

#[test]
fn fixed_collector_reads_listing_notice_before_applying_contract_download_gate() {
    let rows = vec![
        ann("listing", "上市交易公告书"),
        ann("contract", "基金合同"),
    ];
    let dir = tempfile::tempdir().unwrap();
    let (mut collector, requests) = make_collector(
        vec![
            top(json!([{"code":"159001","orgId":"synthetic-org"}])),
            page(2, rows.clone()),
            HttpReply {
                status: 200,
                content_type: "application/pdf".into(),
                bytes: b"%PDF-1.4 synthetic listing".to_vec(),
            },
        ],
        dir.path(),
    );
    collector.collect_fixed_list(&[candidate()], &BTreeSet::new(), &mut CompleteListingPdf);
    assert_eq!(requests.lock().unwrap().len(), 3);
    assert!(requests.lock().unwrap()[2].url.ends_with("/listing.PDF"));

    // A contract can be announced before its listing notice. The complete
    // notice still supplies the fund metadata used by the download gate.
    let mut earlier_contract = ann("contract", "基金合同");
    earlier_contract["announcementTime"] = json!(1767484800000_i64);
    let mut later_listing = ann("listing", "上市交易公告书");
    later_listing["announcementTime"] = json!(1767571200000_i64);
    let dir = tempfile::tempdir().unwrap();
    let (mut collector, requests) = make_collector(
        vec![
            top(json!([{"code":"159001","orgId":"synthetic-org"}])),
            page(2, vec![earlier_contract, later_listing]),
            HttpReply {
                status: 200,
                content_type: "application/pdf".into(),
                bytes: b"%PDF-1.4 synthetic listing".to_vec(),
            },
        ],
        dir.path(),
    );
    collector.collect_fixed_list(&[candidate()], &BTreeSet::new(), &mut CompleteListingPdf);
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests[2].url.ends_with("/listing.PDF"));

    let dir = tempfile::tempdir().unwrap();
    let (mut collector, requests) = make_collector(
        vec![
            top(json!([{"code":"159001","orgId":"synthetic-org"}])),
            page(2, rows),
            HttpReply {
                status: 200,
                content_type: "application/pdf".into(),
                bytes: b"%PDF-1.4 synthetic listing".to_vec(),
            },
            HttpReply {
                status: 200,
                content_type: "application/pdf".into(),
                bytes: b"%PDF-1.4 synthetic contract".to_vec(),
            },
        ],
        dir.path(),
    );
    collector.collect_fixed_list(
        &[candidate()],
        &BTreeSet::from(["fund_contract".into()]),
        &mut CompleteListingPdf,
    );
    assert_eq!(requests.lock().unwrap().len(), 4);
    assert!(requests.lock().unwrap()[3].url.ends_with("/contract.PDF"));
}

#[test]
fn fixed_collector_reports_missing_or_invalid_listing_notice_pdf_as_unresolved() {
    for attachment in [None, Some("../outside.pdf")] {
        let mut listing = ann("listing", "上市交易公告书");
        if let Some(path) = attachment {
            listing["adjunctUrl"] = json!(path);
        } else {
            listing.as_object_mut().unwrap().remove("adjunctUrl");
        }

        let dir = tempfile::tempdir().unwrap();
        let (mut collector, requests) = make_collector(
            vec![
                top(json!([{"code":"159001","orgId":"synthetic-org"}])),
                page(1, vec![listing]),
            ],
            dir.path(),
        );
        let result =
            collector.collect_fixed_list(&[candidate()], &BTreeSet::new(), &mut CompleteListingPdf);

        assert!(
            result
                .unresolved
                .iter()
                .any(|gap| gap == "listing:pdf_unavailable")
        );
        assert_eq!(requests.lock().unwrap().len(), 2);
    }
}

#[test]
fn fixed_collector_reports_unreadable_listing_notice_as_unresolved() {
    let dir = tempfile::tempdir().unwrap();
    let (mut collector, _) = make_collector(
        vec![
            top(json!([{"code":"159001","orgId":"synthetic-org"}])),
            page(1, vec![ann("listing", "上市交易公告书")]),
            HttpReply {
                status: 200,
                content_type: "application/pdf".into(),
                bytes: b"%PDF-1.4 synthetic".to_vec(),
            },
        ],
        dir.path(),
    );
    let result = collector.collect_fixed_list(&[candidate()], &BTreeSet::new(), &mut UnreadablePdf);

    assert!(
        result
            .unresolved
            .iter()
            .any(|gap| gap == "listing:pdf_unavailable")
    );
}

fn v4_session(value: &str) -> Session {
    let date: NaiveDate = value.parse().unwrap();
    let open = TimestampNs::parse(&format!("{date}T09:30:00+08:00")).unwrap();
    let close = TimestampNs::parse(&format!("{date}T15:00:00+08:00")).unwrap();
    Session::new(VenueId::new("SZSE").unwrap(), date, open, close).unwrap()
}
fn v4_bar(session: &Session, close: &str) -> Bar {
    Bar::new(
        BarData {
            instrument_id: InstrumentId::new("159001", VenueId::new("SZSE").unwrap()).unwrap(),
            bar_spec: "1d@session".parse().unwrap(),
            session_date: session.session_date(),
            ts_open: session.ts_open(),
            ts_close: session.ts_close(),
            open: Price::parse(close).unwrap(),
            high: Price::parse(close).unwrap(),
            low: Price::parse(close).unwrap(),
            close: Price::parse(close).unwrap(),
            volume: Quantity::parse("100").unwrap(),
            amount: None,
            available_at: None,
        },
        Some(session),
    )
    .unwrap()
}
fn manifest_table(name: &str, batch: &RecordBatch, primary_key: &[&str]) -> ManifestTable {
    ManifestTable {
        table: name.into(),
        schema_version: 1,
        schema_fingerprint: schema_fingerprint(batch.schema().as_ref()).unwrap(),
        logical_hash: logical_hash(
            name,
            batch.schema().as_ref(),
            std::slice::from_ref(batch),
            primary_key,
        )
        .unwrap(),
        row_count: batch.num_rows() as u64,
    }
}
fn publish_synthetic_v4(
    root: &Path,
    sessions: &[Session],
    bars: &[Bar],
    code_tables: &[TdxCodeTableRow],
) -> String {
    let sessions_batch = sessions_to_record_batch(sessions).unwrap();
    let bars_batch = bars_to_record_batch(bars).unwrap();
    let bar_sources_batch = bar_sources_to_record_batch(
        &bars
            .iter()
            .map(|bar| BarSourceRow {
                instrument_id: bar.instrument_id().clone(),
                session_date: bar.session_date(),
                source: "tdx".into(),
                source_raw_sha256: format!("sha256:{}", "3".repeat(64)),
                reference_price: Some(bar.close()),
            })
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let code_batch = tdx_code_table_to_record_batch(code_tables).unwrap();
    let manifest = Manifest::new(
        ManifestCore {
            manifest_version: 1,
            normalizer: prajna_data::NormalizerIdentity {
                id: "szse-etf-frozen".into(),
                version: "4".into(),
                config_sha256: format!("sha256:{}", "2".repeat(64)),
            },
            inputs: Vec::new(),
            tables: vec![
                manifest_table(
                    "bar_sources",
                    &bar_sources_batch,
                    &["instrument_id", "session_date"],
                ),
                manifest_table(
                    "bars",
                    &bars_batch,
                    &["instrument_id", "bar_spec", "session_date"],
                ),
                manifest_table("sessions", &sessions_batch, &["venue_id", "session_date"]),
                manifest_table("tdx_code_table", &code_batch, &["session_date"]),
            ],
        },
        ManifestProvenance {
            files: Vec::new(),
            writer: json!({"name": "synthetic-test"}),
            created_at: "synthetic".into(),
            git_rev: None,
            git_dirty: None,
            host: None,
            synthetic_assumptions: vec!["synthetic v4 projection".into()],
            coverage: json!({}),
            ignored_input_fields: Vec::new(),
        },
    )
    .unwrap();
    write_manifest(root, &manifest).unwrap();
    let hex = manifest.dsv.strip_prefix("dsv:sha256:").unwrap();
    for (name, batch) in [
        ("bar_sources", &bar_sources_batch),
        ("bars", &bars_batch),
        ("sessions", &sessions_batch),
        ("tdx_code_table", &code_batch),
    ] {
        let directory = root.join("normalized").join(name).join(hex);
        fs::create_dir_all(&directory).unwrap();
        write_parquet(directory.join("part-00000.parquet"), batch, &manifest.dsv).unwrap();
    }
    manifest.dsv
}

#[test]
fn local_acceptance_loads_v4_projection_and_persists_the_crosscheck_report() {
    let (dir, store, evidence) = raw_fixture();
    let root = dir.path();
    let sessions = [
        v4_session("2026-01-05"),
        v4_session("2026-01-06"),
        v4_session("2026-01-07"),
    ];
    let bars = [v4_bar(&sessions[0], "1.0"), v4_bar(&sessions[1], "1.0")];
    let code_tables = sessions
        .iter()
        .map(|session| TdxCodeTableRow {
            session_date: session.session_date(),
            native_symbols: vec!["159001".into()],
            code_ranges: vec![(158000, 159999)],
            source_raw_sha256: format!("sha256:{}", "1".repeat(64)),
        })
        .collect::<Vec<_>>();
    let dsv = publish_synthetic_v4(root, &sessions, &bars, &code_tables);
    let acceptance_dir = root.join("acceptance/cninfo-fixed/synthetic");
    let acceptance =
        write_facts_acceptance(&acceptance_dir, root, &dsv, &facts(), &evidence, &store).unwrap();

    assert!(acceptance.facts_session_path.exists());
    assert_eq!(acceptance.report.mismatches.len(), 1);
    assert_eq!(
        acceptance.report.mismatches[0].kind,
        "action_date_vs_reference_price_jump"
    );
    let report: Value =
        serde_json::from_slice(&fs::read(&acceptance.crosscheck_path).unwrap()).unwrap();
    assert_eq!(report["contract"], "mvp-6-cninfo-crosscheck-v1");
    assert_eq!(report["dsv"], dsv);
    assert_eq!(report["mismatch_count"], 1);
    assert_eq!(report["facts_rows"]["identities"], 1);

    let projection = load_v4_projection(root, &dsv, &facts()).unwrap();
    let keyed = projection
        .bars
        .iter()
        .find(|bar| bar.date == date("2026-01-07"))
        .unwrap();
    assert!(keyed.keyed_no_trade);
    assert!(keyed.close.is_none());
    assert_eq!(projection.calendar.len(), 3);
}

#[test]
fn local_fixed_list_workflow_writes_collection_facts_and_crosscheck_reports() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = RawStore::open(root).unwrap();
    let citations = citation_fixture(&store);
    let sessions = [
        v4_session("2026-01-05"),
        v4_session("2026-01-06"),
        v4_session("2026-01-07"),
    ];
    let bars = [v4_bar(&sessions[0], "1.0"), v4_bar(&sessions[1], "1.0")];
    let code_tables = sessions
        .iter()
        .map(|session| TdxCodeTableRow {
            session_date: session.session_date(),
            native_symbols: vec!["159001".into()],
            code_ranges: vec![(158000, 159999)],
            source_raw_sha256: format!("sha256:{}", "1".repeat(64)),
        })
        .collect::<Vec<_>>();
    let dsv = publish_synthetic_v4(root, &sessions, &bars, &code_tables);

    let facts_dir = root.join("facts");
    fs::create_dir_all(&facts_dir).unwrap();
    fs::write(
        facts_dir.join("identity.csv"),
        "code,org_id,legal_name,listing_date,termination_date,fund_category,tracked_index,evidence_level,source_announcement_id,source_page\n\
         159001,org,合成ETF,2026-01-05,,index,,official,identity,1\n",
    )
    .unwrap();
    fs::write(
        facts_dir.join("actions.csv"),
        "code,kind,announcement_date,record_date,ex_date,pay_date,cash_per_share,split_ratio,liquidation_per_share,evidence_level,source_announcement_id,source_page\n\
         159001,dividend,2026-01-05,,2026-01-06,,,,,official,action,1\n",
    )
    .unwrap();
    fs::write(
        facts_dir.join("events.csv"),
        "code,kind,announcement_date,effective_date,session_part,evidence_level,source_announcement_id,source_page\n\
         159001,suspension,2026-01-05,2026-01-07,full_day,official,event,1\n",
    )
    .unwrap();
    fs::write(
        facts_dir.join("citations.csv"),
        format!(
            "announcement_id,page,raw_sha256,evidenced_fields\n\
             identity,1,{},code;org_id;legal_name;listing_date;fund_category\n\
             action,1,{},code;kind;announcement_date;ex_date\n\
             event,1,{},code;kind;announcement_date;effective_date;session_part\n",
            citations[0].raw_sha256, citations[1].raw_sha256, citations[2].raw_sha256
        ),
    )
    .unwrap();
    fs::create_dir_all(root.join("acceptance")).unwrap();
    fs::write(
        root.join("acceptance/mvp6-coverage.json"),
        serde_json::to_vec(&json!({"dataset_published": true, "dsv": dsv})).unwrap(),
    )
    .unwrap();

    let (mut collector, requests) = make_collector(
        vec![
            top(json!([{"code":"159001","orgId":"synthetic-org"}])),
            page(0, vec![]),
        ],
        root,
    );
    let acceptance = collect_and_accept(
        root,
        &store,
        &mut collector,
        &[candidate()],
        &BTreeSet::new(),
        &mut NoopPdf,
    )
    .unwrap();

    assert!(acceptance.collection_path.exists());
    assert!(acceptance.unresolved.is_empty());
    assert_eq!(requests.lock().unwrap().len(), 2);
    let facts = acceptance.facts.unwrap();
    assert!(facts.facts_session_path.exists());
    assert!(facts.crosscheck_path.exists());
    assert_eq!(facts.report.mismatches.len(), 1);
    let report: Value = serde_json::from_slice(&fs::read(&facts.crosscheck_path).unwrap()).unwrap();
    assert_eq!(report["mismatch_count"], 1);
    assert_eq!(
        report["mismatches"][0]["kind"],
        "action_date_vs_reference_price_jump"
    );
}

#[test]
fn crosscheck_accepts_a_one_tick_action_jump() {
    let bars = vec![V4DailyBar {
        code: "159001".into(),
        date: date("2026-01-06"),
        close: Some(price("1.001")),
        previous_close: Some(price("1.0")),
        reference_price: Some(price("1.001")),
        keyed_no_trade: false,
    }];
    let report = crosscheck_v4(&facts(), &bars);
    assert!(
        !report
            .mismatches
            .iter()
            .any(|mismatch| mismatch.kind == "action_date_vs_reference_price_jump")
    );
}
