//! Synthetic-only tests for the public MVP-6 fixed-list contract.
use prajna_data::{RawStore, cninfo::*};
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, VecDeque},
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
fn citation(id: &str, fields: &[&str]) -> RawCitation {
    RawCitation {
        announcement_id: id.into(),
        page: 1,
        evidenced_fields: fields.iter().map(|s| s.to_string()).collect(),
    }
}
fn calendar() -> BTreeSet<chrono::NaiveDate> {
    ["2026-01-05", "2026-01-06", "2026-01-07"]
        .into_iter()
        .map(|s| s.parse().unwrap())
        .collect()
}
fn date(value: &str) -> chrono::NaiveDate {
    value.parse().unwrap()
}
fn raw() -> Vec<RawCitation> {
    vec![
        citation(
            "identity",
            &[
                "code",
                "org_id",
                "legal_name",
                "listing_date",
                "fund_category",
            ],
        ),
        citation("action", &["code", "kind", "announcement_date", "ex_date"]),
        citation(
            "event",
            &[
                "code",
                "kind",
                "announcement_date",
                "effective_date",
                "session_part",
            ],
        ),
    ]
}

#[test]
fn public_facts_validate_raw_calendar_lifecycle_required_fields_and_explicit_evidence() {
    let f = facts();
    let converted = validate_and_convert(&f, &raw(), &calendar()).unwrap();
    assert_eq!(
        converted.corporate_actions.schema,
        "cninfo_corporateActions_structured_v2"
    );
    assert_eq!(converted.risk_events[0].session_part, "full_day");
    assert!(
        validate_and_convert(&f, &raw()[1..], &calendar())
            .unwrap_err()
            .to_string()
            .contains("missing Raw citation")
    );
    let mut outside = f.clone();
    outside.events[0].effective_date = "2026-01-08".into();
    assert!(
        validate_and_convert(&outside, &raw(), &calendar())
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
        validate_and_convert(&overlap, &raw(), &calendar())
            .unwrap_err()
            .to_string()
            .contains("overlapping lifecycle")
    );
    let mut missing = f.clone();
    missing.identities[0].legal_name.clear();
    assert!(
        validate_and_convert(&missing, &raw(), &calendar())
            .unwrap_err()
            .to_string()
            .contains("legal_name")
    );
    let mut inferred = f;
    inferred.identities[0].tracked_index = "synthetic-index".into();
    assert!(
        validate_and_convert(&inferred, &raw(), &calendar())
            .unwrap_err()
            .to_string()
            .contains("not evidenced")
    );
    let mut reversed = facts();
    reversed.identities[0].termination_date = "2025-12-31".into();
    let mut evidence = raw();
    evidence[0]
        .evidenced_fields
        .insert("termination_date".into());
    assert!(
        validate_and_convert(&reversed, &evidence, &calendar())
            .unwrap_err()
            .to_string()
            .contains("precedes")
    );
    let mut malformed = facts();
    malformed.actions[0].cash_per_share = "1e-3".into();
    let mut evidence = raw();
    evidence[1].evidenced_fields.insert("cash_per_share".into());
    assert!(
        validate_and_convert(&malformed, &evidence, &calendar())
            .unwrap_err()
            .to_string()
            .contains("invalid decimal")
    );
    let mut bad_code = facts();
    bad_code.identities[0].code = "160001".into();
    assert!(
        validate_and_convert(&bad_code, &raw(), &calendar())
            .unwrap_err()
            .to_string()
            .contains("invalid fund code")
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
            date: "2026-01-05".parse().unwrap(),
            close: Some(1.0),
            previous_close: Some(1.0),
            keyed_no_trade: true,
        },
        V4DailyBar {
            code: "159001".into(),
            date: "2026-01-06".parse().unwrap(),
            close: Some(1.0),
            previous_close: Some(1.0),
            keyed_no_trade: false,
        },
        V4DailyBar {
            code: "159001".into(),
            date: "2026-01-07".parse().unwrap(),
            close: Some(1.0),
            previous_close: Some(1.0),
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
    assert!(kinds.contains("action_date_vs_previous_close_jump"));
    assert!(kinds.contains("suspension_vs_keyed_no_trade"));
    assert!(kinds.contains("keyed_no_trade_vs_suspension"));
    let converted = validate_and_convert(&facts(), &raw(), &calendar()).unwrap();
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
