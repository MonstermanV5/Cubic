use cubic_protocol::{
    CodecReader, CodecWriter, FrameDecoder, FrameLimits, StringLimits, bootstrap::v775,
};

fn body(frame: &[u8]) -> Vec<u8> {
    let mut decoder = FrameDecoder::new(FrameLimits::new(2_097_151, 4 * 1024 * 1024).unwrap());
    decoder.push(frame).unwrap();
    decoder.next_frame().unwrap().unwrap()
}

#[test]
fn movement_and_input_packets_match_independent_protocol_775_vectors() {
    let mut expected_position = vec![0x1e];
    expected_position.extend_from_slice(&1.0_f64.to_be_bytes());
    expected_position.extend_from_slice(&2.0_f64.to_be_bytes());
    expected_position.extend_from_slice(&3.0_f64.to_be_bytes());
    expected_position.push(0x03);
    assert_eq!(
        body(&v775::encode_play_move_position(1.0, 2.0, 3.0, true, true).unwrap()),
        expected_position
    );

    let mut expected_combined = vec![0x1f];
    expected_combined.extend_from_slice(&1.0_f64.to_be_bytes());
    expected_combined.extend_from_slice(&2.0_f64.to_be_bytes());
    expected_combined.extend_from_slice(&3.0_f64.to_be_bytes());
    expected_combined.extend_from_slice(&90.0_f32.to_be_bytes());
    expected_combined.extend_from_slice(&(-45.0_f32).to_be_bytes());
    expected_combined.push(0x01);
    assert_eq!(
        body(
            &v775::encode_play_move_position_rotation(1.0, 2.0, 3.0, 90.0, -45.0, true, false,)
                .unwrap()
        ),
        expected_combined
    );

    assert_eq!(
        body(&v775::encode_play_move_rotation(90.0, -45.0, true, false).unwrap()),
        [
            vec![0x20],
            90.0_f32.to_be_bytes().to_vec(),
            (-45.0_f32).to_be_bytes().to_vec(),
            vec![1]
        ]
        .concat()
    );
    assert_eq!(
        body(&v775::encode_play_move_status(false, true).unwrap()),
        vec![0x21, 0x02]
    );
    assert_eq!(
        body(
            &v775::encode_play_player_input(v775::PlayerInput {
                forward: true,
                left: true,
                jump: true,
                sprint: true,
                ..v775::PlayerInput::default()
            })
            .unwrap()
        ),
        vec![0x2b, 0x55]
    );
    assert_eq!(
        body(
            &v775::encode_play_player_command(300, v775::PlayerCommandAction::StartSprinting,)
                .unwrap()
        ),
        vec![0x2a, 0xac, 0x02, 0x01, 0x00]
    );
    assert_eq!(
        body(&v775::encode_play_player_abilities(true).unwrap()),
        vec![0x28, 0x02]
    );
    assert_eq!(
        body(&v775::encode_play_player_abilities(false).unwrap()),
        vec![0x28, 0x00]
    );
    assert_eq!(body(&v775::encode_play_client_tick_end().unwrap()), [0x0d]);
}

#[test]
fn clientbound_player_abilities_matches_protocol_775_wire_layout() {
    let mut packet = vec![0x40, 0x0f];
    packet.extend_from_slice(&0.05_f32.to_be_bytes());
    packet.extend_from_slice(&0.1_f32.to_be_bytes());
    assert_eq!(
        v775::decode_play_clientbound(&packet).unwrap(),
        v775::PlayClientbound::PlayerAbilities(v775::PlayerAbilities {
            invulnerable: true,
            flying: true,
            may_fly: true,
            instant_build: true,
            flying_speed: 0.05,
            walking_speed: 0.1,
        })
    );
    assert!(v775::decode_play_clientbound(&[0x40, 0x10, 0, 0, 0, 0, 0, 0, 0, 0]).is_err());
}

#[test]
fn live_block_update_packets_match_verified_protocol_775_layouts() {
    let x = -1_i32;
    let y = 64_i32;
    let z = 17_i32;
    let packed_position = (((i64::from(x) as u64) & 0x03ff_ffff) << 38)
        | (((i64::from(z) as u64) & 0x03ff_ffff) << 12)
        | ((i64::from(y) as u64) & 0x0fff);
    let mut single = vec![0x08];
    single.extend_from_slice(&packed_position.to_be_bytes());
    single.extend_from_slice(&[0xac, 0x02]); // runtime state 300
    let v775::PlayClientbound::BlockUpdate(decoded) =
        v775::decode_play_clientbound(&single).unwrap()
    else {
        panic!("expected Block Update")
    };
    assert_eq!((decoded.x, decoded.y, decoded.z), (x, y, z));
    assert_eq!(decoded.state_id, 300);

    // Official SectionPos packing is X:22, Z:22, Y:20. Each VarLong entry is
    // state<<12 | localX<<8 | localZ<<4 | localY.
    let section_x = -2_i32;
    let section_y = -1_i32;
    let section_z = 3_i32;
    let packed_section = (((i64::from(section_x) as u64) & 0x3f_ffff) << 42)
        | (((i64::from(section_z) as u64) & 0x3f_ffff) << 20)
        | ((i64::from(section_y) as u64) & 0x0f_ffff);
    let mut section = CodecWriter::new();
    section.write_var_int(0x54);
    section.write_u64(packed_section);
    section.write_var_int(2);
    section.write_var_long((1_i64 << 12) | (15 << 8));
    section.write_var_long((300_i64 << 12) | (7 << 8) | (8 << 4) | 9);
    let v775::PlayClientbound::SectionBlocksUpdate(decoded) =
        v775::decode_play_clientbound(section.as_slice()).unwrap()
    else {
        panic!("expected Section Blocks Update")
    };
    assert_eq!(
        (decoded.section_x, decoded.section_y, decoded.section_z),
        (section_x, section_y, section_z)
    );
    assert_eq!(decoded.updates[0].state_id, 1);
    assert_eq!(
        (
            decoded.updates[0].local_x,
            decoded.updates[0].local_y,
            decoded.updates[0].local_z
        ),
        (15, 0, 0)
    );
    assert_eq!(decoded.updates[1].state_id, 300);
    assert_eq!(
        (
            decoded.updates[1].local_x,
            decoded.updates[1].local_y,
            decoded.updates[1].local_z
        ),
        (7, 9, 8)
    );
    assert_eq!(
        v775::classify_play_decode_work(&single).unwrap(),
        v775::PlayDecodeWork::Normal
    );
}

#[test]
fn malformed_live_block_updates_are_rejected_with_bounds() {
    let mut negative = CodecWriter::new();
    negative.write_var_int(0x08);
    negative.write_block_position(0, 0, 0).unwrap();
    negative.write_var_int(-1);
    assert!(v775::decode_play_clientbound(negative.as_slice()).is_err());

    let mut oversized = CodecWriter::new();
    oversized.write_var_int(0x54);
    oversized.write_i64(0);
    oversized.write_var_int((v775::MAX_SECTION_BLOCK_UPDATES + 1) as i32);
    assert!(v775::decode_play_clientbound(oversized.as_slice()).is_err());

    let mut truncated = CodecWriter::new();
    truncated.write_var_int(0x54);
    truncated.write_i64(0);
    truncated.write_var_int(1);
    assert!(v775::decode_play_clientbound(truncated.as_slice()).is_err());
}

#[test]
fn current_low_precision_entity_motion_vector_decodes_without_legacy_shorts() {
    // entity 7; LpVec3 scale 1; normalized components +1, 0, -1.
    let packet = [0x65, 0x07, 0xf1, 0xff, 0x00, 0x00, 0xff, 0xff];
    let decoded = v775::decode_play_clientbound(&packet).unwrap();
    let v775::PlayClientbound::SetEntityMotion(motion) = decoded else {
        panic!("expected entity motion")
    };
    assert_eq!(motion.entity_id, 7);
    assert!((motion.delta_x - 1.0).abs() < 1.0e-9);
    assert!(motion.delta_y.abs() < 1.0e-9);
    assert!((motion.delta_z + 1.0).abs() < 1.0e-9);

    let zero = v775::decode_play_clientbound(&[0x65, 0x07, 0x00]).unwrap();
    let v775::PlayClientbound::SetEntityMotion(zero) = zero else {
        panic!("expected zero entity motion")
    };
    assert_eq!((zero.delta_x, zero.delta_y, zero.delta_z), (0.0, 0.0, 0.0));
    assert!(v775::decode_play_clientbound(&[0x65, 0x07, 0x01]).is_err());
}

#[test]
fn movement_encoders_reject_non_finite_local_state() {
    assert!(v775::encode_play_move_position(f64::NAN, 0.0, 0.0, false, false).is_err());
    assert!(
        v775::encode_play_move_position_rotation(0.0, 0.0, 0.0, f32::INFINITY, 0.0, false, false,)
            .is_err()
    );
    assert!(v775::encode_play_move_rotation(0.0, f32::NAN, false, false).is_err());
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

#[test]
fn set_entity_data_projects_standalone_player_air_without_retaining_metadata() {
    // Official 26.1.2 packet report: set_entity_data = 0x63. Entity base
    // metadata index 1 uses serializer 1 (VarInt) for remaining air supply.
    let body = [0x63, 0x2a, 0x01, 0x01, 0x3c, 0xff];
    assert_eq!(
        v775::decode_play_clientbound(&body).unwrap(),
        v775::PlayClientbound::EntityData {
            entity_id: 42,
            air_supply: Some(60),
            payload_bytes: 4,
        }
    );

    let unrelated = [0x63, 0x2a, 0x00, 0x00, 0x01, 0xff];
    assert_eq!(
        v775::decode_play_clientbound(&unrelated).unwrap(),
        v775::PlayClientbound::EntityData {
            entity_id: 42,
            air_supply: None,
            payload_bytes: 4,
        }
    );
    assert!(v775::decode_play_clientbound(&[0x63, 0x2a, 0x01, 0x01]).is_err());
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
