//! Stable domain primitives shared by the platform's data and execution layers.

mod instrument;
mod time_bar;

pub use instrument::{
    CodeError, Currency, InstrumentId, InstrumentIdError, InstrumentKind, InstrumentSpec,
    InstrumentSpecData, InstrumentSpecError, InstrumentSpecs, LegacyInstrumentIdError, VenueId,
    instrument_id_from_legacy,
};
pub use time_bar::{
    Bar, BarData, BarSpec, BarSpecError, BarSpecInterval, BarValidationError, Session,
    SessionError, TimestampNs, TimestampParseError,
};

use core::{cmp::Ordering, fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The fixed number of fractional decimal places used by every domain decimal.
pub const DECIMAL_SCALE: u32 = 18;

const SCALE_FACTOR: i128 = 1_000_000_000_000_000_000;
const MAX_MANTISSA: i128 = 100_000_000_000_000_000_000_000_000_000_000_000_000;

/// An error returned when a fixed-scale decimal cannot be represented safely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixedPointError {
    InvalidLiteral,
    InexactScale,
    OutOfRange,
    Overflow,
    ZeroIncrement,
    DivisionByZero,
}

impl fmt::Display for FixedPointError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidLiteral => "expected a JSON decimal number literal",
            Self::InexactScale => "value cannot be represented exactly at scale 18",
            Self::OutOfRange => "decimal mantissa must have absolute value below 10^38",
            Self::Overflow => "fixed-point operation overflowed",
            Self::ZeroIncrement => "increment must not be zero",
            Self::DivisionByZero => "decimal divisor must not be zero",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for FixedPointError {}

macro_rules! fixed_decimal {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(i128);

        impl $name {
            /// Zero at the domain's fixed scale.
            pub const ZERO: Self = Self(0);
            /// The greatest positive representable decimal.
            pub const MAX: Self = Self(MAX_MANTISSA - 1);
            /// The least negative representable decimal.
            pub const MIN: Self = Self(-(MAX_MANTISSA - 1));

            /// Parses a JSON number literal without passing through binary floating point.
            pub fn parse(input: &str) -> Result<Self, FixedPointError> {
                parse_mantissa(input).map(Self)
            }

            /// Constructs a value from its scale-18 mantissa after validating its range.
            pub const fn from_mantissa(mantissa: i128) -> Result<Self, FixedPointError> {
                if mantissa <= -MAX_MANTISSA || mantissa >= MAX_MANTISSA {
                    Err(FixedPointError::OutOfRange)
                } else {
                    Ok(Self(mantissa))
                }
            }

            /// Returns the scale-18 integer representation used for Decimal128 storage.
            pub const fn mantissa(self) -> i128 {
                self.0
            }

            /// Explicitly converts the decimal to `f64` for an API that requires it.
            pub fn to_f64(self) -> f64 {
                self.0 as f64 / SCALE_FACTOR as f64
            }

            /// Multiplies by an integer, reporting an error instead of wrapping.
            pub fn checked_mul(self, multiplier: i128) -> Result<Self, FixedPointError> {
                let mantissa = self
                    .0
                    .checked_mul(multiplier)
                    .ok_or(FixedPointError::Overflow)?;
                Self::from_mantissa(mantissa)
            }

            /// Multiplies two decimals, rounding to scale 18 with ties to even.
            /// No currency-unit rounding or binary floating-point conversion is applied.
            /// Returns `Overflow` if the rounded result is outside the domain range.
            pub fn checked_mul_decimal(self, other: Self) -> Result<Self, FixedPointError> {
                product_ratio(
                    self.0.unsigned_abs(),
                    other.0.unsigned_abs(),
                    SCALE_FACTOR as u128,
                    (self.0 < 0) != (other.0 < 0),
                )
                .map(Self)
            }

            /// Divides two decimals, rounding to scale 18 with ties to even.
            /// Returns an error for a zero divisor; no currency-unit rounding is applied.
            /// Returns `Overflow` if the rounded result is outside the domain range.
            pub fn checked_div_decimal(self, other: Self) -> Result<Self, FixedPointError> {
                if other.0 == 0 {
                    return Err(FixedPointError::DivisionByZero);
                }
                product_ratio(
                    self.0.unsigned_abs(),
                    SCALE_FACTOR as u128,
                    other.0.unsigned_abs(),
                    (self.0 < 0) != (other.0 < 0),
                )
                .map(Self)
            }

            /// Compares two fixed-scale values, reporting an error if their difference
            /// cannot be represented by an `i128` intermediate.
            pub fn checked_cmp(self, other: &Self) -> Result<Ordering, FixedPointError> {
                let difference = self
                    .0
                    .checked_sub(other.0)
                    .ok_or(FixedPointError::Overflow)?;
                Ok(difference.cmp(&0))
            }

            /// Returns whether this value is an integral multiple of a non-zero increment.
            pub fn is_aligned_to(self, increment: Self) -> Result<bool, FixedPointError> {
                if increment.0 == 0 {
                    return Err(FixedPointError::ZeroIncrement);
                }
                Ok(self.0 % increment.0 == 0)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                format_mantissa(self.0, formatter)
            }
        }

        impl FromStr for $name {
            type Err = FixedPointError;

            fn from_str(input: &str) -> Result<Self, Self::Err> {
                Self::parse(input)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.to_string())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let input = <String as Deserialize>::deserialize(deserializer)?;
                Self::parse(&input).map_err(serde::de::Error::custom)
            }
        }
    };
}

fixed_decimal!(Price, "A price represented as an exact scale-18 decimal.");
fixed_decimal!(
    Quantity,
    "A quantity represented as an exact scale-18 decimal."
);
fixed_decimal!(
    Notional,
    "A trade notional represented as an exact scale-18 decimal."
);

/// Backwards-friendly spelling for a trade's notional amount.
pub type Amount = Notional;

fn product_ratio(
    left: u128,
    right: u128,
    divisor: u128,
    negative: bool,
) -> Result<i128, FixedPointError> {
    let (mut quotient, remainder) = if let Some(product) = left.checked_mul(right) {
        (product / divisor, product % divisor)
    } else {
        // Four 64-bit limb products retain the full 256-bit intermediate.
        let mask = u128::from(u64::MAX);
        let low_product = (left & mask) * (right & mask);
        let middle = (left >> 64) * (right & mask) + (low_product >> 64);
        let upper = middle >> 64;
        let middle = (left & mask) * (right >> 64) + (middle & mask);
        let high = (left >> 64) * (right >> 64) + upper + (middle >> 64);
        let low = (middle << 64) | (low_product & mask);
        if high >= divisor {
            return Err(FixedPointError::Overflow);
        }
        let mut quotient = 0_u128;
        let mut remainder = high;
        // Domain divisors are below 10^38 < 2^127, so doubling the remainder fits.
        for bit in (0..128).rev() {
            remainder = (remainder << 1) | ((low >> bit) & 1);
            if remainder >= divisor {
                remainder -= divisor;
                quotient |= 1_u128 << bit;
            }
        }
        (quotient, remainder)
    };
    if remainder > divisor - remainder || (remainder == divisor - remainder && quotient % 2 != 0) {
        quotient = quotient.checked_add(1).ok_or(FixedPointError::Overflow)?;
    }
    if quotient >= MAX_MANTISSA as u128 {
        return Err(FixedPointError::Overflow);
    }
    let mantissa = quotient as i128;
    Ok(if negative { -mantissa } else { mantissa })
}

fn parse_mantissa(input: &str) -> Result<i128, FixedPointError> {
    let bytes = input.as_bytes();
    let mut index = 0;
    let negative = matches!(bytes.first(), Some(b'-'));
    if negative {
        index += 1;
    }

    let integer_start = index;
    match bytes.get(index) {
        Some(b'0') => {
            index += 1;
            if matches!(bytes.get(index), Some(b'0'..=b'9')) {
                return Err(FixedPointError::InvalidLiteral);
            }
        }
        Some(b'1'..=b'9') => {
            index += 1;
            while matches!(bytes.get(index), Some(b'0'..=b'9')) {
                index += 1;
            }
        }
        _ => return Err(FixedPointError::InvalidLiteral),
    }
    let integer_end = index;

    let fraction_start = if bytes.get(index) == Some(&b'.') {
        index += 1;
        let start = index;
        while matches!(bytes.get(index), Some(b'0'..=b'9')) {
            index += 1;
        }
        if start == index {
            return Err(FixedPointError::InvalidLiteral);
        }
        start
    } else {
        index
    };
    let fraction_end = index;

    let mut exponent = 0_i64;
    let mut exponent_overflow = false;
    if matches!(bytes.get(index), Some(b'e' | b'E')) {
        index += 1;
        let exponent_negative = match bytes.get(index) {
            Some(b'+') => {
                index += 1;
                false
            }
            Some(b'-') => {
                index += 1;
                true
            }
            _ => false,
        };
        let exponent_start = index;
        while let Some(digit @ b'0'..=b'9') = bytes.get(index) {
            if !exponent_overflow {
                exponent = exponent
                    .checked_mul(10)
                    .and_then(|value| value.checked_add(i64::from(digit - b'0')))
                    .unwrap_or_else(|| {
                        exponent_overflow = true;
                        0
                    });
            }
            index += 1;
        }
        if exponent_start == index {
            return Err(FixedPointError::InvalidLiteral);
        }
        if exponent_negative && !exponent_overflow {
            exponent = exponent.checked_neg().ok_or(FixedPointError::OutOfRange)?;
        }
    }
    if index != bytes.len() {
        return Err(FixedPointError::InvalidLiteral);
    }

    let mut digits =
        String::with_capacity(integer_end - integer_start + fraction_end - fraction_start);
    digits.push_str(&input[integer_start..integer_end]);
    digits.push_str(&input[fraction_start..fraction_end]);
    let significant = digits.trim_start_matches('0');
    if significant.is_empty() {
        return Ok(0);
    }
    if exponent_overflow {
        return Err(FixedPointError::OutOfRange);
    }

    let fraction_len =
        i64::try_from(fraction_end - fraction_start).map_err(|_| FixedPointError::OutOfRange)?;
    let power = exponent
        .checked_sub(fraction_len)
        .and_then(|value| value.checked_add(i64::from(DECIMAL_SCALE)))
        .ok_or(FixedPointError::OutOfRange)?;
    let mantissa_digits = if power >= 0 {
        let zeroes = usize::try_from(power).map_err(|_| FixedPointError::OutOfRange)?;
        if significant
            .len()
            .checked_add(zeroes)
            .ok_or(FixedPointError::OutOfRange)?
            > 38
        {
            return Err(FixedPointError::OutOfRange);
        }
        let mut expanded = String::with_capacity(significant.len() + zeroes);
        expanded.push_str(significant);
        expanded.extend(std::iter::repeat_n('0', zeroes));
        expanded
    } else {
        let removed =
            usize::try_from(power.unsigned_abs()).map_err(|_| FixedPointError::OutOfRange)?;
        if removed >= significant.len() {
            return Err(FixedPointError::InexactScale);
        }
        let retained = significant.len() - removed;
        if !significant[retained..].bytes().all(|digit| digit == b'0') {
            return Err(FixedPointError::InexactScale);
        }
        significant[..retained].to_owned()
    };

    let absolute = mantissa_digits.bytes().try_fold(0_i128, |value, digit| {
        value
            .checked_mul(10)
            .and_then(|value| value.checked_add(i128::from(digit - b'0')))
            .ok_or(FixedPointError::OutOfRange)
    })?;
    if absolute >= MAX_MANTISSA {
        return Err(FixedPointError::OutOfRange);
    }
    if negative {
        Ok(-absolute)
    } else {
        Ok(absolute)
    }
}

fn format_mantissa(mantissa: i128, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    if mantissa == 0 {
        return formatter.write_str("0");
    }
    let negative = mantissa < 0;
    let absolute = if negative { -mantissa } else { mantissa };
    let whole = absolute / SCALE_FACTOR;
    let fractional = absolute % SCALE_FACTOR;
    if negative {
        formatter.write_str("-")?;
    }
    write!(formatter, "{whole}")?;
    if fractional != 0 {
        let mut fraction = format!("{fractional:018}");
        while fraction.ends_with('0') {
            fraction.pop();
        }
        write!(formatter, ".{fraction}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use core::cmp::Ordering;

    use super::{Notional, Price, Quantity};

    #[test]
    fn parses_json_number_literals_exactly_at_scale_18() {
        assert_eq!(Price::parse("100").unwrap().to_string(), "100");
        assert_eq!(Price::parse("100.0").unwrap().to_string(), "100");
        assert_eq!(Price::parse("-0.001").unwrap().to_string(), "-0.001");
        assert_eq!(Price::parse("1e-3").unwrap().to_string(), "0.001");
        assert_eq!(
            Price::parse("0.123456789012345678").unwrap().to_string(),
            "0.123456789012345678"
        );
        assert_eq!(
            Price::parse("99999999999999999999.999999999999999999")
                .unwrap()
                .to_string(),
            "99999999999999999999.999999999999999999"
        );
    }

    #[test]
    fn rejects_non_json_or_inexact_or_out_of_range_literals() {
        for literal in [
            "",
            "+1",
            "01",
            ".1",
            "1.",
            "NaN",
            "inf",
            "0.0000000000000000001",
            "100000000000000000000",
        ] {
            assert!(Price::parse(literal).is_err(), "{literal} should fail");
        }
    }

    #[test]
    fn canonical_text_round_trips_and_trims_fractional_trailing_zeroes() {
        for literal in ["-42", "0", "10.5000", "1e-18", "100e-20"] {
            let value = Quantity::parse(literal).unwrap();
            assert_eq!(Quantity::parse(&value.to_string()).unwrap(), value);
        }
        assert_eq!(Quantity::parse("10.5000").unwrap().to_string(), "10.5");
        assert_eq!(Quantity::parse("-0").unwrap().to_string(), "0");
    }

    #[test]
    fn checks_alignment_multiplication_and_comparison() {
        let price = Price::parse("10.05").unwrap();
        let tick = Price::parse("0.01").unwrap();
        assert!(price.is_aligned_to(tick).unwrap());
        assert!(!Price::parse("10.051").unwrap().is_aligned_to(tick).unwrap());
        assert!(price.is_aligned_to(Price::ZERO).is_err());

        assert_eq!(price.checked_mul(2).unwrap().to_string(), "20.1");
        assert_eq!(
            price.checked_cmp(&Price::parse("10.5").unwrap()),
            Ok(Ordering::Less)
        );
        assert!(Price::MIN.checked_cmp(&Price::MAX).is_err());
        assert!(Price::MAX.checked_mul(2).is_err());
    }

    #[test]
    fn has_only_an_explicit_f64_conversion() {
        let value = Notional::parse("123.25").unwrap();
        assert_eq!(value.to_f64(), 123.25);
    }

    #[test]
    fn serde_represents_decimals_as_strings() {
        let value = Price::parse("12.5").unwrap();
        let mut serializer = StringOnlySerializer::default();
        serde::Serialize::serialize(&value, &mut serializer).unwrap();
        assert_eq!(serializer.value.as_deref(), Some("12.5"));

        let decoded: Price =
            serde::Deserialize::deserialize(StringOnlyDeserializer("-0.25")).unwrap();
        assert_eq!(decoded.to_string(), "-0.25");
    }

    #[derive(Default)]
    struct StringOnlySerializer {
        value: Option<String>,
    }

    #[derive(Debug)]
    struct TestSerdeError;

    impl core::fmt::Display for TestSerdeError {
        fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            formatter.write_str("test serde error")
        }
    }

    impl std::error::Error for TestSerdeError {}

    impl serde::ser::Error for TestSerdeError {
        fn custom<T: core::fmt::Display>(_message: T) -> Self {
            Self
        }
    }

    impl serde::Serializer for &mut StringOnlySerializer {
        type Ok = ();
        type Error = TestSerdeError;
        type SerializeSeq = serde::ser::Impossible<(), TestSerdeError>;
        type SerializeTuple = serde::ser::Impossible<(), TestSerdeError>;
        type SerializeTupleStruct = serde::ser::Impossible<(), TestSerdeError>;
        type SerializeTupleVariant = serde::ser::Impossible<(), TestSerdeError>;
        type SerializeMap = serde::ser::Impossible<(), TestSerdeError>;
        type SerializeStruct = serde::ser::Impossible<(), TestSerdeError>;
        type SerializeStructVariant = serde::ser::Impossible<(), TestSerdeError>;

        fn serialize_str(self, value: &str) -> Result<Self::Ok, Self::Error> {
            self.value = Some(value.to_owned());
            Ok(())
        }

        fn serialize_bool(self, _: bool) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError)
        }
        fn serialize_i8(self, _: i8) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError)
        }
        fn serialize_i16(self, _: i16) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError)
        }
        fn serialize_i32(self, _: i32) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError)
        }
        fn serialize_i64(self, _: i64) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError)
        }
        fn serialize_i128(self, _: i128) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError)
        }
        fn serialize_u8(self, _: u8) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError)
        }
        fn serialize_u16(self, _: u16) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError)
        }
        fn serialize_u32(self, _: u32) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError)
        }
        fn serialize_u64(self, _: u64) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError)
        }
        fn serialize_u128(self, _: u128) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError)
        }
        fn serialize_f32(self, _: f32) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError)
        }
        fn serialize_f64(self, _: f64) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError)
        }
        fn serialize_char(self, _: char) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError)
        }
        fn serialize_bytes(self, _: &[u8]) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError)
        }
        fn serialize_none(self) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError)
        }
        fn serialize_some<T: ?Sized + serde::Serialize>(
            self,
            _: &T,
        ) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError)
        }
        fn serialize_unit(self) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError)
        }
        fn serialize_unit_struct(self, _: &'static str) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError)
        }
        fn serialize_unit_variant(
            self,
            _: &'static str,
            _: u32,
            _: &'static str,
        ) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError)
        }
        fn serialize_newtype_struct<T: ?Sized + serde::Serialize>(
            self,
            _: &'static str,
            _: &T,
        ) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError)
        }
        fn serialize_newtype_variant<T: ?Sized + serde::Serialize>(
            self,
            _: &'static str,
            _: u32,
            _: &'static str,
            _: &T,
        ) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError)
        }
        fn serialize_seq(self, _: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
            Err(TestSerdeError)
        }
        fn serialize_tuple(self, _: usize) -> Result<Self::SerializeTuple, Self::Error> {
            Err(TestSerdeError)
        }
        fn serialize_tuple_struct(
            self,
            _: &'static str,
            _: usize,
        ) -> Result<Self::SerializeTupleStruct, Self::Error> {
            Err(TestSerdeError)
        }
        fn serialize_tuple_variant(
            self,
            _: &'static str,
            _: u32,
            _: &'static str,
            _: usize,
        ) -> Result<Self::SerializeTupleVariant, Self::Error> {
            Err(TestSerdeError)
        }
        fn serialize_map(self, _: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
            Err(TestSerdeError)
        }
        fn serialize_struct(
            self,
            _: &'static str,
            _: usize,
        ) -> Result<Self::SerializeStruct, Self::Error> {
            Err(TestSerdeError)
        }
        fn serialize_struct_variant(
            self,
            _: &'static str,
            _: u32,
            _: &'static str,
            _: usize,
        ) -> Result<Self::SerializeStructVariant, Self::Error> {
            Err(TestSerdeError)
        }
    }

    struct StringOnlyDeserializer<'a>(&'a str);

    impl<'de> serde::Deserializer<'de> for StringOnlyDeserializer<'de> {
        type Error = serde::de::value::Error;

        fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: serde::de::Visitor<'de>,
        {
            visitor.visit_borrowed_str(self.0)
        }

        serde::forward_to_deserialize_any! {
            bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf
            option unit unit_struct newtype_struct seq tuple tuple_struct map struct enum identifier
            ignored_any
        }
    }
}
