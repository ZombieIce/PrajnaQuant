use std::{
    cell::RefCell,
    collections::VecDeque,
    fs,
    process::{Command, Stdio},
    rc::Rc,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use chrono::NaiveDate;
use prajna_data::RawStore;
use prajna_data::daily_raw::{
    ChunkStatus, CollectionStatus, DailyRawCollector, DailyRawRequest, DailyRawTiming,
    RawResponseOutcome, SecurityKind, Venue,
};
use tencent_transport::{HttpReply, HttpRequest, HttpTransport, TransportFailure};

#[derive(Clone)]
struct FixtureTransport {
    replies: VecDeque<Result<HttpReply, TransportFailure>>,
    requests: Rc<RefCell<Vec<HttpRequest>>>,
}

impl HttpTransport for FixtureTransport {
    fn get(&mut self, request: &HttpRequest) -> Result<HttpReply, TransportFailure> {
        self.requests.borrow_mut().push(request.clone());
        self.replies
            .pop_front()
            .expect("one configured reply for each requested candidate")
    }
}

struct TimedFixtureTransport {
    replies: VecDeque<Result<HttpReply, TransportFailure>>,
    timing: FakeTiming,
    request_starts: Arc<Mutex<Vec<Duration>>>,
}

struct ProcessFixtureTransport {
    reply: Option<HttpReply>,
    request_times_root: std::path::PathBuf,
}

impl HttpTransport for ProcessFixtureTransport {
    fn get(&mut self, request: &HttpRequest) -> Result<HttpReply, TransportFailure> {
        assert_eq!(request.host, "https://web.ifzq.gtimg.cn");
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis()
            .to_string();
        let path = self
            .request_times_root
            .join(format!("start-{}.txt", std::process::id()));
        fs::write(path, timestamp).unwrap();
        Ok(self.reply.take().expect("one synthetic reply"))
    }
}

impl HttpTransport for TimedFixtureTransport {
    fn get(&mut self, request: &HttpRequest) -> Result<HttpReply, TransportFailure> {
        assert_eq!(request.host, "https://web.ifzq.gtimg.cn");
        self.request_starts.lock().unwrap().push(self.timing.now());
        self.replies
            .pop_front()
            .expect("one configured reply for each requested candidate")
    }
}

#[derive(Clone, Default)]
struct FakeTiming {
    now: Arc<Mutex<Duration>>,
    sleeps: Arc<Mutex<Vec<Duration>>>,
}

impl DailyRawTiming for FakeTiming {
    fn now(&self) -> Duration {
        *self.now.lock().unwrap()
    }

    fn sleep(&self, duration: Duration) {
        self.sleeps.lock().unwrap().push(duration);
        *self.now.lock().unwrap() += duration;
    }
}

fn collector(
    transport: FixtureTransport,
    raw_root: impl AsRef<std::path::Path>,
) -> DailyRawCollector<FixtureTransport> {
    DailyRawCollector::new_with_timing(transport, raw_root, Arc::new(FakeTiming::default()))
        .unwrap()
}

fn date(value: &str) -> NaiveDate {
    NaiveDate::parse_from_str(value, "%Y-%m-%d").unwrap()
}

fn request(symbol: &str, venue: Venue, kind: SecurityKind) -> DailyRawRequest {
    DailyRawRequest {
        symbol: symbol.into(),
        venue,
        kind,
        start: date("2026-01-05"),
        end: date("2026-01-09"),
    }
}

fn valid_reply(native_symbol: &str, source: &str, dates: &[&str]) -> (HttpReply, Vec<u8>) {
    let rows = dates
        .iter()
        .map(|date| serde_json::json!([date, "10.00", "11.00", "12.00", "9.00", "100"]))
        .collect::<Vec<_>>();
    let bytes = serde_json::to_vec(&serde_json::json!({
        "code": 0,
        "data": { native_symbol: { "day": rows } }
    }))
    .unwrap();
    (
        HttpReply {
            source: source.into(),
            bytes: bytes.clone(),
        },
        bytes,
    )
}

#[test]
fn stock_collection_preserves_raw_bytes_and_persists_a_readable_coverage_report() {
    let root = tempfile::tempdir().unwrap();
    let (reply, original_bytes) = valid_reply(
        "sh600519",
        "https://synthetic.invalid/daily",
        &["2026-01-05", "2026-01-06", "2026-01-08"],
    );
    let requests = Rc::new(RefCell::new(Vec::new()));
    let transport = FixtureTransport {
        replies: VecDeque::from([Ok(reply)]),
        requests: Rc::clone(&requests),
    };
    let mut collector = collector(transport, root.path());

    let run = collector
        .collect(request("600519", Venue::Xshg, SecurityKind::Stock))
        .unwrap();

    assert_eq!(run.report.status, CollectionStatus::Complete);
    assert_eq!(run.report.instrument_id, "600519.XSHG");
    assert_eq!(run.report.native_symbol, "sh600519");
    assert_eq!(run.report.requested_range.start, date("2026-01-05"));
    assert_eq!(run.report.requested_range.end, date("2026-01-09"));
    assert_eq!(run.report.method_version, "tencent-fqkline-day/1");
    assert_eq!(run.report.availability, "unknown");
    assert!(
        run.report
            .observed_at_semantics
            .contains("not vendor publication time")
    );
    assert_eq!(run.report.field_units["open"], "unknown");
    assert_eq!(run.report.field_units["volume"], "unknown");
    assert!(run.report.instrument_metadata["lot_size"].contains("T02-meta"));
    assert!(run.report.instrument_metadata["established_date"].contains("T02-meta"));
    assert_eq!(run.report.summary.accepted_response_chunks, 1);
    assert_eq!(run.report.summary.candidate_http_attempts, 1);
    assert_eq!(run.report.chunks.len(), 1);
    let chunk = &run.report.chunks[0];
    assert_eq!(
        chunk.actual_response_range,
        Some(prajna_data::daily_raw::DateRange {
            start: date("2026-01-05"),
            end: date("2026-01-08")
        })
    );
    assert_eq!(
        chunk.actual_response_dates,
        vec![date("2026-01-05"), date("2026-01-06"), date("2026-01-08"),]
    );
    assert_eq!(
        chunk.not_returned_calendar_dates,
        vec![date("2026-01-07"), date("2026-01-09")]
    );
    assert!(
        run.report
            .scope_statement
            .contains("not a claim of full history")
    );

    let raw_reference = chunk
        .raw_objects
        .iter()
        .find(|reference| reference.outcome == RawResponseOutcome::AcceptedResponse)
        .unwrap();
    let raw = RawStore::open(root.path()).unwrap();
    assert_eq!(raw.get(&raw_reference.raw_sha256).unwrap(), original_bytes);
    let source_records = raw.sources(&raw_reference.raw_sha256).unwrap();
    assert_eq!(source_records.len(), 1);
    assert_eq!(source_records[0].source_id, "tencent_fqkline_daily");
    assert!(source_records[0].request["method_version"].is_string());
    assert!(!source_records[0].observed_at.is_empty());

    let saved_report = fs::read(&run.report_path).unwrap();
    let restored =
        serde_json::from_slice::<prajna_data::daily_raw::DailyRawReport>(&saved_report).unwrap();
    assert_eq!(restored.report_id, run.report.report_id);
    assert_eq!(
        restored.chunks[0].raw_objects[0].raw_sha256,
        raw_reference.raw_sha256
    );

    let sent = requests.borrow();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].parameter, "sh600519,day,2026-01-05,2026-01-09,640,");
}

#[test]
fn etf_repeats_reuse_identical_raw_and_revisions_preserve_prior_bytes() {
    let root = tempfile::tempdir().unwrap();
    let (first_reply, first_bytes) = valid_reply(
        "sz159952",
        "https://synthetic.invalid/first",
        &["2026-02-02"],
    );
    let (same_reply, _) = valid_reply(
        "sz159952",
        "https://synthetic.invalid/repeat",
        &["2026-02-02"],
    );
    let (_, mut revised_bytes) = valid_reply(
        "sz159952",
        "https://synthetic.invalid/revision",
        &["2026-02-02"],
    );
    let mut revised: serde_json::Value = serde_json::from_slice(&revised_bytes).unwrap();
    revised["data"]["sz159952"]["day"][0][1] = serde_json::json!("10.01");
    revised_bytes = serde_json::to_vec(&revised).unwrap();
    let revised_reply = HttpReply {
        source: "https://synthetic.invalid/revision".into(),
        bytes: revised_bytes.clone(),
    };
    let requests = Rc::new(RefCell::new(Vec::new()));
    let transport = FixtureTransport {
        replies: VecDeque::from([Ok(first_reply), Ok(same_reply), Ok(revised_reply)]),
        requests,
    };
    let mut collector = collector(transport, root.path());
    let mut request = request("159952", Venue::Xshe, SecurityKind::Etf);
    request.start = date("2026-02-02");
    request.end = request.start;

    let first = collector.collect(request.clone()).unwrap();
    let repeated = collector.collect(request.clone()).unwrap();
    let revision = collector.collect(request).unwrap();

    assert_eq!(first.report.status, CollectionStatus::Complete);
    assert_eq!(first.report.instrument_id, "159952.XSHE");
    let raw_at = |run: &prajna_data::daily_raw::CollectionRun| {
        run.report.chunks[0].raw_objects[0].raw_sha256.clone()
    };
    assert_eq!(raw_at(&first), raw_at(&repeated));
    assert_ne!(raw_at(&first), raw_at(&revision));

    let raw = RawStore::open(root.path()).unwrap();
    assert_eq!(raw.get(&raw_at(&first)).unwrap(), first_bytes);
    assert_eq!(raw.get(&raw_at(&revision)).unwrap(), revised_bytes);
    assert_eq!(raw.sources(&raw_at(&first)).unwrap().len(), 2);
    assert!(first.report_path.exists());
    assert!(repeated.report_path.exists());
    assert!(revision.report_path.exists());
}

#[test]
fn empty_and_failed_attempts_keep_prior_raw_and_can_be_retried() {
    let root = tempfile::tempdir().unwrap();
    let (success, successful_bytes) = valid_reply(
        "sh600519",
        "https://synthetic.invalid/success",
        &["2026-01-05"],
    );
    let (empty, empty_bytes) = valid_reply("sh600519", "https://synthetic.invalid/empty", &[]);
    let (retry, _) = valid_reply(
        "sh600519",
        "https://synthetic.invalid/retry",
        &["2026-01-05"],
    );
    let requests = Rc::new(RefCell::new(Vec::new()));
    let mut replies = VecDeque::from([Ok(success), Ok(empty)]);
    replies.extend((0..12).map(|_| {
        Err(TransportFailure::new(
            tencent_transport::TransportFailureKind::Request,
            "synthetic network failure",
        ))
    }));
    replies.push_back(Ok(retry));
    let transport = FixtureTransport {
        replies,
        requests: Rc::clone(&requests),
    };
    let mut collector = collector(transport, root.path());
    let request = request("600519", Venue::Xshg, SecurityKind::Stock);

    let successful_run = collector.collect(request.clone()).unwrap();
    let empty_run = collector.collect(request.clone()).unwrap();
    let error_run = collector.collect(request.clone()).unwrap();
    let retry_run = collector.collect(request).unwrap();

    assert_eq!(successful_run.report.status, CollectionStatus::Complete);
    assert_eq!(empty_run.report.status, CollectionStatus::Failed);
    assert_eq!(empty_run.report.chunks[0].status, ChunkStatus::Empty);
    assert_eq!(empty_run.report.summary.data_chunks, 0);
    assert_eq!(empty_run.report.summary.empty_chunks, 1);
    assert_eq!(empty_run.report.summary.accepted_response_chunks, 1);
    assert_eq!(error_run.report.status, CollectionStatus::Failed);
    assert_eq!(error_run.report.chunks[0].status, ChunkStatus::Failed);
    assert_eq!(error_run.report.chunks[0].candidate_attempts, 12);
    assert_eq!(retry_run.report.status, CollectionStatus::Complete);
    assert!(successful_run.report_path.exists());
    assert!(empty_run.report_path.exists());
    assert!(error_run.report_path.exists());
    assert!(retry_run.report_path.exists());

    let raw = RawStore::open(root.path()).unwrap();
    let original_hash = successful_run.report.chunks[0].raw_objects[0]
        .raw_sha256
        .clone();
    let empty_hash = empty_run.report.chunks[0].raw_objects[0].raw_sha256.clone();
    assert_eq!(raw.get(&original_hash).unwrap(), successful_bytes);
    assert_eq!(raw.get(&empty_hash).unwrap(), empty_bytes);
    assert_eq!(
        retry_run.report.chunks[0].raw_objects[0].raw_sha256,
        original_hash
    );
    assert_eq!(requests.borrow().len(), 15);
}

#[test]
fn terminal_body_read_failure_counts_every_candidate_and_retains_diagnostics() {
    let root = tempfile::tempdir().unwrap();
    let requests = Rc::new(RefCell::new(Vec::new()));
    let transport = FixtureTransport {
        replies: VecDeque::from([
            Err(TransportFailure::new(
                tencent_transport::TransportFailureKind::Request,
                "synthetic connection failure",
            )),
            Err(TransportFailure::new(
                tencent_transport::TransportFailureKind::BodyRead,
                "synthetic body read failure",
            )),
            Ok(valid_reply(
                "sh600519",
                "https://synthetic.invalid/retry-after-body-read",
                &["2026-01-05"],
            )
            .0),
        ]),
        requests: Rc::clone(&requests),
    };
    let mut collector = collector(transport, root.path());

    let run = collector
        .collect(request("600519", Venue::Xshg, SecurityKind::Stock))
        .unwrap();

    assert_eq!(run.report.status, CollectionStatus::Complete);
    assert_eq!(run.report.summary.candidate_http_attempts, 3);
    assert_eq!(run.report.summary.candidate_failures, 2);
    assert_eq!(run.report.chunks[0].candidate_attempts, 3);
    assert_eq!(run.report.chunks[0].candidate_failures.len(), 2);
    assert_eq!(run.report.chunks[0].candidate_failures[0].kind, "Request");
    assert_eq!(run.report.chunks[0].candidate_failures[1].kind, "BodyRead");
    assert_eq!(requests.borrow().len(), 3);
}

#[test]
fn retries_use_bounded_exponential_backoff_and_preserve_candidate_counts() {
    let root = tempfile::tempdir().unwrap();
    let timing = FakeTiming::default();
    let requests = Rc::new(RefCell::new(Vec::new()));
    let failures = (0..3).map(|_| {
        Err(TransportFailure::new(
            tencent_transport::TransportFailureKind::BodyRead,
            "synthetic body read failure",
        ))
    });
    let (success, _) = valid_reply(
        "sh600519",
        "https://synthetic.invalid/success-after-retries",
        &["2026-01-05"],
    );
    let transport = FixtureTransport {
        replies: failures.chain([Ok(success)]).collect(),
        requests: Rc::clone(&requests),
    };
    let mut collector =
        DailyRawCollector::new_with_timing(transport, root.path(), Arc::new(timing.clone()))
            .unwrap();

    let run = collector
        .collect(request("600519", Venue::Xshg, SecurityKind::Stock))
        .unwrap();

    assert_eq!(run.report.status, CollectionStatus::Complete);
    assert_eq!(run.report.summary.candidate_http_attempts, 4);
    assert_eq!(run.report.summary.candidate_failures, 3);
    assert_eq!(run.report.chunks[0].candidate_attempts, 4);
    assert_eq!(requests.borrow().len(), 4);
    assert_eq!(
        *timing.sleeps.lock().unwrap(),
        vec![
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(4)
        ]
    );
}

#[test]
fn consecutive_same_domain_requests_are_spaced_by_at_least_one_second() {
    let root = tempfile::tempdir().unwrap();
    let timing = FakeTiming::default();
    let requests = Rc::new(RefCell::new(Vec::new()));
    let (first, _) = valid_reply(
        "sh600519",
        "https://synthetic.invalid/first",
        &["2026-01-05"],
    );
    let (second, _) = valid_reply(
        "sh600519",
        "https://synthetic.invalid/second",
        &["2026-01-05"],
    );
    let transport = FixtureTransport {
        replies: VecDeque::from([Ok(first), Ok(second)]),
        requests: Rc::clone(&requests),
    };
    let mut collector =
        DailyRawCollector::new_with_timing(transport, root.path(), Arc::new(timing.clone()))
            .unwrap();

    for _ in 0..2 {
        collector
            .collect(request("600519", Venue::Xshg, SecurityKind::Stock))
            .unwrap();
    }

    assert_eq!(requests.borrow().len(), 2);
    assert_eq!(*timing.sleeps.lock().unwrap(), vec![Duration::from_secs(1)]);
    assert_eq!(*timing.now.lock().unwrap(), Duration::from_secs(1));
}

#[test]
fn concurrent_collectors_serialize_same_domain_adapter_calls() {
    let root = tempfile::tempdir().unwrap();
    let timing = FakeTiming::default();
    let request_starts = Arc::new(Mutex::new(Vec::new()));
    let (first_reply, _) = valid_reply(
        "sh600519",
        "https://synthetic.invalid/first",
        &["2026-01-05"],
    );
    let (second_reply, _) = valid_reply(
        "sh600519",
        "https://synthetic.invalid/second",
        &["2026-01-05"],
    );
    let first_transport = TimedFixtureTransport {
        replies: VecDeque::from([Ok(first_reply)]),
        timing: timing.clone(),
        request_starts: Arc::clone(&request_starts),
    };
    let second_transport = TimedFixtureTransport {
        replies: VecDeque::from([Ok(second_reply)]),
        timing: timing.clone(),
        request_starts: Arc::clone(&request_starts),
    };
    let first_collector =
        DailyRawCollector::new_with_timing(first_transport, root.path(), Arc::new(timing.clone()))
            .unwrap();
    let second_collector =
        DailyRawCollector::new_with_timing(second_transport, root.path(), Arc::new(timing.clone()))
            .unwrap();

    let first = std::thread::spawn(move || {
        let mut collector = first_collector;
        collector.collect(request("600519", Venue::Xshg, SecurityKind::Stock))
    });
    let second = std::thread::spawn(move || {
        let mut collector = second_collector;
        collector.collect(request("600519", Venue::Xshg, SecurityKind::Stock))
    });

    assert_eq!(
        first.join().unwrap().unwrap().report.status,
        CollectionStatus::Complete
    );
    assert_eq!(
        second.join().unwrap().unwrap().report.status,
        CollectionStatus::Complete
    );
    let mut starts = request_starts.lock().unwrap().clone();
    starts.sort_unstable();
    assert_eq!(starts.len(), 2);
    assert!(starts[1].saturating_sub(starts[0]) >= Duration::from_secs(1));
}

#[test]
fn cross_process_same_domain_request_starts_are_spaced() {
    const CHILD_ROOT_ENV: &str = "PRAJNA_DAILY_RAW_PACING_TEST_ROOT";
    if let Some(root) = std::env::var_os(CHILD_ROOT_ENV) {
        let root = std::path::PathBuf::from(root);
        let pid = std::process::id();
        fs::write(root.join(format!("ready-{pid}")), b"ready").unwrap();
        let start_deadline = Instant::now() + Duration::from_secs(15);
        while !root.join("go").exists() && Instant::now() < start_deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            root.join("go").exists(),
            "parent must release the test barrier"
        );
        let bytes = serde_json::json!({
            "code": 0,
            "data": {"sh600519": {"day": [["2026-01-05", "10", "11", "12", "9", "100"]]}}
        });
        let collector = DailyRawCollector::new_with_timing(
            ProcessFixtureTransport {
                reply: Some(HttpReply {
                    source: "https://synthetic.invalid/process".into(),
                    bytes: serde_json::to_vec(&bytes).unwrap(),
                }),
                request_times_root: root.clone(),
            },
            &root,
            Arc::new(prajna_data::daily_raw::SystemDailyRawTiming),
        )
        .unwrap();
        let mut collector = collector;
        let run = collector
            .collect(request("600519", Venue::Xshg, SecurityKind::Stock))
            .unwrap();
        assert_eq!(run.report.status, CollectionStatus::Complete);
        return;
    }

    let root = tempfile::tempdir().unwrap();
    let executable = std::env::current_exe().unwrap();
    let mut children = (0..2)
        .map(|_| {
            Command::new(&executable)
                .arg("--exact")
                .arg("cross_process_same_domain_request_starts_are_spaced")
                .env(CHILD_ROOT_ENV, root.path())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect::<Vec<_>>();
    let ready_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let ready_count = fs::read_dir(root.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("ready-"))
            .count();
        if ready_count == 2 || Instant::now() >= ready_deadline {
            fs::write(root.path().join("go"), b"go").unwrap();
            assert_eq!(ready_count, 2, "both pacing test processes must start");
            break;
        }
        for child in &mut children {
            assert!(
                child.try_wait().unwrap().is_none(),
                "child process exited early"
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    for child in children {
        assert!(child.wait_with_output().unwrap().status.success());
    }

    let mut timestamps = fs::read_dir(root.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("start-"))
        .map(|entry| {
            fs::read_to_string(entry.path())
                .unwrap()
                .parse::<u128>()
                .unwrap()
        })
        .collect::<Vec<_>>();
    timestamps.sort_unstable();
    assert_eq!(timestamps.len(), 2);
    assert!(timestamps[1].saturating_sub(timestamps[0]) >= 1_000);
}

#[test]
fn requests_are_split_at_the_transport_limit_without_overlap() {
    let root = tempfile::tempdir().unwrap();
    let start = date("2024-01-01");
    let first_end = start + chrono::Duration::days(799);
    let second_start = first_end.succ_opt().unwrap();
    let end = second_start + chrono::Duration::days(799);
    let first_date = start.to_string();
    let second_date = second_start.to_string();
    let (first_reply, _) = valid_reply(
        "sh600519",
        "https://synthetic.invalid/first-window",
        &[first_date.as_str()],
    );
    let (second_reply, _) = valid_reply(
        "sh600519",
        "https://synthetic.invalid/second-window",
        &[second_date.as_str()],
    );
    let requests = Rc::new(RefCell::new(Vec::new()));
    let transport = FixtureTransport {
        replies: VecDeque::from([Ok(first_reply), Ok(second_reply)]),
        requests: Rc::clone(&requests),
    };
    let mut collector = collector(transport, root.path());
    let run = collector
        .collect(DailyRawRequest {
            symbol: "600519".into(),
            venue: Venue::Xshg,
            kind: SecurityKind::Stock,
            start,
            end,
        })
        .unwrap();

    assert_eq!(run.report.status, CollectionStatus::Complete);
    assert_eq!(run.report.chunks.len(), 2);
    assert_eq!(
        run.report.chunks[0].requested_range,
        prajna_data::daily_raw::DateRange {
            start,
            end: first_end,
        }
    );
    assert_eq!(
        run.report.chunks[1].requested_range,
        prajna_data::daily_raw::DateRange {
            start: second_start,
            end,
        }
    );
    let sent = requests.borrow();
    assert_eq!(sent.len(), 2);
    assert_eq!(
        sent[0].parameter,
        format!("sh600519,day,{start},{first_end},640,")
    );
    assert_eq!(
        sent[1].parameter,
        format!("sh600519,day,{second_start},{end},640,")
    );
}

#[test]
fn repeated_access_restrictions_stop_before_the_next_candidate_or_range() {
    let root = tempfile::tempdir().unwrap();
    let blocked_one = TransportFailure::new(
        tencent_transport::TransportFailureKind::HttpStatus,
        "HTTP 403",
    )
    .with_status_code(403)
    .with_response_body(b"synthetic access denial".to_vec());
    let blocked_two = TransportFailure::new(
        tencent_transport::TransportFailureKind::HttpStatus,
        "HTTP 501",
    )
    .with_status_code(501)
    .with_response_body(b"synthetic access denial".to_vec());
    let (never_requested, _) = valid_reply(
        "sh600519",
        "https://synthetic.invalid/unused",
        &["2026-01-05"],
    );
    let requests = Rc::new(RefCell::new(Vec::new()));
    let transport = FixtureTransport {
        replies: VecDeque::from([Err(blocked_one), Err(blocked_two), Ok(never_requested)]),
        requests: Rc::clone(&requests),
    };
    let mut collector = collector(transport, root.path());
    let start = date("2024-01-01");
    let end = start + chrono::Duration::days(800);

    let run = collector
        .collect(DailyRawRequest {
            symbol: "600519".into(),
            venue: Venue::Xshg,
            kind: SecurityKind::Stock,
            start,
            end,
        })
        .unwrap();

    assert_eq!(run.report.status, CollectionStatus::SourceStopped);
    assert_eq!(run.report.summary.chunks_requested, 2);
    assert_eq!(run.report.summary.chunks_attempted, 1);
    assert_eq!(run.report.summary.not_attempted_chunks, 1);
    assert_eq!(run.report.chunks[0].candidate_attempts, 2);
    assert_eq!(run.report.chunks[1].status, ChunkStatus::NotAttempted);
    assert_eq!(run.report.chunks[0].raw_objects.len(), 2);
    assert_eq!(requests.borrow().len(), 2);
}

#[test]
fn configured_root_cannot_target_the_mvp6_archive() {
    let root = prajna_data::default_mvp6_root();
    let transport = FixtureTransport {
        replies: VecDeque::new(),
        requests: Rc::new(RefCell::new(Vec::new())),
    };

    let error = match DailyRawCollector::new(transport.clone(), &root) {
        Err(error) => error,
        Ok(_) => panic!("MVP-6 root must be rejected"),
    };
    assert!(error.to_string().contains("independent of the fixed MVP-6"));
    let child_error = match DailyRawCollector::new(transport, root.join("daily-raw")) {
        Err(error) => error,
        Ok(_) => panic!("child of MVP-6 root must be rejected"),
    };
    assert!(
        child_error
            .to_string()
            .contains("independent of the fixed MVP-6")
    );
}
