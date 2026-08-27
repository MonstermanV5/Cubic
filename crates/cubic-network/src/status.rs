use std::{
    sync::atomic::{AtomicI64, Ordering},
    time::Duration,
};

use cubic_protocol::{
    CodecError, FrameLimits,
    status::{
        MAX_STATUS_FRAME_SIZE, STATUS_PROBE_PROTOCOL_VERSION, StatusHandshake, StatusJsonLimits,
        StatusResponse, decode_status_pong, decode_status_response, encode_status_handshake,
        encode_status_ping, encode_status_request,
    },
};
use tokio::time::{Instant, timeout};

use crate::{
    ServerAddress, StatusQueryError,
    connection::{ConnectionError, MinecraftConnection},
};

const MAX_STATUS_BUFFERED_BYTES: usize = 256 * 1024;
static NEXT_PING_NONCE: AtomicI64 = AtomicI64::new(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StatusQueryOptions {
    pub handshake_protocol_version: i32,
    pub connect_timeout: Duration,
    pub io_timeout: Duration,
    pub overall_timeout: Duration,
    pub ping_nonce: Option<i64>,
    pub json_limits: StatusJsonLimits,
}

impl Default for StatusQueryOptions {
    fn default() -> Self {
        Self {
            handshake_protocol_version: STATUS_PROBE_PROTOCOL_VERSION,
            connect_timeout: Duration::from_secs(3),
            io_timeout: Duration::from_secs(5),
            overall_timeout: Duration::from_secs(10),
            ping_nonce: None,
            json_limits: StatusJsonLimits::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ServerStatus {
    pub address: ServerAddress,
    pub response: StatusResponse,
    pub latency: Duration,
}

pub async fn query_server_status(
    address: &ServerAddress,
    options: &StatusQueryOptions,
) -> Result<ServerStatus, StatusQueryError> {
    match timeout(options.overall_timeout, query_inner(address, options)).await {
        Ok(result) => result,
        Err(_) => Err(StatusQueryError::OverallTimeout {
            timeout: options.overall_timeout,
        }),
    }
}

async fn query_inner(
    address: &ServerAddress,
    options: &StatusQueryOptions,
) -> Result<ServerStatus, StatusQueryError> {
    let limits = FrameLimits::new(MAX_STATUS_FRAME_SIZE, MAX_STATUS_BUFFERED_BYTES)
        .map_err(StatusQueryError::Framing)?;
    let mut connection =
        MinecraftConnection::connect(address, options.connect_timeout, options.io_timeout, limits)
            .await
            .map_err(map_connection_error)?;

    let handshake = encode_status_handshake(&StatusHandshake {
        protocol_version: options.handshake_protocol_version,
        server_address: address.host(),
        server_port: address.port(),
    })
    .map_err(StatusQueryError::Framing)?;
    let request = encode_status_request().map_err(StatusQueryError::Framing)?;
    connection
        .write_all(&handshake, "Handshake write")
        .await
        .map_err(map_connection_error)?;
    connection
        .write_all(&request, "Status Request write")
        .await
        .map_err(map_connection_error)?;

    let response_frame = connection
        .read_frame("Status Response")
        .await
        .map_err(map_status_response_error)?;
    let response = decode_status_response(&response_frame, options.json_limits)?;

    let nonce = options
        .ping_nonce
        .unwrap_or_else(|| NEXT_PING_NONCE.fetch_add(1, Ordering::Relaxed));
    let ping = encode_status_ping(nonce).map_err(StatusQueryError::Framing)?;
    let ping_started = Instant::now();
    connection
        .write_all(&ping, "Ping write")
        .await
        .map_err(map_connection_error)?;
    let pong_frame = connection
        .read_frame("Pong")
        .await
        .map_err(map_connection_error)?;
    decode_status_pong(&pong_frame, nonce)?;
    let latency = ping_started.elapsed();

    Ok(ServerStatus {
        address: address.clone(),
        response,
        latency,
    })
}

fn map_status_response_error(error: ConnectionError) -> StatusQueryError {
    match error {
        ConnectionError::Framing(CodecError::FrameTooLong { length, max }) => {
            StatusQueryError::StatusResponseTooLarge { length, max }
        }
        other => map_connection_error(other),
    }
}

fn map_connection_error(error: ConnectionError) -> StatusQueryError {
    match error {
        ConnectionError::ConnectTimeout { timeout } => StatusQueryError::ConnectTimeout { timeout },
        ConnectionError::ConnectFailed { source } => StatusQueryError::ConnectFailed { source },
        ConnectionError::IoTimeout { operation, timeout } => {
            StatusQueryError::IoTimeout { operation, timeout }
        }
        ConnectionError::Io { operation, source } => StatusQueryError::Io { operation, source },
        ConnectionError::PrematureDisconnect {
            phase,
            buffered_bytes,
        } => StatusQueryError::PrematureDisconnect {
            phase,
            buffered_bytes,
        },
        ConnectionError::Framing(error) => StatusQueryError::Framing(error),
        ConnectionError::Transform(error) => StatusQueryError::WireTransform {
            reason: error.to_string(),
        },
    }
}
