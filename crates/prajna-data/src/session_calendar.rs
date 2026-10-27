//! Independent, immutable XSHG/XSHE session-calendar input for daily datasets.

use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use chrono::{DateTime, NaiveDate};
use prajna_domain::{Session, TimestampNs, VenueId};
use serde::{Deserialize, Serialize};

use crate::{
    daily_raw::{checked_output_path, validate_raw_root},
    default_mvp6_root, sha256,
};

/// The source asserts a complete list of sessions for its inclusive coverage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalendarSource {
    pub source_id: String,
    pub source_version: String,
    pub source_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalendarSession {
    pub session_date: NaiveDate,
    pub ts_open: TimestampNs,
    pub ts_close: TimestampNs,
}

/// One venue's independently sourced session list and inclusive coverage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VenueCalendar {
    pub venue_id: VenueId,
    pub source: CalendarSource,
    pub coverage_start: NaiveDate,
    pub coverage_end: NaiveDate,
    pub sessions: Vec<CalendarSession>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalendarInput {
    pub schema_version: u32,
    pub venues: Vec<VenueCalendar>,
}

impl CalendarInput {
    /// Parses and validates the exact bytes of a pinned calendar version.
    pub fn parse_and_validate(bytes: &[u8]) -> Result<Self, CalendarError> {
        let mut input: Self = serde_json::from_slice(bytes)?;
        input.normalize_and_validate()?;
        Ok(input)
    }

    fn normalize_and_validate(&mut self) -> Result<(), CalendarError> {
        if self.schema_version != 1 {
            return Err(CalendarError::Invalid(
                "unsupported calendar schema version",
            ));
        }
        self.venues
            .sort_by_key(|venue| match venue.venue_id.as_str() {
                "XSHG" => 0,
                "XSHE" => 1,
                _ => 2,
            });
        if self.venues.len() != 2
            || self.venues[0].venue_id.as_str() != "XSHG"
            || self.venues[1].venue_id.as_str() != "XSHE"
        {
            return Err(CalendarError::Invalid(
                "calendar must contain XSHG and XSHE exactly once",
            ));
        }
        for venue in &mut self.venues {
            if venue.source.source_id.trim().is_empty()
                || venue.source.source_version.trim().is_empty()
                || venue.source.source_ref.trim().is_empty()
            {
                return Err(CalendarError::Invalid(
                    "calendar source fields must not be empty",
                ));
            }
            if venue.coverage_start > venue.coverage_end {
                return Err(CalendarError::Invalid(
                    "calendar coverage start exceeds end",
                ));
            }
            venue.sessions.sort_by_key(|session| session.session_date);
            let mut previous = None;
            for session in &venue.sessions {
                if previous == Some(session.session_date) {
                    return Err(CalendarError::Invalid(
                        "duplicate or conflicting session date within venue",
                    ));
                }
                previous = Some(session.session_date);
                if !(venue.coverage_start..=venue.coverage_end).contains(&session.session_date) {
                    return Err(CalendarError::Invalid(
                        "session date is outside declared coverage",
                    ));
                }
                if session.ts_open >= session.ts_close
                    || shanghai_date(session.ts_open) != Some(session.session_date)
                    || shanghai_date(session.ts_close) != Some(session.session_date)
                {
                    return Err(CalendarError::Invalid(
                        "invalid session open or close for Shanghai local date",
                    ));
                }
            }
        }
        Ok(())
    }
}

fn shanghai_date(timestamp: TimestampNs) -> Option<NaiveDate> {
    let nanos = timestamp.as_unix_nanos();
    DateTime::from_timestamp(
        nanos.div_euclid(1_000_000_000),
        nanos.rem_euclid(1_000_000_000) as u32,
    )
    .map(|time| time.with_timezone(&chrono_tz::Asia::Shanghai).date_naive())
}

/// A validated content-addressed calendar version. Identities belong in the local data area.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarVersionId(String);

impl CalendarVersionId {
    pub fn parse(value: &str) -> Result<Self, CalendarError> {
        let Some(hex) = value.strip_prefix("sha256:") else {
            return Err(CalendarError::InvalidVersion);
        };
        if hex.len() != 64
            || !hex
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err(CalendarError::InvalidVersion);
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug)]
pub enum CalendarError {
    Io(io::Error),
    Json(serde_json::Error),
    Invalid(&'static str),
    InvalidVersion,
    CorruptVersion,
    MissingVenue,
    OutsideCoverage,
    InvalidReceipt,
    ReceiptConflict,
    Path(String),
}

impl std::fmt::Display for CalendarError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "calendar I/O error: {error}"),
            Self::Json(error) => write!(f, "invalid calendar JSON: {error}"),
            Self::Invalid(reason) => write!(f, "invalid calendar: {reason}"),
            Self::InvalidVersion => f.write_str("invalid calendar version id"),
            Self::CorruptVersion => f.write_str("calendar version is corrupt"),
            Self::MissingVenue => f.write_str("requested venue is absent from calendar version"),
            Self::OutsideCoverage => {
                f.write_str("requested date range is outside venue calendar coverage")
            }
            Self::InvalidReceipt => f.write_str("invalid calendar receipt name"),
            Self::ReceiptConflict => {
                f.write_str("calendar receipt already pins a different version")
            }
            Self::Path(reason) => write!(f, "invalid calendar local path: {reason}"),
        }
    }
}
impl std::error::Error for CalendarError {}
impl From<io::Error> for CalendarError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}
impl From<serde_json::Error> for CalendarError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

/// Persistent versions under an explicitly chosen local root, separate from market Raw.
#[derive(Debug, Clone)]
pub struct CalendarStore {
    root: PathBuf,
}

impl CalendarStore {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, CalendarError> {
        let root = validate_raw_root(root.as_ref(), &default_mvp6_root())
            .map_err(|error| CalendarError::Path(error.to_string()))?;
        Ok(Self { root })
    }

    /// Production import entry: reads a local JSON file; no market data is consulted.
    pub fn import_file(&self, path: impl AsRef<Path>) -> Result<CalendarVersionId, CalendarError> {
        self.import_json(&fs::read(path)?)
    }

    pub fn import_json(&self, bytes: &[u8]) -> Result<CalendarVersionId, CalendarError> {
        let input = CalendarInput::parse_and_validate(bytes)?;
        let canonical = serde_json::to_vec(&input)?;
        let version = CalendarVersionId(format!("sha256:{}", sha256(&canonical)));
        let path = self.object_path(&version)?;
        let parent = path.parent().expect("version path has a parent");
        fs::create_dir_all(parent)?;
        // Recheck after directory creation; never replace an existing version.
        let path = self.object_path(&version)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        temp.write_all(&canonical)?;
        temp.as_file().sync_all()?;
        match temp.persist_noclobber(&path) {
            Ok(_) => {}
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
                if fs::read(&path)? != canonical {
                    return Err(CalendarError::CorruptVersion);
                }
            }
            Err(error) => return Err(error.error.into()),
        }
        Ok(version)
    }

    /// Read and validate exactly the supplied version; never select a latest version.
    pub fn get(&self, version: &CalendarVersionId) -> Result<CalendarInput, CalendarError> {
        let bytes = fs::read(self.object_path(version)?)?;
        if format!("sha256:{}", sha256(&bytes)) != version.as_str() {
            return Err(CalendarError::CorruptVersion);
        }
        let mut input: CalendarInput = serde_json::from_slice(&bytes)?;
        input.normalize_and_validate()?;
        Ok(input)
    }

    /// Returns only this venue's sessions in the requested inclusive covered range.
    pub fn sessions(
        &self,
        version: &CalendarVersionId,
        venue: &VenueId,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<Session>, CalendarError> {
        let input = self.get(version)?;
        let calendar = input
            .venues
            .iter()
            .find(|item| &item.venue_id == venue)
            .ok_or(CalendarError::MissingVenue)?;
        if start > end || start < calendar.coverage_start || end > calendar.coverage_end {
            return Err(CalendarError::OutsideCoverage);
        }
        calendar
            .sessions
            .iter()
            .filter(|row| (start..=end).contains(&row.session_date))
            .map(|row| {
                Session::new(
                    calendar.venue_id.clone(),
                    row.session_date,
                    row.ts_open,
                    row.ts_close,
                )
                .map_err(|_| CalendarError::Invalid("invalid session bounds"))
            })
            .collect()
    }

    /// Writes a local, non-overwriting receipt so CLI users can pin the version.
    ///
    /// Repeating a pin for the version already recorded is a no-op; a receipt
    /// that names another version (or is unreadable) is never replaced.
    pub fn write_receipt(
        &self,
        name: &str,
        version: &CalendarVersionId,
    ) -> Result<(), CalendarError> {
        self.get(version)?;
        let path = self.receipt_path(name)?;
        match self.read_receipt(name) {
            Ok(existing) if &existing == version => return Ok(()),
            Ok(_) => return Err(CalendarError::ReceiptConflict),
            Err(CalendarError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let parent = path.parent().expect("receipt path has a parent");
        fs::create_dir_all(parent)?;
        let path = self.receipt_path(name)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        writeln!(temp, "{}", version.as_str())?;
        temp.as_file().sync_all()?;
        match temp.persist_noclobber(&path) {
            Ok(_) => {}
            // A concurrent import may have created the receipt after the read.
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
                return match self.read_receipt(name) {
                    Ok(existing) if &existing == version => Ok(()),
                    Ok(_) => Err(CalendarError::ReceiptConflict),
                    Err(error) => Err(error),
                };
            }
            Err(error) => return Err(error.error.into()),
        }
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    }

    /// Validates a receipt name without writing a version or a receipt.
    pub fn validate_receipt_name(&self, name: &str) -> Result<(), CalendarError> {
        self.receipt_path(name).map(|_| ())
    }

    pub fn read_receipt(&self, name: &str) -> Result<CalendarVersionId, CalendarError> {
        let value = fs::read_to_string(self.receipt_path(name)?)?;
        CalendarVersionId::parse(value.trim())
    }

    fn object_path(&self, version: &CalendarVersionId) -> Result<PathBuf, CalendarError> {
        let hex = &version.as_str()[7..];
        self.path(
            Path::new("session-calendars/sha256")
                .join(&hex[..2])
                .join(format!("{hex}.json")),
        )
    }

    fn receipt_path(&self, name: &str) -> Result<PathBuf, CalendarError> {
        if name.is_empty()
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        {
            return Err(CalendarError::InvalidReceipt);
        }
        self.path(Path::new("session-calendars/receipts").join(format!("{name}.txt")))
    }

    fn path(&self, relative: PathBuf) -> Result<PathBuf, CalendarError> {
        checked_output_path(&self.root, &relative)
            .map_err(|error| CalendarError::Path(error.to_string()))
    }
}
