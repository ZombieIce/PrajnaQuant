mod common;

#[path = "common/cn_daily.rs"]
mod cn_daily;

use std::fs;

use chrono::NaiveDate;
use polars::prelude::{DataType, TimeUnit, TimeZone};
use prajna_domain::{TimestampNs, VenueId};
use prajna_research::{Panel, PanelError, load_panel, load_panel_for_venues};

fn venue(name: &str) -> VenueId {
    VenueId::new(name).unwrap()
}

fn instrument_ids(panel: &Panel) -> Vec<String> {
    panel
        .instruments
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
}

fn panel_error(calendar: Vec<cn_daily::CalendarVenue>) -> PanelError {
    let dataset = cn_daily::SyntheticDailyDataset::publish(
        &calendar,
        cn_daily::shanghai_shenzhen_records(),
        &[],
    );
    load_panel_for_venues(
        dataset.root(),
        &dataset.dsv,
        &[venue("XSHG"), venue("XSHE")],
    )
    .unwrap_err()
}

#[test]
fn window_slices_sessions_and_remaps_indices_without_deleting_the_axis() {
    let lake = tempfile::tempdir().unwrap();
    let dsv = common::publish_v1(lake.path());
    let venue = VenueId::new("SYNTH").unwrap();
    let panel = load_panel(lake.path(), &dsv, &venue).unwrap();
    let start = panel.sessions[3].session_date;
    let end = panel.sessions[7].session_date;
    let windowed = panel.window(start, end).unwrap();
    assert_eq!(windowed.sessions.len(), 5);
    assert_eq!(windowed.sessions.first().unwrap().session_date, start);
    assert_eq!(windowed.sessions.last().unwrap().session_date, end);
    assert_eq!(windowed.instruments, panel.instruments);
    assert_eq!(windowed.grid.height(), 5 * windowed.instruments.len());
    let indices = windowed
        .grid
        .column("session_index")
        .unwrap()
        .u32()
        .unwrap();
    assert!(
        indices.iter().flatten().all(|index| index < 5),
        "session_index must be remapped onto the windowed axis"
    );
    // Raw decimals are preserved for the same instrument/session pair.
    let id = |panel: &Panel, row: usize| {
        panel
            .grid
            .column("instrument_id")
            .unwrap()
            .str()
            .unwrap()
            .get(row)
            .unwrap()
            .to_owned()
    };
    let original_row = (0..panel.grid.height())
        .find(|row| {
            id(&panel, *row) == "A.SYNTH"
                && panel
                    .grid
                    .column("session_index")
                    .unwrap()
                    .u32()
                    .unwrap()
                    .get(*row)
                    == Some(3)
        })
        .unwrap();
    let windowed_row = (0..windowed.grid.height())
        .find(|row| {
            id(&windowed, *row) == "A.SYNTH"
                && windowed
                    .grid
                    .column("session_index")
                    .unwrap()
                    .u32()
                    .unwrap()
                    .get(*row)
                    == Some(0)
        })
        .unwrap();
    let decimal = |panel: &Panel, row: usize, column: &str| {
        panel
            .grid
            .column(column)
            .unwrap()
            .str()
            .unwrap()
            .get(row)
            .map(str::to_owned)
    };
    assert_eq!(
        decimal(&panel, original_row, "open_decimal"),
        decimal(&windowed, windowed_row, "open_decimal")
    );
    assert_eq!(
        decimal(&panel, original_row, "close_decimal"),
        decimal(&windowed, windowed_row, "close_decimal")
    );
    assert!(panel.window(end, start).is_err());
    assert!(
        panel
            .window(NaiveDate::from_ymd_opt(2030, 1, 1).unwrap(), end)
            .is_err()
    );
}

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
            "close_available_at",
            "open_decimal",
            "close_decimal"
        ]
    );

    let instrument_ids = panel.grid.column("instrument_id").unwrap().str().unwrap();
    let session_indices = panel.grid.column("session_index").unwrap().u32().unwrap();
    let open = panel.grid.column("open").unwrap().f64().unwrap();
    let close = panel.grid.column("close").unwrap().f64().unwrap();
    let has_bar = panel.grid.column("has_bar").unwrap().bool().unwrap();
    let open_decimal = panel.grid.column("open_decimal").unwrap().str().unwrap();
    let close_decimal = panel.grid.column("close_decimal").unwrap().str().unwrap();
    let available_at = panel.grid.column("close_available_at").unwrap();
    let missing = (0..panel.grid.height())
        .find(|&row| {
            instrument_ids.get(row) == Some("B.SYNTH") && session_indices.get(row) == Some(9)
        })
        .unwrap();
    assert_eq!(open_decimal.get(0), Some("100"));
    assert_eq!(close_decimal.get(0), Some("100"));
    assert_eq!(open_decimal.get(missing), None);
    assert_eq!(close_decimal.get(missing), None);

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

const SHARED_DATES: [&str; 3] = ["2026-01-05", "2026-01-06", "2026-01-07"];

#[test]
fn loads_a_shared_shanghai_shenzhen_session_axis_and_preserves_missing_bars() {
    let dataset = cn_daily::SyntheticDailyDataset::publish(
        &cn_daily::matched_calendar(&SHARED_DATES),
        cn_daily::shanghai_shenzhen_records(),
        &[("510300", "2026-01-06")],
    );
    let panel = load_panel_for_venues(
        dataset.root(),
        &dataset.dsv,
        &[venue("XSHG"), venue("XSHE")],
    )
    .unwrap();

    assert_eq!(panel.sessions.len(), 3);
    assert_eq!(panel.sessions[0].session_date, cn_daily::date("2026-01-05"));
    assert_eq!(panel.sessions[2].session_date, cn_daily::date("2026-01-07"));
    assert_eq!(
        panel.sessions[0].ts_open,
        TimestampNs::parse("2026-01-05T09:30:00+08:00").unwrap()
    );
    assert_eq!(
        panel.sessions[0].ts_close,
        TimestampNs::parse("2026-01-05T15:00:00+08:00").unwrap()
    );
    assert_eq!(
        instrument_ids(&panel),
        ["000001.XSHE", "159952.XSHE", "510300.XSHG", "600519.XSHG"]
    );
    assert!(
        panel
            .instruments
            .iter()
            .all(|instrument| matches!(instrument.venue().as_str(), "XSHG" | "XSHE"))
    );
    assert_eq!(panel.grid.height(), 12);

    let ids = panel.grid.column("instrument_id").unwrap().str().unwrap();
    let indices = panel.grid.column("session_index").unwrap().u32().unwrap();
    let has_bar = panel.grid.column("has_bar").unwrap().bool().unwrap();
    let opens = panel.grid.column("open").unwrap().f64().unwrap();
    let closes = panel.grid.column("close").unwrap().f64().unwrap();
    let missing = (0..panel.grid.height())
        .find(|&row| ids.get(row) == Some("510300.XSHG") && indices.get(row) == Some(1))
        .unwrap();
    assert_eq!(has_bar.get(missing), Some(false));
    assert_eq!(opens.get(missing), None);
    assert_eq!(closes.get(missing), None);
    assert_eq!(
        panel
            .grid
            .column("close_available_at")
            .unwrap()
            .is_null()
            .get(missing),
        Some(true)
    );

    let present = (0..panel.grid.height())
        .find(|&row| ids.get(row) == Some("600519.XSHG") && indices.get(row) == Some(0))
        .unwrap();
    let open_decimal = panel.grid.column("open_decimal").unwrap().str().unwrap();
    let close_decimal = panel.grid.column("close_decimal").unwrap().str().unwrap();
    assert_eq!(has_bar.get(present), Some(true));
    assert_eq!(open_decimal.get(present), Some("10"));
    assert_eq!(close_decimal.get(present), Some("10.1"));
    assert_eq!(
        panel
            .grid
            .column("close_available_at")
            .unwrap()
            .is_null()
            .get(present),
        Some(true)
    );
}

#[test]
fn explicit_single_venue_loading_matches_the_legacy_panel_entry() {
    let lake = tempfile::tempdir().unwrap();
    let dsv = common::publish_v1(lake.path());
    let synth = venue("SYNTH");
    let legacy = load_panel(lake.path(), &dsv, &synth).unwrap();
    let explicit = load_panel_for_venues(lake.path(), &dsv, std::slice::from_ref(&synth)).unwrap();
    assert_eq!(explicit.sessions, legacy.sessions);
    assert_eq!(explicit.instruments, legacy.instruments);
    assert!(explicit.grid.equals_missing(&legacy.grid));

    let dataset = cn_daily::SyntheticDailyDataset::publish(
        &cn_daily::matched_calendar(&SHARED_DATES),
        cn_daily::shanghai_shenzhen_records(),
        &[],
    );
    let xshg = venue("XSHG");
    let legacy = load_panel(dataset.root(), &dataset.dsv, &xshg).unwrap();
    let explicit = load_panel_for_venues(dataset.root(), &dataset.dsv, &[xshg]).unwrap();
    assert_eq!(explicit.sessions, legacy.sessions);
    assert_eq!(explicit.instruments, legacy.instruments);
    assert_eq!(explicit.instruments.len(), 2);
    assert!(explicit.grid.equals_missing(&legacy.grid));
}

#[test]
fn rejects_venues_whose_session_dates_differ_without_filling_the_gap() {
    let mut calendars = cn_daily::matched_calendar(&SHARED_DATES);
    calendars[1].dates = vec!["2026-01-05", "2026-01-07"];
    let dataset = cn_daily::SyntheticDailyDataset::publish(
        &calendars,
        cn_daily::shanghai_shenzhen_records(),
        &[],
    );

    let error = load_panel_for_venues(
        dataset.root(),
        &dataset.dsv,
        &[venue("XSHG"), venue("XSHE")],
    )
    .unwrap_err();
    match error {
        PanelError::InconsistentSessionAxis(message) => {
            assert!(message.contains("2026-01-06"), "{message}");
            assert!(message.contains("XSHG"), "{message}");
            assert!(message.contains("XSHE"), "{message}");
        }
        other => panic!("unexpected error: {other:?}"),
    }

    // Each Venue alone still exposes only its own published Sessions: the
    // loader neither unions nor deletes either calendar.
    let xshg = load_panel_for_venues(dataset.root(), &dataset.dsv, &[venue("XSHG")]).unwrap();
    assert_eq!(xshg.sessions.len(), 3);
    let xshe = load_panel_for_venues(dataset.root(), &dataset.dsv, &[venue("XSHE")]).unwrap();
    assert_eq!(xshe.sessions.len(), 2);
}

#[test]
fn rejects_venues_whose_open_or_close_times_differ() {
    let mut calendars = cn_daily::matched_calendar(&SHARED_DATES);
    calendars[1].open = "09:31:00";
    match panel_error(calendars) {
        PanelError::InconsistentSessionAxis(message) => {
            assert!(
                message.contains("disagree on Session 2026-01-05"),
                "{message}"
            );
        }
        other => panic!("unexpected error: {other:?}"),
    }

    let mut calendars = cn_daily::matched_calendar(&SHARED_DATES);
    calendars[1].close = "15:01:00";
    match panel_error(calendars) {
        PanelError::InconsistentSessionAxis(message) => {
            assert!(
                message.contains("disagree on Session 2026-01-05"),
                "{message}"
            );
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn rejects_empty_duplicate_or_absent_venue_selections() {
    let dataset = cn_daily::SyntheticDailyDataset::publish(
        &cn_daily::matched_calendar(&SHARED_DATES),
        cn_daily::shanghai_shenzhen_records(),
        &[],
    );
    let root = dataset.root();
    let dsv = &dataset.dsv;

    assert!(matches!(
        load_panel_for_venues(root, dsv, &[]),
        Err(PanelError::InvalidVenueSelection(message)) if message.contains("at least one")
    ));
    assert!(matches!(
        load_panel_for_venues(root, dsv, &[venue("XSHG"), venue("XSHG")]),
        Err(PanelError::InvalidVenueSelection(message)) if message.contains("duplicate")
    ));

    let error = load_panel_for_venues(root, dsv, &[venue("XSHG"), venue("SYNTH")]).unwrap_err();
    match error {
        PanelError::InvalidVenueSelection(message) => {
            assert!(message.contains("SYNTH"), "{message}");
            assert!(message.contains("Instruments"), "{message}");
        }
        other => panic!("unexpected error: {other:?}"),
    }

    let mut records = cn_daily::shanghai_shenzhen_records();
    records.retain(|record| record.instrument_id.venue().as_str() == "XSHG");
    let dataset = cn_daily::SyntheticDailyDataset::publish(
        &cn_daily::matched_calendar(&SHARED_DATES),
        records,
        &[],
    );
    let error = load_panel_for_venues(
        dataset.root(),
        &dataset.dsv,
        &[venue("XSHG"), venue("XSHE")],
    )
    .unwrap_err();
    match error {
        PanelError::InvalidVenueSelection(message) => {
            assert!(message.contains("XSHE"), "{message}");
            assert!(message.contains("Instruments"), "{message}");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}
