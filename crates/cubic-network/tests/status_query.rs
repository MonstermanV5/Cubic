use std::{str::FromStr, time::Duration};

use cubic_network::{ServerAddress, StatusQueryError, StatusQueryOptions, query_server_status};
use cubic_protocol::{
    CodecReader, CodecWriter, FrameDecoder, FrameLimits, StringLimits, encode_frame,
    split_raw_packet,
    status::{MAX_STATUS_FRAME_SIZE, StatusProtocolError},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
    time::sleep,
};

const NONCE: i64 = 0x0102_0304_0506_0708;

#[derive(Clone, Copy)]
enum Chunking {
    Whole,
    PrefixSplit,
    ByteByByte,
}

#[derive(Clone, Copy)]
enum MockMode {
    Success {
        response_chunks: Chunking,
        pong_chunks: Chunking,
        coalesced_early_pong: bool,
        padded_json: bool,
    },
    MalformedFrame,
    OversizedFrame,
    WrongStatusId,
    MalformedString,
    MalformedJson,
    MissingField,
    WrongPongId,
    WrongPongNonce,
    DisconnectBeforeResponse,
    DisconnectDuringFrame,
    ReadTimeout,
}

#[derive(Debug, Eq, PartialEq)]
struct ObservedHandshake {
    protocol: i32,
    host: String,
    port: u16,
    next_state: i32,
}

fn test_options(protocol: i32) -> StatusQueryOptions {
    StatusQueryOptions {
        handshake_protocol_version: protocol,
        connect_timeout: Duration::from_millis(500),
        io_timeout: Duration::from_millis(150),
        overall_timeout: Duration::from_secs(2),
        ping_nonce: Some(NONCE),
        ..StatusQueryOptions::default()
    }
}

async fn spawn_mock(
    mode: MockMode,
    connect_host: &str,
    expected_protocol: i32,
) -> (ServerAddress, JoinHandle<ObservedHandshake>) {
    let listener = TcpListener::bind((connect_host, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let address = ServerAddress::from_str(&format!("{connect_host}:{port}")).unwrap();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut frames = MockFrameReader::new();
        let handshake = decode_handshake(&frames.next(&mut stream).await);
        assert_eq!(handshake.protocol, expected_protocol);
        assert_eq!(handshake.port, port);
        assert_eq!(handshake.next_state, 1);
        let request_frame = frames.next(&mut stream).await;
        let request = split_raw_packet(&request_frame).unwrap();
        assert_eq!(request.id, 0);
        assert!(request.payload.is_empty());

        match mode {
            MockMode::DisconnectBeforeResponse => return handshake,
            MockMode::DisconnectDuringFrame => {
                stream.write_all(&[5, 0]).await.unwrap();
                return handshake;
            }
            MockMode::ReadTimeout => {
                sleep(Duration::from_millis(500)).await;
                return handshake;
            }
            MockMode::MalformedFrame => {
                stream.write_all(&[0x80; 5]).await.unwrap();
                return handshake;
            }
            MockMode::OversizedFrame => {
                let mut writer = CodecWriter::new();
                writer.write_var_int(i32::try_from(MAX_STATUS_FRAME_SIZE + 1).unwrap());
                stream.write_all(writer.as_slice()).await.unwrap();
                return handshake;
            }
            MockMode::WrongStatusId => {
                write_chunks(&mut stream, &packet(1, &[]), Chunking::Whole).await;
                return handshake;
            }
            MockMode::MalformedString => {
                write_chunks(&mut stream, &packet(0, &[2, 0xc3, 0x28]), Chunking::Whole).await;
                return handshake;
            }
            MockMode::MalformedJson => {
                write_chunks(&mut stream, &status_packet("{"), Chunking::Whole).await;
                return handshake;
            }
            MockMode::MissingField => {
                write_chunks(
                    &mut stream,
                    &status_packet(r#"{"players":{"max":1,"online":0},"description":"x"}"#),
                    Chunking::Whole,
                )
                .await;
                return handshake;
            }
            MockMode::Success {
                response_chunks,
                pong_chunks,
                coalesced_early_pong,
                padded_json,
            } => {
                let json = valid_json(padded_json);
                let response = status_packet(&json);
                if coalesced_early_pong {
                    let mut combined = response;
                    combined.extend(pong_packet(1, NONCE));
                    write_chunks(&mut stream, &combined, Chunking::Whole).await;
                } else {
                    write_chunks(&mut stream, &response, response_chunks).await;
                }
                let ping_frame = frames.next(&mut stream).await;
                let ping = split_raw_packet(&ping_frame).unwrap();
                assert_eq!(ping.id, 1);
                let mut reader = CodecReader::new(ping.payload);
                assert_eq!(reader.read_i64().unwrap(), NONCE);
                assert_eq!(reader.remaining(), 0);
                if !coalesced_early_pong {
                    write_chunks(&mut stream, &pong_packet(1, NONCE), pong_chunks).await;
                }
            }
            MockMode::WrongPongId | MockMode::WrongPongNonce => {
                write_chunks(
                    &mut stream,
                    &status_packet(&valid_json(false)),
                    Chunking::Whole,
                )
                .await;
                let ping_frame = frames.next(&mut stream).await;
                let ping = split_raw_packet(&ping_frame).unwrap();
                assert_eq!(ping.id, 1);
                let (id, nonce) = match mode {
                    MockMode::WrongPongId => (2, NONCE),
                    MockMode::WrongPongNonce => (1, NONCE + 1),
                    _ => unreachable!(),
                };
                write_chunks(&mut stream, &pong_packet(id, nonce), Chunking::Whole).await;
            }
        }
        handshake
    });
    (address, task)
}

#[tokio::test(flavor = "current_thread")]
async fn successful_query_validates_logical_host_port_protocol_and_typed_json() {
    let (address, server) = spawn_mock(
        MockMode::Success {
            response_chunks: Chunking::Whole,
            pong_chunks: Chunking::Whole,
            coalesced_early_pong: false,
            padded_json: false,
        },
        "localhost",
        -1,
    )
    .await;
    let status = query_server_status(&address, &test_options(-1))
        .await
        .unwrap();
    assert_eq!(status.response.version.name, "Mock 1.0");
    assert_eq!(status.response.version.protocol, 999);
    assert_eq!(status.response.players.online, 0);
    assert_eq!(status.response.players.max, 20);
    assert_eq!(status.response.motd_preview(), Some("Hello 🌍"));
    assert!(status.response.favicon.is_some());
    assert!(status.response.additional_fields.contains_key("custom"));
    let observed = server.await.unwrap();
    assert_eq!(observed.host, "localhost");
    assert_eq!(observed.protocol, -1);
}

#[tokio::test(flavor = "current_thread")]
async fn fragmented_response_prefix_payload_and_pong_are_reassembled() {
    for (response_chunks, pong_chunks, padded_json) in [
        (Chunking::PrefixSplit, Chunking::Whole, true),
        (Chunking::ByteByByte, Chunking::Whole, false),
        (Chunking::Whole, Chunking::ByteByByte, false),
    ] {
        let (address, server) = spawn_mock(
            MockMode::Success {
                response_chunks,
                pong_chunks,
                coalesced_early_pong: false,
                padded_json,
            },
            "127.0.0.1",
            47,
        )
        .await;
        assert!(
            query_server_status(&address, &test_options(47))
                .await
                .is_ok()
        );
        server.await.unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn coalesced_complete_frames_are_retained_between_reads() {
    let (address, server) = spawn_mock(
        MockMode::Success {
            response_chunks: Chunking::Whole,
            pong_chunks: Chunking::Whole,
            coalesced_early_pong: true,
            padded_json: false,
        },
        "127.0.0.1",
        5,
    )
    .await;
    assert!(
        query_server_status(&address, &test_options(5))
            .await
            .is_ok()
    );
    server.await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn malformed_and_oversized_frames_are_structured_errors() {
    for mode in [MockMode::MalformedFrame, MockMode::OversizedFrame] {
        let (address, server) = spawn_mock(mode, "127.0.0.1", -1).await;
        let error = query_server_status(&address, &test_options(-1))
            .await
            .unwrap_err();
        match mode {
            MockMode::MalformedFrame => assert!(matches!(error, StatusQueryError::Framing(_))),
            MockMode::OversizedFrame => {
                assert!(matches!(
                    error,
                    StatusQueryError::StatusResponseTooLarge { .. }
                ))
            }
            _ => unreachable!(),
        }
        server.await.unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn wrong_status_id_string_json_and_schema_are_rejected() {
    for mode in [
        MockMode::WrongStatusId,
        MockMode::MalformedString,
        MockMode::MalformedJson,
        MockMode::MissingField,
    ] {
        let (address, server) = spawn_mock(mode, "127.0.0.1", -1).await;
        let error = query_server_status(&address, &test_options(-1))
            .await
            .unwrap_err();
        match mode {
            MockMode::WrongStatusId => assert!(matches!(
                error,
                StatusQueryError::Protocol(StatusProtocolError::UnexpectedPacketId { .. })
            )),
            MockMode::MalformedString => assert!(matches!(
                error,
                StatusQueryError::Protocol(StatusProtocolError::Codec(_))
            )),
            MockMode::MalformedJson => assert!(matches!(
                error,
                StatusQueryError::Protocol(StatusProtocolError::MalformedJson { .. })
            )),
            MockMode::MissingField => assert!(matches!(
                error,
                StatusQueryError::Protocol(StatusProtocolError::InvalidStatusData)
            )),
            _ => unreachable!(),
        }
        server.await.unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn wrong_pong_id_and_nonce_are_rejected() {
    for mode in [MockMode::WrongPongId, MockMode::WrongPongNonce] {
        let (address, server) = spawn_mock(mode, "127.0.0.1", -1).await;
        let error = query_server_status(&address, &test_options(-1))
            .await
            .unwrap_err();
        match mode {
            MockMode::WrongPongId => assert!(matches!(
                error,
                StatusQueryError::Protocol(StatusProtocolError::UnexpectedPacketId { .. })
            )),
            MockMode::WrongPongNonce => assert!(matches!(
                error,
                StatusQueryError::Protocol(StatusProtocolError::PongMismatch { .. })
            )),
            _ => unreachable!(),
        }
        server.await.unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn early_and_partial_disconnects_are_distinguished() {
    for (mode, expected_buffered) in [
        (MockMode::DisconnectBeforeResponse, 0),
        (MockMode::DisconnectDuringFrame, 2),
    ] {
        let (address, server) = spawn_mock(mode, "127.0.0.1", -1).await;
        assert!(matches!(
            query_server_status(&address, &test_options(-1)).await,
            Err(StatusQueryError::PrematureDisconnect { buffered_bytes, .. })
                if buffered_bytes == expected_buffered
        ));
        server.await.unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn connect_read_and_overall_timeouts_are_bounded() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = ServerAddress::from_str(&listener.local_addr().unwrap().to_string()).unwrap();
    let zero_connect = StatusQueryOptions {
        connect_timeout: Duration::ZERO,
        overall_timeout: Duration::from_secs(1),
        ..test_options(-1)
    };
    assert!(matches!(
        query_server_status(&address, &zero_connect).await,
        Err(StatusQueryError::ConnectTimeout { .. })
    ));
    drop(listener);

    let (address, server) = spawn_mock(MockMode::ReadTimeout, "127.0.0.1", -1).await;
    let read_timeout = StatusQueryOptions {
        io_timeout: Duration::from_millis(20),
        overall_timeout: Duration::from_secs(1),
        ..test_options(-1)
    };
    assert!(matches!(
        query_server_status(&address, &read_timeout).await,
        Err(StatusQueryError::IoTimeout {
            operation: "Status Response",
            ..
        })
    ));
    server.await.unwrap();

    let (address, server) = spawn_mock(MockMode::ReadTimeout, "127.0.0.1", -1).await;
    let overall = StatusQueryOptions {
        io_timeout: Duration::from_secs(1),
        overall_timeout: Duration::from_millis(20),
        ..test_options(-1)
    };
    assert!(matches!(
        query_server_status(&address, &overall).await,
        Err(StatusQueryError::OverallTimeout { .. })
    ));
    server.abort();
}

struct MockFrameReader {
    decoder: FrameDecoder,
}

impl MockFrameReader {
    fn new() -> Self {
        Self {
            decoder: FrameDecoder::new(FrameLimits::new(128 * 1024, 256 * 1024).unwrap()),
        }
    }

    async fn next(&mut self, stream: &mut TcpStream) -> Vec<u8> {
        let mut buffer = [0_u8; 4096];
        loop {
            if let Some(frame) = self.decoder.next_frame().unwrap() {
                return frame;
            }
            let count = stream.read(&mut buffer).await.unwrap();
            assert_ne!(
                count, 0,
                "client disconnected before sending expected frame"
            );
            self.decoder.push(&buffer[..count]).unwrap();
        }
    }
}

fn decode_handshake(frame: &[u8]) -> ObservedHandshake {
    let packet = split_raw_packet(frame).unwrap();
    assert_eq!(packet.id, 0);
    let mut reader = CodecReader::new(packet.payload);
    let protocol = reader.read_var_int().unwrap();
    let host = reader
        .read_string(StringLimits::new(255, 765))
        .unwrap()
        .to_owned();
    let port = reader.read_u16().unwrap();
    let next_state = reader.read_var_int().unwrap();
    assert_eq!(reader.remaining(), 0);
    ObservedHandshake {
        protocol,
        host,
        port,
        next_state,
    }
}

fn valid_json(padded: bool) -> String {
    let padding = if padded {
        "x".repeat(200)
    } else {
        String::new()
    };
    format!(
        r#"{{"version":{{"name":"Mock 1.0","protocol":999}},"players":{{"max":20,"online":0,"sample":[]}},"description":"Hello 🌍","favicon":"data:image/png;base64,AA==","custom":{{"padding":"{padding}"}}}}"#
    )
}

fn status_packet(json: &str) -> Vec<u8> {
    let mut payload = CodecWriter::new();
    payload
        .write_string(json, StringLimits::new(32_767, 98_301))
        .unwrap();
    packet(0, payload.as_slice())
}

fn pong_packet(id: i32, nonce: i64) -> Vec<u8> {
    let mut payload = CodecWriter::new();
    payload.write_i64(nonce);
    packet(id, payload.as_slice())
}

fn packet(id: i32, payload: &[u8]) -> Vec<u8> {
    let mut body = CodecWriter::new();
    body.write_var_int(id);
    body.write_bytes(payload);
    encode_frame(body.as_slice(), MAX_STATUS_FRAME_SIZE).unwrap()
}

async fn write_chunks(stream: &mut TcpStream, bytes: &[u8], chunking: Chunking) {
    match chunking {
        Chunking::Whole => stream.write_all(bytes).await.unwrap(),
        Chunking::PrefixSplit => {
            let first = bytes.get(..1).unwrap();
            let rest = bytes.get(1..).unwrap();
            stream.write_all(first).await.unwrap();
            tokio::task::yield_now().await;
            stream.write_all(rest).await.unwrap();
        }
        Chunking::ByteByByte => {
            for byte in bytes {
                stream.write_all(std::slice::from_ref(byte)).await.unwrap();
                tokio::task::yield_now().await;
            }
        }
    }
}
