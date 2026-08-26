use cubic_protocol::{
    BitSet, BitSetLimits, BlockPosition, CodecError, CodecReader, CodecWriter, LengthKind,
    ProtocolUuid, StringLimits,
};

#[test]
fn fixed_width_values_use_big_endian_order() {
    let mut writer = CodecWriter::new();
    writer.write_i8(-2);
    writer.write_u8(254);
    writer.write_i16(-0x1234);
    writer.write_u16(0xabcd);
    writer.write_i32(-0x0123_4567);
    writer.write_u32(0x89ab_cdef);
    writer.write_i64(-0x0123_4567_89ab_cdef);
    writer.write_u64(0xfedc_ba98_7654_3210);

    let mut reader = CodecReader::new(writer.as_slice());
    assert_eq!(reader.read_i8(), Ok(-2));
    assert_eq!(reader.read_u8(), Ok(254));
    assert_eq!(reader.read_i16(), Ok(-0x1234));
    assert_eq!(reader.read_u16(), Ok(0xabcd));
    assert_eq!(reader.read_i32(), Ok(-0x0123_4567));
    assert_eq!(reader.read_u32(), Ok(0x89ab_cdef));
    assert_eq!(reader.read_i64(), Ok(-0x0123_4567_89ab_cdef));
    assert_eq!(reader.read_u64(), Ok(0xfedc_ba98_7654_3210));
    assert_eq!(reader.remaining(), 0);
}

#[test]
fn floating_point_bits_round_trip_special_values() {
    let f32_bits = [
        0,
        0x8000_0000,
        f32::INFINITY.to_bits(),
        f32::NEG_INFINITY.to_bits(),
        0x7fc0_1234,
    ];
    let f64_bits = [
        0,
        0x8000_0000_0000_0000,
        f64::INFINITY.to_bits(),
        f64::NEG_INFINITY.to_bits(),
        0x7ff8_0000_0000_1234,
    ];
    let mut writer = CodecWriter::new();
    for bits in f32_bits {
        writer.write_f32(f32::from_bits(bits));
    }
    for bits in f64_bits {
        writer.write_f64(f64::from_bits(bits));
    }
    let mut reader = CodecReader::new(writer.as_slice());
    for bits in f32_bits {
        assert_eq!(reader.read_f32().unwrap().to_bits(), bits);
    }
    for bits in f64_bits {
        assert_eq!(reader.read_f64().unwrap().to_bits(), bits);
    }
}

#[test]
fn booleans_encode_canonically_and_decode_nonzero_as_true() {
    let mut writer = CodecWriter::new();
    writer.write_bool(false);
    writer.write_bool(true);
    assert_eq!(writer.as_slice(), &[0, 1]);
    assert_eq!(CodecReader::new(&[0]).read_bool(), Ok(false));
    assert_eq!(CodecReader::new(&[1]).read_bool(), Ok(true));
    assert_eq!(CodecReader::new(&[2]).read_bool(), Ok(true));
    assert_eq!(CodecReader::new(&[255]).read_bool(), Ok(true));
}

#[test]
fn truncated_fixed_width_value_reports_context() {
    assert!(matches!(
        CodecReader::new(&[1, 2, 3]).read_i64(),
        Err(CodecError::UnexpectedEnd {
            context: "i64",
            needed: 8,
            remaining: 3
        })
    ));
}

#[test]
fn bounded_strings_cover_unicode_and_empty_values() {
    let values = ["", "ASCII", "é", "😀", "e\u{301}"];
    let limits = StringLimits::new(32, 96);
    for value in values {
        let mut writer = CodecWriter::new();
        writer.write_string(value, limits).unwrap();
        assert_eq!(
            CodecReader::new(writer.as_slice()).read_string(limits),
            Ok(value)
        );
    }
}

#[test]
fn strings_count_java_utf16_code_units() {
    let one_unit = StringLimits::new(1, 8);
    assert!(matches!(
        CodecWriter::new().write_string("😀", one_unit),
        Err(CodecError::StringTooLong { utf16_units: 2, .. })
    ));

    let two_units = StringLimits::new(2, 8);
    let mut writer = CodecWriter::new();
    writer.write_string("😀", two_units).unwrap();
    assert_eq!(
        CodecReader::new(writer.as_slice()).read_string(two_units),
        Ok("😀")
    );
}

#[test]
fn malformed_strings_return_specific_errors_before_allocation() {
    let limits = StringLimits::new(8, 24);
    assert!(matches!(
        CodecReader::new(&[0xff, 0xff, 0xff, 0xff, 0x0f]).read_string(limits),
        Err(CodecError::NegativeLength {
            kind: LengthKind::String,
            ..
        })
    ));

    let mut huge = CodecWriter::new();
    huge.write_var_int(10_000);
    assert_eq!(
        CodecReader::new(huge.as_slice()).read_string(limits),
        Err(CodecError::EncodedStringTooLong {
            encoded_bytes: 10_000,
            max_encoded_bytes: 24,
        })
    );
    assert!(matches!(
        CodecReader::new(&[1, 0xff]).read_string(limits),
        Err(CodecError::InvalidUtf8 { .. })
    ));
    assert!(matches!(
        CodecReader::new(&[3, b'a']).read_string(limits),
        Err(CodecError::UnexpectedEnd {
            context: "string bytes",
            ..
        })
    ));
}

#[test]
fn byte_arrays_are_borrowed_and_bounded() {
    let mut writer = CodecWriter::new();
    writer.write_byte_array(&[], 4).unwrap();
    writer.write_byte_array(&[1, 2, 3], 4).unwrap();
    let encoded = writer.into_inner();
    let mut reader = CodecReader::new(&encoded);
    assert_eq!(reader.read_byte_array(4), Ok(&[][..]));
    assert_eq!(reader.read_byte_array(4), Ok(&[1, 2, 3][..]));
}

#[test]
fn malformed_byte_arrays_return_specific_errors() {
    assert!(matches!(
        CodecReader::new(&[0xff, 0xff, 0xff, 0xff, 0x0f]).read_byte_array(8),
        Err(CodecError::NegativeLength {
            kind: LengthKind::ByteArray,
            ..
        })
    ));
    assert_eq!(
        CodecReader::new(&[100]).read_byte_array(8),
        Err(CodecError::ByteArrayTooLong {
            length: 100,
            max: 8
        })
    );
    assert!(matches!(
        CodecReader::new(&[3, 1]).read_byte_array(8),
        Err(CodecError::UnexpectedEnd {
            context: "byte array",
            ..
        })
    ));
}

#[test]
fn uuid_known_fixture_uses_network_order() {
    let bytes = [
        0x12, 0x3e, 0x45, 0x67, 0xe8, 0x9b, 0x12, 0xd3, 0xa4, 0x56, 0x42, 0x66, 0x14, 0x17, 0x40,
        0x00,
    ];
    let uuid = ProtocolUuid::from_bytes(bytes);
    let mut writer = CodecWriter::new();
    writer.write_uuid(uuid);
    assert_eq!(writer.as_slice(), bytes);
    assert_eq!(CodecReader::new(&bytes).read_uuid(), Ok(uuid));
    assert_eq!(ProtocolUuid::from_u128(uuid.as_u128()), uuid);
}

#[test]
fn block_position_boundaries_and_negatives_round_trip() {
    let positions = [
        (0, 0, 0),
        (-1, -1, -1),
        (
            BlockPosition::MIN_XZ,
            BlockPosition::MIN_Y,
            BlockPosition::MIN_XZ,
        ),
        (
            BlockPosition::MAX_XZ,
            BlockPosition::MAX_Y,
            BlockPosition::MAX_XZ,
        ),
    ];
    for (x, y, z) in positions {
        let mut writer = CodecWriter::new();
        writer.write_block_position(x, y, z).unwrap();
        let decoded = CodecReader::new(writer.as_slice())
            .read_block_position()
            .unwrap();
        assert_eq!((decoded.x(), decoded.y(), decoded.z()), (x, y, z));
    }
}

#[test]
fn block_position_rejects_unrepresentable_coordinates() {
    assert!(matches!(
        CodecWriter::new().write_block_position(BlockPosition::MAX_XZ + 1, 0, 0),
        Err(CodecError::InvalidBlockPosition { axis: "x", .. })
    ));
    assert!(matches!(
        CodecWriter::new().write_block_position(0, BlockPosition::MIN_Y - 1, 0),
        Err(CodecError::InvalidBlockPosition { axis: "y", .. })
    ));
}

#[test]
fn bitset_round_trip_is_big_endian_and_canonical() {
    let limits = BitSetLimits::new(4, 256);
    let bitset = BitSet::from_words(vec![1, 0x8000_0000_0000_0001, 0], limits).unwrap();
    assert_eq!(bitset.words(), &[1, 0x8000_0000_0000_0001]);
    assert!(bitset.is_set(0));
    assert!(bitset.is_set(64));
    assert!(bitset.is_set(127));
    assert!(!bitset.is_set(128));

    let mut writer = CodecWriter::new();
    writer.write_bitset(&bitset, limits).unwrap();
    assert_eq!(writer.as_slice().first(), Some(&2));
    let decoded = CodecReader::new(writer.as_slice())
        .read_bitset(limits)
        .unwrap();
    assert_eq!(decoded, bitset);
}

#[test]
fn malformed_bitsets_are_rejected_before_unbounded_allocation() {
    let limits = BitSetLimits::new(2, 128);
    assert!(matches!(
        CodecReader::new(&[0xff, 0xff, 0xff, 0xff, 0x0f]).read_bitset(limits),
        Err(CodecError::NegativeLength {
            kind: LengthKind::BitSet,
            ..
        })
    ));
    assert_eq!(
        CodecReader::new(&[100]).read_bitset(limits),
        Err(CodecError::BitSetTooManyWords {
            words: 100,
            max_words: 2,
        })
    );
    assert!(matches!(
        CodecReader::new(&[1, 0, 0]).read_bitset(limits),
        Err(CodecError::UnexpectedEnd { context: "u64", .. })
    ));
    assert!(matches!(
        BitSet::from_words(vec![0, 1 << 6], BitSetLimits::new(2, 70)),
        Err(CodecError::BitSetBitOutOfRange { bit: 70, .. })
    ));
}
