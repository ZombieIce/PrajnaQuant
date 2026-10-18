use prajna_data::sina_adjust_factor::{
    AdjustmentEvent, CollectionStatus, Collector, HttpReply, LifecycleRange, Request,
    SymbolOutcome, Transport, compare_adjustment_series, parse_adjustment_series,
    parse_adjustment_series_for_lifecycles,
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

struct FailingTransport {
    requests: Arc<Mutex<Vec<(Request, Instant)>>>,
}

impl Transport for FailingTransport {
    fn send(&mut self, request: &Request) -> Result<HttpReply, Box<dyn Error + Send + Sync>> {
        self.requests
            .lock()
            .unwrap()
            .push((request.clone(), Instant::now()));
        Err("synthetic connection reset".into())
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
    let report = collector
        .collect(
            &["sz159901".into()],
            "2026-01-01".parse().unwrap(),
            "2026-01-31".parse().unwrap(),
        )
        .unwrap();
    assert_eq!(report.status, CollectionStatus::Complete);
    let [
        SymbolOutcome::Completed {
            symbol,
            qfq_raw_sha256,
            hfq_raw_sha256,
        },
    ] = report.outcomes.as_slice()
    else {
        panic!("collector should complete one qfq/hfq pair");
    };
    assert_eq!(symbol, "sz159901");
    assert_ne!(qfq_raw_sha256, hfq_raw_sha256);

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
    for hash in [qfq_raw_sha256, hfq_raw_sha256] {
        let sources = store.sources(hash).unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].source_id, "sina_adjust_factor");
        assert_eq!(sources[0].request["http_status"], json!(200));
        assert_eq!(sources[0].request["network_path"], "injected_transport");
        assert_eq!(
            sources[0].request["terms_conclusion"],
            "issue_172_owner_authorization_for_local_noncommercial_research"
        );
    }
}

#[test]
fn collector_accepts_cumulative_reset_from_reused_code() {
    let qfq = br#"var qfq = {"data":[
        {"d":"2026-01-05","u":"0.05","s":"1"},
        {"d":"2026-01-06","u":"0","s":"1"}
    ]};"#;
    let hfq = br#"var hfq = {"data":[
        {"d":"2026-01-05","u":"0.05","s":"1"},
        {"d":"2026-01-06","u":"0","s":"1"}
    ]};"#;
    let transport = QueueTransport {
        replies: VecDeque::from([HttpReply::ok(qfq.to_vec()), HttpReply::ok(hfq.to_vec())]),
        requests: Arc::default(),
    };
    let dir = tempfile::tempdir().unwrap();
    let mut collector = Collector::new(transport, RawStore::open(dir.path()).unwrap());
    let report = collector
        .collect(
            &["sz159901".into()],
            "2026-01-01".parse().unwrap(),
            "2026-01-31".parse().unwrap(),
        )
        .unwrap();
    assert_eq!(report.status, CollectionStatus::Complete);
    assert!(matches!(
        report.outcomes.as_slice(),
        [SymbolOutcome::Completed { symbol, .. }] if symbol == "sz159901"
    ));
}

#[test]
fn collector_keeps_symbol_gaps_and_continues_to_later_candidates() {
    let qfq = br#"var qfq = {"data":[{"d":"2026-01-05","u":"0","s":"1"}]};"#;
    let hfq = br#"var hfq = {"data":[{"d":"2026-01-05","u":"0","s":"1"}]};"#;
    let mismatch = br#"var hfq = {"data":[{"d":"2026-01-05","u":"0.01","s":"1"}]};"#;
    let responses = [
        (404, b"missing symbol".to_vec()),
        (200, b"not JavaScript".to_vec()),
        (200, br#"var qfq = {"data":[]};"#.to_vec()),
        (200, qfq.to_vec()),
        (200, mismatch.to_vec()),
        (200, qfq.to_vec()),
        (200, hfq.to_vec()),
    ];
    let transport = QueueTransport {
        replies: responses
            .iter()
            .map(|(status, bytes)| HttpReply {
                status: *status,
                content_type: "application/javascript".into(),
                bytes: bytes.clone(),
            })
            .collect(),
        requests: Arc::default(),
    };
    let requests = Arc::clone(&transport.requests);
    let dir = tempfile::tempdir().unwrap();
    let store = RawStore::open(dir.path()).unwrap();
    let mut collector = Collector::new(transport, store.clone());
    let report = collector
        .collect(
            &[
                "sz158901".into(),
                "sz158902".into(),
                "sz158903".into(),
                "sz158904".into(),
                "sz158905".into(),
            ],
            "2026-01-01".parse().unwrap(),
            "2026-01-31".parse().unwrap(),
        )
        .unwrap();

    assert_eq!(report.status, CollectionStatus::CompletedWithGaps);
    assert_eq!(report.source_stop_reason, None);
    assert_eq!(report.outcomes.len(), 5);
    assert!(matches!(
        &report.outcomes[0],
        SymbolOutcome::Gap { symbol, reason }
            if symbol == "sz158901" && reason.contains("HTTP 404")
    ));
    assert!(matches!(
        &report.outcomes[1],
        SymbolOutcome::Gap { symbol, reason }
            if symbol == "sz158902" && reason.contains("invalid HTTP 200")
    ));
    assert!(matches!(
        &report.outcomes[2],
        SymbolOutcome::Gap { symbol, reason }
            if symbol == "sz158903" && reason.contains("no factor points")
    ));
    assert!(matches!(
        &report.outcomes[3],
        SymbolOutcome::Gap { symbol, reason }
            if symbol == "sz158904" && reason.contains("qfq and hfq cumulative factors differ")
    ));
    assert!(matches!(
        &report.outcomes[4],
        SymbolOutcome::Completed { symbol, .. } if symbol == "sz158905"
    ));

    let requests = requests.lock().unwrap();
    let urls = requests
        .iter()
        .map(|(request, _)| request.url.as_str())
        .collect::<Vec<_>>();
    assert_eq!(urls.len(), responses.len());
    assert_eq!(
        urls,
        [
            "https://finance.sina.com.cn/realstock/company/sz158901/qfq.js",
            "https://finance.sina.com.cn/realstock/company/sz158902/qfq.js",
            "https://finance.sina.com.cn/realstock/company/sz158903/qfq.js",
            "https://finance.sina.com.cn/realstock/company/sz158904/qfq.js",
            "https://finance.sina.com.cn/realstock/company/sz158904/hfq.js",
            "https://finance.sina.com.cn/realstock/company/sz158905/qfq.js",
            "https://finance.sina.com.cn/realstock/company/sz158905/hfq.js",
        ]
    );
    for (status, bytes) in responses {
        let raw_sha256 = RawInput::from_bytes(bytes).raw_sha256;
        assert!(
            store
                .sources(&raw_sha256)
                .unwrap()
                .iter()
                .any(|source| source.request["http_status"] == json!(status))
        );
    }
}

#[test]
fn collector_stops_on_access_control_redirect_or_challenge_and_marks_remaining_symbols() {
    let responses = [
        HttpReply {
            status: 403,
            content_type: "text/html".into(),
            bytes: b"access denied".to_vec(),
        },
        HttpReply {
            status: 302,
            content_type: "text/html".into(),
            bytes: b"redirect".to_vec(),
        },
        HttpReply {
            status: 429,
            content_type: "text/plain".into(),
            bytes: b"rate limited".to_vec(),
        },
        HttpReply {
            status: 200,
            content_type: "text/html".into(),
            bytes: b"<html>captcha required</html>".to_vec(),
        },
        HttpReply {
            status: 200,
            content_type: "text/html".into(),
            bytes: b"<html>login required</html>".to_vec(),
        },
    ];
    for response in responses {
        let transport = QueueTransport {
            replies: VecDeque::from([
                response.clone(),
                HttpReply::ok(br#"var qfq = {"data":[{"d":"2026-01-05","u":"0","s":"1"}]};"#),
            ]),
            requests: Arc::default(),
        };
        let requests = Arc::clone(&transport.requests);
        let dir = tempfile::tempdir().unwrap();
        let store = RawStore::open(dir.path()).unwrap();
        let mut collector = Collector::new(transport, store.clone());
        let report = collector
            .collect(
                &["sz159901".into(), "sz159902".into()],
                "2026-01-01".parse().unwrap(),
                "2026-01-31".parse().unwrap(),
            )
            .unwrap();
        assert_eq!(report.status, CollectionStatus::SourceStopped);
        assert_eq!(report.outcomes.len(), 2);
        assert!(matches!(
            &report.outcomes[0],
            SymbolOutcome::Gap { symbol, reason }
                if symbol == "sz159901" && !reason.is_empty()
        ));
        assert!(matches!(
            &report.outcomes[1],
            SymbolOutcome::NotAttempted {
                symbol,
                source_stop_reason
            } if symbol == "sz159902" && Some(source_stop_reason) == report.source_stop_reason.as_ref()
        ));
        assert_eq!(requests.lock().unwrap().len(), 1);
        let raw_sha256 = RawInput::from_bytes(response.bytes).raw_sha256;
        assert_eq!(
            store.sources(&raw_sha256).unwrap()[0].request["http_status"],
            json!(response.status)
        );
    }
}

#[test]
fn collector_stops_after_transport_retries_are_exhausted_and_records_every_failure() {
    let transport = FailingTransport {
        requests: Arc::default(),
    };
    let requests = Arc::clone(&transport.requests);
    let dir = tempfile::tempdir().unwrap();
    let store = RawStore::open(dir.path()).unwrap();
    let mut collector = Collector::new(transport, store.clone());
    let report = collector
        .collect(
            &["sz159901".into(), "sz159902".into()],
            "2026-01-01".parse().unwrap(),
            "2026-01-31".parse().unwrap(),
        )
        .unwrap();
    assert_eq!(report.status, CollectionStatus::SourceStopped);
    assert!(
        report
            .source_stop_reason
            .as_deref()
            .unwrap()
            .contains("synthetic connection reset")
    );
    assert!(matches!(
        &report.outcomes[0],
        SymbolOutcome::Gap { symbol, reason }
            if symbol == "sz159901" && reason.contains("retries exhausted after 4 attempts")
    ));
    assert!(matches!(
        &report.outcomes[1],
        SymbolOutcome::NotAttempted { symbol, .. } if symbol == "sz159902"
    ));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests.iter().all(|(request, _)| {
        request.url == "https://finance.sina.com.cn/realstock/company/sz159901/qfq.js"
    }));
    let raw_sha256 = RawInput::from_bytes(Vec::new()).raw_sha256;
    let sources = store.sources(&raw_sha256).unwrap();
    assert_eq!(sources.len(), 4);
    for (attempt, source) in sources.iter().enumerate() {
        assert_eq!(source.request["http_status"], json!(0));
        assert_eq!(source.request["attempt"], json!(attempt));
        assert_eq!(
            source.request["transport_error"],
            json!("synthetic connection reset")
        );
    }
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
