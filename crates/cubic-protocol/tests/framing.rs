use cubic_protocol::{
    CodecError, CodecWriter, FrameDecoder, FrameLimits, LengthKind, encode_frame, split_raw_packet,
};

fn limits() -> FrameLimits {
    FrameLimits::new(1024, 4096).unwrap()
}

#[test]
fn one_complete_frame_is_emitted() {
    let encoded = encode_frame(b"hello", 1024).unwrap();
    let mut decoder = FrameDecoder::new(limits());
    decoder.push(&encoded).unwrap();
    assert_eq!(decoder.next_frame(), Ok(Some(b"hello".to_vec())));
    assert_eq!(decoder.next_frame(), Ok(None));
}

#[test]
fn frame_split_one_byte_at_a_time_is_reconstructed() {
    let body = vec![0x5a; 300];
    let encoded = encode_frame(&body, 1024).unwrap();
    let mut decoder = FrameDecoder::new(limits());
    for byte in encoded {
        decoder.push(&[byte]).unwrap();
    }
    assert_eq!(decoder.next_frame(), Ok(Some(body)));
}

#[test]
fn split_length_prefix_waits_for_more_input() {
    let encoded = encode_frame(&[7; 200], 1024).unwrap();
    let mut decoder = FrameDecoder::new(limits());
    decoder.push(&encoded[..1]).unwrap();
    assert_eq!(decoder.next_frame(), Ok(None));
    decoder.push(&encoded[1..]).unwrap();
    assert_eq!(decoder.next_frame(), Ok(Some(vec![7; 200])));
}

#[test]
fn several_frames_in_one_buffer_are_emitted_in_order() {
    let bodies = [b"one".as_slice(), b"two".as_slice(), b"three".as_slice()];
    let mut combined = Vec::new();
    for body in bodies {
        combined.extend(encode_frame(body, 1024).unwrap());
    }
    let mut decoder = FrameDecoder::new(limits());
    decoder.push(&combined).unwrap();
    assert_eq!(decoder.next_frame(), Ok(Some(b"one".to_vec())));
    assert_eq!(decoder.next_frame(), Ok(Some(b"two".to_vec())));
    assert_eq!(decoder.next_frame(), Ok(Some(b"three".to_vec())));
    assert_eq!(decoder.next_frame(), Ok(None));
}

#[test]
fn complete_frame_plus_partial_next_frame_is_preserved() {
    let first = encode_frame(b"complete", 1024).unwrap();
    let second = encode_frame(b"fragmented", 1024).unwrap();
    let split = 4;
    let mut decoder = FrameDecoder::new(limits());
    let mut initial = first;
    initial.extend_from_slice(&second[..split]);
    decoder.push(&initial).unwrap();
    assert_eq!(decoder.next_frame(), Ok(Some(b"complete".to_vec())));
    assert_eq!(decoder.next_frame(), Ok(None));
    decoder.push(&second[split..]).unwrap();
    assert_eq!(decoder.next_frame(), Ok(Some(b"fragmented".to_vec())));
}

#[test]
fn empty_input_and_partial_body_need_more_data() {
    let mut decoder = FrameDecoder::new(limits());
    assert_eq!(decoder.next_frame(), Ok(None));
    decoder.push(&[3, 1]).unwrap();
    assert_eq!(decoder.next_frame(), Ok(None));
    assert_eq!(decoder.buffered_len(), 2);
}

#[test]
fn malformed_negative_and_oversized_lengths_are_structured_errors() {
    let mut malformed = FrameDecoder::new(limits());
    malformed.push(&[0x80; 5]).unwrap();
    assert_eq!(
        malformed.next_frame(),
        Err(CodecError::MalformedLengthPrefix {
            kind: LengthKind::Frame,
        })
    );

    let mut negative = FrameDecoder::new(limits());
    negative.push(&[0xff, 0xff, 0xff, 0xff, 0x0f]).unwrap();
    assert_eq!(
        negative.next_frame(),
        Err(CodecError::NegativeLength {
            kind: LengthKind::Frame,
            value: -1,
        })
    );

    let mut writer = CodecWriter::new();
    writer.write_var_int(1025);
    let mut oversized = FrameDecoder::new(limits());
    oversized.push(writer.as_slice()).unwrap();
    assert_eq!(
        oversized.next_frame(),
        Err(CodecError::FrameTooLong {
            length: 1025,
            max: 1024,
        })
    );
}

#[test]
fn four_byte_nonnegative_frame_prefix_is_rejected() {
    let mut decoder = FrameDecoder::new(limits());
    decoder.push(&[0x81, 0x80, 0x80, 0x00]).unwrap();
    assert_eq!(
        decoder.next_frame(),
        Err(CodecError::MalformedLengthPrefix {
            kind: LengthKind::Frame,
        })
    );
}

#[test]
fn aggregate_buffer_limit_is_enforced() {
    let limits = FrameLimits::new(8, 11).unwrap();
    let mut decoder = FrameDecoder::new(limits);
    assert_eq!(
        decoder.push(&[0; 12]),
        Err(CodecError::FrameBufferTooLong {
            buffered: 12,
            max: 11,
        })
    );
}

#[test]
fn raw_packet_helper_only_splits_id_and_payload() {
    let mut body = CodecWriter::new();
    body.write_var_int(255);
    body.write_bytes(&[9, 8, 7]);
    let packet = split_raw_packet(body.as_slice()).unwrap();
    assert_eq!(packet.id, 255);
    assert_eq!(packet.payload, &[9, 8, 7]);
    assert!(matches!(
        split_raw_packet(&[]),
        Err(CodecError::UnexpectedEnd {
            context: "VarInt",
            ..
        })
    ));
}
