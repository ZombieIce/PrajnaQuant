//! Versioned JSON-lines messages; no numeric token crosses this boundary.
use prajna_domain::{InstrumentId, Notional, Price, Quantity};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub const VERSION: &str = "prajna-adapter@1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireOrder {
    pub order_id: String,
    pub portfolio_id: String,
    pub instrument_id: InstrumentId,
    pub side: Side,
    #[serde(with = "decimal")]
    pub quantity: Quantity,
    pub submitted_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireFill {
    pub order_id: String,
    pub portfolio_id: String,
    pub instrument_id: InstrumentId,
    pub side: Side,
    #[serde(with = "decimal")]
    pub quantity: Quantity,
    #[serde(with = "decimal")]
    pub price: Price,
    /// Backend fee is diagnostic only; the platform recomputes authoritative costs.
    #[serde(with = "decimal")]
    pub fee: Notional,
    pub executed_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireReject {
    pub order_id: String,
    pub portfolio_id: String,
    pub instrument_id: InstrumentId,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireQuote {
    pub instrument_id: InstrumentId,
    #[serde(with = "decimal")]
    pub raw_open: Price,
    #[serde(with = "decimal")]
    pub bid: Price,
    #[serde(with = "decimal")]
    pub ask: Price,
    pub tradable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    BeginRun {
        protocol: String,
        packet: String,
    },
    Open {
        session_date: String,
        ts_open: String,
        quotes: Vec<WireQuote>,
    },
    Order(WireOrder),
    Close {
        session_date: String,
        ts_close: String,
    },
    EndRun,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Response {
    Ready { protocol: String },
    Opened { session_date: String },
    Closed { session_date: String },
    Fill(WireFill),
    Reject(WireReject),
    Done,
}

pub fn encode_line<T: Serialize>(message: &T) -> Result<String, serde_json::Error> {
    let mut line = serde_json::to_string(message)?;
    line.push('\n');
    Ok(line)
}

pub fn decode_line<T: DeserializeOwned>(line: &str) -> Result<T, serde_json::Error> {
    serde_json::from_str(line)
}

mod decimal {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::{fmt::Display, str::FromStr};

    pub fn serialize<T: Display, S: Serializer>(
        value: &T,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.to_string())
    }

    pub fn deserialize<'de, T: FromStr, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<T, D::Error>
    where
        T::Err: Display,
    {
        let text = String::deserialize(deserializer)?;
        let unsigned = text.strip_prefix('-').unwrap_or(&text);
        let mut parts = unsigned.split('.');
        let integer = parts.next().unwrap_or("");
        let fraction = parts.next();
        if integer.is_empty()
            || !integer.bytes().all(|c| c.is_ascii_digit())
            || fraction.is_some_and(|f| {
                f.is_empty() || f.len() > 18 || !f.bytes().all(|c| c.is_ascii_digit())
            })
            || parts.next().is_some()
        {
            return Err(serde::de::Error::custom(
                "expected a plain scale <= 18 decimal string",
            ));
        }
        text.parse().map_err(serde::de::Error::custom)
    }
}
