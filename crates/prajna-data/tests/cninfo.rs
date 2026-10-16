//! All names, PDFs, identifiers and pages here are invented synthetic fixtures.
use prajna_data::{ManifestProvenance, RawStore, cninfo::*};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Instant,
};

fn announcement(id: &str, code: &str, org: Value, title: &str) -> Value {
    json!({"announcementId":id,"secCode":code,"orgId":org,"announcementTitle":title,"announcementTime":1767571200000_i64,"adjunctUrl":"finalpage/2026-01-05/synthetic.PDF"})
}
fn page(total: usize, rows: Vec<Value>) -> Vec<u8> {
    serde_json::to_vec(&json!({"totalAnnouncement":total,"announcements":rows,"hasMore":false}))
        .unwrap()
}
fn name() -> &'static str {
    "合成甲宽基交易型开放式指数证券投资基金"
}
fn title() -> String {
    format!("{}二〇二六年第一季度报告", name())
}
fn dataset() -> IndexDataset {
    IndexDataset::new("2026-01-01".parse().unwrap(), "2026-01-31".parse().unwrap()).unwrap()
}
fn provenance() -> ManifestProvenance {
    serde_json::from_value(json!({"created_at":"synthetic"})).unwrap()
}

#[test]
fn pagination_deduplicates_ids_checks_counts_revisions_and_gaps() {
    let a = announcement("a", "158001", json!("manager"), &title());
    let b = announcement("b", "159001", json!("manager"), &title());
    let mut pages = PageSet::default();
    pages.push(1, &page(2, vec![a.clone()])).unwrap();
    pages.push(2, &page(2, vec![a.clone(), b])).unwrap();
    let (items, raw) = pages.finish().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(raw.len(), 2);
    let mut pages = PageSet::default();
    assert!(pages.push(2, &page(1, vec![a.clone()])).is_err());
    pages.push(1, &page(2, vec![a.clone()])).unwrap();
    assert!(pages.finish().is_err());
    let mut pages = PageSet::default();
    pages.push(1, &page(2, vec![a.clone()])).unwrap();
    assert!(pages.push(2, &page(3, vec![a.clone()])).is_err());
    let mut changed = a.clone();
    changed["orgId"] = json!("different-manager");
    let mut pages = PageSet::default();
    pages.push(1, &page(1, vec![a])).unwrap();
    assert!(pages.push(2, &page(1, vec![changed])).is_err());
    let mut empty = PageSet::default();
    empty.push(1, &page(0, vec![])).unwrap();
    assert!(empty.finish().unwrap().0.is_empty());
}

#[test]
fn categories_are_multilabel_and_do_not_depend_on_arabic_years_or_quarters() {
    for title in [
        "二〇二六年第一季度报告",
        "2026年第1季度报告",
        "二零二五年度报告",
        "二〇二六年中期报告",
        "半年度报告",
    ] {
        assert!(classify(title).contains(&"periodic_report".into()));
    }
    let categories = classify("召开基金份额持有人大会暨计票日停牌及复牌公告");
    for kind in ["holders_meeting", "suspension", "resumption"] {
        assert!(categories.contains(&kind.into()));
    }
    for (title, kind) in [
        ("收益分配公告", "dividend"),
        ("基金份额折算公告", "conversion"),
        ("基金合同终止及剩余财产分配公告", "liquidation"),
        ("上市交易公告书", "listing_notice"),
        ("标的指数变更和更名公告", "underlying_index"),
    ] {
        assert!(classify(title).contains(&kind.into()));
    }
    assert!(scoped_code("158001"));
    assert!(scoped_code("159001"));
    assert!(!scoped_code("160001"));
}

struct Text(&'static str);
impl PdfText for Text {
    fn text(&mut self, _: &[u8]) -> Result<String> {
        Ok(self.0.into())
    }
}
struct BadPdf;
impl PdfText for BadPdf {
    fn text(&mut self, _: &[u8]) -> Result<String> {
        Err("cannot decode synthetic PDF".into())
    }
}
const SYNTHETIC_PDF: &[u8] = b"%PDF-1.4\n% synthetic test converter boundary only\n%%EOF";

#[test]
fn pdf_fields_link_to_original_pdf_and_never_read_record_date_or_cash_dividend_amount() {
    let fields = extract_fields(
        "收益分配公告",
        SYNTHETIC_PDF,
        &mut Text("现金红利发放日：2026年1月9日\n权益登记日：2026年1月7日 每份分红0.123元"),
    );
    assert_eq!(fields["fields"]["payDate"], "2026-01-09");
    assert_eq!(fields["fieldSources"]["payDate"], fields["pdfRawSha256"]);
    assert!(fields["corporateActions"][0]["recordDate"].is_null());
    assert!(fields["corporateActions"][0]["cashPerShare"].is_null());
    let liquidation = extract_fields(
        "剩余财产分配公告",
        SYNTHETIC_PDF,
        &mut Text("剩余财产分配日：2026-01-12 每份基金份额分配：1.234567元"),
    );
    assert_eq!(liquidation["fields"]["liquidationCashPerShare"], "1.234567");
    assert_eq!(liquidation["fields"]["payDate"], "2026-01-12");
    let halt = extract_fields(
        "计票日停牌及复牌公告",
        SYNTHETIC_PDF,
        &mut Text("停牌起始日：2026年1月8日 复牌日期：2026年1月9日"),
    );
    assert_eq!(halt["fields"]["suspensionStart"], "2026-01-08");
    assert_eq!(halt["fields"]["resumptionDate"], "2026-01-09");
    let conflicting = extract_fields(
        "收益分配公告",
        SYNTHETIC_PDF,
        &mut Text("现金红利发放日：2026年1月9日 红利发放日：2026年1月10日"),
    );
    assert!(conflicting["fields"]["payDate"].is_null());
    let invalid = extract_fields(
        "收益分配公告",
        SYNTHETIC_PDF,
        &mut Text("现金红利发放日：2026年2月30日"),
    );
    assert!(invalid["fields"]["payDate"].is_null());
    let broken = extract_fields("收益分配公告", SYNTHETIC_PDF, &mut BadPdf);
    assert_eq!(broken["unknown"], json!(["payDate"]));
}

#[test]
fn full_names_are_a_publication_sequence_not_abbreviations_or_retroactive_attributes() {
    let a = announcement("a", "158001", json!("manager"), &title());
    let mut b = announcement(
        "b",
        "158001",
        json!("manager"),
        "关于基金名称更名为“合成乙宽基交易型开放式指数证券投资基金”的公告",
    );
    b["announcementTime"] = json!(1767657600000_i64);
    let mut pages = PageSet::default();
    pages.push(1, &page(2, vec![b, a])).unwrap();
    let fund = FundIndex::new("158001".into(), pages.finish().unwrap().0);
    assert_eq!(fund.full_names.len(), 2);
    assert_eq!(fund.full_names[0].name, name());
    assert!(fund.full_names[1].name.starts_with("合成乙"));
    assert!(fund.full_names[0].announced_ms < fund.full_names[1].announced_ms);
    let fund = FundIndex::new("159099".into(), vec![]);
    assert!(fund.require_usable().is_err());
    assert!(fund.unknown.contains(&"orgId_unknown".into()));
    assert!(fund.unknown.contains(&"legal_name_unknown".into()));
}

struct Mock {
    replies: VecDeque<HttpReply>,
    requests: Arc<Mutex<Vec<(Request, Instant)>>>,
}
impl Transport for Mock {
    fn send(&mut self, request: &Request) -> Result<HttpReply> {
        self.requests
            .lock()
            .unwrap()
            .push((request.clone(), Instant::now()));
        self.replies.pop_front().ok_or("unexpected request".into())
    }
}
fn reply(bytes: Vec<u8>) -> HttpReply {
    HttpReply {
        status: 200,
        content_type: "application/json".into(),
        bytes,
    }
}

#[test]
fn fund_lookup_uses_manager_id_retains_missing_candidates_and_freezes_request_evidence() {
    let row = announcement("a", "158001", json!("fund-manager"), &title());
    let bytes = page(1, vec![row]);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let mock = Mock {
        replies: vec![reply(bytes.clone()), reply(bytes)].into(),
        requests: requests.clone(),
    };
    let dir = tempfile::tempdir().unwrap();
    let store = RawStore::open(dir.path()).unwrap();
    let mut collector = Collector::new(mock, store);
    let dataset = collector.collect(dataset(), &["158001".into(), "159099".into()], &mut BadPdf);
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].0.form["column"], "fund");
    assert_eq!(requests[1].0.form["stock"], "158001,fund-manager");
    assert!(requests[1].1.duration_since(requests[0].1).as_millis() >= 1000);
    assert!(dataset.funds[0].require_usable().is_ok());
    assert!(dataset.funds[1].require_usable().is_err());
    let store = RawStore::open(dir.path()).unwrap();
    for hash in &dataset.source_raw_sha256 {
        let sources = store.sources(hash).unwrap();
        assert!(
            sources
                .iter()
                .all(|s| s.request["network_path"] == "direct_no_proxy")
        );
        assert!(sources.iter().all(|s| s.request["http_status"] == 200));
    }
    assert!(requests.iter().all(|(r, _)| !r.url.contains("topSearch")));
}

#[test]
fn access_denial_stops_source_and_keeps_failure_raw_local() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let mock = Mock {
        replies: vec![HttpReply {
            status: 403,
            content_type: "text/html".into(),
            bytes: b"synthetic denial".to_vec(),
        }]
        .into(),
        requests: requests.clone(),
    };
    let dir = tempfile::tempdir().unwrap();
    let store = RawStore::open(dir.path()).unwrap();
    let mut collector = Collector::new(mock, store);
    let dataset = collector.collect(dataset(), &["158001".into()], &mut BadPdf);
    assert!(!dataset.unknown.is_empty());
    assert_eq!(requests.lock().unwrap().len(), 1);
    assert_eq!(dataset.source_raw_sha256.len(), 1);
    let store = RawStore::open(dir.path()).unwrap();
    let hash = dataset.source_raw_sha256.iter().next().unwrap();
    assert_eq!(store.get(hash).unwrap(), b"synthetic denial");
    assert_eq!(store.sources(hash).unwrap()[0].request["http_status"], 403);
}

#[test]
fn rules_extractor_and_unknown_fields_enter_standard_dataset_identity() {
    let mut dataset = dataset();
    let (manifest, _) = dataset.manifest(provenance()).unwrap();
    manifest.validate().unwrap();
    dataset.rules_version.push_str("-changed");
    let (rules, _) = dataset.manifest(provenance()).unwrap();
    assert_ne!(manifest.dsv, rules.dsv);
    dataset.extractor_version.push_str("-changed");
    let (extractor, _) = dataset.manifest(provenance()).unwrap();
    assert_ne!(rules.dsv, extractor.dsv);
    dataset.unknown.push("missing_pdf".into());
    assert_ne!(extractor.dsv, dataset.manifest(provenance()).unwrap().0.dsv);
    assert_ne!(
        rules.core.normalizer.config_sha256,
        extractor.core.normalizer.config_sha256
    );
}

#[test]
fn collection_connects_pdf_evidence_to_events_and_manifest_inputs() {
    let row = announcement(
        "dividend-a",
        "158001",
        json!("manager"),
        &format!("{}收益分配公告", name()),
    );
    let bytes = page(1, vec![row]);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let mock = Mock {
        replies: vec![
            reply(bytes.clone()),
            reply(bytes),
            reply(SYNTHETIC_PDF.to_vec()),
        ]
        .into(),
        requests: requests.clone(),
    };
    let dir = tempfile::tempdir().unwrap();
    let mut collector = Collector::new(mock, RawStore::open(dir.path()).unwrap());
    let dataset = collector.collect(
        dataset(),
        &["158001".into()],
        &mut Text("现金红利发放日：2026年1月9日"),
    );
    let event = &dataset.funds[0].events["dividend-a"];
    assert_eq!(event["fields"]["payDate"], "2026-01-09");
    let pdf_hash = event["fieldSources"]["payDate"].as_str().unwrap();
    let (manifest, _) = dataset.manifest(provenance()).unwrap();
    assert!(
        manifest
            .core
            .inputs
            .iter()
            .any(|i| i.raw_sha256 == pdf_hash)
    );
    assert_eq!(
        requests.lock().unwrap()[2].0.url,
        "https://static.cninfo.com.cn/finalpage/2026-01-05/synthetic.PDF"
    );
    assert!(
        dataset.funds[0].announcements[0]
            .categories
            .contains(&"dividend".into())
    );
    assert!(dataset.funds[0].full_names[0].available_at.is_none());
}
