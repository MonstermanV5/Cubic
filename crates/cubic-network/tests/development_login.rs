use std::{str::FromStr, time::Duration};

use cubic_network::{
    ConnectionState, DevelopmentLoginError, DevelopmentLoginOptions, DevelopmentUsername,
    ServerAddress, UnsupportedPhase7Feature, development_login,
};
use cubic_protocol::{
    CodecReader, CodecWriter, FrameDecoder, FrameLimits, StringLimits, encode_frame,
    split_raw_packet,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
    time::sleep,
};

const TEST_UUID: u128 = 0x0011_2233_4455_6677_8899_aabb_ccdd_eeff;

#[derive(Clone, Copy)]
enum MockMode {
    Success {
        fragmented: bool,
        coalesced_login: bool,
    },
    LoginDisconnect,
    Encryption,
    Compression,
    UnexpectedLogin,
    MalformedLoginSuccess,
    EofLogin,
    TimeoutLogin,
    ConfigurationDisconnect,
    UnexpectedConfiguration,
    EofConfiguration,
    TimeoutConfiguration,
    OversizedFrame,
    EarlyPlayTraffic,
    EarlyReconfiguration,
    PlayAcceptanceDisconnect,
}

#[derive(Debug, Eq, PartialEq)]
struct Observation {
    protocol: i32,
    host: String,
    port: u16,
    next_state: i32,
    username: String,
    supplied_uuid: u128,
}

fn test_options() -> DevelopmentLoginOptions {
    DevelopmentLoginOptions {
        connect_timeout: Duration::from_millis(500),
        io_timeout: Duration::from_millis(100),
        overall_timeout: Duration::from_secs(2),
    }
}

async fn spawn_mock(mode: MockMode) -> (ServerAddress, JoinHandle<Observation>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let address = ServerAddress::from_str(&format!("127.0.0.1:{port}")).unwrap();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut reader = MockFrameReader::new();
        let (protocol, host, observed_port, next_state) =
            decode_handshake(&reader.next(&mut stream).await);
        let (username, supplied_uuid) = decode_login_start(&reader.next(&mut stream).await);
        let observation = Observation {
            protocol,
            host,
            port: observed_port,
            next_state,
            username,
            supplied_uuid,
        };

        match mode {
            MockMode::LoginDisconnect => {
                write_packet(&mut stream, 0x00, string_payload(r#"{"text":"no"}"#)).await;
                return observation;
            }
            MockMode::Encryption => {
                write_packet(
                    &mut stream,
                    0x01,
                    vec![0x00, 0x01, 0x30, 0x04, 1, 2, 3, 4, 0x01],
                )
                .await;
                return observation;
            }
            MockMode::Compression => {
                write_packet(&mut stream, 0x03, vec![0x00]).await;
                return observation;
            }
            MockMode::UnexpectedLogin => {
                write_packet(&mut stream, 0x7f, Vec::new()).await;
                return observation;
            }
            MockMode::MalformedLoginSuccess => {
                write_packet(&mut stream, 0x02, vec![0x00]).await;
                return observation;
            }
            MockMode::EofLogin => return observation,
            MockMode::TimeoutLogin => {
                sleep(Duration::from_millis(300)).await;
                return observation;
            }
            MockMode::OversizedFrame => {
                let mut prefix = CodecWriter::new();
                prefix.write_var_int(2_097_152);
                stream.write_all(prefix.as_slice()).await.unwrap();
                return observation;
            }
            _ => {}
        }

        let login_success = packet(0x02, login_success_payload(&observation.username));
        match mode {
            MockMode::Success {
                fragmented,
                coalesced_login: true,
            } => {
                let mut plugin = packet(0x04, plugin_request_payload());
                plugin.extend_from_slice(&login_success);
                write_bytes(&mut stream, &plugin, fragmented).await;
                assert_plugin_response(&reader.next(&mut stream).await);
            }
            MockMode::Success {
                fragmented,
                coalesced_login: false,
            } => write_bytes(&mut stream, &login_success, fragmented).await,
            _ => stream.write_all(&login_success).await.unwrap(),
        }
        assert_packet_id(&reader.next(&mut stream).await, 0x03);
        assert_client_information(&reader.next(&mut stream).await);

        match mode {
            MockMode::ConfigurationDisconnect => {
                write_packet(&mut stream, 0x02, vec![0x0a, 0x00]).await;
                return observation;
            }
            MockMode::UnexpectedConfiguration => {
                write_packet(&mut stream, 0x7f, Vec::new()).await;
                return observation;
            }
            MockMode::EofConfiguration => return observation,
            MockMode::TimeoutConfiguration => {
                sleep(Duration::from_millis(300)).await;
                return observation;
            }
            _ => {}
        }

        let fragmented = match mode {
            MockMode::Success { fragmented, .. } => fragmented,
            MockMode::EarlyPlayTraffic
            | MockMode::EarlyReconfiguration
            | MockMode::PlayAcceptanceDisconnect => false,
            _ => return observation,
        };
        let mut configuration = packet(0x01, custom_payload());
        configuration.extend_from_slice(&packet(0x07, vec![0xaa, 0xbb]));
        configuration.extend_from_slice(&packet(0x0e, known_packs_payload()));
        configuration.extend_from_slice(&packet(
            0x04,
            0x0102_0304_0506_0708_i64.to_be_bytes().to_vec(),
        ));
        configuration.extend_from_slice(&packet(0x05, 0x0102_0304_i32.to_be_bytes().to_vec()));
        configuration.extend_from_slice(&packet(0x03, Vec::new()));
        write_bytes(&mut stream, &configuration, fragmented).await;

        assert_known_packs_response(&reader.next(&mut stream).await);
        assert_i64_response(&reader.next(&mut stream).await, 0x04, 0x0102_0304_0506_0708);
        assert_i32_response(&reader.next(&mut stream).await, 0x05, 0x0102_0304);
        assert_packet_id(&reader.next(&mut stream).await, 0x03);
        match mode {
            MockMode::EarlyPlayTraffic => {
                let mut early = packet(0x18, custom_payload());
                early.extend_from_slice(&packet(
                    0x2c,
                    0x1122_3344_5566_7788_i64.to_be_bytes().to_vec(),
                ));
                early.extend_from_slice(&packet(0x3d, 0x1234_5678_i32.to_be_bytes().to_vec()));
                let mut position = vec![0x2a];
                position.extend_from_slice(&[0_u8; 60]);
                early.extend_from_slice(&packet(0x48, position));
                early.extend_from_slice(&packet(0x15, identifier_payload("minecraft:test")));
                stream.write_all(&early).await.unwrap();

                assert_i64_response(&reader.next(&mut stream).await, 0x1c, 0x1122_3344_5566_7788);
                assert_i32_response(&reader.next(&mut stream).await, 0x2d, 0x1234_5678);
                assert_var_int_response(&reader.next(&mut stream).await, 0x00, 42);
                assert_cookie_response(&reader.next(&mut stream).await, "minecraft:test");
            }
            MockMode::PlayAcceptanceDisconnect => {
                write_packet(
                    &mut stream,
                    0x20,
                    vec![
                        8, 0, 9, b'g', b'o', b' ', b'a', b'w', b'a', b'y', b'!', b'!',
                    ],
                )
                .await;
                return observation;
            }
            MockMode::EarlyReconfiguration => {
                write_packet(&mut stream, 0x76, Vec::new()).await;
                assert_packet_id(&reader.next(&mut stream).await, 0x10);
                assert_client_information(&reader.next(&mut stream).await);
                write_packet(&mut stream, 0x03, Vec::new()).await;
                assert_packet_id(&reader.next(&mut stream).await, 0x03);
            }
            _ => {}
        }
        write_packet(&mut stream, 0x31, vec![0x01]).await;
        observation
    });
    (address, task)
}

#[tokio::test(flavor = "current_thread")]
async fn successful_offline_login_reaches_play_and_validates_outbound_fields() {
    let (address, server) = spawn_mock(MockMode::Success {
        fragmented: false,
        coalesced_login: false,
    })
    .await;
    let username = DevelopmentUsername::new("CubicTest").unwrap();
    let result = development_login(&address, &username, &test_options())
        .await
        .unwrap();
    assert_eq!(result.minecraft_version.as_str(), "26.1.2");
    assert_eq!(result.protocol_version.value(), 775);
    assert_eq!(result.profile_uuid.as_u128(), TEST_UUID);
    assert_eq!(result.state, ConnectionState::Play);
    assert_eq!(result.skipped_configuration_packets, 2);
    let observed = server.await.unwrap();
    assert_eq!(
        observed,
        Observation {
            protocol: 775,
            host: "127.0.0.1".to_owned(),
            port: address.port(),
            next_state: 2,
            username: "CubicTest".to_owned(),
            supplied_uuid: 0,
        }
    );
}

#[tokio::test(flavor = "current_thread")]
async fn early_irrelevant_play_packet_is_skipped_and_controls_are_answered() {
    let (address, server) = spawn_mock(MockMode::EarlyPlayTraffic).await;
    let result = development_login(
        &address,
        &DevelopmentUsername::new("EarlyPlay").unwrap(),
        &test_options(),
    )
    .await;
    assert!(result.is_ok());
    server.await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn disconnect_during_play_acceptance_retains_the_reason() {
    let (address, server) = spawn_mock(MockMode::PlayAcceptanceDisconnect).await;
    assert!(matches!(
        development_login(
            &address,
            &DevelopmentUsername::new("PlayBye").unwrap(),
            &test_options(),
        )
        .await,
        Err(DevelopmentLoginError::ServerDisconnect {
            state: "Play",
            reason
        }) if reason == "go away!!"
    ));
    server.await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn reconfiguration_during_play_acceptance_returns_to_play_cleanly() {
    let (address, server) = spawn_mock(MockMode::EarlyReconfiguration).await;
    let result = development_login(
        &address,
        &DevelopmentUsername::new("Reconfigure").unwrap(),
        &test_options(),
    )
    .await;
    assert!(result.is_ok());
    server.await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn fragmented_login_and_configuration_packets_are_reassembled() {
    let (address, server) = spawn_mock(MockMode::Success {
        fragmented: true,
        coalesced_login: false,
    })
    .await;
    let result = development_login(
        &address,
        &DevelopmentUsername::new("Fragmented").unwrap(),
        &test_options(),
    )
    .await;
    assert!(result.is_ok());
    server.await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn coalesced_login_packets_are_retained_and_plugin_request_is_declined() {
    let (address, server) = spawn_mock(MockMode::Success {
        fragmented: false,
        coalesced_login: true,
    })
    .await;
    assert!(
        development_login(
            &address,
            &DevelopmentUsername::new("Coalesced").unwrap(),
            &test_options(),
        )
        .await
        .is_ok()
    );
    server.await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn login_disconnect_is_a_bounded_structured_error() {
    let (address, server) = spawn_mock(MockMode::LoginDisconnect).await;
    assert!(matches!(
        development_login(
            &address,
            &DevelopmentUsername::new("Tester").unwrap(),
            &test_options()
        )
        .await,
        Err(DevelopmentLoginError::ServerDisconnect { state: "Login", .. })
    ));
    server.await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn encryption_and_compression_explain_required_server_settings() {
    for (mode, feature) in [
        (MockMode::Encryption, UnsupportedPhase7Feature::Encryption),
        (MockMode::Compression, UnsupportedPhase7Feature::Compression),
    ] {
        let (address, server) = spawn_mock(mode).await;
        assert!(matches!(
            development_login(&address, &DevelopmentUsername::new("Tester").unwrap(), &test_options()).await,
            Err(DevelopmentLoginError::UnsupportedForPhase7 { feature: found, .. }) if found == feature
        ));
        server.await.unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn unexpected_and_malformed_login_packets_are_rejected() {
    for mode in [MockMode::UnexpectedLogin, MockMode::MalformedLoginSuccess] {
        let (address, server) = spawn_mock(mode).await;
        assert!(matches!(
            development_login(
                &address,
                &DevelopmentUsername::new("Tester").unwrap(),
                &test_options()
            )
            .await,
            Err(DevelopmentLoginError::Protocol(_))
        ));
        server.await.unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn configuration_disconnect_and_unexpected_packet_are_rejected() {
    for mode in [
        MockMode::ConfigurationDisconnect,
        MockMode::UnexpectedConfiguration,
    ] {
        let (address, server) = spawn_mock(mode).await;
        let error = development_login(
            &address,
            &DevelopmentUsername::new("Tester").unwrap(),
            &test_options(),
        )
        .await
        .unwrap_err();
        match mode {
            MockMode::ConfigurationDisconnect => assert!(matches!(
                error,
                DevelopmentLoginError::ServerDisconnect {
                    state: "Configuration",
                    ..
                }
            )),
            MockMode::UnexpectedConfiguration => {
                assert!(matches!(error, DevelopmentLoginError::Protocol(_)))
            }
            _ => unreachable!(),
        }
        server.await.unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn eof_is_attributed_to_login_or_configuration() {
    for (mode, expected_phase) in [
        (MockMode::EofLogin, "Login packet read"),
        (MockMode::EofConfiguration, "Configuration packet read"),
    ] {
        let (address, server) = spawn_mock(mode).await;
        assert!(matches!(
            development_login(&address, &DevelopmentUsername::new("Tester").unwrap(), &test_options()).await,
            Err(DevelopmentLoginError::PrematureDisconnect { phase, .. }) if phase == expected_phase
        ));
        server.await.unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn individual_login_and_configuration_reads_time_out() {
    for (mode, expected_operation) in [
        (MockMode::TimeoutLogin, "Login packet read"),
        (MockMode::TimeoutConfiguration, "Configuration packet read"),
    ] {
        let (address, server) = spawn_mock(mode).await;
        let options = DevelopmentLoginOptions {
            io_timeout: Duration::from_millis(20),
            ..test_options()
        };
        assert!(matches!(
            development_login(&address, &DevelopmentUsername::new("Tester").unwrap(), &options).await,
            Err(DevelopmentLoginError::IoTimeout { operation, .. }) if operation == expected_operation
        ));
        server.await.unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn overall_timeout_bounds_the_whole_sequence() {
    let (address, server) = spawn_mock(MockMode::TimeoutLogin).await;
    let options = DevelopmentLoginOptions {
        io_timeout: Duration::from_secs(1),
        overall_timeout: Duration::from_millis(20),
        ..test_options()
    };
    assert!(matches!(
        development_login(
            &address,
            &DevelopmentUsername::new("Tester").unwrap(),
            &options
        )
        .await,
        Err(DevelopmentLoginError::OverallTimeout { .. })
    ));
    server.abort();
}

#[tokio::test(flavor = "current_thread")]
async fn oversized_server_frame_is_rejected_before_payload_allocation() {
    let (address, server) = spawn_mock(MockMode::OversizedFrame).await;
    assert!(matches!(
        development_login(
            &address,
            &DevelopmentUsername::new("Tester").unwrap(),
            &test_options()
        )
        .await,
        Err(DevelopmentLoginError::Framing(_))
    ));
    server.await.unwrap();
}

struct MockFrameReader {
    decoder: FrameDecoder,
}

impl MockFrameReader {
    fn new() -> Self {
        Self {
            decoder: FrameDecoder::new(FrameLimits::new(2_097_151, 4 * 1024 * 1024).unwrap()),
        }
    }

    async fn next(&mut self, stream: &mut TcpStream) -> Vec<u8> {
        let mut buffer = [0_u8; 4096];
        loop {
            if let Some(frame) = self.decoder.next_frame().unwrap() {
                return frame;
            }
            let count = stream.read(&mut buffer).await.unwrap();
            assert_ne!(count, 0, "client disconnected before expected packet");
            self.decoder.push(&buffer[..count]).unwrap();
        }
    }
}

fn decode_handshake(frame: &[u8]) -> (i32, String, u16, i32) {
    let packet = split_raw_packet(frame).unwrap();
    assert_eq!(packet.id, 0);
    let mut reader = CodecReader::new(packet.payload);
    let protocol = reader.read_var_int().unwrap();
    let host = reader
        .read_string(StringLimits::new(255, 765))
        .unwrap()
        .to_owned();
    let port = reader.read_u16().unwrap();
    let state = reader.read_var_int().unwrap();
    assert_eq!(reader.remaining(), 0);
    (protocol, host, port, state)
}

fn decode_login_start(frame: &[u8]) -> (String, u128) {
    let packet = split_raw_packet(frame).unwrap();
    assert_eq!(packet.id, 0);
    let mut reader = CodecReader::new(packet.payload);
    let username = reader
        .read_string(StringLimits::new(16, 48))
        .unwrap()
        .to_owned();
    let uuid = reader.read_uuid().unwrap().as_u128();
    assert_eq!(reader.remaining(), 0);
    (username, uuid)
}

fn login_success_payload(username: &str) -> Vec<u8> {
    let mut writer = CodecWriter::new();
    writer.write_uuid(cubic_protocol::ProtocolUuid::from_u128(TEST_UUID));
    writer
        .write_string(username, StringLimits::new(16, 48))
        .unwrap();
    writer.write_var_int(0);
    writer.into_inner()
}

fn plugin_request_payload() -> Vec<u8> {
    let mut writer = CodecWriter::new();
    writer.write_var_int(9);
    writer
        .write_string("test:probe", StringLimits::new(32, 96))
        .unwrap();
    writer.write_bytes(&[1, 2, 3]);
    writer.into_inner()
}

fn custom_payload() -> Vec<u8> {
    let mut writer = CodecWriter::new();
    writer
        .write_string("minecraft:brand", StringLimits::new(32, 96))
        .unwrap();
    writer.write_bytes(b"mock");
    writer.into_inner()
}

fn identifier_payload(value: &str) -> Vec<u8> {
    let mut writer = CodecWriter::new();
    writer
        .write_string(value, StringLimits::new(32, 96))
        .unwrap();
    writer.into_inner()
}

fn known_packs_payload() -> Vec<u8> {
    let limits = StringLimits::new(64, 192);
    let mut writer = CodecWriter::new();
    writer.write_var_int(1);
    writer.write_string("minecraft", limits).unwrap();
    writer.write_string("core", limits).unwrap();
    writer.write_string("26.1.2", limits).unwrap();
    writer.into_inner()
}

fn string_payload(value: &str) -> Vec<u8> {
    let mut writer = CodecWriter::new();
    writer
        .write_string(value, StringLimits::new(1024, 3072))
        .unwrap();
    writer.into_inner()
}

fn packet(id: i32, payload: Vec<u8>) -> Vec<u8> {
    let mut body = CodecWriter::new();
    body.write_var_int(id);
    body.write_bytes(&payload);
    encode_frame(body.as_slice(), 2 * 1024 * 1024).unwrap()
}

async fn write_packet(stream: &mut TcpStream, id: i32, payload: Vec<u8>) {
    stream.write_all(&packet(id, payload)).await.unwrap();
}

async fn write_bytes(stream: &mut TcpStream, bytes: &[u8], fragmented: bool) {
    if fragmented {
        for byte in bytes {
            stream.write_all(std::slice::from_ref(byte)).await.unwrap();
            tokio::task::yield_now().await;
        }
    } else {
        stream.write_all(bytes).await.unwrap();
    }
}

fn assert_packet_id(frame: &[u8], expected: i32) {
    let packet = split_raw_packet(frame).unwrap();
    assert_eq!(packet.id, expected);
    assert!(packet.payload.is_empty());
}

fn assert_plugin_response(frame: &[u8]) {
    let packet = split_raw_packet(frame).unwrap();
    assert_eq!(packet.id, 0x02);
    let mut reader = CodecReader::new(packet.payload);
    assert_eq!(reader.read_var_int().unwrap(), 9);
    assert!(!reader.read_bool().unwrap());
    assert_eq!(reader.remaining(), 0);
}

fn assert_client_information(frame: &[u8]) {
    let packet = split_raw_packet(frame).unwrap();
    assert_eq!(packet.id, 0x00);
    let mut reader = CodecReader::new(packet.payload);
    assert_eq!(
        reader.read_string(StringLimits::new(16, 48)).unwrap(),
        "en_us"
    );
    assert_eq!(reader.read_i8().unwrap(), 8);
    assert_eq!(reader.read_var_int().unwrap(), 0);
    assert!(reader.read_bool().unwrap());
    assert_eq!(reader.read_u8().unwrap(), 0x7f);
    assert_eq!(reader.read_var_int().unwrap(), 1);
    assert!(!reader.read_bool().unwrap());
    assert!(reader.read_bool().unwrap());
    assert_eq!(reader.read_var_int().unwrap(), 0);
    assert_eq!(reader.remaining(), 0);
}

fn assert_known_packs_response(frame: &[u8]) {
    let packet = split_raw_packet(frame).unwrap();
    assert_eq!(packet.id, 0x07);
    assert_eq!(packet.payload, [0x00]);
}

fn assert_i64_response(frame: &[u8], expected_id: i32, expected_value: i64) {
    let packet = split_raw_packet(frame).unwrap();
    assert_eq!(packet.id, expected_id);
    let mut reader = CodecReader::new(packet.payload);
    assert_eq!(reader.read_i64().unwrap(), expected_value);
    assert_eq!(reader.remaining(), 0);
}

fn assert_i32_response(frame: &[u8], expected_id: i32, expected_value: i32) {
    let packet = split_raw_packet(frame).unwrap();
    assert_eq!(packet.id, expected_id);
    let mut reader = CodecReader::new(packet.payload);
    assert_eq!(reader.read_i32().unwrap(), expected_value);
    assert_eq!(reader.remaining(), 0);
}

fn assert_var_int_response(frame: &[u8], expected_id: i32, expected_value: i32) {
    let packet = split_raw_packet(frame).unwrap();
    assert_eq!(packet.id, expected_id);
    let mut reader = CodecReader::new(packet.payload);
    assert_eq!(reader.read_var_int().unwrap(), expected_value);
    assert_eq!(reader.remaining(), 0);
}

fn assert_cookie_response(frame: &[u8], expected_key: &str) {
    let packet = split_raw_packet(frame).unwrap();
    assert_eq!(packet.id, 0x15);
    let mut reader = CodecReader::new(packet.payload);
    assert_eq!(
        reader.read_string(StringLimits::new(32, 96)).unwrap(),
        expected_key
    );
    assert!(!reader.read_bool().unwrap());
    assert_eq!(reader.remaining(), 0);
}
