use prajna_data::sina_adjust_factor::{
    AdjustmentEvent, Collector, HttpReply, LifecycleRange, Request, Transport,
    compare_adjustment_series, parse_adjustment_series, parse_adjustment_series_for_lifecycles,
};
use prajna_data::{RawInput, RawStore};
use serde_json::json;
use std::{
    collections::VecDeque,
    error::Error,
    sync::{Arc, Mutex},
    time::Instant,
};

struct QueueTransport {
    replies: VecDeque<HttpReply>,
    requests: Arc<Mutex<Vec<(Request, Instant)>>>,
}

impl Transport for QueueTransport {
    fn send(&mut self, request: &Request) -> Result<HttpReply, Box<dyn Error + Send + Sync>> {
        self.requests
            .lock()
            .unwrap()
            .push((request.clone(), Instant::now()));
        self.replies
            .pop_front()
            .ok_or_else(|| "unexpected Sina request".into())
    }
}

#[test]
fn adjustment_events_preserve_source_precision_and_cross_validate_both_directions() {
    let qfq = br#"var qfq = {"data":[
        {"d":"2026-01-05","u":"0","s":"1"},
        {"d":"2026-01-06","u":"0.072812345678901234","s":"1.5"}
    ]};"#;
    let hfq = br#"var hfq = {"data":[
        {"d":"2026-01-05","u":"0","s":"1"},
        {"d":"2026-01-06","u":"0.072812345678901234","s":"1.5"}
    ]};"#;
    let events = parse_adjustment_series(qfq).unwrap();
    assert_eq!(
        events,
        vec![AdjustmentEvent {
            effective_date: "2026-01-06".parse().unwrap(),
            cash_per_share: Some("0.072812345678901234".into()),
            share_ratio: Some("1.5".into()),
        }]
    );
    assert_eq!(compare_adjustment_series(qfq, hfq).unwrap(), events);
    let numeric = br#"var qfq = {"data":[
        {"d":"2026-01-05","u":0,"s":1},
        {"d":"2026-01-06","u":0.050000000000000001,"s":1.5}
    ]};"#;
    assert_eq!(
        parse_adjustment_series(numeric).unwrap()[0]
            .cash_per_share
            .as_deref(),
        Some("0.050000000000000001")
    );

    let mismatched = br#"var hfq = {"data":[
        {"d":"2026-01-05","u":"0","s":"1"},
        {"d":"2026-01-06","u":"0.0728","s":"1.5"}
    ]};"#;
    assert!(compare_adjustment_series(qfq, mismatched).is_err());
}

#[test]
fn lifecycle_scoping_does_not_derive_events_across_reused_codes() {
    let series = br#"var qfq = {"data":[
        {"d":"2026-01-05","u":"0","s":"1"},
        {"d":"2026-01-06","u":"0.05","s":"1"},
        {"d":"2026-01-07","u":"0.10","s":"1"}
    ]};"#;
    let events = parse_adjustment_series_for_lifecycles(
        series,
        &[
            LifecycleRange {
                native_symbol: "159901".into(),
                start: "2026-01-05".parse().unwrap(),
                end_exclusive: Some("2026-01-06".parse().unwrap()),
            },
            LifecycleRange {
                native_symbol: "159901".into(),
                start: "2026-01-06".parse().unwrap(),
                end_exclusive: None,
            },
        ],
    )
    .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].effective_date, "2026-01-07".parse().unwrap());
    assert_eq!(events[0].cash_per_share.as_deref(), Some("0.05"));
}

#[test]
fn collector_stores_paired_originals_and_paces_requests_to_sina() {
    let qfq = br#"var qfq = {"data":[{"d":"2026-01-05","u":"0","s":"1"}]};"#;
    let hfq = br#"var hfq = {"data":[{"d":"2026-01-05","u":"0","s":"1"}]};"#;
    let transport = QueueTransport {
        replies: VecDeque::from([HttpReply::ok(qfq.to_vec()), HttpReply::ok(hfq.to_vec())]),
        requests: Arc::default(),
    };
    let requests = Arc::clone(&transport.requests);
    let dir = tempfile::tempdir().unwrap();
    let store = RawStore::open(dir.path()).unwrap();
    let mut collector = Collector::new(transport, store.clone());
    let pairs = collector
        .collect(
            &["sz159901".into()],
            "2026-01-01".parse().unwrap(),
            "2026-01-31".parse().unwrap(),
        )
        .unwrap();
    assert_eq!(pairs.len(), 1);
    assert_ne!(pairs[0].qfq_raw_sha256, pairs[0].hfq_raw_sha256);

    assert_eq!(requests.lock().unwrap().len(), 2);
    let requests = requests.lock().unwrap();
    let [qfq_request, hfq_request] = requests.as_slice() else {
        panic!("collector should request exactly qfq and hfq");
    };
    assert_eq!(
        qfq_request.0.url,
        "https://finance.sina.com.cn/realstock/company/sz159901/qfq.js"
    );
    assert_eq!(
        hfq_request.0.url,
        "https://finance.sina.com.cn/realstock/company/sz159901/hfq.js"
    );
    assert!(hfq_request.1.duration_since(qfq_request.1).as_secs() >= 1);
    for hash in [&pairs[0].qfq_raw_sha256, &pairs[0].hfq_raw_sha256] {
        let sources = store.sources(hash).unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].source_id, "sina_adjust_factor");
        assert_eq!(sources[0].request["http_status"], json!(200));
    }
}

#[test]
fn collector_stops_on_access_control_and_records_the_failed_response() {
    let transport = QueueTransport {
        replies: VecDeque::from([
            HttpReply {
                status: 403,
                content_type: "text/html".into(),
                bytes: b"access denied".to_vec(),
            },
            HttpReply::ok(br#"var hfq = {"data":[{"d":"2026-01-05","u":"0","s":"1"}]};"#.to_vec()),
        ]),
        requests: Arc::default(),
    };
    let requests = Arc::clone(&transport.requests);
    let dir = tempfile::tempdir().unwrap();
    let store = RawStore::open(dir.path()).unwrap();
    let mut collector = Collector::new(transport, store.clone());
    assert!(
        collector
            .collect(
                &["sz159901".into()],
                "2026-01-01".parse().unwrap(),
                "2026-01-31".parse().unwrap(),
            )
            .is_err()
    );
    assert_eq!(requests.lock().unwrap().len(), 1);
    let hash = RawInput::from_bytes(b"access denied".to_vec()).raw_sha256;
    let sources = store.sources(&hash).unwrap();
    assert_eq!(sources[0].request["http_status"], json!(403));
}

#[test]
fn collector_rejects_empty_symbol_scope_without_sending_requests() {
    let transport = QueueTransport {
        replies: VecDeque::new(),
        requests: Arc::default(),
    };
    let requests = Arc::clone(&transport.requests);
    let dir = tempfile::tempdir().unwrap();
    let mut collector = Collector::new(transport, RawStore::open(dir.path()).unwrap());
    assert!(
        collector
            .collect(
                &[],
                "2026-01-01".parse().unwrap(),
                "2026-01-31".parse().unwrap(),
            )
            .unwrap_err()
            .to_string()
            .contains("at least one symbol")
    );
    assert!(requests.lock().unwrap().is_empty());
}
