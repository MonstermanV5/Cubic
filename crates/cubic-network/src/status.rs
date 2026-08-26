use std::{
    sync::atomic::{AtomicI64, Ordering},
    time::Duration,
};

use cubic_protocol::{
    CodecError, FrameDecoder, FrameLimits,
    status::{
        MAX_STATUS_FRAME_SIZE, STATUS_PROBE_PROTOCOL_VERSION, StatusHandshake, StatusJsonLimits,
        StatusResponse, decode_status_pong, decode_status_response, encode_status_handshake,
        encode_status_ping, encode_status_request,
    },
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::{Instant, timeout},
};

use crate::{ServerAddress, StatusQueryError};

const MAX_STATUS_BUFFERED_BYTES: usize = 256 * 1024;
const READ_BUFFER_SIZE: usize = 8 * 1024;
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
    if options.connect_timeout.is_zero() {
        return Err(StatusQueryError::ConnectTimeout {
            timeout: options.connect_timeout,
        });
    }
    let mut stream = match timeout(
        options.connect_timeout,
        TcpStream::connect(address.socket_target()),
    )
    .await
    {
        Ok(Ok(stream)) => stream,
        Ok(Err(source)) => return Err(StatusQueryError::ConnectFailed { source }),
        Err(_) => {
            return Err(StatusQueryError::ConnectTimeout {
                timeout: options.connect_timeout,
            });
        }
    };

    let handshake = encode_status_handshake(&StatusHandshake {
        protocol_version: options.handshake_protocol_version,
        server_address: address.host(),
        server_port: address.port(),
    })
    .map_err(StatusQueryError::Framing)?;
    let request = encode_status_request().map_err(StatusQueryError::Framing)?;
    write_all(
        &mut stream,
        &handshake,
        options.io_timeout,
        "Handshake write",
    )
    .await?;
    write_all(
        &mut stream,
        &request,
        options.io_timeout,
        "Status Request write",
    )
    .await?;

    let limits = FrameLimits::new(MAX_STATUS_FRAME_SIZE, MAX_STATUS_BUFFERED_BYTES)
        .map_err(StatusQueryError::Framing)?;
    let mut decoder = FrameDecoder::new(limits);
    let response_frame = read_with_timeout(
        &mut stream,
        &mut decoder,
        options.io_timeout,
        "Status Response",
    )
    .await?;
    let response = decode_status_response(&response_frame, options.json_limits)?;

    let nonce = options
        .ping_nonce
        .unwrap_or_else(|| NEXT_PING_NONCE.fetch_add(1, Ordering::Relaxed));
    let ping = encode_status_ping(nonce).map_err(StatusQueryError::Framing)?;
    let ping_started = Instant::now();
    write_all(&mut stream, &ping, options.io_timeout, "Ping write").await?;
    let pong_frame =
        read_with_timeout(&mut stream, &mut decoder, options.io_timeout, "Pong").await?;
    decode_status_pong(&pong_frame, nonce)?;
    let latency = ping_started.elapsed();

    Ok(ServerStatus {
        address: address.clone(),
        response,
        latency,
    })
}

async fn write_all(
    stream: &mut TcpStream,
    bytes: &[u8],
    duration: Duration,
    operation: &'static str,
) -> Result<(), StatusQueryError> {
    match timeout(duration, stream.write_all(bytes)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(source)) => Err(StatusQueryError::Io { operation, source }),
        Err(_) => Err(StatusQueryError::IoTimeout {
            operation,
            timeout: duration,
        }),
    }
}

async fn read_with_timeout(
    stream: &mut TcpStream,
    decoder: &mut FrameDecoder,
    duration: Duration,
    phase: &'static str,
) -> Result<Vec<u8>, StatusQueryError> {
    match timeout(duration, read_next_frame(stream, decoder, phase)).await {
        Ok(result) => result,
        Err(_) => Err(StatusQueryError::IoTimeout {
            operation: phase,
            timeout: duration,
        }),
    }
}

async fn read_next_frame(
    stream: &mut TcpStream,
    decoder: &mut FrameDecoder,
    phase: &'static str,
) -> Result<Vec<u8>, StatusQueryError> {
    let mut read_buffer = [0_u8; READ_BUFFER_SIZE];
    loop {
        match decoder.next_frame() {
            Ok(Some(frame)) => return Ok(frame),
            Ok(None) => {}
            Err(CodecError::FrameTooLong { length, max }) if phase == "Status Response" => {
                return Err(StatusQueryError::StatusResponseTooLarge { length, max });
            }
            Err(error) => return Err(StatusQueryError::Framing(error)),
        }

        let read = stream
            .read(&mut read_buffer)
            .await
            .map_err(|source| StatusQueryError::Io {
                operation: phase,
                source,
            })?;
        if read == 0 {
            return Err(StatusQueryError::PrematureDisconnect {
                phase,
                buffered_bytes: decoder.buffered_len(),
            });
        }
        decoder
            .push(
                read_buffer
                    .get(..read)
                    .ok_or(StatusQueryError::PrematureDisconnect {
                        phase,
                        buffered_bytes: decoder.buffered_len(),
                    })?,
            )
            .map_err(StatusQueryError::Framing)?;
    }
}
