use core::{fmt, str::FromStr};

use chrono::{DateTime, FixedOffset, NaiveDate, TimeZone};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{InstrumentId, Notional, Price, Quantity, VenueId};

/// Nanoseconds since the Unix epoch, with the represented instant normalized to UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TimestampNs(i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimestampParseError {
    InvalidRfc3339,
    OutOfRange,
}

impl TimestampNs {
    /// Parses RFC 3339 only. An explicit `Z` or numeric UTC offset is required.
    pub fn parse(value: &str) -> Result<Self, TimestampParseError> {
        let parsed =
            DateTime::parse_from_rfc3339(value).map_err(|_| TimestampParseError::InvalidRfc3339)?;
        parsed
            .timestamp_nanos_opt()
            .map(Self)
            .ok_or(TimestampParseError::OutOfRange)
    }

    pub const fn from_unix_nanos(value: i64) -> Self {
        Self(value)
    }

    pub const fn as_unix_nanos(self) -> i64 {
        self.0
    }

    fn from_datetime(value: DateTime<FixedOffset>) -> Result<Self, TimestampParseError> {
        value
            .timestamp_nanos_opt()
            .map(Self)
            .ok_or(TimestampParseError::OutOfRange)
    }
}

impl FromStr for TimestampNs {
    type Err = TimestampParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl fmt::Display for TimestampNs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let seconds = self.0.div_euclid(1_000_000_000);
        let nanos = self.0.rem_euclid(1_000_000_000) as u32;
        let utc = DateTime::from_timestamp(seconds, nanos).ok_or(fmt::Error)?;
        write!(f, "{}", utc.format("%Y-%m-%dT%H:%M:%S"))?;
        if nanos != 0 {
            let mut fractional = format!("{nanos:09}");
            while fractional.ends_with('0') {
                fractional.pop();
            }
            write!(f, ".{fractional}")?;
        }
        f.write_str("Z")
    }
}

impl fmt::Display for TimestampParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidRfc3339 => "timestamp must be RFC 3339 with an explicit UTC offset",
            Self::OutOfRange => "timestamp is outside the representable UTC nanosecond range",
        })
    }
}

impl std::error::Error for TimestampParseError {}

impl Serialize for TimestampNs {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for TimestampNs {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BarSpecInterval {
    Minutes(u32),
    Hours(u32),
    Days(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum BarAnchor {
    Session,
    FixedOffset { seconds_east: i32 },
}

/// A bar interval and its session or fixed-offset calendar anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BarSpec {
    interval: BarSpecInterval,
    anchor: BarAnchor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarSpecError {
    InvalidSyntax,
    ZeroInterval,
    IntervalOutOfRange,
    InvalidOffset,
    SessionDateMismatch,
    SessionRequired,
    UnsupportedBounds,
    TimestampOutOfRange,
}

impl BarSpec {
    pub const fn interval(&self) -> BarSpecInterval {
        self.interval
    }

    /// Computes the supported one-day interval for a trading session or fixed UTC offset.
    pub fn bounds(
        &self,
        session_date: NaiveDate,
        session: Option<&Session>,
    ) -> Result<(TimestampNs, TimestampNs), BarSpecError> {
        if self.interval != BarSpecInterval::Days(1) {
            return Err(BarSpecError::UnsupportedBounds);
        }
        match self.anchor {
            BarAnchor::Session => {
                let session = session.ok_or(BarSpecError::SessionRequired)?;
                if session.session_date != session_date {
                    return Err(BarSpecError::SessionDateMismatch);
                }
                Ok((session.ts_open, session.ts_close))
            }
            BarAnchor::FixedOffset { seconds_east } => {
                let offset =
                    FixedOffset::east_opt(seconds_east).ok_or(BarSpecError::InvalidOffset)?;
                let start = session_date
                    .and_hms_opt(0, 0, 0)
                    .ok_or(BarSpecError::TimestampOutOfRange)?;
                let next_date = session_date
                    .succ_opt()
                    .ok_or(BarSpecError::TimestampOutOfRange)?;
                let end = next_date
                    .and_hms_opt(0, 0, 0)
                    .ok_or(BarSpecError::TimestampOutOfRange)?;
                let start = offset
                    .from_local_datetime(&start)
                    .single()
                    .ok_or(BarSpecError::InvalidOffset)?;
                let end = offset
                    .from_local_datetime(&end)
                    .single()
                    .ok_or(BarSpecError::InvalidOffset)?;
                Ok((
                    TimestampNs::from_datetime(start)
                        .map_err(|_| BarSpecError::TimestampOutOfRange)?,
                    TimestampNs::from_datetime(end)
                        .map_err(|_| BarSpecError::TimestampOutOfRange)?,
                ))
            }
        }
    }
}

impl FromStr for BarSpec {
    type Err = BarSpecError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (interval, anchor) = value.split_once('@').ok_or(BarSpecError::InvalidSyntax)?;
        if anchor.contains('@') || interval.len() < 2 {
            return Err(BarSpecError::InvalidSyntax);
        }
        let unit_index = interval.len() - 1;
        let (count_text, unit) = interval.split_at(unit_index);
        if count_text.is_empty() || !count_text.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(BarSpecError::InvalidSyntax);
        }
        if count_text.len() > 1 && count_text.starts_with('0') {
            return Err(BarSpecError::InvalidSyntax);
        }
        let count = count_text
            .parse::<u32>()
            .map_err(|_| BarSpecError::IntervalOutOfRange)?;
        if count == 0 {
            return Err(BarSpecError::ZeroInterval);
        }
        let interval = match unit {
            "m" => BarSpecInterval::Minutes(count),
            "h" => BarSpecInterval::Hours(count),
            "d" => BarSpecInterval::Days(count),
            _ => return Err(BarSpecError::InvalidSyntax),
        };
        let anchor = if anchor == "session" {
            BarAnchor::Session
        } else {
            BarAnchor::FixedOffset {
                seconds_east: parse_offset(anchor)?,
            }
        };
        Ok(Self { interval, anchor })
    }
}

fn parse_offset(value: &str) -> Result<i32, BarSpecError> {
    let bytes = value.as_bytes();
    if bytes.len() != 6 || !matches!(bytes[0], b'+' | b'-') || bytes[3] != b':' {
        return Err(BarSpecError::InvalidOffset);
    }
    if !bytes[1..3]
        .iter()
        .chain(&bytes[4..6])
        .all(u8::is_ascii_digit)
    {
        return Err(BarSpecError::InvalidOffset);
    }
    let hours = i32::from(
        value[1..3]
            .parse::<u8>()
            .map_err(|_| BarSpecError::InvalidOffset)?,
    );
    let minutes = i32::from(
        value[4..6]
            .parse::<u8>()
            .map_err(|_| BarSpecError::InvalidOffset)?,
    );
    if hours > 23 || minutes > 59 {
        return Err(BarSpecError::InvalidOffset);
    }
    let seconds = (hours * 60 + minutes) * 60;
    Ok(if bytes[0] == b'-' { -seconds } else { seconds })
}

impl fmt::Display for BarSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.interval {
            BarSpecInterval::Minutes(count) => write!(f, "{count}m")?,
            BarSpecInterval::Hours(count) => write!(f, "{count}h")?,
            BarSpecInterval::Days(count) => write!(f, "{count}d")?,
        }
        match self.anchor {
            BarAnchor::Session => f.write_str("@session"),
            BarAnchor::FixedOffset { seconds_east } => {
                let sign = if seconds_east < 0 { '-' } else { '+' };
                let minutes = seconds_east.unsigned_abs() / 60;
                write!(f, "@{sign}{:02}:{:02}", minutes / 60, minutes % 60)
            }
        }
    }
}

impl Serialize for BarSpec {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for BarSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for BarSpecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidSyntax => {
                "invalid bar spec; expected <positive integer><m|h|d>@<session|±HH:MM>"
            }
            Self::ZeroInterval => "bar interval must be positive",
            Self::IntervalOutOfRange => "bar interval is outside the u32 range",
            Self::InvalidOffset => "invalid fixed UTC offset; expected ±HH:MM",
            Self::SessionDateMismatch => "bar session date does not match the supplied session",
            Self::SessionRequired => "session-anchored bar bounds require a session",
            Self::UnsupportedBounds => "bounds are currently defined only for one-day bars",
            Self::TimestampOutOfRange => "bar bounds exceed the representable timestamp range",
        })
    }
}

impl std::error::Error for BarSpecError {}

/// One exchange session. Its bounds are a half-open interval `[ts_open, ts_close)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    venue_id: VenueId,
    session_date: NaiveDate,
    ts_open: TimestampNs,
    ts_close: TimestampNs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionError {
    InvalidBounds,
}

impl Session {
    pub fn new(
        venue_id: VenueId,
        session_date: NaiveDate,
        ts_open: TimestampNs,
        ts_close: TimestampNs,
    ) -> Result<Self, SessionError> {
        if ts_open >= ts_close {
            return Err(SessionError::InvalidBounds);
        }
        Ok(Self {
            venue_id,
            session_date,
            ts_open,
            ts_close,
        })
    }

    pub fn venue_id(&self) -> &VenueId {
        &self.venue_id
    }
    pub const fn session_date(&self) -> NaiveDate {
        self.session_date
    }
    pub const fn ts_open(&self) -> TimestampNs {
        self.ts_open
    }
    pub const fn ts_close(&self) -> TimestampNs {
        self.ts_close
    }
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("session ts_open must be less than ts_close")
    }
}

impl std::error::Error for SessionError {}

/// A validated normalized bar. `available_at` remains optional: unknown is not inferred.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bar {
    instrument_id: InstrumentId,
    bar_spec: BarSpec,
    session_date: NaiveDate,
    ts_open: TimestampNs,
    ts_close: TimestampNs,
    open: Price,
    high: Price,
    low: Price,
    close: Price,
    volume: Quantity,
    amount: Option<Notional>,
    available_at: Option<TimestampNs>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarValidationError {
    VenueMismatch,
    SessionDateMismatch,
    SessionRequired,
    InvalidBounds,
    NegativeOpen,
    NegativeHigh,
    NegativeLow,
    NegativeClose,
    NegativeVolume,
    LowAboveHigh,
    LowAboveOpenOrClose,
    HighBelowOpenOrClose,
    SpecBoundsMismatch,
    UnsupportedBarBounds,
}

impl Bar {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        instrument_id: InstrumentId,
        bar_spec: BarSpec,
        session: Option<&Session>,
        session_date: NaiveDate,
        ts_open: TimestampNs,
        ts_close: TimestampNs,
        open: Price,
        high: Price,
        low: Price,
        close: Price,
        volume: Quantity,
        amount: Option<Notional>,
        available_at: Option<TimestampNs>,
    ) -> Result<Self, BarValidationError> {
        if let Some(session) = session {
            if instrument_id.venue() != session.venue_id() {
                return Err(BarValidationError::VenueMismatch);
            }
            if session_date != session.session_date() {
                return Err(BarValidationError::SessionDateMismatch);
            }
        } else if matches!(bar_spec.anchor, BarAnchor::Session) {
            return Err(BarValidationError::SessionRequired);
        }
        if ts_open >= ts_close {
            return Err(BarValidationError::InvalidBounds);
        }
        if open < Price::ZERO {
            return Err(BarValidationError::NegativeOpen);
        }
        if high < Price::ZERO {
            return Err(BarValidationError::NegativeHigh);
        }
        if low < Price::ZERO {
            return Err(BarValidationError::NegativeLow);
        }
        if close < Price::ZERO {
            return Err(BarValidationError::NegativeClose);
        }
        if volume < Quantity::ZERO {
            return Err(BarValidationError::NegativeVolume);
        }
        if low > high {
            return Err(BarValidationError::LowAboveHigh);
        }
        if low > open || low > close {
            return Err(BarValidationError::LowAboveOpenOrClose);
        }
        if high < open || high < close {
            return Err(BarValidationError::HighBelowOpenOrClose);
        }
        let (expected_open, expected_close) = bar_spec
            .bounds(session_date, session)
            .map_err(|_| BarValidationError::UnsupportedBarBounds)?;
        if ts_open != expected_open || ts_close != expected_close {
            return Err(BarValidationError::SpecBoundsMismatch);
        }
        Ok(Self {
            instrument_id,
            bar_spec,
            session_date,
            ts_open,
            ts_close,
            open,
            high,
            low,
            close,
            volume,
            amount,
            available_at,
        })
    }

    pub fn instrument_id(&self) -> &InstrumentId {
        &self.instrument_id
    }
    pub const fn bar_spec(&self) -> BarSpec {
        self.bar_spec
    }
    pub const fn session_date(&self) -> NaiveDate {
        self.session_date
    }
    pub const fn ts_open(&self) -> TimestampNs {
        self.ts_open
    }
    pub const fn ts_close(&self) -> TimestampNs {
        self.ts_close
    }
    pub const fn open(&self) -> Price {
        self.open
    }
    pub const fn high(&self) -> Price {
        self.high
    }
    pub const fn low(&self) -> Price {
        self.low
    }
    pub const fn close(&self) -> Price {
        self.close
    }
    pub const fn volume(&self) -> Quantity {
        self.volume
    }
    pub const fn amount(&self) -> Option<Notional> {
        self.amount
    }
    pub const fn available_at(&self) -> Option<TimestampNs> {
        self.available_at
    }
}

impl fmt::Display for BarValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::VenueMismatch => "instrument venue does not match session venue",
            Self::SessionDateMismatch => "bar session date does not match session date",
            Self::SessionRequired => "session-anchored bar requires a session",
            Self::InvalidBounds => "bar ts_open must be less than ts_close",
            Self::NegativeOpen => "bar open must be non-negative",
            Self::NegativeHigh => "bar high must be non-negative",
            Self::NegativeLow => "bar low must be non-negative",
            Self::NegativeClose => "bar close must be non-negative",
            Self::NegativeVolume => "bar volume must be non-negative",
            Self::LowAboveHigh => "bar low must not exceed high",
            Self::LowAboveOpenOrClose => "bar low must not exceed open or close",
            Self::HighBelowOpenOrClose => "bar high must not be below open or close",
            Self::SpecBoundsMismatch => "bar bounds do not match its bar spec and session",
            Self::UnsupportedBarBounds => {
                "bar spec cannot be validated with current interval rules"
            }
        })
    }
}

impl std::error::Error for BarValidationError {}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use crate::{InstrumentId, Price, Quantity, VenueId};

    use super::{
        Bar, BarSpec, BarSpecError, BarValidationError, Session, SessionError, TimestampNs,
        TimestampParseError,
    };

    fn date() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 1, 5).unwrap()
    }

    #[test]
    fn parses_explicit_offset_timestamps_to_manually_checked_utc_instants() {
        let open = TimestampNs::parse("2026-01-05T09:30:00+08:00").unwrap();
        let close = TimestampNs::parse("2026-01-05T15:00:00+08:00").unwrap();
        assert_eq!(open.to_string(), "2026-01-05T01:30:00Z");
        assert_eq!(close.to_string(), "2026-01-05T07:00:00Z");
        assert_eq!(open.as_unix_nanos(), 1_767_576_600_000_000_000);
        assert_eq!(close.as_unix_nanos(), 1_767_596_400_000_000_000);
    }

    #[test]
    fn rejects_timestamps_without_explicit_offset_or_outside_rfc3339() {
        for value in ["2026-01-05T09:30:00", "2026-01-05 09:30:00 CST"] {
            assert_eq!(
                TimestampNs::parse(value),
                Err(TimestampParseError::InvalidRfc3339)
            );
        }
    }

    #[test]
    fn bar_spec_grammar_round_trips_and_rejects_invalid_values() {
        for text in [
            "1m@session",
            "15m@-05:30",
            "2h@+00:00",
            "1d@session",
            "1d@+08:00",
            "10d@-12:45",
        ] {
            assert_eq!(text.parse::<BarSpec>().unwrap().to_string(), text);
        }
        for text in [
            "0d@session",
            "01d@session",
            "1w@session",
            "1d@UTC",
            "1d@+24:00",
            "1d@+08:60",
            "-1d@session",
            "1d@+0800",
        ] {
            assert!(text.parse::<BarSpec>().is_err(), "{text}");
        }
        assert_eq!(
            "0d@session".parse::<BarSpec>(),
            Err(BarSpecError::ZeroInterval)
        );
    }

    #[test]
    fn utc_offset_daily_bounds_match_hand_calculation() {
        let spec: BarSpec = "1d@+08:00".parse().unwrap();
        let (start, end) = spec.bounds(date(), None).unwrap();
        assert_eq!(start.to_string(), "2026-01-04T16:00:00Z");
        assert_eq!(end.to_string(), "2026-01-05T16:00:00Z");
    }

    #[test]
    fn session_daily_bounds_are_the_session_half_open_interval() {
        let session = Session::new(
            VenueId::new("XSHG").unwrap(),
            date(),
            TimestampNs::parse("2026-01-05T01:30:00Z").unwrap(),
            TimestampNs::parse("2026-01-05T07:00:00Z").unwrap(),
        )
        .unwrap();
        let spec: BarSpec = "1d@session".parse().unwrap();
        assert_eq!(
            spec.bounds(date(), Some(&session)).unwrap(),
            (session.ts_open(), session.ts_close())
        );
        assert_eq!(
            spec.bounds(date().succ_opt().unwrap(), Some(&session)),
            Err(BarSpecError::SessionDateMismatch)
        );
        assert_eq!(
            spec.bounds(date(), None),
            Err(BarSpecError::SessionRequired)
        );
        assert_eq!(
            Session::new(
                VenueId::new("XSHG").unwrap(),
                date(),
                session.ts_close(),
                session.ts_open()
            ),
            Err(SessionError::InvalidBounds)
        );
    }

    fn sample_session() -> Session {
        Session::new(
            VenueId::new("XSHG").unwrap(),
            date(),
            TimestampNs::parse("2026-01-05T01:30:00Z").unwrap(),
            TimestampNs::parse("2026-01-05T07:00:00Z").unwrap(),
        )
        .unwrap()
    }

    fn sample_bar(
        session: &Session,
        open: &str,
        high: &str,
        low: &str,
        close: &str,
        volume: &str,
        available_at: Option<TimestampNs>,
    ) -> Result<Bar, BarValidationError> {
        Bar::new(
            "510300.XSHG".parse::<InstrumentId>().unwrap(),
            "1d@session".parse().unwrap(),
            Some(session),
            date(),
            session.ts_open(),
            session.ts_close(),
            Price::parse(open).unwrap(),
            Price::parse(high).unwrap(),
            Price::parse(low).unwrap(),
            Price::parse(close).unwrap(),
            Quantity::parse(volume).unwrap(),
            None,
            available_at,
        )
    }

    #[test]
    fn bar_validates_session_bounds_ohlc_prices_volume_and_keeps_unknown_availability() {
        let session = sample_session();
        assert_eq!(
            sample_bar(&session, "10", "12", "9", "11", "100", None)
                .unwrap()
                .available_at(),
            None
        );
        assert_eq!(
            sample_bar(&session, "-1", "12", "0", "11", "100", None),
            Err(BarValidationError::NegativeOpen)
        );
        assert_eq!(
            sample_bar(&session, "10", "-1", "0", "11", "100", None),
            Err(BarValidationError::NegativeHigh)
        );
        assert_eq!(
            sample_bar(&session, "10", "12", "-1", "11", "100", None),
            Err(BarValidationError::NegativeLow)
        );
        assert_eq!(
            sample_bar(&session, "10", "12", "9", "-1", "100", None),
            Err(BarValidationError::NegativeClose)
        );
        assert_eq!(
            sample_bar(&session, "10", "12", "9", "11", "-1", None),
            Err(BarValidationError::NegativeVolume)
        );
        assert_eq!(
            sample_bar(&session, "10", "9", "8", "11", "1", None),
            Err(BarValidationError::HighBelowOpenOrClose)
        );
        assert_eq!(
            sample_bar(&session, "10", "12", "11", "10", "1", None),
            Err(BarValidationError::LowAboveOpenOrClose)
        );
        assert_eq!(
            sample_bar(&session, "10", "12", "13", "11", "1", None),
            Err(BarValidationError::LowAboveHigh)
        );
    }

    #[test]
    fn bar_rejects_bounds_that_disagree_with_daily_spec_and_session_identity() {
        let session = sample_session();
        assert_eq!(
            Bar::new(
                "510300.XSHG".parse().unwrap(),
                "1d@+08:00".parse().unwrap(),
                Some(&session),
                date(),
                session.ts_open(),
                session.ts_close(),
                Price::parse("10").unwrap(),
                Price::parse("12").unwrap(),
                Price::parse("9").unwrap(),
                Price::parse("11").unwrap(),
                Quantity::parse("1").unwrap(),
                None,
                None,
            ),
            Err(BarValidationError::SpecBoundsMismatch)
        );
        assert_eq!(
            Bar::new(
                "510300.XSHG".parse().unwrap(),
                "1d@session".parse().unwrap(),
                Some(&session),
                date(),
                session.ts_close(),
                session.ts_open(),
                Price::parse("10").unwrap(),
                Price::parse("12").unwrap(),
                Price::parse("9").unwrap(),
                Price::parse("11").unwrap(),
                Quantity::parse("1").unwrap(),
                None,
                None,
            ),
            Err(BarValidationError::InvalidBounds)
        );
        assert_eq!(
            Bar::new(
                "510300.XSHE".parse().unwrap(),
                "1d@session".parse().unwrap(),
                Some(&session),
                date(),
                session.ts_open(),
                session.ts_close(),
                Price::parse("10").unwrap(),
                Price::parse("12").unwrap(),
                Price::parse("9").unwrap(),
                Price::parse("11").unwrap(),
                Quantity::parse("1").unwrap(),
                None,
                None,
            ),
            Err(BarValidationError::VenueMismatch)
        );
        assert_eq!(
            Bar::new(
                "510300.XSHG".parse().unwrap(),
                "1d@session".parse().unwrap(),
                Some(&session),
                date().succ_opt().unwrap(),
                session.ts_open(),
                session.ts_close(),
                Price::parse("10").unwrap(),
                Price::parse("12").unwrap(),
                Price::parse("9").unwrap(),
                Price::parse("11").unwrap(),
                Quantity::parse("1").unwrap(),
                None,
                None,
            ),
            Err(BarValidationError::SessionDateMismatch)
        );
        let (ts_open, ts_close) = "1d@+08:00"
            .parse::<BarSpec>()
            .unwrap()
            .bounds(date(), None)
            .unwrap();
        assert!(
            Bar::new(
                "BTC-USDT.BINANCE".parse().unwrap(),
                "1d@+08:00".parse().unwrap(),
                None,
                date(),
                ts_open,
                ts_close,
                Price::parse("10").unwrap(),
                Price::parse("12").unwrap(),
                Price::parse("9").unwrap(),
                Price::parse("11").unwrap(),
                Quantity::parse("1").unwrap(),
                None,
                None,
            )
            .is_ok()
        );
        assert_eq!(
            Bar::new(
                "510300.XSHG".parse().unwrap(),
                "1d@session".parse().unwrap(),
                None,
                date(),
                session.ts_open(),
                session.ts_close(),
                Price::parse("10").unwrap(),
                Price::parse("12").unwrap(),
                Price::parse("9").unwrap(),
                Price::parse("11").unwrap(),
                Quantity::parse("1").unwrap(),
                None,
                None,
            ),
            Err(BarValidationError::SessionRequired)
        );
    }
}
