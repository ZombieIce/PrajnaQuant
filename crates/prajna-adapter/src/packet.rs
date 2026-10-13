use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use prajna_data::{
    ExecutionStatusRow, bars_from_record_batch, bars_to_record_batch,
    execution_status_from_record_batch, execution_status_to_record_batch,
    instruments_from_record_batch, instruments_to_record_batch, read_manifest, read_parquet,
    sessions_from_record_batch, sessions_to_record_batch, write_parquet,
};
use prajna_domain::{Bar, InstrumentId, InstrumentSpec, InstrumentSpecData, Quantity, Session};
use serde::{Deserialize, Serialize};

use crate::{AdapterError, invalid, protocol::VERSION};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunPacket {
    pub instruments: Vec<InstrumentSpec>,
    pub sessions: Vec<Session>,
    pub bars: Vec<Bar>,
    pub statuses: Vec<ExecutionStatusRow>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    protocol: String,
    dsv: String,
}

impl RunPacket {
    pub fn from_dataset(root: &Path, dsv: &str) -> Result<Self, AdapterError> {
        Self::load_dataset(root, dsv, None)
    }

    /// Resolve identity-bearing Run lot overrides before validating the neutral packet.
    pub fn from_dataset_with_lots(
        root: &Path,
        dsv: &str,
        lots: &BTreeMap<InstrumentId, Quantity>,
    ) -> Result<Self, AdapterError> {
        Self::load_dataset(root, dsv, Some(lots))
    }

    fn load_dataset(
        root: &Path,
        dsv: &str,
        lots: Option<&BTreeMap<InstrumentId, Quantity>>,
    ) -> Result<Self, AdapterError> {
        let manifest = read_manifest(root, dsv)?;
        let hex = manifest
            .dsv
            .strip_prefix("dsv:sha256:")
            .ok_or_else(|| invalid("invalid Dataset Version"))?;
        Self::read_tables(
            |table| {
                root.join("normalized")
                    .join(table)
                    .join(hex)
                    .join("part-00000.parquet")
            },
            dsv,
            lots,
        )
    }

    pub fn read(path: &Path) -> Result<Self, AdapterError> {
        let header: Header = serde_json::from_slice(&std::fs::read(path.join("packet.json"))?)?;
        if header.protocol != VERSION {
            return Err(invalid("unsupported Run packet protocol"));
        }
        Self::read_tables(
            |table| path.join(format!("{table}.parquet")),
            &header.dsv,
            None,
        )
    }

    fn read_tables(
        path: impl Fn(&str) -> PathBuf,
        dsv: &str,
        lots: Option<&BTreeMap<InstrumentId, Quantity>>,
    ) -> Result<Self, AdapterError> {
        let mut packet = Self {
            instruments: vec![],
            sessions: vec![],
            bars: vec![],
            statuses: vec![],
        };
        for table in ["instruments", "sessions", "bars", "execution_status"] {
            for batch in read_parquet(path(table), table, "1")? {
                if batch
                    .schema()
                    .metadata()
                    .get("prajna.dsv")
                    .map(String::as_str)
                    != Some(dsv)
                {
                    return Err(invalid(format!("{table} Dataset Version mismatch")));
                }
                match table {
                    "instruments" => packet
                        .instruments
                        .extend(instruments_from_record_batch(&batch)?),
                    "sessions" => packet.sessions.extend(sessions_from_record_batch(&batch)?),
                    "bars" => packet
                        .bars
                        .extend(bars_from_record_batch(&batch, &packet.sessions)?),
                    "execution_status" => packet
                        .statuses
                        .extend(execution_status_from_record_batch(&batch)?),
                    _ => unreachable!(),
                }
            }
        }
        if let Some(lots) = lots {
            for instrument in &mut packet.instruments {
                let mut data: InstrumentSpecData =
                    serde_json::from_value(serde_json::to_value(&*instrument)?)?;
                data.lot_size =
                    Some(*lots.get(instrument.id()).ok_or_else(|| {
                        invalid("Run packet Instrument has no resolved lot size")
                    })?);
                *instrument =
                    InstrumentSpec::new(data).map_err(|error| invalid(error.to_string()))?;
            }
        }
        packet.validate()?;
        Ok(packet)
    }

    pub fn write(&self, path: &Path, dsv: &str) -> Result<(), AdapterError> {
        self.validate()?;
        let batches = [
            (
                "instruments",
                instruments_to_record_batch(&self.instruments)?,
            ),
            ("sessions", sessions_to_record_batch(&self.sessions)?),
            ("bars", bars_to_record_batch(&self.bars)?),
            (
                "execution_status",
                execution_status_to_record_batch(&self.statuses)?,
            ),
        ];
        // A packet is immutable; never overwrite an earlier Run's files.
        std::fs::create_dir(path)?;
        for (table, batch) in batches {
            write_parquet(path.join(format!("{table}.parquet")), &batch, dsv)?;
        }
        std::fs::write(
            path.join("packet.json"),
            serde_json::to_vec(&Header {
                protocol: VERSION.into(),
                dsv: dsv.into(),
            })?,
        )?;
        Ok(())
    }

    pub fn validate(&self) -> Result<(), AdapterError> {
        let ids = self
            .instruments
            .iter()
            .map(|i| i.id())
            .collect::<BTreeSet<_>>();
        let sessions = self
            .sessions
            .iter()
            .map(|s| (s.venue_id(), s.session_date()))
            .collect::<BTreeSet<_>>();
        if ids.is_empty()
            || sessions.is_empty()
            || self.bars.is_empty()
            || ids.len() != self.instruments.len()
            || sessions.len() != self.sessions.len()
            || self.instruments.iter().any(|i| {
                i.lot_size()
                    .is_none_or(|q| q <= prajna_domain::Quantity::ZERO)
            })
        {
            return Err(invalid(
                "packet requires unique Instruments/Sessions and positive explicit lot sizes",
            ));
        }
        let venue = self.sessions[0].venue_id();
        if self.sessions.iter().any(|s| s.venue_id() != venue)
            || self.instruments.iter().any(|i| i.id().venue() != venue)
        {
            return Err(invalid("MVP-4 Run packet requires one Venue"));
        }
        let mut bars = BTreeSet::new();
        for bar in &self.bars {
            if !ids.contains(bar.instrument_id())
                || !sessions.contains(&(bar.instrument_id().venue(), bar.session_date()))
                || !bars.insert((bar.instrument_id(), bar.session_date()))
                || !self.sessions.iter().any(|s| {
                    s.session_date() == bar.session_date()
                        && s.ts_open() == bar.ts_open()
                        && s.ts_close() == bar.ts_close()
                })
            {
                return Err(invalid("unknown or duplicate bar key"));
            }
        }
        let mut statuses = BTreeSet::new();
        for status in &self.statuses {
            if !ids.contains(&status.instrument_id)
                || !sessions.contains(&(status.instrument_id.venue(), status.session_date))
                || !statuses.insert((&status.instrument_id, status.session_date))
            {
                return Err(invalid("unknown or duplicate execution status key"));
            }
        }
        if statuses.len() != ids.len() * sessions.len() {
            return Err(invalid(
                "every Instrument/Session requires explicit execution status",
            ));
        }
        Ok(())
    }
}
