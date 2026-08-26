use cubic_protocol::{
    BitSet, BitSetLimits, BlockPosition, CodecReader, CodecWriter, FrameDecoder, FrameLimits,
    ProtocolUuid, StringLimits, encode_frame,
};
use proptest::prelude::*;

proptest! {
    #[test]
    fn arbitrary_var_ints_round_trip(value: i32) {
        let mut writer = CodecWriter::new();
        writer.write_var_int(value);
        prop_assert!(writer.len() <= 5);
        prop_assert_eq!(CodecReader::new(writer.as_slice()).read_var_int(), Ok(value));
    }

    #[test]
    fn arbitrary_var_longs_round_trip(value: i64) {
        let mut writer = CodecWriter::new();
        writer.write_var_long(value);
        prop_assert!(writer.len() <= 10);
        prop_assert_eq!(CodecReader::new(writer.as_slice()).read_var_long(), Ok(value));
    }

    #[test]
    fn fixed_width_integers_round_trip(
        i8_value: i8,
        u8_value: u8,
        i16_value: i16,
        u16_value: u16,
        i32_value: i32,
        u32_value: u32,
        i64_value: i64,
        u64_value: u64,
    ) {
        let mut writer = CodecWriter::new();
        writer.write_i8(i8_value);
        writer.write_u8(u8_value);
        writer.write_i16(i16_value);
        writer.write_u16(u16_value);
        writer.write_i32(i32_value);
        writer.write_u32(u32_value);
        writer.write_i64(i64_value);
        writer.write_u64(u64_value);
        let mut reader = CodecReader::new(writer.as_slice());
        prop_assert_eq!(reader.read_i8(), Ok(i8_value));
        prop_assert_eq!(reader.read_u8(), Ok(u8_value));
        prop_assert_eq!(reader.read_i16(), Ok(i16_value));
        prop_assert_eq!(reader.read_u16(), Ok(u16_value));
        prop_assert_eq!(reader.read_i32(), Ok(i32_value));
        prop_assert_eq!(reader.read_u32(), Ok(u32_value));
        prop_assert_eq!(reader.read_i64(), Ok(i64_value));
        prop_assert_eq!(reader.read_u64(), Ok(u64_value));
    }

    #[test]
    fn f32_raw_bits_round_trip(bits: u32) {
        let mut writer = CodecWriter::new();
        writer.write_f32(f32::from_bits(bits));
        prop_assert_eq!(CodecReader::new(writer.as_slice()).read_f32().unwrap().to_bits(), bits);
    }

    #[test]
    fn f64_raw_bits_round_trip(bits: u64) {
        let mut writer = CodecWriter::new();
        writer.write_f64(f64::from_bits(bits));
        prop_assert_eq!(CodecReader::new(writer.as_slice()).read_f64().unwrap().to_bits(), bits);
    }

    #[test]
    fn bounded_utf8_strings_round_trip(chars in proptest::collection::vec(any::<char>(), 0..64)) {
        let value: String = chars.into_iter().collect();
        let limits = StringLimits::new(128, 384);
        let mut writer = CodecWriter::new();
        writer.write_string(&value, limits).unwrap();
        prop_assert_eq!(CodecReader::new(writer.as_slice()).read_string(limits), Ok(value.as_str()));
    }

    #[test]
    fn uuid_bits_round_trip(value: u128) {
        let uuid = ProtocolUuid::from_u128(value);
        let mut writer = CodecWriter::new();
        writer.write_uuid(uuid);
        prop_assert_eq!(CodecReader::new(writer.as_slice()).read_uuid(), Ok(uuid));
    }

    #[test]
    fn representable_positions_round_trip(
        x in BlockPosition::MIN_XZ..=BlockPosition::MAX_XZ,
        y in BlockPosition::MIN_Y..=BlockPosition::MAX_Y,
        z in BlockPosition::MIN_XZ..=BlockPosition::MAX_XZ,
    ) {
        let mut writer = CodecWriter::new();
        writer.write_block_position(x, y, z).unwrap();
        let decoded = CodecReader::new(writer.as_slice()).read_block_position().unwrap();
        prop_assert_eq!((decoded.x(), decoded.y(), decoded.z()), (x, y, z));
    }

    #[test]
    fn bounded_byte_arrays_round_trip(value in proptest::collection::vec(any::<u8>(), 0..512)) {
        let mut writer = CodecWriter::new();
        writer.write_byte_array(&value, 512).unwrap();
        prop_assert_eq!(CodecReader::new(writer.as_slice()).read_byte_array(512), Ok(value.as_slice()));
    }

    #[test]
    fn bounded_bitsets_round_trip(words in proptest::collection::vec(any::<u64>(), 0..8)) {
        let limits = BitSetLimits::new(8, 512);
        let bitset = BitSet::from_words(words, limits).unwrap();
        let mut writer = CodecWriter::new();
        writer.write_bitset(&bitset, limits).unwrap();
        prop_assert_eq!(CodecReader::new(writer.as_slice()).read_bitset(limits), Ok(bitset));
    }

    #[test]
    fn frames_survive_arbitrary_fragmentation(
        frames in proptest::collection::vec(proptest::collection::vec(any::<u8>(), 0..128), 1..8),
        fragment_sizes in proptest::collection::vec(0usize..64, 0..64),
    ) {
        let mut encoded = Vec::new();
        for frame in &frames {
            encoded.extend(encode_frame(frame, 1024).unwrap());
        }
        let limits = FrameLimits::new(1024, 8192).unwrap();
        let mut decoder = FrameDecoder::new(limits);
        let mut emitted = Vec::new();
        let mut offset = 0;
        for fragment_size in fragment_sizes {
            if offset == encoded.len() {
                break;
            }
            let size = fragment_size.max(1);
            let end = offset.saturating_add(size).min(encoded.len());
            decoder.push(&encoded[offset..end]).unwrap();
            offset = end;
            while let Some(frame) = decoder.next_frame().unwrap() {
                emitted.push(frame);
            }
        }
        if offset < encoded.len() {
            decoder.push(&encoded[offset..]).unwrap();
        }
        while let Some(frame) = decoder.next_frame().unwrap() {
            emitted.push(frame);
        }
        prop_assert_eq!(emitted, frames);
    }
}
