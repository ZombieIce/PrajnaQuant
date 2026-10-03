mod common;

use std::fs;

use polars::prelude::{DataType, TimeUnit, TimeZone};
use prajna_domain::VenueId;
use prajna_research::{PanelError, load_panel};

#[test]
fn loads_v1_as_a_complete_venue_session_grid_with_missing_bars_preserved() {
    let lake = tempfile::tempdir().unwrap();
    let dsv = common::publish_v1(lake.path());
    let venue = VenueId::new("SYNTH").unwrap();

    let panel = load_panel(lake.path(), &dsv, &venue).unwrap();

    assert_eq!(panel.sessions.len(), 10);
    assert_eq!(panel.sessions[9].session_date.to_string(), "2026-01-16");
    assert_eq!(
        panel
            .instruments
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["A.SYNTH", "B.SYNTH", "C.SYNTH"]
    );
    assert_eq!(panel.grid.height(), 30);
    assert_eq!(
        panel
            .grid
            .get_column_names()
            .iter()
            .map(|name| name.as_str())
            .collect::<Vec<_>>(),
        [
            "instrument_id",
            "session_date",
            "session_index",
            "open",
            "close",
            "has_bar",
            "close_available_at"
        ]
    );

    let instrument_ids = panel.grid.column("instrument_id").unwrap().str().unwrap();
    let session_indices = panel.grid.column("session_index").unwrap().u32().unwrap();
    let open = panel.grid.column("open").unwrap().f64().unwrap();
    let close = panel.grid.column("close").unwrap().f64().unwrap();
    let has_bar = panel.grid.column("has_bar").unwrap().bool().unwrap();
    let available_at = panel.grid.column("close_available_at").unwrap();
    let missing = (0..panel.grid.height())
        .find(|&row| {
            instrument_ids.get(row) == Some("B.SYNTH") && session_indices.get(row) == Some(9)
        })
        .unwrap();

    assert_eq!(
        panel.grid.column("session_date").unwrap().dtype(),
        &DataType::Date
    );
    assert_eq!(
        panel.grid.column("session_index").unwrap().dtype(),
        &DataType::UInt32
    );
    assert_eq!(
        panel.grid.column("open").unwrap().dtype(),
        &DataType::Float64
    );
    assert_eq!(
        panel.grid.column("close").unwrap().dtype(),
        &DataType::Float64
    );
    assert_eq!(
        panel.grid.column("has_bar").unwrap().dtype(),
        &DataType::Boolean
    );
    match available_at.dtype() {
        DataType::Datetime(unit, timezone) => {
            assert_eq!(*unit, TimeUnit::Nanoseconds);
            assert!(TimeZone::eq_none_as_utc(
                timezone.as_ref(),
                Some(&TimeZone::UTC)
            ));
        }
        dtype => panic!("expected UTC nanosecond datetime, got {dtype}"),
    }
    assert_eq!(has_bar.get(missing), Some(false));
    assert_eq!(open.get(missing), None);
    assert_eq!(close.get(missing), None);
    assert_eq!(available_at.is_null().get(missing), Some(true));

    let first_bar = (0..panel.grid.height())
        .find(|&row| {
            instrument_ids.get(row) == Some("A.SYNTH") && session_indices.get(row) == Some(0)
        })
        .unwrap();
    assert_eq!(open.get(first_bar), Some(100.0));
    assert_eq!(close.get(first_bar), Some(100.0));
    assert_eq!(
        available_at.datetime().unwrap().physical().get(first_bar),
        Some(panel.sessions[0].ts_close.as_unix_nanos())
    );

    for instrument in ["A.SYNTH", "B.SYNTH", "C.SYNTH"] {
        let indices = (0..panel.grid.height())
            .filter(|&row| instrument_ids.get(row) == Some(instrument))
            .filter_map(|row| session_indices.get(row))
            .collect::<Vec<_>>();
        assert_eq!(indices, (0..10).collect::<Vec<_>>());
    }
}

#[test]
fn rejects_a_dsv_without_a_manifest() {
    let lake = tempfile::tempdir().unwrap();
    let venue = VenueId::new("SYNTH").unwrap();
    let dsv = format!("dsv:sha256:{}", "0".repeat(64));

    assert!(matches!(
        load_panel(lake.path(), &dsv, &venue),
        Err(PanelError::Manifest(_))
    ));
}

#[test]
fn rejects_a_manifest_whose_core_no_longer_matches_the_dsv() {
    let lake = tempfile::tempdir().unwrap();
    let dsv = common::publish_v1(lake.path());
    let dsv_hex = dsv.strip_prefix("dsv:sha256:").unwrap();
    let manifest_path = lake
        .path()
        .join("manifests")
        .join(format!("{dsv_hex}.json"));
    let manifest = fs::read_to_string(&manifest_path).unwrap();
    let tampered = manifest.replacen(
        "\"id\": \"synthetic-etf-daily\"",
        "\"id\": \"synthetic-etf-corrupt\"",
        1,
    );
    assert_ne!(manifest, tampered);
    fs::write(manifest_path, tampered).unwrap();

    assert!(matches!(
        load_panel(lake.path(), &dsv, &VenueId::new("SYNTH").unwrap()),
        Err(PanelError::Manifest(_))
    ));
}
