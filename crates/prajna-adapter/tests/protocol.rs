use prajna_adapter::protocol::{Response, decode_line, encode_line};
use prajna_domain::{Notional, Price, Quantity};

#[test]
fn scale_18_fill_round_trips_without_float_numbers() {
    let line = concat!(
        r#"{"type":"fill","order_id":"1","portfolio_id":"2","instrument_id":"A.SIM","side":"buy","#,
        r#""quantity":"0.000000000000000001","price":"1.123456789012345678","#,
        r#""fee":"99999999999999999999.999999999999999999","executed_at":"1767756600000000000"}"#,
        "\n"
    );
    let response: Response = decode_line(line).unwrap();
    let Response::Fill(fill) = &response else {
        panic!("expected Fill")
    };
    assert_eq!(
        fill.quantity,
        "0.000000000000000001".parse::<Quantity>().unwrap()
    );
    assert_eq!(fill.price, "1.123456789012345678".parse::<Price>().unwrap());
    assert_eq!(fill.fee, Notional::MAX);
    assert_eq!(
        decode_line::<Response>(&encode_line(&response).unwrap()).unwrap(),
        response
    );
    for (string, number) in [
        ("\"0.000000000000000001\"", "0.000000000000000001"),
        ("\"1.123456789012345678\"", "1.123456789012345678"),
        ("\"99999999999999999999.999999999999999999\"", "1.0"),
        ("\"1\"", "1"),
    ] {
        assert!(decode_line::<Response>(&line.replace(string, number)).is_err());
    }
    assert!(decode_line::<Response>(&line.replace("1.123456789012345678", "1e0")).is_err());
    assert!(
        decode_line::<Response>(&line.replace("1.123456789012345678", "1.1234567890123456789"))
            .is_err()
    );
    assert!(decode_line::<Response>(&format!("{line}{line}")).is_err());
}
