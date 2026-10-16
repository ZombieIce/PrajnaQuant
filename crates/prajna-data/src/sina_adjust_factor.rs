use chrono::NaiveDate;
use prajna_domain::Notional;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    error::Error,
    fmt,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use crate::{RawStore, SourceKind, SourceRecordInput};

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(0);
static LAST_REQUEST: Mutex<Option<Instant>> = Mutex::new(None);

const SINA_HOST: &str = "https://finance.sina.com.cn";
const USER_AGENT: &str = "PrajnaQuant/0.1 (local noncommercial research)";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdjustmentEvent {
    pub effective_date: NaiveDate,
    pub cash_per_share: Option<String>,
    pub share_ratio: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifecycleRange {
    pub native_symbol: String,
    pub start: NaiveDate,
    pub end_exclusive: Option<NaiveDate>,
}

impl LifecycleRange {
    pub fn contains(&self, date: NaiveDate) -> bool {
        date >= self.start && self.end_exclusive.is_none_or(|end| date < end)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AdjustmentPoint {
    effective_date: NaiveDate,
    cumulative_cash_per_share: Notional,
    cumulative_share_ratio: Notional,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SinaAdjustmentError(String);

impl fmt::Display for SinaAdjustmentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for SinaAdjustmentError {}

type Result<T> = std::result::Result<T, SinaAdjustmentError>;

fn error(message: impl Into<String>) -> SinaAdjustmentError {
    SinaAdjustmentError(message.into())
}

#[derive(Debug, Clone)]
pub struct Request {
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct HttpReply {
    pub status: u16,
    pub content_type: String,
    pub bytes: Vec<u8>,
}

impl HttpReply {
    pub fn ok(bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            status: 200,
            content_type: "application/javascript".into(),
            bytes: bytes.into(),
        }
    }
}

pub trait Transport {
    fn send(
        &mut self,
        request: &Request,
    ) -> std::result::Result<HttpReply, Box<dyn Error + Send + Sync>>;
}

impl Transport for reqwest::blocking::Client {
    fn send(
        &mut self,
        request: &Request,
    ) -> std::result::Result<HttpReply, Box<dyn Error + Send + Sync>> {
        let response = self.get(&request.url).send()?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_owned();
        Ok(HttpReply {
            status,
            content_type,
            bytes: response.bytes()?.to_vec(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdjustmentPair {
    pub symbol: String,
    pub qfq_raw_sha256: String,
    pub hfq_raw_sha256: String,
}

pub struct Collector<T> {
    transport: T,
    raw: RawStore,
    network_path: String,
    stopped: bool,
}

impl Collector<reqwest::blocking::Client> {
    pub fn direct(raw: RawStore) -> Result<Self> {
        Self::direct_with_network_path(raw, "direct_no_proxy; egress region unverified")
    }

    /// Creates the direct client and records the operator-supplied egress path
    /// with every local Raw response. The value is provenance, not a network
    /// route override.
    pub fn direct_with_network_path(
        raw: RawStore,
        network_path: impl Into<String>,
    ) -> Result<Self> {
        let network_path = network_path.into();
        if network_path.trim().is_empty() {
            return Err(error("direct network path description is required"));
        }
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(60))
            .user_agent(USER_AGENT)
            .build()
            .map_err(|e| error(format!("could not create direct Sina client: {e}")))?;
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
            stopped: false,
        }
    }

    pub fn collect(
        &mut self,
        symbols: &[String],
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<AdjustmentPair>> {
        if self.stopped {
            return Err(error("Sina source is stopped: unresolved"));
        }
        if start > end {
            return Err(error("Sina adjustment window start must not exceed end"));
        }
        let symbols = symbols
            .iter()
            .map(|symbol| {
                validate_symbol(symbol)?;
                Ok(symbol.clone())
            })
            .collect::<Result<BTreeSet<_>>>()?;
        if symbols.is_empty() {
            return Err(error(
                "Sina adjustment collection requires at least one symbol",
            ));
        }
        let mut pairs = Vec::with_capacity(symbols.len());
        for symbol in symbols {
            let (qfq_raw_sha256, qfq) = self.fetch(&symbol, "qfq", start, end)?;
            let (hfq_raw_sha256, hfq) = self.fetch(&symbol, "hfq", start, end)?;
            if let Err(problem) = verify_paired_series(&qfq, &hfq) {
                self.stopped = true;
                return Err(problem);
            }
            pairs.push(AdjustmentPair {
                symbol,
                qfq_raw_sha256,
                hfq_raw_sha256,
            });
        }
        Ok(pairs)
    }

    fn fetch(
        &mut self,
        symbol: &str,
        kind: &str,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<(String, Vec<u8>)> {
        if self.stopped {
            return Err(error("Sina source is stopped: unresolved"));
        }
        let url = format!("{SINA_HOST}/realstock/company/{symbol}/{kind}.js");
        for attempt in 0..=3 {
            let observed_at = chrono::Utc::now().to_rfc3339();
            let reply = send_paced(&mut self.transport, &Request { url: url.clone() });
            let (status, content_type, bytes, transport_error) = match reply {
                Ok(reply) => (reply.status, reply.content_type, reply.bytes, None),
                Err(problem) => (
                    0,
                    "application/octet-stream".into(),
                    Vec::new(),
                    Some(problem.to_string()),
                ),
            };
            let raw_sha256 = self
                .raw
                .put(
                    &bytes,
                    SourceRecordInput {
                        content_type: content_type.clone(),
                        source_kind: SourceKind::VendorApi,
                        source_id: "sina_adjust_factor".into(),
                        request: serde_json::json!({
                            "url": url,
                            "adjustment_kind": kind,
                            "symbol": symbol,
                            "window_start": start,
                            "window_end": end,
                            "http_status": status,
                            "byte_len": bytes.len(),
                            "terms_conclusion": "issue_172_owner_authorization_for_local_noncommercial_research",
                            "transport_error": transport_error,
                            "attempt": attempt,
                            "network_path": self.network_path.clone(),
                            "request_id": NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed),
                        }),
                        observed_at,
                        ingested_by: "prajna-sina-adjust-factor/1".into(),
                    },
                )
                .map_err(|e| error(format!("could not store Sina Raw response: {e}")))?;
            if status == 200 {
                if let Err(problem) = points(&bytes) {
                    self.stopped = true;
                    return Err(problem);
                }
                return Ok((raw_sha256, bytes));
            }
            let text = String::from_utf8_lossy(&bytes).to_lowercase();
            let access_control = matches!(status, 301..=399 | 401 | 403 | 429 | 501)
                || text.contains("验证码")
                || text.contains("captcha")
                || (text.contains("<html") && (text.contains("登录") || text.contains("login")));
            if access_control {
                self.stopped = true;
                return Err(error(format!(
                    "Sina access control or redirect returned HTTP {status}; source stopped"
                )));
            }
            if attempt == 3 {
                self.stopped = true;
                return Err(error(format!(
                    "Sina request exhausted retries after HTTP {status}; source stopped"
                )));
            }
            thread::sleep(Duration::from_secs(1_u64 << attempt));
        }
        unreachable!("the retry loop returns on success or exhaustion")
    }
}

fn validate_symbol(symbol: &str) -> Result<()> {
    let valid = symbol.len() == 8
        && symbol.starts_with("sz")
        && symbol[2..].bytes().all(|byte| byte.is_ascii_digit())
        && matches!(&symbol[2..5], "158" | "159");
    if valid {
        Ok(())
    } else {
        Err(error(
            "Sina adjustment collection requires an explicit sz158xxx or sz159xxx symbol",
        ))
    }
}

fn send_paced<T: Transport>(
    transport: &mut T,
    request: &Request,
) -> std::result::Result<HttpReply, Box<dyn Error + Send + Sync>> {
    let mut last = LAST_REQUEST.lock().expect("Sina request clock poisoned");
    if let Some(previous) = *last {
        thread::sleep(Duration::from_secs(1).saturating_sub(previous.elapsed()));
    }
    *last = Some(Instant::now());
    transport.send(request)
}

fn decimal(value: &Value, field: &str) -> Result<Notional> {
    let text = match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        _ => return Err(error(format!("adjustment row {field} must be a decimal"))),
    };
    Notional::parse(&text).map_err(|e| error(format!("invalid adjustment row {field}: {e}")))
}

fn exact_ratio(numerator: Notional, denominator: Notional) -> Result<Notional> {
    let numerator = numerator.mantissa();
    let denominator = denominator.mantissa();
    if denominator <= 0 || numerator <= 0 {
        return Err(error("cumulative share ratio must be positive"));
    }
    let gcd = gcd(numerator as u128, denominator as u128) as i128;
    let numerator = numerator / gcd;
    let denominator = denominator / gcd;
    let scale = 1_000_000_000_000_000_000_i128;
    if scale % denominator != 0 {
        return Err(error(
            "share conversion ratio is not exactly representable at domain scale",
        ));
    }
    let mantissa = numerator
        .checked_mul(scale / denominator)
        .ok_or_else(|| error("share conversion ratio is out of range"))?;
    Notional::from_mantissa(mantissa)
        .map_err(|e| error(format!("share conversion ratio is out of range: {e}")))
}

fn gcd(mut left: u128, mut right: u128) -> u128 {
    while right != 0 {
        (left, right) = (right, left % right);
    }
    left
}

fn points(bytes: &[u8]) -> Result<Vec<AdjustmentPoint>> {
    let text = std::str::from_utf8(bytes)
        .map_err(|e| error(format!("Sina adjustment response is not UTF-8: {e}")))?;
    let start = text
        .find('{')
        .ok_or_else(|| error("Sina adjustment response has no JSON object"))?;
    let mut deserializer = serde_json::Deserializer::from_str(&text[start..]);
    let payload = Value::deserialize(&mut deserializer)
        .map_err(|e| error(format!("Sina adjustment JSON failed to parse: {e}")))?;
    let rows = payload
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| error("Sina adjustment response lacks data array"))?;
    if rows.is_empty() {
        return Err(error("Sina adjustment response has no factor points"));
    }
    let mut points = Vec::with_capacity(rows.len());
    for row in rows {
        let date = row
            .get("d")
            .and_then(Value::as_str)
            .ok_or_else(|| error("Sina adjustment point lacks date"))?
            .parse::<NaiveDate>()
            .map_err(|e| error(format!("invalid Sina adjustment date: {e}")))?;
        let cash = decimal(
            row.get("u")
                .ok_or_else(|| error("Sina adjustment point lacks cumulative dividend"))?,
            "u",
        )?;
        let shares = decimal(
            row.get("s")
                .ok_or_else(|| error("Sina adjustment point lacks cumulative share ratio"))?,
            "s",
        )?;
        if cash < Notional::ZERO || shares <= Notional::ZERO {
            return Err(error(
                "cumulative dividend must be non-negative and share ratio positive",
            ));
        }
        if points
            .last()
            .is_some_and(|previous: &AdjustmentPoint| previous.effective_date >= date)
        {
            return Err(error(
                "Sina adjustment points must have unique ascending dates",
            ));
        }
        points.push(AdjustmentPoint {
            effective_date: date,
            cumulative_cash_per_share: cash,
            cumulative_share_ratio: shares,
        });
    }
    Ok(points)
}

fn events(points: &[AdjustmentPoint]) -> Result<Vec<AdjustmentEvent>> {
    points
        .windows(2)
        .map(|pair| {
            let previous = &pair[0];
            let current = &pair[1];
            let cash_delta = current
                .cumulative_cash_per_share
                .mantissa()
                .checked_sub(previous.cumulative_cash_per_share.mantissa())
                .ok_or_else(|| error("cumulative dividend delta overflow"))?;
            if cash_delta < 0 {
                return Err(error("cumulative dividend decreased"));
            }
            let ratio = exact_ratio(
                current.cumulative_share_ratio,
                previous.cumulative_share_ratio,
            )?;
            let cash_per_share = (cash_delta != 0).then(|| {
                Notional::from_mantissa(cash_delta)
                    .expect("difference of representable non-negative values is representable")
                    .to_string()
            });
            let share_ratio = (ratio != Notional::parse("1").expect("one is representable"))
                .then(|| ratio.to_string());
            Ok(AdjustmentEvent {
                effective_date: current.effective_date,
                cash_per_share,
                share_ratio,
            })
        })
        .filter_map(|event| match event {
            Ok(event) if event.cash_per_share.is_some() || event.share_ratio.is_some() => {
                Some(Ok(event))
            }
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .collect()
}

pub fn parse_adjustment_series(bytes: &[u8]) -> Result<Vec<AdjustmentEvent>> {
    events(&points(bytes)?)
}

pub fn parse_adjustment_series_for_lifecycles(
    bytes: &[u8],
    lifecycles: &[LifecycleRange],
) -> Result<Vec<AdjustmentEvent>> {
    let points = points(bytes)?;
    let mut output = Vec::new();
    for lifecycle in lifecycles {
        let lifecycle_points = points
            .iter()
            .filter(|point| lifecycle.contains(point.effective_date))
            .cloned()
            .collect::<Vec<_>>();
        output.extend(events(&lifecycle_points)?);
    }
    output.sort_by_key(|event| event.effective_date);
    Ok(output)
}

/// Compares source points only; events are derived later per #203 lifecycle,
/// so reused codes whose cumulative factors reset are not rejected here.
fn verify_paired_series(qfq: &[u8], hfq: &[u8]) -> Result<()> {
    paired_points(qfq, hfq).map(|_| ())
}

fn paired_points(qfq: &[u8], hfq: &[u8]) -> Result<Vec<AdjustmentPoint>> {
    let qfq_points = points(qfq)?;
    let hfq_points = points(hfq)?;
    if qfq_points != hfq_points {
        return Err(error("Sina qfq and hfq cumulative factors differ"));
    }
    Ok(qfq_points)
}

pub fn compare_adjustment_series(qfq: &[u8], hfq: &[u8]) -> Result<Vec<AdjustmentEvent>> {
    events(&paired_points(qfq, hfq)?)
}
