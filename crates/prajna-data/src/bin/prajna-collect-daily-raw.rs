use std::{
    error::Error,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::atomic::{AtomicU64, Ordering},
};

use chrono::{NaiveDate, Utc};
use prajna_data::daily_raw::{
    CollectionStatus, DailyRawCollector, DailyRawRequest, SecurityKind, Venue,
};
use tencent_transport::{HttpTransport, ReqwestTransport};

static NEXT_DIAGNOSTIC_ID: AtomicU64 = AtomicU64::new(0);

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("daily Raw collection could not be prepared or persisted: {error}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode, Box<dyn Error>> {
    run_with_transport(std::env::args().skip(1), ReqwestTransport::new())
}

fn run_with_transport<T: HttpTransport>(
    arguments: impl Iterator<Item = String>,
    transport: T,
) -> Result<ExitCode, Box<dyn Error>> {
    let Some(args) = parse_args(arguments)? else {
        print_usage();
        return Ok(ExitCode::SUCCESS);
    };
    let request = DailyRawRequest {
        symbol: args.symbol,
        venue: args.venue,
        kind: args.kind,
        start: args.start,
        end: args.end,
    };
    let mut collector = DailyRawCollector::new(transport, &args.raw_root)?;
    let run = match collector.collect(request.clone()) {
        Ok(run) => run,
        Err(error) => {
            if write_local_failure_diagnostic(&args.raw_root, &request, &error).is_err() {
                return Err(CliCollectionFailure::DiagnosticUnavailable.into());
            }
            return Err(CliCollectionFailure::DiagnosticSaved.into());
        }
    };
    let summary = &run.report.summary;
    println!(
        "report={} status={:?} chunks={}/{} data={} accepted={} empty={} failed={} not_attempted={}",
        run.report_path.display(),
        run.report.status,
        summary.chunks_attempted,
        summary.chunks_requested,
        summary.data_chunks,
        summary.accepted_response_chunks,
        summary.empty_chunks,
        summary.failed_chunks,
        summary.not_attempted_chunks,
    );
    Ok(if run.report.status == CollectionStatus::Complete {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    })
}

#[derive(Debug)]
enum CliCollectionFailure {
    DiagnosticSaved,
    DiagnosticUnavailable,
}

impl std::fmt::Display for CliCollectionFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::DiagnosticSaved => {
                "daily Raw collection failed; detailed diagnostics were saved under the local raw root"
            }
            Self::DiagnosticUnavailable => {
                "daily Raw collection failed; local diagnostics could not be saved"
            }
        })
    }
}

impl Error for CliCollectionFailure {}

fn write_local_failure_diagnostic(
    raw_root: &Path,
    request: &DailyRawRequest,
    error: &dyn Error,
) -> Result<(), Box<dyn Error>> {
    let root = fs::canonicalize(raw_root)?;
    let diagnostic_dir = root.join("reports/daily-raw/diagnostics");
    fs::create_dir_all(&diagnostic_dir)?;
    let observed_at = Utc::now().to_rfc3339();
    let diagnostic = serde_json::json!({
        "schema_version": 1,
        "method_version": prajna_data::daily_raw::METHOD_VERSION,
        "observed_at": observed_at,
        "request": request,
        "error": error.to_string(),
    });
    let timestamp = Utc::now().timestamp_millis();
    let id = NEXT_DIAGNOSTIC_ID.fetch_add(1, Ordering::Relaxed);
    let target = diagnostic_dir.join(format!(
        "failure-{timestamp}-{}-{id}.json",
        std::process::id()
    ));
    let mut temporary = tempfile::NamedTempFile::new_in(&diagnostic_dir)?;
    serde_json::to_writer(&mut temporary, &diagnostic)?;
    temporary.as_file_mut().write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary.persist(target)?;
    Ok(())
}

#[derive(Debug)]
struct Args {
    raw_root: PathBuf,
    symbol: String,
    venue: Venue,
    kind: SecurityKind,
    start: NaiveDate,
    end: NaiveDate,
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Option<Args>, Box<dyn Error>> {
    let mut raw_root = None;
    let mut symbol = None;
    let mut venue = None;
    let mut kind = None;
    let mut start = None;
    let mut end = None;
    let mut args = args;

    while let Some(flag) = args.next() {
        if flag == "--help" || flag == "-h" {
            return Ok(None);
        }
        let value = args
            .next()
            .ok_or_else(|| invalid(format!("missing value for {flag}")))?;
        match flag.as_str() {
            "--raw-root" => set_once(&mut raw_root, PathBuf::from(value), &flag)?,
            "--symbol" => set_once(&mut symbol, value, &flag)?,
            "--venue" => {
                let parsed = match value.to_ascii_uppercase().as_str() {
                    "XSHG" => Venue::Xshg,
                    "XSHE" => Venue::Xshe,
                    _ => return Err(invalid("venue must be XSHG or XSHE").into()),
                };
                set_once(&mut venue, parsed, &flag)?;
            }
            "--kind" => {
                let parsed = match value.to_ascii_lowercase().as_str() {
                    "stock" => SecurityKind::Stock,
                    "etf" => SecurityKind::Etf,
                    _ => return Err(invalid("kind must be stock or etf").into()),
                };
                set_once(&mut kind, parsed, &flag)?;
            }
            "--start" => set_once(&mut start, parse_date(&value)?, &flag)?,
            "--end" => set_once(&mut end, parse_date(&value)?, &flag)?,
            _ => return Err(invalid(format!("unknown option {flag}")).into()),
        }
    }

    Ok(Some(Args {
        raw_root: required(raw_root, "--raw-root")?,
        symbol: required(symbol, "--symbol")?,
        venue: required(venue, "--venue")?,
        kind: required(kind, "--kind")?,
        start: required(start, "--start")?,
        end: required(end, "--end")?,
    }))
}

fn set_once<T>(slot: &mut Option<T>, value: T, flag: &str) -> Result<(), Box<dyn Error>> {
    if slot.replace(value).is_some() {
        return Err(invalid(format!("option {flag} may be supplied only once")).into());
    }
    Ok(())
}

fn required<T>(value: Option<T>, flag: &str) -> Result<T, Box<dyn Error>> {
    value.ok_or_else(|| invalid(format!("required option missing: {flag}")).into())
}

fn parse_date(value: &str) -> Result<NaiveDate, Box<dyn Error>> {
    Ok(NaiveDate::parse_from_str(value, "%Y-%m-%d")?)
}

fn invalid(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, message.into())
}

fn print_usage() {
    println!(
        "Usage: prajna-collect-daily-raw \
         --raw-root <LOCAL_PATH> --symbol <SIX_DIGITS> --venue <XSHG|XSHE> \
         --kind <stock|etf> --start <YYYY-MM-DD> --end <YYYY-MM-DD>"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_requires_explicit_local_root_and_security_identity() {
        let error = parse_args(["--symbol", "600519"].into_iter().map(str::to_owned)).unwrap_err();
        assert!(error.to_string().contains("--raw-root"));
    }

    struct FixtureTransport {
        reply: Option<tencent_transport::HttpReply>,
        requests: Vec<tencent_transport::HttpRequest>,
    }

    impl HttpTransport for FixtureTransport {
        fn get(
            &mut self,
            request: &tencent_transport::HttpRequest,
        ) -> Result<tencent_transport::HttpReply, tencent_transport::TransportFailure> {
            self.requests.push(request.clone());
            Ok(self.reply.take().expect("one synthetic reply"))
        }
    }

    #[test]
    fn formal_cli_entrypoint_uses_injected_transport_and_persists_raw_and_report() {
        let root = tempfile::tempdir().unwrap();
        let bytes =
            br#"{"code":0,"data":{"sz159952":{"day":[["2026-02-02","10","11","12","9","100"]]}}}"#
                .to_vec();
        let transport = FixtureTransport {
            reply: Some(tencent_transport::HttpReply {
                source: "https://synthetic.invalid/cli".into(),
                bytes: bytes.clone(),
            }),
            requests: Vec::new(),
        };
        let args = [
            "--raw-root",
            root.path().to_str().unwrap(),
            "--symbol",
            "159952",
            "--venue",
            "XSHE",
            "--kind",
            "etf",
            "--start",
            "2026-02-02",
            "--end",
            "2026-02-02",
        ];

        let result = run_with_transport(args.into_iter().map(str::to_owned), transport).unwrap();
        assert_eq!(result, ExitCode::SUCCESS);

        let report_dir = root.path().join("reports/daily-raw");
        let reports = std::fs::read_dir(report_dir)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(reports.len(), 1);
        let report: prajna_data::daily_raw::DailyRawReport =
            serde_json::from_slice(&std::fs::read(reports[0].path()).unwrap()).unwrap();
        assert_eq!(report.instrument_id, "159952.XSHE");
        assert_eq!(report.status, CollectionStatus::Complete);
        assert_eq!(report.summary.accepted_response_chunks, 1);

        let reference = &report.chunks[0].raw_objects[0];
        let raw = prajna_data::RawStore::open(root.path()).unwrap();
        assert_eq!(raw.get(&reference.raw_sha256).unwrap(), bytes);
        assert_eq!(reference.source, "https://synthetic.invalid/cli");
    }

    #[test]
    fn cli_sanitizes_corrupt_raw_errors_and_saves_diagnostics_locally() {
        let root = tempfile::tempdir().unwrap();
        let bytes =
            br#"{"code":0,"data":{"sz159952":{"day":[["2026-02-02","10","11","12","9","100"]]}}}"#
                .to_vec();
        let args = [
            "--raw-root",
            root.path().to_str().unwrap(),
            "--symbol",
            "159952",
            "--venue",
            "XSHE",
            "--kind",
            "etf",
            "--start",
            "2026-02-02",
            "--end",
            "2026-02-02",
        ];
        let first_transport = FixtureTransport {
            reply: Some(tencent_transport::HttpReply {
                source: "https://synthetic.invalid/first".into(),
                bytes: bytes.clone(),
            }),
            requests: Vec::new(),
        };
        run_with_transport(args.into_iter().map(str::to_owned), first_transport).unwrap();

        let report_path = std::fs::read_dir(root.path().join("reports/daily-raw"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .unwrap();
        let report: prajna_data::daily_raw::DailyRawReport =
            serde_json::from_slice(&std::fs::read(report_path).unwrap()).unwrap();
        let raw_hash = report.chunks[0].raw_objects[0].raw_sha256.clone();
        let raw_hex = raw_hash.strip_prefix("sha256:").unwrap();
        let object_path = root
            .path()
            .join("raw/sha256")
            .join(&raw_hex[..2])
            .join(raw_hex);
        std::fs::write(object_path, b"corrupted local object").unwrap();

        let second_transport = FixtureTransport {
            reply: Some(tencent_transport::HttpReply {
                source: "https://synthetic.invalid/retry".into(),
                bytes,
            }),
            requests: Vec::new(),
        };
        let error =
            run_with_transport(args.into_iter().map(str::to_owned), second_transport).unwrap_err();
        assert!(error.to_string().contains("detailed diagnostics"));
        assert!(!error.to_string().contains(&raw_hash));

        let diagnostics_dir = root.path().join("reports/daily-raw/diagnostics");
        let diagnostic = std::fs::read_dir(diagnostics_dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let diagnostic_text = std::fs::read_to_string(diagnostic).unwrap();
        assert!(diagnostic_text.contains("raw object is corrupt"));
        assert!(diagnostic_text.contains(&raw_hash));
    }

    #[test]
    fn cli_parses_explicit_security_and_date_range() {
        let args = parse_args(
            [
                "--raw-root",
                "/tmp/prajna-raw",
                "--symbol",
                "510300",
                "--venue",
                "xshg",
                "--kind",
                "etf",
                "--start",
                "2026-01-01",
                "--end",
                "2026-01-30",
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .unwrap()
        .unwrap();

        assert_eq!(args.raw_root, PathBuf::from("/tmp/prajna-raw"));
        assert_eq!(args.symbol, "510300");
        assert_eq!(args.venue, Venue::Xshg);
        assert_eq!(args.kind, SecurityKind::Etf);
        assert_eq!(args.start, NaiveDate::from_ymd_opt(2026, 1, 1).unwrap());
        assert_eq!(args.end, NaiveDate::from_ymd_opt(2026, 1, 30).unwrap());
    }
}
