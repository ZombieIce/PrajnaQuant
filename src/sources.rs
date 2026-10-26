use std::{collections::BTreeSet, io::Cursor, thread, time::Duration};

use anyhow::{Context, Result, bail};
use chrono::{Datelike, NaiveDate};
use encoding_rs::GBK;
use reqwest::{
    StatusCode,
    blocking::Client,
    header::{REFERER, USER_AGENT},
};
use serde::Deserialize;
use serde_json::Value;
use zip::ZipArchive;

const USER_AGENT_VALUE: &str = tencent_transport::DEFAULT_USER_AGENT;
const TDX_BJ_FIRST_DAY: &str = "2022-05-06";

#[derive(Debug, Clone)]
pub struct CalendarDay {
    pub trade_date: NaiveDate,
    pub is_open: bool,
}

#[derive(Debug, Clone)]
pub struct AdjustmentFactor {
    pub effective_date: NaiveDate,
    pub factor: String,
}

#[derive(Debug, Clone)]
pub struct TdxDailyBar {
    pub market: &'static str,
    pub code: String,
    pub name: String,
    pub trade_date: NaiveDate,
    pub prev_close: String,
    pub open: String,
    pub high: String,
    pub low: String,
    pub close: String,
    pub volume_shares: u64,
    pub amount_cny: String,
}

pub fn fetch_tencent_raw_daily(
    symbol: &str,
    start: NaiveDate,
    end: NaiveDate,
) -> Result<(String, Vec<u8>)> {
    fetch_tencent_raw_daily_with(
        &mut tencent_transport::ReqwestTransport::new(),
        symbol,
        start,
        end,
    )
}

fn fetch_tencent_raw_daily_with<T: tencent_transport::HttpTransport>(
    transport: &mut T,
    symbol: &str,
    start: NaiveDate,
    end: NaiveDate,
) -> Result<(String, Vec<u8>)> {
    let response = tencent_transport::fetch_unadjusted_daily(
        &tencent_transport::DailyRequest {
            symbol: symbol.to_owned(),
            start,
            end,
        },
        transport,
        tencent_response_has_data_object,
    )
    .map_err(anyhow::Error::new)?;
    Ok((response.source, response.bytes))
}

pub fn tencent_raw_daily_is_empty(bytes: &[u8], symbol: &str) -> Result<bool> {
    let payload: Value = serde_json::from_slice(bytes).context("Tencent response is not JSON")?;
    if payload.get("code").and_then(Value::as_i64) != Some(0) {
        bail!("Tencent response has a non-zero status code")
    }
    let rows = payload
        .get("data")
        .and_then(|data| data.get(symbol))
        .and_then(|node| node.get("day"))
        .and_then(Value::as_array)
        .context("Tencent response lacks the unadjusted day array")?;
    Ok(rows.is_empty())
}

fn tencent_response_has_data_object(bytes: &[u8]) -> bool {
    serde_json::from_slice::<Value>(bytes)
        .ok()
        .and_then(|value| value.get("data").cloned())
        .is_some_and(|data| data.is_object())
}

impl TdxDailyBar {
    pub fn symbol(&self) -> String {
        let prefix = match self.market {
            "SH" => "sh",
            "SZ" => "sz",
            "BJ" => "bj",
            _ => unreachable!("static market values are constrained"),
        };
        format!("{prefix}{}", self.code)
    }
}

pub fn fetch_szse_calendar(year: i32, month: u32) -> Result<(String, Vec<u8>, Vec<CalendarDay>)> {
    if !(1..=12).contains(&month) {
        bail!("month must be 1 through 12");
    }
    let url = format!(
        "https://www.szse.cn/api/report/exchange/onepersistenthour/monthList?month={year}-{month}"
    );
    let body = fetch_bytes(&url, "https://www.szse.cn/")?;
    let payload: Value = serde_json::from_slice(&body).context("SZSE calendar is not JSON")?;
    let rows = payload
        .get("data")
        .and_then(Value::as_array)
        .context("SZSE calendar has no data array")?;
    if rows.is_empty() {
        bail!("SZSE has not published this calendar month")
    }
    let mut expected = BTreeSet::new();
    let last = days_in_month(year, month)?;
    for day in 1..=last {
        expected.insert(NaiveDate::from_ymd_opt(year, month, day).context("invalid calendar day")?);
    }
    let mut seen = BTreeSet::new();
    let mut output = Vec::with_capacity(rows.len());
    for row in rows {
        let date = parse_date(
            row.get("jyrq")
                .and_then(Value::as_str)
                .context("SZSE calendar row lacks jyrq")?,
        )?;
        let flag = match row.get("jybz") {
            Some(Value::String(value)) => value.clone(),
            Some(Value::Number(value)) => value.to_string(),
            _ => bail!("SZSE calendar row has invalid jybz"),
        };
        if !matches!(flag.as_str(), "0" | "1") || !seen.insert(date) {
            bail!("SZSE calendar has invalid flag or duplicate date")
        }
        output.push(CalendarDay {
            trade_date: date,
            is_open: flag == "1",
        });
    }
    if seen != expected {
        bail!("SZSE calendar is not a complete natural month")
    }
    Ok((url, body, output))
}

pub fn fetch_sina_adjustments(
    symbol: &str,
    kind: &str,
) -> Result<(String, Vec<u8>, Vec<AdjustmentFactor>)> {
    if !matches!(kind, "qfq" | "hfq") {
        bail!("adjustment kind must be qfq or hfq")
    }
    validate_explicit_symbol(symbol, true)?;
    let url = format!("https://finance.sina.com.cn/realstock/company/{symbol}/{kind}.js");
    let body = fetch_bytes(&url, "https://finance.sina.com.cn/")?;
    let text = std::str::from_utf8(&body).context("Sina adjustment response is not UTF-8")?;
    let brace = text
        .find('{')
        .context("Sina adjustment response has no JSON object")?;
    let mut decoder = serde_json::Deserializer::from_str(&text[brace..]);
    let payload =
        Value::deserialize(&mut decoder).context("Sina adjustment JSON failed to parse")?;
    let rows = payload
        .get("data")
        .and_then(Value::as_array)
        .context("Sina adjustment response lacks data array")?;
    if rows.is_empty() {
        bail!("Sina adjustment response is empty; never label raw prices as adjusted")
    }
    let mut seen = BTreeSet::new();
    let mut output = Vec::with_capacity(rows.len());
    for row in rows {
        let date = parse_date(
            row.get("d")
                .and_then(Value::as_str)
                .context("factor lacks date")?,
        )?;
        let factor = number_text(row.get("f").context("factor lacks value")?)?;
        if parse_f64(&factor)? <= 0.0 || !seen.insert(date) {
            bail!("Sina adjustment has a non-positive factor or duplicate date")
        }
        output.push(AdjustmentFactor {
            effective_date: date,
            factor,
        });
    }
    Ok((url, body, output))
}

pub fn fetch_tdx_daily_package(
    trade_date: NaiveDate,
) -> Result<(String, Vec<u8>, Vec<TdxDailyBar>)> {
    let ymd = trade_date.format("%Y%m%d");
    let url = format!("https://www.tdx.com.cn/products/data/data/g4day/{ymd}.zip");
    let client = http_client()?;
    let mut failures = Vec::new();
    let body = (0_u64..3)
        .find_map(|attempt| {
            if attempt > 0 {
                thread::sleep(Duration::from_secs(attempt * 2));
            }
            let response = match client
                .get(&url)
                .header(REFERER, "https://www.tdx.com.cn/")
                .send()
            {
                Ok(response) if response.status() == StatusCode::NOT_FOUND => return None,
                Ok(response) => response,
                Err(error) => {
                    failures.push(format!("connect attempt {}: {error}", attempt + 1));
                    return None;
                }
            };
            match response
                .error_for_status()
                .and_then(|response| response.bytes())
            {
                Ok(bytes) => Some(bytes.to_vec()),
                Err(error) => {
                    failures.push(format!("read attempt {}: {error}", attempt + 1));
                    None
                }
            }
        })
        .with_context(|| {
            format!(
                "TDX package download failed after three attempts: {}",
                failures.join("; ")
            )
        })?;
    if !body.starts_with(b"PK") {
        bail!("TDX response is not a ZIP archive")
    }
    let bars = parse_tdx_daily_package(&body, trade_date)?;
    Ok((url, body, bars))
}

pub fn parse_tdx_daily_package(body: &[u8], trade_date: NaiveDate) -> Result<Vec<TdxDailyBar>> {
    let mut archive = ZipArchive::new(Cursor::new(body)).context("open TDX ZIP")?;
    let day_key = trade_date.format("%y%m%d").to_string();
    let bj_first = NaiveDate::parse_from_str(TDX_BJ_FIRST_DAY, "%Y-%m-%d")?;
    let mut output = Vec::new();
    for (market, minimum_rows) in [("SH", 10_000_usize), ("SZ", 3_000), ("BJ", 50)] {
        let prefix = market.to_lowercase();
        let cod_name = format!("{prefix}{day_key}.cod");
        let md1_name = format!("{prefix}{day_key}.md1");
        let cod = match archive.by_name(&cod_name) {
            Ok(mut file) => {
                let mut bytes = Vec::new();
                std::io::Read::read_to_end(&mut file, &mut bytes)?;
                bytes
            }
            Err(_) if market == "BJ" && trade_date < bj_first => continue,
            Err(_) => bail!("TDX package lacks {cod_name}"),
        };
        let md1 = {
            let mut file = archive
                .by_name(&md1_name)
                .with_context(|| format!("TDX package lacks {md1_name}"))?;
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut file, &mut bytes)?;
            bytes
        };
        if cod.len() % 150 != 0 || md1.len() % 512 != 0 || cod.len() / 150 != md1.len() / 512 {
            bail!("TDX code table and price blocks are misaligned for {market}")
        }
        let before = output.len();
        let mut priced_rows = 0_usize;
        let mut skipped_non_ohlc = 0_usize;
        let mut codes = BTreeSet::new();
        let mut sequences = BTreeSet::new();
        for record in cod.chunks_exact(150) {
            let code = std::str::from_utf8(trim_nul_space(&record[0..6]))
                .context("TDX code is not ASCII")?
                .to_string();
            if code.len() != 6 || !code.bytes().all(|byte| byte.is_ascii_digit()) {
                bail!("TDX code is not a six digit security code")
            }
            let sequence = u16::from_le_bytes(record[32..34].try_into()?);
            if !codes.insert(code.clone()) || !sequences.insert(sequence) {
                bail!("TDX package has duplicate code or price block sequence")
            }
            let offset = usize::from(sequence) * 512;
            let block = md1
                .get(offset..offset + 512)
                .context("TDX price block sequence is out of range")?;
            let prev_close = read_f64(block, 4)?;
            let open = read_f64(block, 12)?;
            let high = read_f64(block, 20)?;
            let low = read_f64(block, 28)?;
            let close = read_f64(block, 36)?;
            let amount = read_f64(block, 72)?;
            if ![prev_close, open, high, low, close, amount]
                .iter()
                .all(|value| value.is_finite())
            {
                bail!("TDX price block has a non-finite value")
            }
            if close <= 0.0 {
                continue;
            }
            priced_rows += 1;
            if amount < 0.0 {
                bail!("TDX price block has a negative amount for {market}{code}")
            }
            // The package contains non-stock instruments. A valid close with an
            // empty intraday range is not a usable stock bar, so leave it in the
            // archived source file but do not publish it as an OHLC record.
            if low <= 0.0 || low > open || low > close || high < open || high < close {
                skipped_non_ohlc += 1;
                continue;
            }
            let volume_shares = u64::from_le_bytes(block[56..64].try_into()?);
            let (name, had_errors) = GBK.decode_without_bom_handling(&record[40..72]);
            if had_errors || name.trim().is_empty() {
                bail!("TDX security name is missing or not GBK")
            }
            output.push(TdxDailyBar {
                market,
                code,
                name: name.trim().to_string(),
                trade_date,
                prev_close: format_number(prev_close)?,
                open: format_number(open)?,
                high: format_number(high)?,
                low: format_number(low)?,
                close: format_number(close)?,
                volume_shares,
                amount_cny: format_number(amount)?,
            });
        }
        if priced_rows < minimum_rows {
            bail!(
                "TDX package has too few priced {market} records: {priced_rows} priced, {} usable OHLC, {skipped_non_ohlc} excluded, minimum is {minimum_rows}",
                output.len() - before
            )
        }
    }
    Ok(output)
}

pub fn validate_explicit_symbol(symbol: &str, allow_bj: bool) -> Result<()> {
    let valid_prefix = symbol.starts_with("sh")
        || symbol.starts_with("sz")
        || (allow_bj && symbol.starts_with("bj"));
    if symbol.len() != 8 || !valid_prefix || !symbol.as_bytes()[2..].iter().all(u8::is_ascii_digit)
    {
        bail!("symbol must use an explicit market prefix, e.g. sh600519, sz000001, bj920000")
    }
    Ok(())
}

fn http_client() -> Result<Client> {
    Ok(Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(180))
        .http1_only()
        .user_agent(USER_AGENT_VALUE)
        .build()?)
}

fn fetch_bytes(url: &str, referer: &str) -> Result<Vec<u8>> {
    Ok(http_client()?
        .get(url)
        .header(REFERER, referer)
        .header(USER_AGENT, USER_AGENT_VALUE)
        .send()?
        .error_for_status()?
        .bytes()?
        .to_vec())
}

#[cfg(test)]
mod tencent_transport_compat_tests {
    use std::collections::VecDeque;

    use super::{fetch_tencent_raw_daily_with, tencent_raw_daily_is_empty};
    use chrono::NaiveDate;
    use tencent_transport::{
        CANDIDATE_HOSTS, HttpReply, HttpRequest, HttpTransport, TransportFailure,
    };

    struct SyntheticTransport {
        replies: VecDeque<Result<HttpReply, TransportFailure>>,
        requests: Vec<HttpRequest>,
    }

    impl SyntheticTransport {
        fn new(replies: impl IntoIterator<Item = Result<HttpReply, TransportFailure>>) -> Self {
            Self {
                replies: replies.into_iter().collect(),
                requests: Vec::new(),
            }
        }
    }

    impl HttpTransport for SyntheticTransport {
        fn get(&mut self, request: &HttpRequest) -> Result<HttpReply, TransportFailure> {
            self.requests.push(request.clone());
            self.replies
                .pop_front()
                .expect("one synthetic response per request")
        }
    }

    fn fixture(symbol: &str) -> Vec<u8> {
        format!(r#"{{"code":0,"data":{{"{symbol}":{{"day":[]}}}}}}"#).into_bytes()
    }

    #[test]
    fn legacy_fetch_keeps_stock_and_etf_source_and_exact_raw_bytes() {
        let start = NaiveDate::from_ymd_opt(2026, 9, 14).unwrap();
        let end = NaiveDate::from_ymd_opt(2026, 9, 18).unwrap();
        for symbol in ["sh600519", "sz159901"] {
            let bytes = fixture(symbol);
            let source =
                format!("https://synthetic.invalid/appstock/app/fqkline/get?symbol={symbol}");
            let mut transport = SyntheticTransport::new([Ok(HttpReply {
                source: source.clone(),
                bytes: bytes.clone(),
            })]);

            let (returned_source, returned_bytes) =
                fetch_tencent_raw_daily_with(&mut transport, symbol, start, end).unwrap();

            assert_eq!(returned_source, source);
            assert_eq!(returned_bytes, bytes);
            assert!(tencent_raw_daily_is_empty(&returned_bytes, symbol).unwrap());
            assert_eq!(transport.requests[0].host, CANDIDATE_HOSTS[0]);
        }
    }

    #[test]
    fn legacy_payload_gate_rejects_non_data_objects_before_accepting_a_later_candidate() {
        let start = NaiveDate::from_ymd_opt(2026, 9, 14).unwrap();
        let end = NaiveDate::from_ymd_opt(2026, 9, 18).unwrap();
        let bytes = fixture("sz159901");
        let mut transport = SyntheticTransport::new([
            Ok(HttpReply {
                source: "https://synthetic.invalid/invalid".into(),
                bytes: br#"{"code":0,"not_data":{}}"#.to_vec(),
            }),
            Ok(HttpReply {
                source: "https://synthetic.invalid/accepted".into(),
                bytes: bytes.clone(),
            }),
        ]);

        let (source, returned_bytes) =
            fetch_tencent_raw_daily_with(&mut transport, "sz159901", start, end).unwrap();

        assert_eq!(source, "https://synthetic.invalid/accepted");
        assert_eq!(returned_bytes, bytes);
        assert_eq!(
            transport
                .requests
                .iter()
                .map(|request| request.host)
                .collect::<Vec<_>>(),
            CANDIDATE_HOSTS[..2]
        );
    }
}

fn days_in_month(year: i32, month: u32) -> Result<u32> {
    let first_next = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)
    }
    .context("invalid calendar month")?;
    Ok((first_next - chrono::Duration::days(1)).day())
}

fn parse_date(text: &str) -> Result<NaiveDate> {
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .or_else(|_| NaiveDate::parse_from_str(text, "%Y%m%d"))
        .with_context(|| format!("invalid source date {text}"))
}

fn trim_nul_space(bytes: &[u8]) -> &[u8] {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    bytes[..end].trim_ascii()
}

fn read_f64(bytes: &[u8], offset: usize) -> Result<f64> {
    Ok(f64::from_le_bytes(
        bytes
            .get(offset..offset + 8)
            .context("short TDX price block")?
            .try_into()?,
    ))
}

fn number_text(value: &Value) -> Result<String> {
    let text = match value {
        Value::String(value) => value.clone(),
        Value::Number(value) => value.to_string(),
        _ => bail!("source numeric field is invalid"),
    };
    format_number(parse_f64(&text)?)
}

fn parse_f64(text: &str) -> Result<f64> {
    let value: f64 = text
        .parse()
        .context("source numeric field cannot be parsed")?;
    if !value.is_finite() {
        bail!("source numeric field is not finite")
    }
    Ok(value)
}

fn format_number(value: f64) -> Result<String> {
    if !value.is_finite() {
        bail!("numeric value is not finite")
    }
    Ok(format!("{value:.10}"))
}
