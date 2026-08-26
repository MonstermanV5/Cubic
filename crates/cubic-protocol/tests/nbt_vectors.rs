use cubic_protocol::{
    CodecReader,
    nbt::{
        NamedNbtRoot, NbtCompound, NbtLimits, NbtList, NbtString, NbtTag, NbtTagType,
        decode_named_root_complete, decode_unnamed_network_root,
        decode_unnamed_network_root_complete, encode_named_root, encode_unnamed_network_root,
    },
};

fn unnamed_child(tag_type: u8, name: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut bytes = vec![10, tag_type, 0, u8::try_from(name.len()).unwrap()];
    bytes.extend_from_slice(name);
    bytes.extend_from_slice(payload);
    bytes.push(0);
    bytes
}

#[test]
fn independent_empty_root_vectors_cover_both_root_forms() {
    let named = decode_named_root_complete(&[10, 0, 0, 0], NbtLimits::default()).unwrap();
    assert!(named.name.as_utf16_units().is_empty());
    assert!(named.compound.is_empty());
    assert_eq!(
        encode_named_root(&named, NbtLimits::default()).unwrap(),
        [10, 0, 0, 0]
    );

    let unnamed = decode_unnamed_network_root_complete(&[10, 0], NbtLimits::default()).unwrap();
    assert!(unnamed.is_empty());
    assert_eq!(
        encode_unnamed_network_root(&unnamed, NbtLimits::default()).unwrap(),
        [10, 0]
    );
}

#[test]
fn independent_compound_int_and_nested_vectors_decode() {
    let int_root = [10, 3, 0, 1, b'a', 0, 0, 0, 42, 0];
    let decoded = decode_unnamed_network_root_complete(&int_root, NbtLimits::default()).unwrap();
    assert_eq!(decoded.get_int("a"), Some(42));

    let nested = [10, 10, 0, 1, b'n', 1, 0, 1, b'b', 0xfe, 0, 0];
    let decoded = decode_unnamed_network_root_complete(&nested, NbtLimits::default()).unwrap();
    let Some(NbtTag::Compound(child)) = decoded.get_str("n") else {
        panic!("expected nested compound");
    };
    assert_eq!(child.get_str("b"), Some(&NbtTag::Byte(-2)));
}

#[test]
fn independent_list_and_array_vectors_decode() {
    let list = [
        10, 9, 0, 1, b'l', 3, 0, 0, 0, 2, 0, 0, 0, 7, 0xff, 0xff, 0xff, 0, 0,
    ];
    let decoded = decode_unnamed_network_root_complete(&list, NbtLimits::default()).unwrap();
    let Some(NbtTag::List(list)) = decoded.get_str("l") else {
        panic!("expected list");
    };
    assert_eq!(list.element_type(), Some(NbtTagType::Int));
    assert_eq!(list.elements(), [NbtTag::Int(7), NbtTag::Int(-256)]);

    let byte_array = unnamed_child(7, b"b", &[0, 0, 0, 3, 0, 0x80, 0xff]);
    let decoded = decode_unnamed_network_root_complete(&byte_array, NbtLimits::default()).unwrap();
    assert_eq!(
        decoded.get_str("b"),
        Some(&NbtTag::ByteArray(vec![0, 128, 255]))
    );

    let int_array = unnamed_child(11, b"i", &[0, 0, 0, 2, 0, 0, 0, 1, 0xff, 0xff, 0xff, 0xfe]);
    let decoded = decode_unnamed_network_root_complete(&int_array, NbtLimits::default()).unwrap();
    assert_eq!(decoded.get_str("i"), Some(&NbtTag::IntArray(vec![1, -2])));

    let long_array = unnamed_child(
        12,
        b"q",
        &[0, 0, 0, 1, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
    );
    let decoded = decode_unnamed_network_root_complete(&long_array, NbtLimits::default()).unwrap();
    assert_eq!(decoded.get_str("q"), Some(&NbtTag::LongArray(vec![-1])));
}

#[test]
fn modified_utf8_known_vectors_are_lossless() {
    let fixtures: &[(&[u16], &[u8])] = &[
        (&[], &[]),
        (&[b'A' as u16], b"A"),
        (&[0], &[0xc0, 0x80]),
        (&[0x00e9], &[0xc3, 0xa9]),
        (&[0x4e2d], &[0xe4, 0xb8, 0xad]),
        (&[0xd83d, 0xde00], &[0xed, 0xa0, 0xbd, 0xed, 0xb8, 0x80]),
        (&[0xd800], &[0xed, 0xa0, 0x80]),
        (&[0xdc00], &[0xed, 0xb0, 0x80]),
    ];

    for (units, encoded) in fixtures {
        let root = NamedNbtRoot {
            name: NbtString::from_utf16_units(units.to_vec()),
            compound: NbtCompound::new(),
        };
        let bytes = encode_named_root(&root, NbtLimits::default()).unwrap();
        assert_eq!(
            &bytes[1..3],
            &(u16::try_from(encoded.len()).unwrap()).to_be_bytes()
        );
        assert_eq!(&bytes[3..3 + encoded.len()], *encoded);
        assert_eq!(
            decode_named_root_complete(&bytes, NbtLimits::default())
                .unwrap()
                .name,
            root.name
        );
    }
}

#[test]
fn supplementary_rust_text_becomes_a_surrogate_pair() {
    let value = NbtString::from("😀");
    assert_eq!(value.as_utf16_units(), [0xd83d, 0xde00]);
    assert_eq!(value.to_rust_string().unwrap(), "😀");
    assert!(
        NbtString::from_utf16_units(vec![0xd800])
            .to_rust_string()
            .is_err()
    );
}

#[test]
fn maximum_modified_utf8_length_is_enforced_without_truncation() {
    let permitted = NamedNbtRoot {
        name: NbtString::from_utf16_units(vec![u16::from(b'a'); u16::MAX as usize]),
        compound: NbtCompound::new(),
    };
    let encoded = encode_named_root(&permitted, NbtLimits::default()).unwrap();
    assert_eq!(&encoded[1..3], &[0xff, 0xff]);

    let oversized = NamedNbtRoot {
        name: NbtString::from_utf16_units(vec![u16::from(b'a'); u16::MAX as usize + 1]),
        compound: NbtCompound::new(),
    };
    assert!(matches!(
        encode_named_root(&oversized, NbtLimits::default()),
        Err(cubic_protocol::nbt::NbtError::StringTooLong {
            encoded_bytes: 65_536,
            max: 65_535
        })
    ));
}

#[test]
fn every_tag_type_round_trips_with_exact_float_bits() {
    let mut nested = NbtCompound::new();
    nested.insert(NbtString::from("inner"), NbtTag::Long(i64::MIN));
    let mut root = NbtCompound::new();
    root.insert(NbtString::from("byte"), NbtTag::Byte(i8::MIN));
    root.insert(NbtString::from("short"), NbtTag::Short(i16::MAX));
    root.insert(NbtString::from("int"), NbtTag::Int(i32::MIN));
    root.insert(NbtString::from("long"), NbtTag::Long(i64::MAX));
    root.insert(
        NbtString::from("float"),
        NbtTag::Float(f32::from_bits(0x7fc1_2345)),
    );
    root.insert(
        NbtString::from("double"),
        NbtTag::Double(f64::from_bits(0xfff8_0000_0000_1234)),
    );
    root.insert(
        NbtString::from("bytes"),
        NbtTag::ByteArray(vec![0, 128, 255]),
    );
    root.insert(
        NbtString::from("string"),
        NbtTag::String(NbtString::from("x\0😀")),
    );
    root.insert(
        NbtString::from("list"),
        NbtTag::List(NbtList::new(NbtTagType::Short, vec![NbtTag::Short(-7)]).unwrap()),
    );
    root.insert(NbtString::from("compound"), NbtTag::Compound(nested));
    root.insert(
        NbtString::from("ints"),
        NbtTag::IntArray(vec![i32::MIN, 0, i32::MAX]),
    );
    root.insert(
        NbtString::from("longs"),
        NbtTag::LongArray(vec![i64::MIN, i64::MAX]),
    );

    let encoded = encode_unnamed_network_root(&root, NbtLimits::default()).unwrap();
    let decoded = decode_unnamed_network_root_complete(&encoded, NbtLimits::default()).unwrap();
    assert_eq!(decoded, root);
}

#[test]
fn numeric_boundaries_and_special_float_patterns_round_trip() {
    for tag in [
        NbtTag::Byte(i8::MIN),
        NbtTag::Byte(-1),
        NbtTag::Byte(0),
        NbtTag::Byte(1),
        NbtTag::Byte(i8::MAX),
        NbtTag::Short(i16::MIN),
        NbtTag::Short(-1),
        NbtTag::Short(0),
        NbtTag::Short(1),
        NbtTag::Short(i16::MAX),
        NbtTag::Int(i32::MIN),
        NbtTag::Int(i32::MAX),
        NbtTag::Long(i64::MIN),
        NbtTag::Long(i64::MAX),
    ] {
        let mut root = NbtCompound::new();
        root.insert(NbtString::from("v"), tag.clone());
        let encoded = encode_unnamed_network_root(&root, NbtLimits::default()).unwrap();
        let decoded = decode_unnamed_network_root_complete(&encoded, NbtLimits::default()).unwrap();
        assert_eq!(decoded.get_str("v"), Some(&tag));
    }

    for bits in [
        0x0000_0000,
        0x8000_0000,
        0x7f80_0000,
        0xff80_0000,
        0x7fc1_2345,
        0x0000_0001,
        1.5_f32.to_bits(),
    ] {
        let tag = NbtTag::Float(f32::from_bits(bits));
        let mut root = NbtCompound::new();
        root.insert(NbtString::from("v"), tag);
        let encoded = encode_unnamed_network_root(&root, NbtLimits::default()).unwrap();
        let decoded = decode_unnamed_network_root_complete(&encoded, NbtLimits::default()).unwrap();
        let Some(NbtTag::Float(value)) = decoded.get_str("v") else {
            panic!("expected float");
        };
        assert_eq!(value.to_bits(), bits);
    }

    for bits in [
        0x0000_0000_0000_0000,
        0x8000_0000_0000_0000,
        0x7ff0_0000_0000_0000,
        0xfff0_0000_0000_0000,
        0x7ff8_0000_0000_1234,
        0x0000_0000_0000_0001,
        1.5_f64.to_bits(),
    ] {
        let tag = NbtTag::Double(f64::from_bits(bits));
        let mut root = NbtCompound::new();
        root.insert(NbtString::from("v"), tag);
        let encoded = encode_unnamed_network_root(&root, NbtLimits::default()).unwrap();
        let decoded = decode_unnamed_network_root_complete(&encoded, NbtLimits::default()).unwrap();
        let Some(NbtTag::Double(value)) = decoded.get_str("v") else {
            panic!("expected double");
        };
        assert_eq!(value.to_bits(), bits);
    }
}

#[test]
fn list_construction_rejects_heterogeneous_and_end_elements() {
    assert!(matches!(
        NbtList::new(NbtTagType::Int, vec![NbtTag::Short(1)]),
        Err(cubic_protocol::nbt::NbtError::HeterogeneousList { .. })
    ));
    assert_eq!(
        NbtList::new(NbtTagType::End, vec![NbtTag::Int(1)]),
        Err(cubic_protocol::nbt::NbtError::EndListWithElements)
    );
}

#[test]
fn empty_list_preserves_any_declared_type_and_reencodes_canonically() {
    let vector = [10, 9, 0, 1, b'e', 99, 0xff, 0xff, 0xff, 0xff, 0];
    let decoded = decode_unnamed_network_root_complete(&vector, NbtLimits::default()).unwrap();
    let Some(NbtTag::List(list)) = decoded.get_str("e") else {
        panic!("expected list");
    };
    assert!(list.is_empty());
    assert_eq!(list.element_type_id(), 99);

    let reencoded = encode_unnamed_network_root(&decoded, NbtLimits::default()).unwrap();
    assert_eq!(reencoded, [10, 9, 0, 1, b'e', 99, 0, 0, 0, 0, 0]);
    assert_eq!(NbtList::empty().element_type(), Some(NbtTagType::End));
}

#[test]
fn compounds_encode_sorted_and_duplicate_wire_keys_use_last_value() {
    let duplicate = [10, 3, 0, 1, b'x', 0, 0, 0, 1, 3, 0, 1, b'x', 0, 0, 0, 2, 0];
    let decoded = decode_unnamed_network_root_complete(&duplicate, NbtLimits::default()).unwrap();
    assert_eq!(decoded.len(), 1);
    assert_eq!(decoded.get_int("x"), Some(2));

    let mut compound = NbtCompound::new();
    compound.insert(NbtString::from("z"), NbtTag::Byte(1));
    compound.insert(NbtString::from("a"), NbtTag::Byte(2));
    let encoded = encode_unnamed_network_root(&compound, NbtLimits::default()).unwrap();
    assert_eq!(encoded, [10, 1, 0, 1, b'a', 2, 1, 0, 1, b'z', 1, 0]);
}

#[test]
fn reader_api_leaves_trailing_packet_fields_available() {
    let mut reader = CodecReader::new(&[10, 0, 0xaa, 0xbb]);
    let root = decode_unnamed_network_root(&mut reader, NbtLimits::default()).unwrap();
    assert!(root.is_empty());
    assert_eq!(reader.read_u16(), Ok(0xaabb));
}
