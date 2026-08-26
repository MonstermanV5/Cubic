use std::{io, time::Duration};

use cubic_protocol::{
    CodecError, bootstrap::v775::BootstrapProtocolError, status::StatusProtocolError,
};
use cubic_version::VersionError;
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnsupportedPhase7Feature {
    Encryption,
    Compression,
    ResourcePack,
    Transfer,
    CodeOfConduct,
}

impl std::fmt::Display for UnsupportedPhase7Feature {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Encryption => formatter.write_str("encryption"),
            Self::Compression => formatter.write_str("packet compression"),
            Self::ResourcePack => formatter.write_str("server resource packs"),
            Self::Transfer => formatter.write_str("server transfer"),
            Self::CodeOfConduct => formatter.write_str("server code of conduct"),
        }
    }
}

#[derive(Debug, Error)]
pub enum DevelopmentLoginError {
    #[error("invalid development username: {reason}")]
    InvalidUsername { reason: &'static str },
    #[error("the built-in development profile is invalid")]
    InvalidBootstrapProfile(#[source] VersionError),
    #[error("timed out connecting after {timeout:?}")]
    ConnectTimeout { timeout: Duration },
    #[error("could not resolve or connect to the development server")]
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
    #[error(transparent)]
    Protocol(#[from] BootstrapProtocolError),
    #[error("server disconnected during {state}: {reason}")]
    ServerDisconnect { state: &'static str, reason: String },
    #[error(
        "server requested {feature}, which Phase 7 does not support; required development setting: {required_setting}"
    )]
    UnsupportedForPhase7 {
        feature: UnsupportedPhase7Feature,
        required_setting: &'static str,
    },
    #[error("Login Success returned username {received:?}, expected {expected:?}")]
    LoginUsernameMismatch { expected: String, received: String },
    #[error("Login Success returned an all-zero UUID")]
    InvalidLoginUuid,
    #[error("invalid state transition from {from} to {to}")]
    InvalidStateTransition {
        from: &'static str,
        to: &'static str,
    },
    #[error("{state} processed more than the configured {max_packets} packets")]
    PacketLimitExceeded {
        state: &'static str,
        max_packets: usize,
    },
    #[error("overall Login and Configuration sequence timed out after {timeout:?}")]
    OverallTimeout { timeout: Duration },
}
