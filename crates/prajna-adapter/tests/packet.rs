#[path = "../../prajna-research/tests/common/mod.rs"]
#[allow(dead_code)]
mod common;

use chrono::NaiveDate;
use prajna_adapter::RunPacket;
use prajna_data::ExecutionStatusRow;
use prajna_domain::{
    Bar, BarData, InstrumentId, InstrumentKind, InstrumentSpec, InstrumentSpecData, Session,
    TimestampNs, VenueId,
};

#[test]
fn run_packet_preserves_normalized_bars_status_and_instrument_precision() {
    let lake = tempfile::tempdir().unwrap();
    let dsv = common::publish_version(lake.path(), "3");
    let packet = RunPacket::from_dataset(lake.path(), &dsv).unwrap();
    let output = tempfile::tempdir().unwrap();
    let path = output.path().join("packet");
    packet.write(&path, &dsv).unwrap();
    assert_eq!(RunPacket::read(&path).unwrap(), packet);
    assert!(!packet.bars.is_empty());
    assert!(!packet.statuses.is_empty());
    assert!(packet.instruments.iter().all(|i| i.lot_size().is_some()));
}

#[test]
fn scale_18_parquet_prices_are_exact_and_incomplete_status_packets_fail() {
    let lake = tempfile::tempdir().unwrap();
    let mut fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../poc/poc0-benchmark/fixtures/dataset-v1.json"
    ))
    .unwrap();
    fixture["bar_defaults"]["open"] = serde_json::from_str("100.123456789012345678").unwrap();
    for instrument in fixture["instruments"].as_array_mut().unwrap() {
        instrument["price_precision"] = serde_json::json!(18);
    }
    let dsv = common::publish_fixture(lake.path(), &serde_json::to_vec(&fixture).unwrap(), "3");
    let mut packet = RunPacket::from_dataset(lake.path(), &dsv).unwrap();
    assert_eq!(packet.bars[0].open().to_string(), "100.123456789012345678");
    let output = tempfile::tempdir().unwrap();
    let path = output.path().join("packet");
    packet.write(&path, &dsv).unwrap();
    assert_eq!(RunPacket::read(&path).unwrap(), packet);
    assert!(packet.write(&path, &dsv).is_err());
    packet.statuses.pop();
    assert!(packet.validate().is_err());
}

fn instrument(symbol: &str, venue: &str) -> InstrumentSpec {
    InstrumentSpec::new(InstrumentSpecData {
        id: InstrumentId::new(symbol, VenueId::new(venue).unwrap()).unwrap(),
        kind: InstrumentKind::Etf,
        native_symbol: symbol.into(),
        market_segment: None,
        base_currency: None,
        quote_currency: Some("CNY".parse().unwrap()),
        settle_currency: None,
        is_inverse: false,
        multiplier: None,
        expiry: None,
        price_increment: "0.001".parse().unwrap(),
        size_increment: "1".parse().unwrap(),
        lot_size: Some("100".parse().unwrap()),
        session_timezone: None,
    })
    .unwrap()
}

fn session(venue: &str, date: &str) -> Session {
    let date = NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap();
    Session::new(
        VenueId::new(venue).unwrap(),
        date,
        TimestampNs::parse(&format!("{date}T09:30:00+08:00")).unwrap(),
        TimestampNs::parse(&format!("{date}T15:00:00+08:00")).unwrap(),
    )
    .unwrap()
}

fn bar(instrument: &InstrumentSpec, session: &Session) -> Bar {
    Bar::new(
        BarData {
            instrument_id: instrument.id().clone(),
            bar_spec: "1d@session".parse().unwrap(),
            session_date: session.session_date(),
            ts_open: session.ts_open(),
            ts_close: session.ts_close(),
            open: "10".parse().unwrap(),
            high: "10.1".parse().unwrap(),
            low: "9.9".parse().unwrap(),
            close: "10".parse().unwrap(),
            volume: "100".parse().unwrap(),
            amount: None,
            available_at: None,
        },
        Some(session),
    )
    .unwrap()
}

fn status(instrument: &InstrumentSpec, session: &Session) -> ExecutionStatusRow {
    ExecutionStatusRow {
        instrument_id: instrument.id().clone(),
        session_date: session.session_date(),
        trade_status: "TRADABLE".into(),
        is_tradable: true,
        available_at: Some(session.ts_open()),
        source: "synthetic".into(),
    }
}

#[test]
fn multi_venue_packet_is_rejected_explicitly() {
    let xshg = instrument("510300", "XSHG");
    let xshe = instrument("159952", "XSHE");
    let xshg_session = session("XSHG", "2026-01-05");
    let xshe_session = session("XSHE", "2026-01-05");
    let packet = RunPacket {
        instruments: vec![xshg.clone(), xshe.clone()],
        sessions: vec![xshg_session.clone(), xshe_session.clone()],
        bars: vec![bar(&xshg, &xshg_session), bar(&xshe, &xshe_session)],
        statuses: vec![status(&xshg, &xshg_session), status(&xshe, &xshe_session)],
    };
    let error = packet.validate().unwrap_err();
    assert!(error.to_string().contains("requires one Venue"), "{error}");
}
