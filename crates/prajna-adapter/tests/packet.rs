#[path = "../../prajna-research/tests/common/mod.rs"]
#[allow(dead_code)]
mod common;

use prajna_adapter::RunPacket;

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
