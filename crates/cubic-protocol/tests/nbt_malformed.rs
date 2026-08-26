use cubic_protocol::{
    CodecError, CodecReader,
    nbt::{
        NbtCollectionKind, NbtError, NbtLimits, decode_named_root, decode_unnamed_network_root,
        decode_unnamed_network_root_complete,
    },
};

fn decode(bytes: &[u8]) -> Result<cubic_protocol::nbt::NbtCompound, NbtError> {
    decode_unnamed_network_root_complete(bytes, NbtLimits::default())
}

#[test]
fn invalid_and_truncated_root_headers_are_structured_errors() {
    assert_eq!(
        decode(&[]),
        Err(NbtError::Codec(CodecError::UnexpectedEnd {
            context: "NBT root type",
            needed: 1,
            remaining: 0,
        }))
    );
    assert_eq!(
        decode(&[0]),
        Err(NbtError::UnexpectedEndTag {
            context: "NBT root"
        })
    );
    assert_eq!(decode(&[3]), Err(NbtError::InvalidRootType { found: 3 }));

    let mut reader = CodecReader::new(&[10, 0]);
    assert!(matches!(
        decode_named_root(&mut reader, NbtLimits::default()),
        Err(NbtError::Codec(CodecError::UnexpectedEnd { .. }))
    ));
}

#[test]
fn malformed_names_and_strings_are_rejected() {
    assert!(matches!(
        decode(&[10, 1, 0]),
        Err(NbtError::Codec(CodecError::UnexpectedEnd { .. }))
    ));
    assert!(matches!(
        decode(&[10, 1, 0, 2, b'a']),
        Err(NbtError::Codec(CodecError::UnexpectedEnd { .. }))
    ));
    assert_eq!(
        decode(&[10, 1, 0, 2, 0xc2, b'x', 0, 0]),
        Err(NbtError::MalformedModifiedUtf8 { offset: 1 })
    );
    let limits = NbtLimits::default().with_max_string_encoded_bytes(1);
    assert_eq!(
        decode_unnamed_network_root_complete(&[10, 1, 0, 2, b'a', b'b', 0, 0], limits),
        Err(NbtError::StringTooLong {
            encoded_bytes: 2,
            max: 1,
        })
    );
    assert_eq!(
        decode(&[10, 8, 0, 0, 0, 1, 0xf0, 0]),
        Err(NbtError::MalformedModifiedUtf8 { offset: 0 })
    );
}

#[test]
fn malformed_arrays_are_rejected_before_unbounded_allocation() {
    for (id, kind) in [
        (7, NbtCollectionKind::ByteArray),
        (11, NbtCollectionKind::IntArray),
        (12, NbtCollectionKind::LongArray),
    ] {
        assert_eq!(
            decode(&[10, id, 0, 0, 0xff, 0xff, 0xff, 0xff, 0]),
            Err(NbtError::NegativeCollectionLength { kind, value: -1 })
        );
        let limits = NbtLimits::default().with_max_array_elements(2);
        assert_eq!(
            decode_unnamed_network_root_complete(&[10, id, 0, 0, 0, 0, 0, 3, 0], limits),
            Err(NbtError::CollectionTooLarge {
                kind,
                length: 3,
                max: 2,
            })
        );
    }

    assert!(matches!(
        decode(&[10, 7, 0, 0, 0, 0, 0, 2, 1]),
        Err(NbtError::Codec(CodecError::UnexpectedEnd { .. }))
    ));
    assert!(matches!(
        decode(&[10, 11, 0, 0, 0, 0, 0, 1, 0, 0]),
        Err(NbtError::Codec(CodecError::UnexpectedEnd { .. }))
    ));
    assert!(matches!(
        decode(&[10, 12, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0]),
        Err(NbtError::Codec(CodecError::UnexpectedEnd { .. }))
    ));
}

#[test]
fn malformed_lists_are_rejected() {
    assert_eq!(
        decode(&[10, 9, 0, 0, 0, 0, 0, 0, 1]),
        Err(NbtError::EndListWithElements)
    );
    assert_eq!(
        decode(&[10, 9, 0, 0, 99, 0, 0, 0, 1]),
        Err(NbtError::InvalidTagId { id: 99 })
    );
    let limits = NbtLimits::default().with_max_list_elements(1);
    assert_eq!(
        decode_unnamed_network_root_complete(&[10, 9, 0, 0, 1, 0, 0, 0, 2], limits),
        Err(NbtError::CollectionTooLarge {
            kind: NbtCollectionKind::List,
            length: 2,
            max: 1,
        })
    );
    assert!(matches!(
        decode(&[10, 9, 0, 0, 3, 0, 0, 0, 1, 0, 0]),
        Err(NbtError::Codec(CodecError::UnexpectedEnd { .. }))
    ));
}

#[test]
fn malformed_compounds_are_rejected() {
    assert_eq!(decode(&[10, 99]), Err(NbtError::InvalidTagId { id: 99 }));
    assert!(matches!(
        decode(&[10, 1, 0, 0]),
        Err(NbtError::Codec(CodecError::UnexpectedEnd { .. }))
    ));
    assert!(matches!(
        decode(&[10, 10, 0, 0, 3, 0, 0, 0, 0]),
        Err(NbtError::Codec(CodecError::UnexpectedEnd { .. }))
    ));

    let limits = NbtLimits::default().with_max_compound_entries(1);
    let input = [10, 1, 0, 0, 1, 1, 0, 0, 2, 0];
    assert_eq!(
        decode_unnamed_network_root_complete(&input, limits),
        Err(NbtError::CollectionTooLarge {
            kind: NbtCollectionKind::Compound,
            length: 2,
            max: 1,
        })
    );
}

#[test]
fn deep_nesting_is_rejected_before_stack_growth() {
    let mut input = vec![10];
    for _ in 0..4 {
        input.extend_from_slice(&[10, 0, 0]);
    }
    input.extend_from_slice(&[0; 5]);
    let limits = NbtLimits::default().with_max_depth(2);
    assert_eq!(
        decode_unnamed_network_root_complete(&input, limits),
        Err(NbtError::DepthLimitExceeded { depth: 3, max: 2 })
    );
}

#[test]
fn cumulative_tag_and_allocation_budgets_are_enforced() {
    let tags = [10, 1, 0, 0, 1, 1, 0, 0, 2, 0];
    let tag_limits = NbtLimits::default().with_max_total_tags(2);
    assert_eq!(
        decode_unnamed_network_root_complete(&tags, tag_limits),
        Err(NbtError::TotalTagLimitExceeded { count: 3, max: 2 })
    );

    let mut arrays = vec![10];
    for name in *b"abcd" {
        arrays.extend_from_slice(&[7, 0, 1, name, 0, 0, 0, 16]);
        arrays.extend_from_slice(&[0; 16]);
    }
    arrays.push(0);
    let allocation_limits = NbtLimits::default().with_max_total_allocated_bytes(300);
    assert!(matches!(
        decode_unnamed_network_root_complete(&arrays, allocation_limits),
        Err(NbtError::AllocationBudgetExceeded { .. })
    ));
}

#[test]
fn complete_helper_rejects_trailing_data_but_reader_form_does_not() {
    assert_eq!(
        decode(&[10, 0, 1]),
        Err(NbtError::TrailingData { remaining: 1 })
    );
    let mut reader = CodecReader::new(&[10, 0, 1]);
    decode_unnamed_network_root(&mut reader, NbtLimits::default()).unwrap();
    assert_eq!(reader.remaining(), 1);
}
