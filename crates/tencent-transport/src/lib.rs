use std::{error::Error, fmt, time::Duration};

use chrono::NaiveDate;
use reqwest::{blocking::Client, header::REFERER};

pub const CANDIDATE_HOSTS: [&str; 3] = [
    "https://web.ifzq.gtimg.cn",
    "https://proxy.finance.qq.com/ifzqgtimg",
    "https://ifzq.gtimg.cn",
];

const DAILY_PATH: &str = "/appstock/app/fqkline/get";

pub const DEFAULT_REFERER: &str = "https://gu.qq.com/";
pub const DEFAULT_USER_AGENT: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 Chrome/126 Safari/537.36";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpSettings {
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    pub http1_only: bool,
}

pub const DEFAULT_HTTP_SETTINGS: HttpSettings = HttpSettings {
    connect_timeout: Duration::from_secs(10),
    request_timeout: Duration::from_secs(180),
    http1_only: true,
};

/// A bounded request for Tencent's unadjusted daily K-line endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyRequest {
    pub symbol: String,
    pub start: NaiveDate,
    pub end: NaiveDate,
}

/// One candidate HTTP request. The transport module constructs these in the legacy order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub host: &'static str,
    pub url: String,
    pub parameter: String,
    pub referer: &'static str,
    pub user_agent: &'static str,
    pub settings: HttpSettings,
}

/// A successful candidate response with its final URL and original response bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpReply {
    pub source: String,
    pub bytes: Vec<u8>,
}

/// Failure at the HTTP adapter seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportFailureKind {
    Request,
    HttpStatus,
    BodyRead,
    ClientBuild,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportFailure {
    pub kind: TransportFailureKind,
    pub status_code: Option<u16>,
    pub message: String,
}

impl TransportFailure {
    pub fn new(kind: TransportFailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            status_code: None,
            message: message.into(),
        }
    }

    pub fn with_status_code(mut self, status_code: u16) -> Self {
        self.status_code = Some(status_code);
        self
    }
}

/// Injectable seam for one HTTP candidate. Production uses [`ReqwestTransport`]; tests and
/// local callers can provide deterministic synthetic responses.
pub trait HttpTransport {
    fn get(&mut self, request: &HttpRequest) -> Result<HttpReply, TransportFailure>;
}

/// Raw unadjusted response and the final source URL that supplied it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawDailyResponse {
    pub source: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateFailureKind {
    Request,
    HttpStatus,
    EmptyResponse,
    InvalidResponse,
}

/// Failure from one configured Tencent candidate address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateFailure {
    pub host: &'static str,
    pub kind: CandidateFailureKind,
    pub status_code: Option<u16>,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchErrorKind {
    InvalidInput,
    ClientBuild,
    BodyRead,
    EndpointsUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchError {
    pub kind: FetchErrorKind,
    pub message: String,
    pub candidate_failures: Vec<CandidateFailure>,
}

impl FetchError {
    fn new(kind: FetchErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            candidate_failures: Vec::new(),
        }
    }
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for FetchError {}

/// Fetch one explicit, bounded, unadjusted daily response.
///
/// The predicate preserves consumer-specific payload acceptance while this module owns the
/// request parameters, headers, timeout settings, fixed candidate order, and raw response
/// provenance. A rejected or empty candidate advances only through [`CANDIDATE_HOSTS`].
pub fn fetch_unadjusted_daily<T, V>(
    request: &DailyRequest,
    transport: &mut T,
    mut accepts_response: V,
) -> Result<RawDailyResponse, FetchError>
where
    T: HttpTransport,
    V: FnMut(&[u8]) -> bool,
{
    validate_request(request)?;

    let parameter = format!(
        "{},day,{},{},640,",
        request.symbol, request.start, request.end
    );
    let mut failures = Vec::new();
    for host in CANDIDATE_HOSTS {
        let http_request = HttpRequest {
            host,
            url: format!("{host}{DAILY_PATH}"),
            parameter: parameter.clone(),
            referer: DEFAULT_REFERER,
            user_agent: DEFAULT_USER_AGENT,
            settings: DEFAULT_HTTP_SETTINGS,
        };
        match transport.get(&http_request) {
            Ok(reply) if reply.bytes.is_empty() => failures.push(CandidateFailure {
                host,
                kind: CandidateFailureKind::EmptyResponse,
                status_code: None,
                message: "empty or invalid data object".into(),
            }),
            Ok(reply) if accepts_response(&reply.bytes) => {
                return Ok(RawDailyResponse {
                    source: reply.source,
                    bytes: reply.bytes,
                });
            }
            Ok(_) => failures.push(CandidateFailure {
                host,
                kind: CandidateFailureKind::InvalidResponse,
                status_code: None,
                message: "empty or invalid data object".into(),
            }),
            Err(failure)
                if matches!(
                    failure.kind,
                    TransportFailureKind::BodyRead | TransportFailureKind::ClientBuild
                ) =>
            {
                let kind = match failure.kind {
                    TransportFailureKind::BodyRead => FetchErrorKind::BodyRead,
                    TransportFailureKind::ClientBuild => FetchErrorKind::ClientBuild,
                    TransportFailureKind::Request | TransportFailureKind::HttpStatus => {
                        unreachable!("only immediate failure kinds reach this arm")
                    }
                };
                return Err(FetchError::new(kind, failure.message));
            }
            Err(failure) => failures.push(CandidateFailure {
                host,
                kind: match failure.kind {
                    TransportFailureKind::Request => CandidateFailureKind::Request,
                    TransportFailureKind::HttpStatus => CandidateFailureKind::HttpStatus,
                    TransportFailureKind::BodyRead | TransportFailureKind::ClientBuild => {
                        unreachable!("immediate failure kinds are handled above")
                    }
                },
                status_code: failure.status_code,
                message: failure.message,
            }),
        }
    }

    let details = failures
        .iter()
        .map(|failure| format!("{}: {}", failure.host, failure.message))
        .collect::<Vec<_>>()
        .join("; ");
    Err(FetchError {
        kind: FetchErrorKind::EndpointsUnavailable,
        message: format!("Tencent K-line endpoints are unavailable: {details}"),
        candidate_failures: failures,
    })
}

fn validate_request(request: &DailyRequest) -> Result<(), FetchError> {
    validate_explicit_symbol(&request.symbol)
        .map_err(|message| FetchError::new(FetchErrorKind::InvalidInput, message))?;
    if request.symbol.starts_with("bj") {
        return Err(FetchError::new(
            FetchErrorKind::InvalidInput,
            "Tencent historical K-lines do not support Beijing exchange symbols",
        ));
    }
    if request.start > request.end || (request.end - request.start).num_days() >= 800 {
        return Err(FetchError::new(
            FetchErrorKind::InvalidInput,
            "Tencent daily window must be between 1 and 800 calendar days",
        ));
    }
    Ok(())
}

fn validate_explicit_symbol(symbol: &str) -> Result<(), String> {
    let valid_prefix = symbol.starts_with("sh") || symbol.starts_with("sz");
    if symbol.len() != 8 || !valid_prefix || !symbol.as_bytes()[2..].iter().all(u8::is_ascii_digit)
    {
        return Err(
            "symbol must use an explicit market prefix, e.g. sh600519, sz000001, bj920000".into(),
        );
    }
    Ok(())
}

/// Direct HTTP adapter. It uses the same connection/request timeouts and HTTP/1-only setting as
/// the legacy warehouse client, and lets reqwest follow the same default redirect policy.
#[derive(Default)]
pub struct ReqwestTransport {
    client: Option<Client>,
    settings: Option<(HttpSettings, &'static str)>,
}

impl ReqwestTransport {
    pub fn new() -> Self {
        Self::default()
    }

    fn client(
        &mut self,
        settings: HttpSettings,
        user_agent: &'static str,
    ) -> Result<&Client, TransportFailure> {
        if self.settings != Some((settings, user_agent)) {
            let mut builder = Client::builder()
                .connect_timeout(settings.connect_timeout)
                .timeout(settings.request_timeout)
                .user_agent(user_agent);
            if settings.http1_only {
                builder = builder.http1_only();
            }
            self.client = Some(builder.build().map_err(|error| {
                TransportFailure::new(TransportFailureKind::ClientBuild, error.to_string())
            })?);
            self.settings = Some((settings, user_agent));
        }
        Ok(self
            .client
            .as_ref()
            .expect("a client is built when settings change"))
    }
}

impl HttpTransport for ReqwestTransport {
    fn get(&mut self, request: &HttpRequest) -> Result<HttpReply, TransportFailure> {
        let response = self
            .client(request.settings, request.user_agent)?
            .get(&request.url)
            .query(&[("param", request.parameter.as_str())])
            .header(REFERER, request.referer)
            .send()
            .map_err(|error| {
                TransportFailure::new(TransportFailureKind::Request, error.to_string())
            })?;
        let status_code = response.status().as_u16();
        let response = response.error_for_status().map_err(|error| {
            TransportFailure::new(TransportFailureKind::HttpStatus, error.to_string())
                .with_status_code(status_code)
        })?;
        let source = response.url().to_string();
        let bytes = response
            .bytes()
            .map_err(|error| {
                TransportFailure::new(TransportFailureKind::BodyRead, error.to_string())
            })?
            .to_vec();
        Ok(HttpReply { source, bytes })
    }
}

#[cfg(test)]
mod tests;
