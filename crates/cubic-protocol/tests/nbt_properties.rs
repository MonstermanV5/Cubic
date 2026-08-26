use cubic_protocol::nbt::{
    NbtCompound, NbtLimits, NbtList, NbtString, NbtTag, NbtTagType,
    decode_unnamed_network_root_complete, encode_unnamed_network_root,
};
use proptest::prelude::*;

fn round_trip(tag: NbtTag) -> NbtTag {
    let mut root = NbtCompound::new();
    root.insert(NbtString::from("v"), tag);
    let encoded = encode_unnamed_network_root(&root, NbtLimits::default()).unwrap();
    decode_unnamed_network_root_complete(&encoded, NbtLimits::default())
        .unwrap()
        .get_str("v")
        .unwrap()
        .clone()
}

proptest! {
    #[test]
    fn integer_tags_round_trip(byte: i8, short: i16, int: i32, long: i64) {
        prop_assert_eq!(round_trip(NbtTag::Byte(byte)), NbtTag::Byte(byte));
        prop_assert_eq!(round_trip(NbtTag::Short(short)), NbtTag::Short(short));
        prop_assert_eq!(round_trip(NbtTag::Int(int)), NbtTag::Int(int));
        prop_assert_eq!(round_trip(NbtTag::Long(long)), NbtTag::Long(long));
    }

    #[test]
    fn float_bits_round_trip(bits: u32) {
        let NbtTag::Float(value) = round_trip(NbtTag::Float(f32::from_bits(bits))) else {
            return Err(TestCaseError::fail("wrong decoded tag type"));
        };
        prop_assert_eq!(value.to_bits(), bits);
    }

    #[test]
    fn double_bits_round_trip(bits: u64) {
        let NbtTag::Double(value) = round_trip(NbtTag::Double(f64::from_bits(bits))) else {
            return Err(TestCaseError::fail("wrong decoded tag type"));
        };
        prop_assert_eq!(value.to_bits(), bits);
    }

    #[test]
    fn arbitrary_java_utf16_units_round_trip(units in proptest::collection::vec(any::<u16>(), 0..64)) {
        let string = NbtString::from_utf16_units(units);
        prop_assert_eq!(round_trip(NbtTag::String(string.clone())), NbtTag::String(string));
    }

    #[test]
    fn byte_arrays_round_trip(values in proptest::collection::vec(any::<u8>(), 0..128)) {
        prop_assert_eq!(round_trip(NbtTag::ByteArray(values.clone())), NbtTag::ByteArray(values));
    }

    #[test]
    fn int_arrays_round_trip(values in proptest::collection::vec(any::<i32>(), 0..64)) {
        prop_assert_eq!(round_trip(NbtTag::IntArray(values.clone())), NbtTag::IntArray(values));
    }

    #[test]
    fn long_arrays_round_trip(values in proptest::collection::vec(any::<i64>(), 0..64)) {
        prop_assert_eq!(round_trip(NbtTag::LongArray(values.clone())), NbtTag::LongArray(values));
    }

    #[test]
    fn homogeneous_lists_round_trip(values in proptest::collection::vec(any::<i32>(), 0..32)) {
        let elements = values.into_iter().map(NbtTag::Int).collect();
        let list = NbtList::new(NbtTagType::Int, elements).unwrap();
        prop_assert_eq!(round_trip(NbtTag::List(list.clone())), NbtTag::List(list));
    }

    #[test]
    fn shallow_compounds_round_trip(left: i32, right: i64, text in ".{0,32}") {
        let mut compound = NbtCompound::new();
        compound.insert(NbtString::from("left"), NbtTag::Int(left));
        compound.insert(NbtString::from("right"), NbtTag::Long(right));
        compound.insert(NbtString::from("text"), NbtTag::String(NbtString::from(text)));
        prop_assert_eq!(round_trip(NbtTag::Compound(compound.clone())), NbtTag::Compound(compound));
    }

    #[test]
    fn bounded_nested_structures_round_trip(values in proptest::collection::vec(any::<i16>(), 0..16)) {
        let list = NbtList::new(
            NbtTagType::Short,
            values.into_iter().map(NbtTag::Short).collect(),
        ).unwrap();
        let mut inner = NbtCompound::new();
        inner.insert(NbtString::from("list"), NbtTag::List(list));
        let mut outer = NbtCompound::new();
        outer.insert(NbtString::from("inner"), NbtTag::Compound(inner));
        prop_assert_eq!(round_trip(NbtTag::Compound(outer.clone())), NbtTag::Compound(outer));
    }
}
