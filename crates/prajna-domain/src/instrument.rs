use core::{fmt, str::FromStr};

use chrono::NaiveDate;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{Price, Quantity};

/// An exchange or market venue identifier, such as `XSHG` or `BINANCE`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VenueId(String);

/// A currency code used by an instrument specification.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Currency(String);

macro_rules! validated_code {
    ($type:ty, $validator:ident) => {
        impl $type {
            pub fn new(value: impl Into<String>) -> Result<Self, CodeError> {
                let value = value.into();
                if $validator(&value) {
                    Ok(Self(value))
                } else {
                    Err(CodeError)
                }
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $type {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl FromStr for $type {
            type Err = CodeError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl Serialize for $type {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $type {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

fn valid_venue_id(value: &str) -> bool {
    (2..=16).contains(&value.len())
        && value.as_bytes()[0].is_ascii_uppercase()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn valid_currency(value: &str) -> bool {
    (2..=12).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

validated_code!(VenueId, valid_venue_id);
validated_code!(Currency, valid_currency);

/// Invalid venue or currency code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodeError;

impl fmt::Display for CodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid uppercase ASCII code")
    }
}

impl std::error::Error for CodeError {}

/// A venue-scoped identity in canonical `{symbol}.{VENUE}` form.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InstrumentId {
    symbol: String,
    venue: VenueId,
}

impl InstrumentId {
    pub fn new(symbol: impl Into<String>, venue: VenueId) -> Result<Self, InstrumentIdError> {
        let symbol = symbol.into();
        if !valid_symbol(&symbol) {
            return Err(InstrumentIdError);
        }
        Ok(Self { symbol, venue })
    }

    pub fn symbol(&self) -> &str {
        &self.symbol
    }

    pub fn venue(&self) -> &VenueId {
        &self.venue
    }
}

fn valid_symbol(symbol: &str) -> bool {
    !symbol.is_empty()
        && symbol.len() <= 64
        && symbol.bytes().all(|byte| {
            byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
        })
}

impl fmt::Display for InstrumentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.symbol, self.venue)
    }
}

impl FromStr for InstrumentId {
    type Err = InstrumentIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (symbol, venue) = value.split_once('.').ok_or(InstrumentIdError)?;
        if venue.contains('.') {
            return Err(InstrumentIdError);
        }
        Self::new(symbol, VenueId::new(venue).map_err(|_| InstrumentIdError)?)
    }
}

impl Serialize for InstrumentId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for InstrumentId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

/// Invalid canonical instrument identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstrumentIdError;

impl fmt::Display for InstrumentIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid instrument id; expected {SYMBOL}.{VENUE}")
    }
}

impl std::error::Error for InstrumentIdError {}

/// The economic kind of an instrument. It is never inferred from its ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstrumentKind {
    Equity,
    Etf,
    Spot,
    Perpetual,
    Future,
}

/// A validated description of an instrument's market semantics and increments.
/// Input data used to construct an [`InstrumentSpec`].
///
/// Values are validated when passed to [`InstrumentSpec::new`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstrumentSpecData {
    pub id: InstrumentId,
    pub kind: InstrumentKind,
    pub native_symbol: String,
    pub market_segment: Option<String>,
    pub base_currency: Option<Currency>,
    pub quote_currency: Option<Currency>,
    pub settle_currency: Option<Currency>,
    pub is_inverse: bool,
    pub multiplier: Option<Quantity>,
    pub expiry: Option<NaiveDate>,
    pub price_increment: Price,
    pub size_increment: Quantity,
    pub lot_size: Option<Quantity>,
    pub session_timezone: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstrumentSpec {
    id: InstrumentId,
    kind: InstrumentKind,
    native_symbol: String,
    market_segment: Option<String>,
    base_currency: Option<Currency>,
    quote_currency: Option<Currency>,
    settle_currency: Option<Currency>,
    is_inverse: bool,
    multiplier: Option<Quantity>,
    expiry: Option<NaiveDate>,
    price_increment: Price,
    size_increment: Quantity,
    lot_size: Option<Quantity>,
    session_timezone: Option<String>,
}

impl<'de> Deserialize<'de> for InstrumentSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let data = InstrumentSpecData::deserialize(deserializer)?;
        Self::new(data).map_err(serde::de::Error::custom)
    }
}

impl InstrumentSpec {
    pub fn new(data: InstrumentSpecData) -> Result<Self, InstrumentSpecError> {
        if matches!(
            data.kind,
            InstrumentKind::Perpetual | InstrumentKind::Future
        ) && data.settle_currency.is_none()
        {
            return Err(InstrumentSpecError::MissingSettleCurrency);
        }
        match (data.kind, data.expiry) {
            (InstrumentKind::Future, None) => return Err(InstrumentSpecError::MissingExpiry),
            (InstrumentKind::Future, Some(_)) => {}
            (_, Some(_)) => return Err(InstrumentSpecError::UnexpectedExpiry),
            (_, None) => {}
        }
        if data.price_increment <= Price::ZERO || data.size_increment <= Quantity::ZERO {
            return Err(InstrumentSpecError::NonPositiveIncrement);
        }
        Ok(Self {
            id: data.id,
            kind: data.kind,
            native_symbol: data.native_symbol,
            market_segment: data.market_segment,
            base_currency: data.base_currency,
            quote_currency: data.quote_currency,
            settle_currency: data.settle_currency,
            is_inverse: data.is_inverse,
            multiplier: data.multiplier,
            expiry: data.expiry,
            price_increment: data.price_increment,
            size_increment: data.size_increment,
            lot_size: data.lot_size,
            session_timezone: data.session_timezone,
        })
    }

    pub fn id(&self) -> &InstrumentId {
        &self.id
    }
    pub fn kind(&self) -> InstrumentKind {
        self.kind
    }
    pub fn native_symbol(&self) -> &str {
        &self.native_symbol
    }
    pub fn market_segment(&self) -> Option<&str> {
        self.market_segment.as_deref()
    }
    pub fn base_currency(&self) -> Option<&Currency> {
        self.base_currency.as_ref()
    }
    pub fn quote_currency(&self) -> Option<&Currency> {
        self.quote_currency.as_ref()
    }
    pub fn settle_currency(&self) -> Option<&Currency> {
        self.settle_currency.as_ref()
    }
    pub fn is_inverse(&self) -> bool {
        self.is_inverse
    }
    pub fn multiplier(&self) -> Option<Quantity> {
        self.multiplier
    }
    pub fn expiry(&self) -> Option<NaiveDate> {
        self.expiry
    }
    pub fn price_increment(&self) -> Price {
        self.price_increment
    }
    pub fn size_increment(&self) -> Quantity {
        self.size_increment
    }
    pub fn lot_size(&self) -> Option<Quantity> {
        self.lot_size
    }
    pub fn session_timezone(&self) -> Option<&str> {
        self.session_timezone.as_deref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstrumentSpecError {
    MissingSettleCurrency,
    MissingExpiry,
    UnexpectedExpiry,
    NonPositiveIncrement,
    IdReused,
}

impl fmt::Display for InstrumentSpecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::MissingSettleCurrency => "Perpetual and Future require a settle currency",
            Self::MissingExpiry => "Future requires an expiry",
            Self::UnexpectedExpiry => "only Future may have an expiry",
            Self::NonPositiveIncrement => "price and size increments must be positive",
            Self::IdReused => "instrument ID is already registered with a different spec",
        };
        f.write_str(message)
    }
}

impl std::error::Error for InstrumentSpecError {}

/// A registry that makes repeated registration of an identical spec idempotent.
#[derive(Debug, Clone, Default)]
pub struct InstrumentSpecs(std::collections::BTreeMap<InstrumentId, InstrumentSpec>);

impl InstrumentSpecs {
    /// Inserts a spec. Returns `true` when inserted and `false` when already identical.
    pub fn insert(&mut self, spec: InstrumentSpec) -> Result<bool, InstrumentSpecError> {
        if let Some(existing) = self.0.get(spec.id()) {
            return if existing == &spec {
                Ok(false)
            } else {
                Err(InstrumentSpecError::IdReused)
            };
        }
        self.0.insert(spec.id.clone(), spec);
        Ok(true)
    }

    pub fn get(&self, id: &InstrumentId) -> Option<&InstrumentSpec> {
        self.0.get(id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyInstrumentIdError {
    InvalidFormat,
    InvalidCode,
}

impl fmt::Display for LegacyInstrumentIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidFormat => "expected legacy exchange prefix and six-digit code",
            Self::InvalidCode => "legacy instrument code must contain six digits",
        })
    }
}

impl std::error::Error for LegacyInstrumentIdError {}

/// Maps the supported legacy A-share IDs to canonical venue-scoped IDs.
pub fn instrument_id_from_legacy(value: &str) -> Result<InstrumentId, LegacyInstrumentIdError> {
    let (prefix, code) = if value.len() == 9 && value.as_bytes().get(2) == Some(&b':') {
        (
            value
                .get(..2)
                .ok_or(LegacyInstrumentIdError::InvalidFormat)?,
            value
                .get(3..)
                .ok_or(LegacyInstrumentIdError::InvalidFormat)?,
        )
    } else if value.len() == 8 {
        (
            value
                .get(..2)
                .ok_or(LegacyInstrumentIdError::InvalidFormat)?,
            value
                .get(2..)
                .ok_or(LegacyInstrumentIdError::InvalidFormat)?,
        )
    } else {
        return Err(LegacyInstrumentIdError::InvalidFormat);
    };
    if !code.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(LegacyInstrumentIdError::InvalidCode);
    }
    let venue = match prefix {
        "SH" if value.as_bytes().get(2) == Some(&b':') => "XSHG",
        "SZ" if value.as_bytes().get(2) == Some(&b':') => "XSHE",
        "BJ" if value.as_bytes().get(2) == Some(&b':') => "BJSE",
        "sh" if value.len() == 8 => "XSHG",
        "sz" if value.len() == 8 => "XSHE",
        "bj" if value.len() == 8 => "BJSE",
        _ => return Err(LegacyInstrumentIdError::InvalidFormat),
    };
    InstrumentId::new(
        code,
        VenueId::new(venue).expect("static venue code is valid"),
    )
    .map_err(|_| LegacyInstrumentIdError::InvalidCode)
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::*;
    use crate::{Price, Quantity};

    fn spec(
        id: &str,
        kind: InstrumentKind,
        settle: Option<&str>,
        expiry: Option<&str>,
        price_increment: &str,
        size_increment: &str,
    ) -> InstrumentSpec {
        InstrumentSpec::new(InstrumentSpecData {
            id: id.parse().unwrap(),
            kind,
            native_symbol: id.split('.').next().unwrap().to_owned(),
            market_segment: Some("MAIN".to_owned()),
            base_currency: Some(Currency::new("BTC").unwrap()),
            quote_currency: Some(Currency::new("USDT").unwrap()),
            settle_currency: settle.map(|value| Currency::new(value).unwrap()),
            is_inverse: false,
            multiplier: Some(Quantity::parse("1").unwrap()),
            expiry: expiry.map(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d").unwrap()),
            price_increment: Price::parse(price_increment).unwrap(),
            size_increment: Quantity::parse(size_increment).unwrap(),
            lot_size: None,
            session_timezone: Some("UTC".to_owned()),
        })
        .unwrap()
    }

    #[test]
    fn parses_and_round_trips_specification_ids_without_inferring_kind() {
        for id in [
            "510300.XSHG",
            "BTC-USDT.BINANCE",
            "BTC-USDT-PERP.BINANCE",
            "BTC-USD-PERP.BINANCE",
            "BTC-USDT-20261225.BINANCE",
        ] {
            let parsed: InstrumentId = id.parse().unwrap();
            assert_eq!(parsed.to_string(), id);
            assert_eq!(parsed, id.parse().unwrap());
        }
        let id: InstrumentId = "BTC-USDT-PERP.BINANCE".parse().unwrap();
        let instrument_spec = spec(
            &id.to_string(),
            InstrumentKind::Spot,
            None,
            None,
            "0.01",
            "0.001",
        );
        assert_eq!(instrument_spec.kind(), InstrumentKind::Spot);
    }

    #[test]
    fn rejects_noncanonical_instrument_ids() {
        for id in ["abc.XSHG", "A.B.C", ".XSHG", "A$B.XSHG", "A.BINANCE-"] {
            assert!(id.parse::<InstrumentId>().is_err(), "{id} should fail");
        }
        assert!(
            InstrumentId::new(format!("{}A", "A".repeat(64)), "XSHG".parse().unwrap()).is_err()
        );
        assert!("X".parse::<VenueId>().is_err());
        assert!("xshg".parse::<VenueId>().is_err());
        assert!("1XSHG".parse::<VenueId>().is_err());
        assert!("A-BC".parse::<VenueId>().is_err());
        assert!("A_1".parse::<VenueId>().is_ok());
        assert!("U$D".parse::<Currency>().is_err());
        assert!("usd".parse::<Currency>().is_err());
        assert!("A".parse::<Currency>().is_err());
        assert!("12".parse::<Currency>().is_ok());
        assert!("USD".parse::<Currency>().is_ok());
    }

    #[test]
    fn validates_spec_kind_requirements_and_positive_increments() {
        assert_eq!(
            spec("A.XSHG", InstrumentKind::Equity, None, None, "0.01", "1").kind(),
            InstrumentKind::Equity
        );
        assert_eq!(
            spec("A.XSHG", InstrumentKind::Etf, None, None, "0.01", "1").kind(),
            InstrumentKind::Etf
        );
        assert_eq!(
            spec("A.BINANCE", InstrumentKind::Spot, None, None, "0.01", "0.1").kind(),
            InstrumentKind::Spot
        );
        assert_eq!(
            spec(
                "A.BINANCE",
                InstrumentKind::Perpetual,
                Some("USDT"),
                None,
                "0.01",
                "0.1"
            )
            .kind(),
            InstrumentKind::Perpetual
        );
        assert_eq!(
            spec(
                "A.BINANCE",
                InstrumentKind::Future,
                Some("USD"),
                Some("2026-12-25"),
                "0.01",
                "1"
            )
            .kind(),
            InstrumentKind::Future
        );

        let build = |kind: InstrumentKind,
                     settle: Option<&str>,
                     expiry: Option<&str>,
                     price: &str,
                     size: &str| {
            InstrumentSpec::new(InstrumentSpecData {
                id: "A.BINANCE".parse().unwrap(),
                kind,
                native_symbol: "A".to_owned(),
                market_segment: None,
                base_currency: None,
                quote_currency: None,
                settle_currency: settle.map(|currency| Currency::new(currency).unwrap()),
                is_inverse: false,
                multiplier: None,
                expiry: expiry.map(|date| NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap()),
                price_increment: Price::parse(price).unwrap(),
                size_increment: Quantity::parse(size).unwrap(),
                lot_size: None,
                session_timezone: None,
            })
        };
        assert_eq!(
            build(InstrumentKind::Perpetual, None, None, "1", "1"),
            Err(InstrumentSpecError::MissingSettleCurrency)
        );
        assert_eq!(
            build(InstrumentKind::Future, Some("USD"), None, "1", "1"),
            Err(InstrumentSpecError::MissingExpiry)
        );
        assert_eq!(
            build(InstrumentKind::Spot, None, Some("2026-12-25"), "1", "1"),
            Err(InstrumentSpecError::UnexpectedExpiry)
        );
        assert_eq!(
            build(InstrumentKind::Equity, None, None, "0", "1"),
            Err(InstrumentSpecError::NonPositiveIncrement)
        );
        assert_eq!(
            build(InstrumentKind::Equity, None, None, "1", "-1"),
            Err(InstrumentSpecError::NonPositiveIncrement)
        );
    }

    #[test]
    fn registry_rejects_id_reuse_and_accepts_identical_specs_idempotently() {
        let original = spec(
            "BTC-USDT.BINANCE",
            InstrumentKind::Spot,
            None,
            None,
            "0.01",
            "0.001",
        );
        let mut specs = InstrumentSpecs::default();
        assert_eq!(specs.insert(original.clone()), Ok(true));
        assert_eq!(specs.insert(original), Ok(false));
        let conflicting = spec(
            "BTC-USDT.BINANCE",
            InstrumentKind::Spot,
            None,
            None,
            "0.1",
            "0.001",
        );
        assert_eq!(
            specs.insert(conflicting),
            Err(InstrumentSpecError::IdReused)
        );
    }

    #[test]
    fn maps_supported_legacy_a_share_ids() {
        for (legacy, canonical) in [
            ("SH:600000", "600000.XSHG"),
            ("SZ:000001", "000001.XSHE"),
            ("BJ:430047", "430047.BJSE"),
            ("sh510300", "510300.XSHG"),
            ("sz159915", "159915.XSHE"),
            ("bj920000", "920000.BJSE"),
        ] {
            assert_eq!(
                instrument_id_from_legacy(legacy).unwrap().to_string(),
                canonical
            );
        }
        for invalid in [
            "XX:600000",
            "SH:12345",
            "SH:1234567",
            "SH:12A456",
            "SH600000",
            "aébcde1",
        ] {
            assert!(
                instrument_id_from_legacy(invalid).is_err(),
                "{invalid} should fail"
            );
        }
    }
}
