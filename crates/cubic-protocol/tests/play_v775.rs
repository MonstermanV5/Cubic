use cubic_protocol::{
    CodecReader, CodecWriter, FrameDecoder, FrameLimits, StringLimits, bootstrap::v775,
};

fn body(frame: &[u8]) -> Vec<u8> {
    let mut decoder = FrameDecoder::new(FrameLimits::new(2_097_151, 4 * 1024 * 1024).unwrap());
    decoder.push(frame).unwrap();
    decoder.next_frame().unwrap().unwrap()
}

fn nbt_string(value: &str) -> Vec<u8> {
    let mut bytes = vec![8];
    bytes.extend_from_slice(&(value.len() as u16).to_be_bytes());
    bytes.extend_from_slice(value.as_bytes());
    bytes
}

#[test]
fn independent_system_chat_string_vector() {
    let mut packet = vec![0x79];
    packet.extend(nbt_string("Hello"));
    packet.push(0);
    let decoded = v775::decode_play_clientbound(&packet).unwrap();
    let v775::PlayClientbound::SystemChat { message, overlay } = decoded else {
        panic!("expected System Chat")
    };
    assert_eq!(message.plain_text, "Hello");
    assert!(!overlay);
}

#[test]
fn nested_text_and_unicode_project_safely() {
    let mut component = vec![10, 8, 0, 4];
    component.extend_from_slice(b"text");
    component.extend_from_slice(&[0, 2, b'H', b'i']);
    component.extend_from_slice(&[9, 0, 5]);
    component.extend_from_slice(b"extra");
    component.extend_from_slice(&[10, 0, 0, 0, 1, 8, 0, 4]);
    component.extend_from_slice(b"text");
    component.extend_from_slice(&[0, 4, 0xed, 0xa0, 0xbd, 0xed]);
    // Replace the deliberately awkward bytes with a valid simple suffix.
    component.truncate(component.len() - 6);
    component.extend_from_slice(&[0, 1, b'!']);
    component.extend_from_slice(&[0, 0]);
    let mut packet = vec![0x79];
    packet.extend(component);
    packet.push(0);
    let decoded = v775::decode_play_clientbound(&packet).unwrap();
    let v775::PlayClientbound::SystemChat { message, .. } = decoded else {
        panic!("expected System Chat")
    };
    assert_eq!(message.plain_text, "Hi!");
}

#[test]
fn independent_disguised_chat_vector() {
    let mut packet = vec![0x21];
    packet.extend(nbt_string("waves"));
    packet.push(1); // registry holder id 0 encoded as id + 1
    packet.extend(nbt_string("Alice"));
    packet.push(0); // no target
    let decoded = v775::decode_play_clientbound(&packet).unwrap();
    let v775::PlayClientbound::DisguisedChat {
        sender_name,
        message,
    } = decoded
    else {
        panic!("expected Disguised Chat")
    };
    assert_eq!(sender_name, "Alice");
    assert_eq!(message.plain_text, "waves");
}

#[test]
fn independent_player_chat_vector_tracks_sender_and_index() {
    let mut packet = vec![0x41, 0x07];
    packet.extend_from_slice(&[0; 16]);
    packet.extend_from_slice(&[0, 0, 5]);
    packet.extend_from_slice(b"hello");
    packet.extend_from_slice(&1_i64.to_be_bytes());
    packet.extend_from_slice(&2_i64.to_be_bytes());
    packet.extend_from_slice(&[0, 0, 0, 1]);
    packet.extend(nbt_string("Alice"));
    packet.push(0);
    let decoded = v775::decode_play_clientbound(&packet).unwrap();
    let v775::PlayClientbound::PlayerChat {
        global_index,
        sender_name,
        message,
        signature,
        ..
    } = decoded
    else {
        panic!("expected Player Chat")
    };
    assert_eq!(global_index, 7);
    assert_eq!(sender_name, "Alice");
    assert_eq!(message.plain_text, "hello");
    assert!(signature.is_none());
}

#[test]
fn player_chat_retains_signed_and_decorated_content_before_presentation() {
    let mut packet = vec![0x41, 0x07];
    packet.extend_from_slice(&[0; 16]);
    packet.extend_from_slice(&[0, 0, 5]);
    packet.extend_from_slice(b"hello");
    packet.extend_from_slice(&1_i64.to_be_bytes());
    packet.extend_from_slice(&2_i64.to_be_bytes());
    packet.extend_from_slice(&[0, 1]);
    packet.extend(nbt_string("[Rank] Alice:"));
    packet.extend_from_slice(&[0, 1]);
    packet.extend(nbt_string("Alice"));
    packet.push(0);
    let decoded = v775::decode_play_clientbound(&packet).unwrap();
    let v775::PlayClientbound::PlayerChat {
        signed_content,
        unsigned_content,
        ..
    } = decoded
    else {
        panic!("expected Player Chat")
    };
    assert_eq!(signed_content, "hello");
    assert_eq!(unsigned_content.unwrap().plain_text, "[Rank] Alice:");
}

#[test]
fn signed_player_chat_requires_acknowledgement_but_unsigned_chat_does_not() {
    let mut packet = vec![0x41, 0x07];
    packet.extend_from_slice(&[0; 16]);
    packet.extend_from_slice(&[0, 1]);
    packet.extend_from_slice(&[0x5a; 256]);
    packet.push(0);
    packet.extend_from_slice(&1_i64.to_be_bytes());
    packet.extend_from_slice(&2_i64.to_be_bytes());
    packet.extend_from_slice(&[0, 0, 0, 1]);
    packet.extend(nbt_string("Alice"));
    packet.push(0);

    let decoded = v775::decode_play_clientbound(&packet).unwrap();
    let v775::PlayClientbound::PlayerChat { signature, .. } = decoded else {
        panic!("expected Player Chat")
    };
    assert_eq!(signature.unwrap().bytes(), [0x5a; 256]);
}

#[test]
fn control_plane_vectors_are_exact() {
    assert_eq!(
        body(&v775::encode_play_keep_alive(7).unwrap()),
        [0x1c, 0, 0, 0, 0, 0, 0, 0, 7]
    );
    assert_eq!(
        body(&v775::encode_play_pong(42).unwrap()),
        [0x2d, 0, 0, 0, 42]
    );
    assert_eq!(
        body(&v775::encode_play_teleport_confirmation(300).unwrap()),
        [0, 0xac, 2]
    );
    assert_eq!(
        body(&v775::encode_play_chat_acknowledgement(2).unwrap()),
        [0x06, 0x02]
    );
    assert!(v775::encode_play_chat_acknowledgement(0).is_err());
    assert_eq!(body(&v775::encode_play_player_loaded().unwrap()), [0x2c]);
}

#[test]
fn outgoing_unsigned_chat_vector_and_validation() {
    let last_seen = v775::ChatLastSeenUpdate::new(300, [1, 2, 3], 0x7f).unwrap();
    let framed = v775::encode_play_chat_message("Hi", 1, 2, None, last_seen).unwrap();
    assert_eq!(framed[0], 27);
    let encoded = body(&framed);
    assert_eq!(
        encoded,
        [
            0x09, 0x02, b'H', b'i', 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0xac, 0x02,
            0x01, 0x02, 0x03, 0x7f,
        ]
    );
    let mut reader = CodecReader::new(&encoded);
    assert_eq!(reader.read_var_int().unwrap(), 0x09);
    assert_eq!(
        reader
            .read_string(cubic_protocol::StringLimits::new(256, 768))
            .unwrap(),
        "Hi"
    );
    assert_eq!(reader.read_i64().unwrap(), 1);
    assert_eq!(reader.read_i64().unwrap(), 2);
    assert!(!reader.read_bool().unwrap());
    assert_eq!(reader.read_var_int().unwrap(), 300);
    assert_eq!(
        reader.read_bytes(3, "fixed acknowledgement").unwrap(),
        [1, 2, 3]
    );
    assert_eq!(reader.read_u8().unwrap(), 0x7f);
    assert_eq!(reader.remaining(), 0);

    let empty = v775::ChatLastSeenUpdate::empty_with_disabled_checksum();
    assert!(v775::encode_play_chat_message("", 0, 0, None, empty).is_err());
    assert!(v775::encode_play_chat_message("bad\nline", 0, 0, None, empty).is_err());
    assert!(v775::encode_play_chat_message(&"x".repeat(257), 0, 0, None, empty).is_err());
    assert!(v775::encode_play_chat_message(&"😀".repeat(128), 0, 0, None, empty).is_ok());
    assert!(v775::ChatLastSeenUpdate::new(-1, [0; 3], 0).is_err());
    assert!(v775::ChatLastSeenUpdate::new(0, [0, 0, 0x10], 0).is_err());
}

#[test]
fn signed_chat_and_session_update_vectors_include_every_trailing_field() {
    let signature = v775::MessageSignature::new([0x5a; 256]);
    let update = v775::ChatLastSeenUpdate::new(1, [0, 0, 8], 0x42).unwrap();
    let encoded =
        body(&v775::encode_play_chat_message("x", 2, 3, Some(signature), update).unwrap());
    let mut reader = CodecReader::new(&encoded);
    assert_eq!(reader.read_var_int().unwrap(), 0x09);
    assert_eq!(
        reader
            .read_string(cubic_protocol::StringLimits::new(256, 768))
            .unwrap(),
        "x"
    );
    assert_eq!(reader.read_i64().unwrap(), 2);
    assert_eq!(reader.read_i64().unwrap(), 3);
    assert!(reader.read_bool().unwrap());
    assert_eq!(reader.read_bytes(256, "signature").unwrap(), [0x5a; 256]);
    assert_eq!(reader.read_var_int().unwrap(), 1);
    assert_eq!(reader.read_bytes(3, "last seen").unwrap(), [0, 0, 8]);
    assert_eq!(reader.read_u8().unwrap(), 0x42);
    assert_eq!(reader.remaining(), 0);

    let session = body(
        &v775::encode_play_chat_session_update(
            cubic_protocol::ProtocolUuid::from_u128(1),
            2,
            &[0xaa, 0xbb],
            &[0xcc],
        )
        .unwrap(),
    );
    assert_eq!(session[0], 0x0a);
    assert_eq!(&session[1..17], &1_u128.to_be_bytes());
    assert_eq!(&session[17..25], &2_i64.to_be_bytes());
    assert_eq!(&session[25..], &[2, 0xaa, 0xbb, 1, 0xcc]);
}

#[test]
fn malformed_known_play_packets_return_errors() {
    for packet in [&[0x2c][..], &[0x3d, 0][..], &[0x20][..], &[0x79, 8, 0][..]] {
        assert!(v775::decode_play_clientbound(packet).is_err());
    }
}

#[test]
fn unrelated_play_payload_is_identified_without_retention_model() {
    let packet = [0x2e, 1, 2, 3, 4, 5];
    assert_eq!(
        v775::decode_play_clientbound(&packet).unwrap(),
        v775::PlayClientbound::Ignored {
            packet_id: 0x2e,
            payload_bytes: 5
        }
    );
}

#[test]
fn world_state_packets_have_independent_protocol_775_vectors() {
    let mut position = CodecWriter::new();
    position.write_var_int(0x48);
    position.write_var_int(9);
    for value in [1.0, 2.0, 3.0, 0.1, 0.2, 0.3] {
        position.write_f64(value);
    }
    position.write_f32(90.0);
    position.write_f32(-10.0);
    position.write_u32(0x11);
    let v775::PlayClientbound::PlayerPosition(decoded) =
        v775::decode_play_clientbound(position.as_slice()).unwrap()
    else {
        panic!("expected Player Position")
    };
    assert_eq!(decoded.teleport_id, 9);
    assert_eq!((decoded.x, decoded.y, decoded.z), (1.0, 2.0, 3.0));
    assert_eq!(decoded.relative_flags, 0x11);

    let mut respawn = CodecWriter::new();
    respawn.write_var_int(0x52);
    write_spawn_info(&mut respawn, "custom:dimension");
    respawn.write_u8(3);
    let v775::PlayClientbound::Respawn(decoded) =
        v775::decode_play_clientbound(respawn.as_slice()).unwrap()
    else {
        panic!("expected Respawn")
    };
    assert_eq!(decoded.spawn.dimension, "custom:dimension");
    assert_eq!(decoded.data_to_keep, 3);

    let mut spawn = CodecWriter::new();
    spawn.write_var_int(0x61);
    spawn
        .write_string("custom:dimension", StringLimits::new(256, 768))
        .unwrap();
    spawn.write_block_position(12, 80, -4).unwrap();
    spawn.write_f32(45.0);
    spawn.write_f32(5.0);
    let v775::PlayClientbound::SetDefaultSpawnPosition(decoded) =
        v775::decode_play_clientbound(spawn.as_slice()).unwrap()
    else {
        panic!("expected Set Default Spawn Position")
    };
    assert_eq!(decoded.position.position.x(), 12);
    assert_eq!(decoded.yaw, 45.0);

    let mut time = CodecWriter::new();
    time.write_var_int(0x71);
    time.write_i64(12_345);
    time.write_var_int(1);
    time.write_var_int(2);
    time.write_var_long(6_000);
    time.write_f32(0.5);
    time.write_f32(1.0);
    let v775::PlayClientbound::SetTime(decoded) =
        v775::decode_play_clientbound(time.as_slice()).unwrap()
    else {
        panic!("expected Set Time")
    };
    assert_eq!(decoded.game_time, 12_345);
    assert_eq!(decoded.clocks[0].clock_type_raw_id, 2);

    assert!(matches!(
        v775::decode_play_clientbound(&[0x0a, 0x03, 0x01]).unwrap(),
        v775::PlayClientbound::ChangeDifficulty {
            difficulty: 3,
            locked: true
        }
    ));
    assert!(matches!(
        v775::decode_play_clientbound(&[0x26, 0x07, 0x3f, 0x00, 0x00, 0x00]).unwrap(),
        v775::PlayClientbound::GameEvent {
            event: 7,
            value: 0.5
        }
    ));

    let mut border = CodecWriter::new();
    border.write_var_int(0x2b);
    border.write_f64(1.0);
    border.write_f64(2.0);
    border.write_f64(1_000.0);
    border.write_f64(500.0);
    border.write_var_long(10_000);
    border.write_var_int(29_999_984);
    border.write_var_int(5);
    border.write_var_int(15);
    let v775::PlayClientbound::InitializeBorder(decoded) =
        v775::decode_play_clientbound(border.as_slice()).unwrap()
    else {
        panic!("expected Initialize Border")
    };
    assert_eq!(decoded.lerp_millis, 10_000);
    assert_eq!(decoded.warning_seconds, 15);
}

#[test]
fn world_state_packet_counts_flags_and_trailing_data_are_rejected() {
    let mut login = CodecWriter::new();
    login.write_var_int(0x31);
    login.write_i32(1);
    login.write_bool(false);
    login.write_var_int(1_025);
    assert!(v775::decode_play_clientbound(login.as_slice()).is_err());

    let mut position = CodecWriter::new();
    position.write_var_int(0x48);
    position.write_var_int(1);
    for _ in 0..6 {
        position.write_f64(0.0);
    }
    position.write_f32(0.0);
    position.write_f32(0.0);
    position.write_u32(0x0200);
    assert!(v775::decode_play_clientbound(position.as_slice()).is_err());

    let mut time = CodecWriter::new();
    time.write_var_int(0x71);
    time.write_i64(0);
    time.write_var_int(65);
    assert!(v775::decode_play_clientbound(time.as_slice()).is_err());

    assert!(v775::decode_play_clientbound(&[0x0a, 0, 0, 0]).is_err());
}

fn write_spawn_info(writer: &mut CodecWriter, dimension: &str) {
    writer.write_var_int(4);
    writer
        .write_string(dimension, StringLimits::new(256, 768))
        .unwrap();
    writer.write_i64(42);
    writer.write_i8(1);
    writer.write_u8(u8::MAX);
    writer.write_bool(false);
    writer.write_bool(true);
    writer.write_bool(false);
    writer.write_var_int(0);
    writer.write_var_int(63);
}
