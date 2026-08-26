use std::{io, time::Duration};

use cubic_protocol::{CodecError, status::StatusProtocolError};
use thiserror::Error;

use crate::ServerAddressError;

#[derive(Debug, Error)]
pub enum StatusQueryError {
    #[error(transparent)]
    InvalidAddress(#[from] ServerAddressError),
    #[error("timed out connecting after {timeout:?}")]
    ConnectTimeout { timeout: Duration },
    #[error("could not resolve or connect to the server")]
    ConnectFailed {
        #[source]
        source: io::Error,
    },
    #[error("timed out during {operation} after {timeout:?}")]
    IoTimeout {
        operation: &'static str,
        timeout: Duration,
    },
    #[error("I/O failure during {operation}")]
    Io {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("server disconnected before {phase}; {buffered_bytes} partial bytes were buffered")]
    PrematureDisconnect {
        phase: &'static str,
        buffered_bytes: usize,
    },
    #[error("malformed Minecraft frame")]
    Framing(#[source] CodecError),
    #[error("Status response frame has {length} bytes, exceeding limit {max}")]
    StatusResponseTooLarge { length: usize, max: usize },
    #[error(transparent)]
    Protocol(#[from] StatusProtocolError),
    #[error("overall status query timed out after {timeout:?}")]
    OverallTimeout { timeout: Duration },
}
