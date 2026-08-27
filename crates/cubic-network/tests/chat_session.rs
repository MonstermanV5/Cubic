use std::{str::FromStr, time::Duration};

use cubic_core::{ChatEvent, ChatMessageKind};
use cubic_network::{
    ChatSessionHandle, ChatSessionOptions, DevelopmentLoginOptions, DevelopmentUsername,
    ServerAddress, run_development_chat_session,
};
use cubic_protocol::{
    CodecReader, CodecWriter, FrameDecoder, FrameLimits, ProtocolUuid, StringLimits, encode_frame,
    split_raw_packet,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};

const TEST_UUID: u128 = 0x0011_2233_4455_6677_8899_aabb_ccdd_eeff;

fn options() -> ChatSessionOptions {
    ChatSessionOptions {
        login: DevelopmentLoginOptions {
            connect_timeout: Duration::from_secs(1),
            io_timeout: Duration::from_secs(1),
            overall_timeout: Duration::from_secs(3),
        },
        event_capacity: 16,
        command_capacity: 4,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn persistent_session_handles_control_chat_and_outgoing_unsigned_message() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let address = ServerAddress::from_str(&format!("127.0.0.1:{port}")).unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        bootstrap_to_play(&mut stream).await;
        let mut reader = MockFrameReader::new();
        assert_eq!(
            split_raw_packet(&reader.next(&mut stream).await)
                .unwrap()
                .id,
            0x0e
        );

        write_packet(&mut stream, 0x2c, 55_i64.to_be_bytes().to_vec()).await;
        assert_i64(&reader.next(&mut stream).await, 0x1c, 55);
        write_packet(&mut stream, 0x3d, 77_i32.to_be_bytes().to_vec()).await;
        assert_i32(&reader.next(&mut stream).await, 0x2d, 77);

        let mut position = CodecWriter::new();
        position.write_var_int(300);
        position.write_bytes(&[0; 60]);
        write_packet(&mut stream, 0x48, position.into_inner()).await;
        let confirmation = reader.next(&mut stream).await;
        let packet = split_raw_packet(&confirmation).unwrap();
        assert_eq!(packet.id, 0);
        assert_eq!(
            CodecReader::new(packet.payload).read_var_int().unwrap(),
            300
        );

        write_packet(&mut stream, 0x0b, vec![2]).await;
        assert_eq!(
            split_raw_packet(&reader.next(&mut stream).await)
                .unwrap()
                .id,
            0x0b
        );
        assert_eq!(
            split_raw_packet(&reader.next(&mut stream).await)
                .unwrap()
                .id,
            0x2c
        );

        let mut system = vec![0x79, 8, 0, 6];
        system.extend_from_slice("héllo".as_bytes());
        system.push(0);
        let framed = encode_frame(&system, 2_097_151).unwrap();
        for byte in framed {
            stream.write_all(&[byte]).await.unwrap();
        }

        let mut player = CodecWriter::new();
        player.write_var_int(0x41);
        player.write_var_int(0);
        player.write_uuid(ProtocolUuid::from_u128(TEST_UUID));
        player.write_var_int(0);
        player.write_bool(false);
        player
            .write_string("player hello", StringLimits::new(256, 768))
            .unwrap();
        player.write_i64(1);
        player.write_i64(2);
        player.write_var_int(0);
        player.write_bool(false);
        player.write_var_int(0);
        player.write_var_int(1);
        player.write_bytes(&[8, 0, 5]);
        player.write_bytes(b"Alice");
        player.write_bool(false);
        stream
            .write_all(&encode_frame(player.as_slice(), 2_097_151).unwrap())
            .await
            .unwrap();
        let outgoing = reader.next(&mut stream).await;
        let packet = split_raw_packet(&outgoing).unwrap();
        assert_eq!(packet.id, 0x09);
        let mut fields = CodecReader::new(packet.payload);
        assert_eq!(
            fields.read_string(StringLimits::new(256, 768)).unwrap(),
            "from Cubic"
        );
        let _timestamp = fields.read_i64().unwrap();
        let _salt = fields.read_i64().unwrap();
        assert!(!fields.read_bool().unwrap());
        assert_eq!(fields.read_var_int().unwrap(), 0);
        assert_eq!(
            fields.read_bytes(3, "fixed acknowledgement").unwrap(),
            [0, 0, 0]
        );
        assert_eq!(fields.read_u8().unwrap(), 0);
        assert_eq!(fields.remaining(), 0);
        write_packet(&mut stream, 0x20, vec![8, 0, 4, b'd', b'o', b'n', b'e']).await;
    });

    let (mut handle, runner) = ChatSessionHandle::bounded(&options());
    let client_address = address.clone();
    let client = tokio::spawn(async move {
        run_development_chat_session(
            &client_address,
            &DevelopmentUsername::new("CubicTest").unwrap(),
            &options(),
            runner,
        )
        .await
    });

    wait_for(&mut handle, |event| matches!(event, ChatEvent::Connected)).await;
    let chat = wait_for(&mut handle, |event| {
        matches!(event, ChatEvent::Message { .. })
    })
    .await;
    let ChatEvent::Message { kind, message, .. } = chat else {
        unreachable!()
    };
    assert_eq!(kind, ChatMessageKind::System);
    assert_eq!(message.plain_text, "héllo");
    let player_chat = wait_for(&mut handle, |event| {
        matches!(
            event,
            ChatEvent::Message {
                kind: ChatMessageKind::Player,
                ..
            }
        )
    })
    .await;
    let ChatEvent::Message {
        sender, message, ..
    } = player_chat
    else {
        unreachable!()
    };
    assert_eq!(sender.as_deref(), Some("Alice"));
    assert_eq!(message.plain_text, "player hello");
    handle.try_send_message("from Cubic".to_owned()).unwrap();
    let disconnected = wait_for(&mut handle, |event| {
        matches!(event, ChatEvent::Disconnected { .. })
    })
    .await;
    assert_eq!(
        disconnected,
        ChatEvent::Disconnected {
            reason: "done".to_owned()
        }
    );
    server.await.unwrap();
    assert!(client.await.unwrap().is_ok());
}

async fn bootstrap_to_play(stream: &mut TcpStream) {
    let mut reader = MockFrameReader::new();
    let handshake = reader.next(stream).await;
    let packet = split_raw_packet(&handshake).unwrap();
    assert_eq!(packet.id, 0);
    let mut fields = CodecReader::new(packet.payload);
    assert_eq!(fields.read_var_int().unwrap(), 775);
    let _host = fields.read_string(StringLimits::new(255, 765)).unwrap();
    let _port = fields.read_u16().unwrap();
    assert_eq!(fields.read_var_int().unwrap(), 2);
    let login = reader.next(stream).await;
    assert_eq!(split_raw_packet(&login).unwrap().id, 0);

    let mut success = CodecWriter::new();
    success.write_uuid(ProtocolUuid::from_u128(TEST_UUID));
    success
        .write_string("CubicTest", StringLimits::new(16, 48))
        .unwrap();
    success.write_var_int(0);
    write_packet(stream, 0x02, success.into_inner()).await;
    assert_eq!(
        split_raw_packet(&reader.next(stream).await).unwrap().id,
        0x03
    );
    assert_eq!(
        split_raw_packet(&reader.next(stream).await).unwrap().id,
        0x00
    );
    write_packet(stream, 0x03, Vec::new()).await;
    assert_eq!(
        split_raw_packet(&reader.next(stream).await).unwrap().id,
        0x03
    );
    write_packet(stream, 0x31, vec![1]).await;
}

async fn wait_for(
    handle: &mut ChatSessionHandle,
    predicate: impl Fn(&ChatEvent) -> bool,
) -> ChatEvent {
    timeout(Duration::from_secs(2), async {
        loop {
            if let Some(event) = handle.take_critical_event()
                && predicate(&event)
            {
                return event;
            }
            if let Some(event) = handle.try_next_event()
                && predicate(&event)
            {
                return event;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
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
            assert_ne!(count, 0);
            self.decoder.push(&buffer[..count]).unwrap();
        }
    }
}

async fn write_packet(stream: &mut TcpStream, id: i32, payload: Vec<u8>) {
    let mut body = CodecWriter::new();
    body.write_var_int(id);
    body.write_bytes(&payload);
    stream
        .write_all(&encode_frame(body.as_slice(), 2_097_151).unwrap())
        .await
        .unwrap();
}

fn assert_i64(frame: &[u8], id: i32, value: i64) {
    let packet = split_raw_packet(frame).unwrap();
    assert_eq!(packet.id, id);
    assert_eq!(CodecReader::new(packet.payload).read_i64().unwrap(), value);
}

fn assert_i32(frame: &[u8], id: i32, value: i32) {
    let packet = split_raw_packet(frame).unwrap();
    assert_eq!(packet.id, id);
    assert_eq!(CodecReader::new(packet.payload).read_i32().unwrap(), value);
}
