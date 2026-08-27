use cubic_protocol::{
    CodecError, FrameDecoder, FrameLimits, ProtocolUuid,
    bootstrap::v775::{
        self, BootstrapProtocolError, ClientInformation, ConfigurationClientbound, LoginClientbound,
    },
    handshake::{Handshake, HandshakeNextState, encode_handshake},
};

fn body(encoded_frame: &[u8]) -> Vec<u8> {
    let mut decoder = FrameDecoder::new(FrameLimits::new(2_097_151, 4 * 1024 * 1024).unwrap());
    decoder.push(encoded_frame).unwrap();
    decoder.next_frame().unwrap().unwrap()
}

#[test]
fn login_handshake_has_an_independent_protocol_775_wire_vector() {
    let encoded = encode_handshake(
        &Handshake {
            protocol_version: 775,
            server_address: "localhost",
            server_port: 25_565,
            next_state: HandshakeNextState::Login,
        },
        1024,
    )
    .unwrap();
    assert_eq!(
        encoded,
        [
            0x10, 0x00, 0x87, 0x06, 0x09, b'l', b'o', b'c', b'a', b'l', b'h', b'o', b's', b't',
            0x63, 0xdd, 0x02,
        ]
    );
}

#[test]
fn login_start_has_an_independent_offline_wire_vector() {
    let encoded = v775::encode_login_start("CubicTest", ProtocolUuid::from_u128(0)).unwrap();
    let mut expected = vec![0x1b, 0x00, 0x09];
    expected.extend_from_slice(b"CubicTest");
    expected.extend_from_slice(&[0; 16]);
    assert_eq!(encoded, expected);
}

#[test]
fn login_success_vector_decodes_profile_fields() {
    let mut packet = vec![0x02];
    packet.extend_from_slice(&0x0011_2233_4455_6677_8899_aabb_ccdd_eeff_u128.to_be_bytes());
    packet.extend_from_slice(&[0x03, b'B', b'o', b'b', 0x01]);
    packet.extend_from_slice(&[0x04, b's', b'k', b'i', b'n']);
    packet.extend_from_slice(&[0x05, b'v', b'a', b'l', b'u', b'e', 0x00]);
    let LoginClientbound::Success(success) = v775::decode_login_clientbound(&packet).unwrap()
    else {
        panic!("expected Login Success");
    };
    assert_eq!(
        success.uuid.as_u128(),
        0x0011_2233_4455_6677_8899_aabb_ccdd_eeff
    );
    assert_eq!(success.username, "Bob");
    assert_eq!(success.properties.len(), 1);
    assert_eq!(success.properties[0].name, "skin");
    assert_eq!(success.properties[0].value, "value");
    assert_eq!(success.properties[0].signature, None);
}

#[test]
fn login_control_packets_have_independent_wire_vectors() {
    assert_eq!(body(&v775::encode_login_acknowledged().unwrap()), [0x03]);
    assert_eq!(
        body(&v775::encode_login_plugin_response(300).unwrap()),
        [0x02, 0xac, 0x02, 0x00]
    );
    assert_eq!(
        body(&v775::encode_login_cookie_response("a:b").unwrap()),
        [0x04, 0x03, b'a', b':', b'b', 0x00]
    );
    assert_eq!(
        body(&v775::encode_encryption_response(&[0xaa, 0xbb], &[1, 2, 3]).unwrap()),
        [0x01, 0x02, 0xaa, 0xbb, 0x03, 1, 2, 3]
    );
}

#[test]
fn login_rejections_and_malformed_success_are_structured() {
    assert!(matches!(
        v775::decode_login_clientbound(&[0x01, 0x00, 0x01, 0x30, 0x04, 1, 2, 3, 4, 0x01]),
        Ok(LoginClientbound::EncryptionRequest(request))
            if request.server_id.is_empty()
                && request.public_key_der == [0x30]
                && request.verify_token == [1, 2, 3, 4]
                && request.should_authenticate
    ));
    assert!(matches!(
        v775::decode_login_clientbound(&[0x03, 0x80, 0x02]),
        Ok(LoginClientbound::SetCompression { threshold: 256 })
    ));
    assert!(matches!(
        v775::decode_login_clientbound(&[0x02]),
        Err(BootstrapProtocolError::Codec(
            CodecError::UnexpectedEnd { .. }
        ))
    ));
    assert!(matches!(
        v775::decode_login_clientbound(&[0x7f]),
        Err(BootstrapProtocolError::UnexpectedPacketId {
            state: "Login",
            id: 127
        })
    ));
}

#[test]
fn client_information_has_an_independent_wire_vector() {
    assert_eq!(
        body(&v775::encode_client_information(&ClientInformation::default()).unwrap()),
        [
            0x00, 0x05, b'e', b'n', b'_', b'u', b's', 0x08, 0x00, 0x01, 0x7f, 0x01, 0x00, 0x01,
            0x00,
        ]
    );
}

#[test]
fn configuration_responses_have_independent_wire_vectors() {
    assert_eq!(
        body(&v775::encode_configuration_cookie_response("a:b").unwrap()),
        [0x01, 0x03, b'a', b':', b'b', 0x00]
    );
    assert_eq!(
        body(&v775::encode_configuration_keep_alive(0x0102_0304_0506_0708).unwrap()),
        [0x04, 1, 2, 3, 4, 5, 6, 7, 8]
    );
    assert_eq!(
        body(&v775::encode_configuration_pong(0x0102_0304).unwrap()),
        [0x05, 1, 2, 3, 4]
    );
    assert_eq!(
        body(&v775::encode_known_packs_response_empty().unwrap()),
        [0x07, 0x00]
    );
    assert_eq!(body(&v775::encode_finish_configuration().unwrap()), [0x03]);
}

#[test]
fn configuration_packets_decode_semantically_or_as_bounded_skips() {
    assert_eq!(
        v775::decode_configuration_clientbound(&[0x04, 1, 2, 3, 4, 5, 6, 7, 8]).unwrap(),
        ConfigurationClientbound::KeepAlive {
            id: 0x0102_0304_0506_0708
        }
    );
    assert_eq!(
        v775::decode_configuration_clientbound(&[0x05, 1, 2, 3, 4]).unwrap(),
        ConfigurationClientbound::Ping { id: 0x0102_0304 }
    );
    assert!(matches!(
        v775::decode_configuration_clientbound(&[0x07, 1, 2, 3]),
        Ok(ConfigurationClientbound::Skipped {
            payload_bytes: 3,
            ..
        })
    ));
    assert!(matches!(
        v775::decode_configuration_clientbound(&[0x7f]),
        Err(BootstrapProtocolError::UnexpectedPacketId {
            state: "Configuration",
            id: 127
        })
    ));
}

#[test]
fn known_pack_count_and_configuration_disconnect_are_bounded() {
    assert!(matches!(
        v775::decode_configuration_clientbound(&[0x0e, 0x41]),
        Err(BootstrapProtocolError::CountTooLarge {
            context: "Known Packs",
            count: 65,
            ..
        })
    ));
    assert!(matches!(
        v775::decode_configuration_clientbound(&[0x02, 0x0a, 0x00]),
        Ok(ConfigurationClientbound::Disconnect { .. })
    ));
    assert!(matches!(
        v775::decode_configuration_clientbound(&[0x02, 0x0a]),
        Err(BootstrapProtocolError::Nbt(_))
    ));
}

#[test]
fn initial_play_acceptance_requires_the_protocol_775_login_packet() {
    assert!(v775::validate_initial_play_login(&[0x31, 0x01]).is_ok());
    assert!(matches!(
        v775::validate_initial_play_login(&[0x31]),
        Err(BootstrapProtocolError::EmptyInitialPlayLogin)
    ));
    assert!(matches!(
        v775::validate_initial_play_login(&[0x30, 0x01]),
        Err(BootstrapProtocolError::UnexpectedPacketId {
            state: "Play acceptance",
            ..
        })
    ));
}
