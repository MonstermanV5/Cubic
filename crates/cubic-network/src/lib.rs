//! Async Minecraft Java Status transport built on `cubic-protocol`.

mod address;
mod error;
mod status;

pub use address::{DEFAULT_MINECRAFT_PORT, ServerAddress, ServerAddressError};
pub use error::StatusQueryError;
pub use status::{ServerStatus, StatusQueryOptions, query_server_status};
