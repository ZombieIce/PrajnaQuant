//! Backend-neutral adapter contracts. Fake execution is not Nautilus evidence.
mod packet;
pub mod protocol;
pub use packet::RunPacket;
mod execution;
pub use execution::{AdapterEvent, AdapterResult, Backend, FakeBackend, run_adapter};

#[derive(Debug)]
pub enum AdapterError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Data(prajna_data::DataError),
    Manifest(prajna_data::ManifestError),
    Execution(prajna_research::fast_event::FastEventError),
    Invalid(String),
}

impl std::fmt::Display for AdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "adapter I/O: {e}"),
            Self::Json(e) => write!(f, "adapter protocol: {e}"),
            Self::Data(e) => write!(f, "adapter data: {e}"),
            Self::Manifest(e) => write!(f, "adapter manifest: {e}"),
            Self::Execution(e) => write!(f, "adapter execution: {e}"),
            Self::Invalid(e) => write!(f, "invalid adapter input: {e}"),
        }
    }
}
impl std::error::Error for AdapterError {}

macro_rules! from_error {
    ($type:ty, $variant:ident) => {
        impl From<$type> for AdapterError {
            fn from(error: $type) -> Self {
                Self::$variant(error)
            }
        }
    };
}
from_error!(std::io::Error, Io);
from_error!(serde_json::Error, Json);
from_error!(prajna_data::DataError, Data);
from_error!(prajna_data::ManifestError, Manifest);
from_error!(prajna_research::fast_event::FastEventError, Execution);

fn invalid(message: impl Into<String>) -> AdapterError {
    AdapterError::Invalid(message.into())
}
