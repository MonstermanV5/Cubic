use cubic_protocol::{CodecReader, FrameDecoder, FrameLimits, bootstrap::v775};

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
        acknowledgement_required,
        ..
    } = decoded
    else {
        panic!("expected Player Chat")
    };
    assert_eq!(global_index, 7);
    assert_eq!(sender_name, "Alice");
    assert_eq!(message.plain_text, "hello");
    assert!(!acknowledgement_required);
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
    let v775::PlayClientbound::PlayerChat {
        acknowledgement_required,
        ..
    } = decoded
    else {
        panic!("expected Player Chat")
    };
    assert!(acknowledgement_required);
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
    let framed = v775::encode_play_chat_message("Hi", 1, 2, last_seen).unwrap();
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
    assert!(v775::encode_play_chat_message("", 0, 0, empty).is_err());
    assert!(v775::encode_play_chat_message("bad\nline", 0, 0, empty).is_err());
    assert!(v775::encode_play_chat_message(&"x".repeat(257), 0, 0, empty).is_err());
    assert!(v775::encode_play_chat_message(&"😀".repeat(128), 0, 0, empty).is_ok());
    assert!(v775::ChatLastSeenUpdate::new(-1, [0; 3], 0).is_err());
    assert!(v775::ChatLastSeenUpdate::new(0, [0, 0, 0x10], 0).is_err());
}

#[test]
fn malformed_known_play_packets_return_errors() {
    for packet in [&[0x2c][..], &[0x3d, 0][..], &[0x20][..], &[0x79, 8, 0][..]] {
        assert!(v775::decode_play_clientbound(packet).is_err());
    }
}

#[test]
fn irrelevant_world_payload_is_identified_without_retention_model() {
    let packet = [0x2d, 1, 2, 3, 4, 5];
    assert_eq!(
        v775::decode_play_clientbound(&packet).unwrap(),
        v775::PlayClientbound::Ignored {
            packet_id: 0x2d,
            payload_bytes: 5
        }
    );
}
