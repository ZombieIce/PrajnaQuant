use std::collections::VecDeque;

use chrono::NaiveDate;

use crate::{
    CANDIDATE_HOSTS, CandidateAction, DEFAULT_HTTP_SETTINGS, DEFAULT_REFERER, DEFAULT_USER_AGENT,
    DailyRequest, FetchErrorKind, HttpReply, HttpRequest, HttpTransport, TransportFailure,
    TransportFailureKind, fetch_unadjusted_daily, fetch_unadjusted_daily_with_policy,
};

const VALID_BODY: &[u8] = br#"{"code":0,"data":{"sh600519":{"day":[]}}}"#;

struct QueueTransport {
    replies: VecDeque<Result<HttpReply, TransportFailure>>,
    requests: Vec<HttpRequest>,
}

impl QueueTransport {
    fn new(replies: impl IntoIterator<Item = Result<HttpReply, TransportFailure>>) -> Self {
        Self {
            replies: replies.into_iter().collect(),
            requests: Vec::new(),
        }
    }
}

impl HttpTransport for QueueTransport {
    fn get(&mut self, request: &HttpRequest) -> Result<HttpReply, TransportFailure> {
        self.requests.push(request.clone());
        self.replies
            .pop_front()
            .expect("one synthetic response per request")
    }
}

fn day_request(symbol: &str, start: &str, end: &str) -> DailyRequest {
    DailyRequest {
        symbol: symbol.into(),
        start: start.parse::<NaiveDate>().unwrap(),
        end: end.parse::<NaiveDate>().unwrap(),
    }
}

fn reply(source: &str, bytes: impl Into<Vec<u8>>) -> Result<HttpReply, TransportFailure> {
    Ok(HttpReply {
        source: source.into(),
        bytes: bytes.into(),
    })
}

#[test]
fn stock_and_etf_requests_keep_the_legacy_parameters_and_raw_response() {
    for symbol in ["sh600519", "sz159901"] {
        let source = format!("https://synthetic.invalid/appstock/app/fqkline/get?symbol={symbol}");
        let mut transport = QueueTransport::new([reply(&source, VALID_BODY.to_vec())]);

        let response = fetch_unadjusted_daily(
            &day_request(symbol, "2026-09-14", "2026-09-18"),
            &mut transport,
            |bytes| bytes == VALID_BODY,
        )
        .unwrap();

        assert_eq!(response.source, source);
        assert_eq!(response.bytes, VALID_BODY);
        assert_eq!(transport.requests.len(), 1);
        let request = &transport.requests[0];
        assert_eq!(request.host, CANDIDATE_HOSTS[0]);
        assert_eq!(
            request.url,
            format!("{}/appstock/app/fqkline/get", CANDIDATE_HOSTS[0])
        );
        assert_eq!(
            request.parameter,
            format!("{symbol},day,2026-09-14,2026-09-18,640,")
        );
        assert_eq!(request.referer, DEFAULT_REFERER);
        assert_eq!(request.user_agent, DEFAULT_USER_AGENT);
        assert_eq!(request.settings, DEFAULT_HTTP_SETTINGS);
    }
}

#[test]
fn candidate_fallback_keeps_the_existing_order_after_network_and_empty_responses() {
    let mut transport = QueueTransport::new([
        Err(TransportFailure::new(
            TransportFailureKind::Request,
            "connection refused",
        )),
        reply("https://synthetic.invalid/empty", Vec::<u8>::new()),
        reply("https://synthetic.invalid/accepted", VALID_BODY.to_vec()),
    ]);

    let response = fetch_unadjusted_daily(
        &day_request("sh600519", "2026-09-14", "2026-09-18"),
        &mut transport,
        |bytes| bytes == VALID_BODY,
    )
    .unwrap();

    assert_eq!(response.source, "https://synthetic.invalid/accepted");
    assert_eq!(response.bytes, VALID_BODY);
    assert_eq!(
        transport
            .requests
            .iter()
            .map(|request| request.host)
            .collect::<Vec<_>>(),
        CANDIDATE_HOSTS
    );
}

#[test]
fn nonempty_invalid_payload_uses_the_next_existing_candidate() {
    let mut transport = QueueTransport::new([
        reply(
            "https://synthetic.invalid/invalid",
            b"not the expected payload".to_vec(),
        ),
        reply("https://synthetic.invalid/accepted", VALID_BODY.to_vec()),
    ]);

    let response = fetch_unadjusted_daily(
        &day_request("sh600519", "2026-09-14", "2026-09-18"),
        &mut transport,
        |bytes| bytes == VALID_BODY,
    )
    .unwrap();

    assert_eq!(response.source, "https://synthetic.invalid/accepted");
    assert_eq!(transport.requests.len(), 2);
}

#[test]
fn exhausted_candidates_report_each_existing_candidate_error() {
    let mut transport = QueueTransport::new([
        Err(TransportFailure::new(
            TransportFailureKind::Request,
            "connect refused",
        )),
        Err(
            TransportFailure::new(TransportFailureKind::HttpStatus, "HTTP 501")
                .with_status_code(501),
        ),
        Err(
            TransportFailure::new(TransportFailureKind::HttpStatus, "HTTP 429")
                .with_status_code(429),
        ),
    ]);

    let error = fetch_unadjusted_daily(
        &day_request("sh600519", "2026-09-14", "2026-09-18"),
        &mut transport,
        |_| true,
    )
    .unwrap_err();

    assert_eq!(error.kind, FetchErrorKind::EndpointsUnavailable);
    assert_eq!(
        error.to_string(),
        format!(
            "Tencent K-line endpoints are unavailable: {}: connect refused; {}: HTTP 501; {}: HTTP 429",
            CANDIDATE_HOSTS[0], CANDIDATE_HOSTS[1], CANDIDATE_HOSTS[2]
        )
    );
    assert_eq!(
        error
            .candidate_failures
            .iter()
            .map(|failure| failure.status_code)
            .collect::<Vec<_>>(),
        [None, Some(501), Some(429)]
    );
}

#[test]
fn body_read_failure_stops_candidate_fallback() {
    let mut transport = QueueTransport::new([Err(TransportFailure::new(
        TransportFailureKind::BodyRead,
        "response body read failed",
    ))]);

    let error = fetch_unadjusted_daily(
        &day_request("sh600519", "2026-09-14", "2026-09-18"),
        &mut transport,
        |_| true,
    )
    .unwrap_err();

    assert_eq!(error.kind, FetchErrorKind::BodyRead);
    assert_eq!(error.to_string(), "response body read failed");
    assert_eq!(transport.requests.len(), 1);
}

fn access_restriction_policy(
    consecutive_restrictions: &mut usize,
) -> impl FnMut(&crate::CandidateFailure, Option<&[u8]>) -> CandidateAction + '_ {
    move |failure, response_body| {
        let restricted = matches!(failure.status_code, Some(403 | 501))
            || response_body.is_some_and(|body| {
                let text = String::from_utf8_lossy(body).to_lowercase();
                ["captcha", "验证码", "访问验证"]
                    .iter()
                    .any(|marker| text.contains(marker))
            });
        if restricted {
            *consecutive_restrictions += 1;
        } else {
            *consecutive_restrictions = 0;
        }
        if *consecutive_restrictions >= 2 {
            CandidateAction::Stop
        } else {
            CandidateAction::Continue
        }
    }
}

#[test]
fn candidate_policy_stops_after_two_http_or_captcha_restrictions_before_next_host() {
    let scenarios = vec![
        (
            vec![
                Err(
                    TransportFailure::new(TransportFailureKind::HttpStatus, "HTTP 403")
                        .with_status_code(403),
                ),
                Err(
                    TransportFailure::new(TransportFailureKind::HttpStatus, "HTTP 501")
                        .with_status_code(501),
                ),
                reply("https://synthetic.invalid/accepted", VALID_BODY.to_vec()),
            ],
            true,
            2,
            2,
        ),
        (
            vec![
                reply(
                    "https://synthetic.invalid/captcha-1",
                    b"CAPTCHA required".to_vec(),
                ),
                reply(
                    "https://synthetic.invalid/captcha-2",
                    "访问验证".as_bytes().to_vec(),
                ),
                reply("https://synthetic.invalid/accepted", VALID_BODY.to_vec()),
            ],
            true,
            2,
            2,
        ),
        (
            vec![
                Err(
                    TransportFailure::new(TransportFailureKind::HttpStatus, "HTTP 403")
                        .with_status_code(403),
                ),
                Err(TransportFailure::new(
                    TransportFailureKind::Request,
                    "synthetic network failure",
                )),
                reply("https://synthetic.invalid/accepted", VALID_BODY.to_vec()),
            ],
            false,
            3,
            0,
        ),
    ];

    for (replies, should_stop, expected_requests, expected_restrictions) in scenarios {
        let mut transport = QueueTransport::new(replies);
        let mut consecutive_restrictions = 0;
        let result = {
            let mut policy = access_restriction_policy(&mut consecutive_restrictions);
            fetch_unadjusted_daily_with_policy(
                &day_request("sh600519", "2026-09-14", "2026-09-18"),
                &mut transport,
                |bytes| bytes == VALID_BODY,
                &mut policy,
            )
        };

        assert_eq!(result.is_err(), should_stop);
        if let Err(error) = result {
            assert_eq!(error.kind, FetchErrorKind::PolicyStopped);
            assert_eq!(error.candidate_failures.len(), 2);
        }
        assert_eq!(transport.requests.len(), expected_requests);
        assert_eq!(consecutive_restrictions, expected_restrictions);
    }
}

#[test]
fn explicit_symbol_and_eight_hundred_day_boundaries_are_checked_before_transport() {
    for request in [
        day_request("600519", "2026-09-14", "2026-09-18"),
        day_request("bj920000", "2026-09-14", "2026-09-18"),
        day_request("sh600519", "2026-09-18", "2026-09-14"),
        day_request("sh600519", "2025-01-01", "2027-03-12"),
    ] {
        let mut transport = QueueTransport::new([]);
        assert!(fetch_unadjusted_daily(&request, &mut transport, |_| true).is_err());
        assert!(transport.requests.is_empty());
    }

    let mut transport = QueueTransport::new([reply("https://synthetic.invalid/ok", VALID_BODY)]);
    assert!(
        fetch_unadjusted_daily(
            &day_request("sh600519", "2025-01-01", "2027-03-11"),
            &mut transport,
            |bytes| bytes == VALID_BODY,
        )
        .is_ok()
    );
}
