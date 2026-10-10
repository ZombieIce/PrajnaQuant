use prajna_domain::{FixedPointError, Notional, Price, Quantity};

macro_rules! rounding_and_range_tests {
    ($module:ident, $decimal:ident) => {
        mod $module {
            use super::*;

            #[test]
            fn half_even_rounding_is_symmetric_at_and_around_ties() {
                for (value, expected) in [
                    ($decimal::MAX, "50000000000000000000"),
                    ($decimal::MIN, "-50000000000000000000"),
                    (
                        $decimal::from_mantissa($decimal::MAX.mantissa() - 2).unwrap(),
                        "49999999999999999999.999999999999999998",
                    ),
                ] {
                    let expected = $decimal::parse(expected).unwrap();
                    assert_eq!(
                        value.checked_mul_decimal($decimal::parse("0.5").unwrap()),
                        Ok(expected)
                    );
                    assert_eq!(
                        value.checked_div_decimal($decimal::parse("2").unwrap()),
                        Ok(expected)
                    );
                }
                for (mantissa, expected) in [
                    (0, 0),
                    (1, 0),
                    (2, 1),
                    (3, 2),
                    (5, 2),
                    (7, 4),
                    (-1, 0),
                    (-2, -1),
                    (-3, -2),
                    (-5, -2),
                    (-7, -4),
                ] {
                    let value = $decimal::from_mantissa(mantissa).unwrap();
                    let expected = $decimal::from_mantissa(expected).unwrap();
                    assert_eq!(
                        value.checked_mul_decimal($decimal::parse("0.5").unwrap()),
                        Ok(expected)
                    );
                    assert_eq!(
                        value.checked_div_decimal($decimal::parse("2").unwrap()),
                        Ok(expected)
                    );
                    assert_eq!(
                        value
                            .checked_mul_decimal($decimal::parse("-0.5").unwrap())
                            .unwrap()
                            .mantissa(),
                        -expected.mantissa()
                    );
                    assert_eq!(
                        value
                            .checked_div_decimal($decimal::parse("-2").unwrap())
                            .unwrap()
                            .mantissa(),
                        -expected.mantissa()
                    );
                }
                let unit = $decimal::from_mantissa(1).unwrap();
                for (factor, expected) in [
                    ("0.499999999999999999", 0),
                    ("0.500000000000000001", 1),
                    ("-0.499999999999999999", 0),
                    ("-0.500000000000000001", -1),
                ] {
                    assert_eq!(
                        unit.checked_mul_decimal($decimal::parse(factor).unwrap())
                            .unwrap()
                            .mantissa(),
                        expected
                    );
                }
                for (divisor, expected) in [
                    ("1.999999999999999999", 1),
                    ("2.000000000000000001", 0),
                    ("-1.999999999999999999", -1),
                    ("-2.000000000000000001", 0),
                ] {
                    assert_eq!(
                        unit.checked_div_decimal($decimal::parse(divisor).unwrap())
                            .unwrap()
                            .mantissa(),
                        expected
                    );
                }
            }

            #[test]
            fn final_range_overflow_and_division_by_zero_are_explicit_errors() {
                // Exact product is MAX + 0.99740666130458787 mantissa units.
                let left =
                    $decimal::from_mantissa(74_812_236_255_655_383_158_461_577_910_691_736_805)
                        .unwrap();
                let right = $decimal::from_mantissa(1_336_679_733_222_659_334).unwrap();
                for sign in [-1, 1] {
                    assert_eq!(
                        left.checked_mul(sign).unwrap().checked_mul_decimal(right),
                        Err(FixedPointError::Overflow)
                    );
                }
                for extreme in [$decimal::MIN, $decimal::MAX] {
                    for multiplier in ["2", "-2", "1.000000000000000001"] {
                        assert_eq!(
                            extreme.checked_mul_decimal($decimal::parse(multiplier).unwrap()),
                            Err(FixedPointError::Overflow)
                        );
                    }
                    for divisor in ["0.5", "-0.5", "1e-18", "-1e-18", "0.999999999999999999"] {
                        assert_eq!(
                            extreme.checked_div_decimal($decimal::parse(divisor).unwrap()),
                            Err(FixedPointError::Overflow)
                        );
                    }
                    assert_eq!(
                        extreme.checked_mul_decimal($decimal::ZERO),
                        Ok($decimal::ZERO)
                    );
                }
                assert_eq!(
                    $decimal::ZERO.checked_div_decimal($decimal::ZERO),
                    Err(FixedPointError::DivisionByZero)
                );
            }
        }
    };
}

rounding_and_range_tests!(price, Price);
rounding_and_range_tests!(quantity, Quantity);
rounding_and_range_tests!(notional, Notional);

#[test]
fn wide_intermediates_match_independent_decimal_reference_values() {
    // Python decimal, precision 180, quantized to 1e-18 with ROUND_HALF_EVEN.
    for (left, right, product, quotient) in [
        (
            "12345678901234567890.123456789012345678",
            "0.123456789012345678",
            Some("1524157875323883663.923182566392318257"),
            None,
        ),
        (
            "99999999999999999999.999999999999999999",
            "0.999999999999999999",
            Some("99999999999999999899.999999999999999999"),
            None,
        ),
        (
            "99999999999999999999.999999999999999999",
            "1.000000000000000001",
            None,
            Some("99999999999999999900.000000000000000099"),
        ),
        (
            "18446744073709551615.999999999999999999",
            "3.141592653589793238",
            Some("57952155664616982730.540344367128772605"),
            Some("5871781006564002453.857189402619736351"),
        ),
    ] {
        let left = Notional::parse(left).unwrap();
        let right = Notional::parse(right).unwrap();
        let expected = |value: Option<&str>| {
            value.map_or(Err(FixedPointError::Overflow), |value| {
                Ok(Notional::parse(value).unwrap())
            })
        };
        for (left_sign, right_sign) in [(1, 1), (-1, 1), (1, -1), (-1, -1)] {
            let signed_left = left.checked_mul(left_sign).unwrap();
            let signed_right = right.checked_mul(right_sign).unwrap();
            let signed_expected = |value| {
                expected(value).map(|value| value.checked_mul(left_sign * right_sign).unwrap())
            };
            assert_eq!(
                signed_left.checked_mul_decimal(signed_right),
                signed_expected(product)
            );
            assert_eq!(
                signed_left.checked_div_decimal(signed_right),
                signed_expected(quotient)
            );
        }
    }
}

#[test]
fn decimal_multiplication_matches_hand_calculated_values_for_all_domain_types() {
    assert_eq!(
        Price::parse("12.5")
            .unwrap()
            .checked_mul_decimal(Price::parse("0.08").unwrap())
            .unwrap()
            .to_string(),
        "1"
    );
    assert_eq!(
        Quantity::parse("-1.25")
            .unwrap()
            .checked_mul_decimal(Quantity::parse("2.4").unwrap())
            .unwrap()
            .to_string(),
        "-3"
    );
    assert_eq!(
        Notional::parse("-1.25")
            .unwrap()
            .checked_mul_decimal(Notional::parse("-2.4").unwrap())
            .unwrap()
            .to_string(),
        "3"
    );
}

#[test]
fn decimal_division_matches_hand_calculated_values_and_rejects_zero() {
    for (left, right, expected) in [
        ("12.5", "0.08", "156.25"),
        ("-1", "3", "-0.333333333333333333"),
        ("1", "-6", "-0.166666666666666667"),
        ("-7.5", "-2.5", "3"),
        ("0", "-2", "0"),
    ] {
        assert_eq!(
            Notional::parse(left)
                .unwrap()
                .checked_div_decimal(Notional::parse(right).unwrap())
                .unwrap()
                .to_string(),
            expected
        );
    }

    for numerator in [Price::ZERO, Price::MIN, Price::MAX] {
        assert_eq!(
            numerator.checked_div_decimal(Price::ZERO),
            Err(FixedPointError::DivisionByZero)
        );
    }
}

#[test]
fn representable_results_survive_intermediate_integer_overflow() {
    let one = Quantity::parse("1").unwrap();
    let negative_one = Quantity::parse("-1").unwrap();
    for value in [Quantity::MAX, Quantity::MIN] {
        assert_eq!(value.checked_mul_decimal(one), Ok(value));
        assert_eq!(value.checked_div_decimal(one), Ok(value));
        assert_eq!(value.checked_div_decimal(value), Ok(one));
        assert_eq!(
            value.checked_mul_decimal(negative_one).unwrap().mantissa(),
            -value.mantissa()
        );
        assert_eq!(
            value.checked_div_decimal(negative_one).unwrap().mantissa(),
            -value.mantissa()
        );
    }
    assert_eq!(
        Quantity::MAX
            .checked_mul_decimal(Quantity::parse("1e-18").unwrap())
            .unwrap()
            .to_string(),
        "100"
    );
    assert_eq!(
        Quantity::parse("1e-18")
            .unwrap()
            .checked_div_decimal(Quantity::MAX),
        Ok(Quantity::ZERO)
    );
}
