use cubic_protocol::{
    CodecError, CodecWriter, LengthKind,
    status::{
        StatusHandshake, StatusJsonLimits, StatusProtocolError, decode_status_pong,
        decode_status_response, encode_status_handshake, encode_status_ping, encode_status_request,
        parse_status_json,
    },
};
use serde_json::json;

fn status_body(json: &str) -> Vec<u8> {
    let mut writer = CodecWriter::new();
    writer.write_var_int(0);
    writer
        .write_string(json, cubic_protocol::StringLimits::new(32_767, 98_301))
        .unwrap();
    writer.into_inner()
}

#[test]
fn independent_handshake_and_request_vectors_match_wire_format() {
    let handshake = encode_status_handshake(&StatusHandshake {
        protocol_version: -1,
        server_address: "localhost",
        server_port: 25_565,
    })
    .unwrap();
    assert_eq!(
        handshake,
        [
            0x13, 0x00, 0xff, 0xff, 0xff, 0xff, 0x0f, 0x09, b'l', b'o', b'c', b'a', b'l', b'h',
            b'o', b's', b't', 0x63, 0xdd, 0x01,
        ]
    );
    assert_eq!(encode_status_request().unwrap(), [0x01, 0x00]);
}

#[test]
fn ping_and_pong_vectors_preserve_signed_nonce() {
    assert_eq!(
        encode_status_ping(-2).unwrap(),
        [0x09, 0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe]
    );
    let pong = [0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe];
    assert!(decode_status_pong(&pong, -2).is_ok());
    assert!(matches!(
        decode_status_pong(&pong, 7),
        Err(StatusProtocolError::PongMismatch {
            expected: 7,
            actual: -2
        })
    ));
}

#[test]
fn simple_and_object_motds_have_plain_previews() {
    for (description, expected) in [
        (json!("Hello 🌍"), "Hello 🌍"),
        (json!({"text":"Hi"}), "Hi"),
    ] {
        let document = json!({
            "version": {"name": "1.test", "protocol": 999},
            "players": {"max": 20, "online": 0},
            "description": description,
        });
        let response =
            parse_status_json(&document.to_string(), StatusJsonLimits::default()).unwrap();
        assert_eq!(response.motd_preview(), Some(expected));
        assert_eq!(response.players.sample, []);
    }
}

#[test]
fn rich_description_favicon_sample_and_unknown_fields_are_preserved() {
    let document = json!({
        "version": {"name": "Proxy", "protocol": i32::MAX},
        "players": {
            "max": i32::MAX,
            "online": 1,
            "sample": [{"name": "Player", "id": "00000000-0000-0000-0000-000000000000"}]
        },
        "description": {"text": "Root", "extra": [{"text": " child", "color": "gold"}]},
        "favicon": "data:image/png;base64,AA==",
        "modinfo": {"type": "custom"}
    });
    let json_text = document.to_string();
    let response =
        decode_status_response(&status_body(&json_text), StatusJsonLimits::default()).unwrap();
    assert_eq!(response.version.protocol, i32::MAX);
    assert_eq!(response.players.sample.len(), 1);
    assert!(response.favicon.is_some());
    assert_eq!(
        response.additional_fields.get("modinfo"),
        Some(&json!({"type":"custom"}))
    );
    assert_eq!(response.raw_json, json_text);
    assert_eq!(response.description["extra"][0]["color"], "gold");
}

#[test]
fn malformed_json_and_invalid_required_fields_are_distinct() {
    assert!(matches!(
        parse_status_json("{", StatusJsonLimits::default()),
        Err(StatusProtocolError::MalformedJson { .. })
    ));
    for invalid in [
        json!({"players":{"max":1,"online":0},"description":"x"}),
        json!({"version":{"name":1,"protocol":1},"players":{"max":1,"online":0},"description":"x"}),
        json!({"version":{"name":"x","protocol":1},"players":{"max":"many","online":0},"description":"x"}),
    ] {
        assert!(matches!(
            parse_status_json(&invalid.to_string(), StatusJsonLimits::default()),
            Err(StatusProtocolError::InvalidStatusData)
        ));
    }
    let negative = json!({
        "version":{"name":"x","protocol":1},
        "players":{"max":1,"online":-1},
        "description":"x"
    });
    assert!(matches!(
        parse_status_json(&negative.to_string(), StatusJsonLimits::default()),
        Err(StatusProtocolError::InvalidPlayerCounts { .. })
    ));
}

#[test]
fn status_sample_favicon_and_text_fields_are_bounded() {
    let sample = json!({
        "version":{"name":"x","protocol":1},
        "players":{"max":2,"online":2,"sample":[{"name":"a","id":"1"},{"name":"b","id":"2"}]},
        "description":"x"
    });
    assert!(matches!(
        parse_status_json(&sample.to_string(), StatusJsonLimits::new(1, 32, 32)),
        Err(StatusProtocolError::PlayerSampleTooLarge { count: 2, max: 1 })
    ));

    let favicon = json!({
        "version":{"name":"x","protocol":1},
        "players":{"max":0,"online":0},
        "description":"x",
        "favicon":"12345"
    });
    assert!(matches!(
        parse_status_json(&favicon.to_string(), StatusJsonLimits::new(1, 4, 32)),
        Err(StatusProtocolError::TextFieldTooLarge {
            field: "favicon",
            ..
        })
    ));
}

#[test]
fn malformed_string_packet_ids_and_trailing_payload_are_rejected() {
    let malformed_string = [0x00, 0xff, 0xff, 0xff, 0xff, 0x0f];
    assert!(matches!(
        decode_status_response(&malformed_string, StatusJsonLimits::default()),
        Err(StatusProtocolError::Codec(CodecError::NegativeLength {
            kind: LengthKind::String,
            value: -1
        }))
    ));
    assert!(matches!(
        decode_status_response(&[1], StatusJsonLimits::default()),
        Err(StatusProtocolError::UnexpectedPacketId {
            expected: 0,
            actual: 1
        })
    ));
    assert!(matches!(
        decode_status_pong(&[2, 0, 0, 0, 0, 0, 0, 0, 0], 0),
        Err(StatusProtocolError::UnexpectedPacketId {
            expected: 1,
            actual: 2
        })
    ));
    assert!(matches!(
        decode_status_pong(&[1, 0, 0, 0, 0, 0, 0, 0, 0, 7], 0),
        Err(StatusProtocolError::TrailingPacketData { remaining: 1 })
    ));
}
