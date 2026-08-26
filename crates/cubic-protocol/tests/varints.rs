use cubic_protocol::{CodecError, CodecReader, CodecWriter};

const VAR_INT_CASES: &[(i32, &[u8])] = &[
    (0, &[0x00]),
    (1, &[0x01]),
    (2, &[0x02]),
    (127, &[0x7f]),
    (128, &[0x80, 0x01]),
    (255, &[0xff, 0x01]),
    (25_565, &[0xdd, 0xc7, 0x01]),
    (2_097_151, &[0xff, 0xff, 0x7f]),
    (i32::MAX, &[0xff, 0xff, 0xff, 0xff, 0x07]),
    (-1, &[0xff, 0xff, 0xff, 0xff, 0x0f]),
    (-2, &[0xfe, 0xff, 0xff, 0xff, 0x0f]),
    (i32::MIN, &[0x80, 0x80, 0x80, 0x80, 0x08]),
];

const VAR_LONG_CASES: &[(i64, &[u8])] = &[
    (0, &[0x00]),
    (1, &[0x01]),
    (127, &[0x7f]),
    (128, &[0x80, 0x01]),
    (255, &[0xff, 0x01]),
    (
        i64::MAX,
        &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f],
    ),
    (
        -1,
        &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01],
    ),
    (
        -2,
        &[0xfe, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01],
    ),
    (
        i64::MIN,
        &[0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01],
    ),
];

#[test]
fn var_int_known_vectors_encode_and_decode() {
    for &(value, encoded) in VAR_INT_CASES {
        let mut writer = CodecWriter::new();
        writer.write_var_int(value);
        assert_eq!(writer.as_slice(), encoded);
        assert_eq!(CodecReader::new(encoded).read_var_int(), Ok(value));
    }
}

#[test]
fn var_long_known_vectors_encode_and_decode() {
    for &(value, encoded) in VAR_LONG_CASES {
        let mut writer = CodecWriter::new();
        writer.write_var_long(value);
        assert_eq!(writer.as_slice(), encoded);
        assert_eq!(CodecReader::new(encoded).read_var_long(), Ok(value));
    }
}

#[test]
fn empty_and_truncated_var_ints_report_unexpected_end() {
    assert!(matches!(
        CodecReader::new(&[]).read_var_int(),
        Err(CodecError::UnexpectedEnd {
            context: "VarInt",
            ..
        })
    ));
    assert!(matches!(
        CodecReader::new(&[0x80, 0x80]).read_var_int(),
        Err(CodecError::UnexpectedEnd {
            context: "VarInt",
            ..
        })
    ));
}

#[test]
fn continuation_beyond_var_int_limit_is_malformed() {
    assert_eq!(
        CodecReader::new(&[0x80; 6]).read_var_int(),
        Err(CodecError::MalformedVarInt)
    );
}

#[test]
fn continuation_beyond_var_long_limit_is_malformed() {
    assert_eq!(
        CodecReader::new(&[0x80; 11]).read_var_long(),
        Err(CodecError::MalformedVarLong)
    );
}
