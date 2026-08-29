use std::collections::BTreeMap;

use cubic_protocol::{
    BitSet, BitSetLimits, BlockPosition, ProtocolUuid,
    nbt::{NbtCompound, NbtString, NbtTag},
    packet_schema::{
        FieldCodec, NamedPacketValue, PacketDefinition, PacketDirection, PacketIdentityCheck,
        PacketLayout, PacketRegistry, PacketSchemaArtifact, PacketSchemaError,
        PacketSchemaFormatVersion, PacketSchemaProvenance, PacketValue, ProtoDefIdentityAlias,
        ProtoDefSource, ProtocolState, UnsupportedLayoutReason, generate_packet_schema_from_report,
        merge_protodef_layouts, parse_packet_schema, serialize_packet_schema,
    },
};
use cubic_version::{MinecraftIdentifier, MinecraftVersionId, ProtocolVersion, Sha1Digest};
use proptest::prelude::*;

const HASH: &str = "0123456789abcdef0123456789abcdef01234567";

fn report() -> &'static [u8] {
    br#"{
      "play": {
        "serverbound": {
          "minecraft:chat": {"protocol_id": 9},
          "minecraft:sparse": {"protocol_id": 100}
        },
        "clientbound": {"minecraft:chat": {"protocol_id": 9}}
      },
      "status": {"serverbound": {"minecraft:status_request": {"protocol_id": 0}}}
    }"#
}

fn generated(version: &str) -> PacketSchemaArtifact {
    generate_packet_schema_from_report(
        MinecraftVersionId::new(version).unwrap(),
        ProtocolVersion::new(775),
        HASH.parse::<Sha1Digest>().unwrap(),
        report(),
    )
    .unwrap()
}

fn field(name: &str, codec: FieldCodec) -> cubic_protocol::packet_schema::PacketField {
    cubic_protocol::packet_schema::PacketField {
        name: name.to_owned(),
        codec,
    }
}

fn value(name: &str, value: PacketValue) -> NamedPacketValue {
    NamedPacketValue {
        name: name.to_owned(),
        value,
    }
}

fn supplemental<'a>(bytes: &'a [u8], aliases: &'a [ProtoDefIdentityAlias]) -> ProtoDefSource<'a> {
    ProtoDefSource {
        bytes,
        source: "Synthetic ProtoDef",
        revision: "0123456789abcdef",
        source_schema: "synthetic-v1",
        content_sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        license: "MIT",
        aliases,
    }
}

fn protodef(packet_id: &str, packet_name: &str, packet_type: &str, layout: &str) -> Vec<u8> {
    format!(r#"{{
      "types": {{
        "identifier": "string",
        "nested": ["container", [
          {{"name":"playerId","type":"UUID"}},
          {{"name":"position","type":"position"}}
        ]]
      }},
      "play": {{"toServer": {{"types": {{
        "{packet_type}": {layout},
        "packet": ["container", [
          {{"name":"name","type":["mapper",{{"type":"varint","mappings":{{"{packet_id}":"{packet_name}"}}}}]}},
          {{"name":"params","type":["switch",{{"compareTo":"name","fields":{{"{packet_name}":"{packet_type}"}}}}]}}
        ]]
      }}}}}}
    }}"#).into_bytes()
}

#[test]
fn official_report_preserves_states_directions_sparse_ids_and_is_deterministic() {
    let first = generated("synthetic-a");
    let second = generated("synthetic-a");
    assert_eq!(
        serialize_packet_schema(&first).unwrap(),
        serialize_packet_schema(&second).unwrap()
    );
    let registry = PacketRegistry::new(first).unwrap();
    assert_eq!(
        registry
            .by_id(ProtocolState::Play, PacketDirection::Serverbound, 100)
            .unwrap()
            .identity
            .as_str(),
        "minecraft:sparse"
    );
    assert_eq!(
        registry
            .by_id(ProtocolState::Play, PacketDirection::Clientbound, 9)
            .unwrap()
            .identity
            .as_str(),
        "minecraft:chat"
    );
    assert!(
        registry
            .by_id(ProtocolState::Status, PacketDirection::Clientbound, 0)
            .is_none()
    );
}

#[test]
fn official_report_rejects_duplicate_keys_ids_bad_states_and_directions() {
    let make = |bytes: &[u8]| {
        generate_packet_schema_from_report(
            MinecraftVersionId::new("x").unwrap(),
            ProtocolVersion::new(1),
            HASH.parse().unwrap(),
            bytes,
        )
    };
    assert!(make(br#"{"play":{"serverbound":{"minecraft:a":{"protocol_id":1},"minecraft:a":{"protocol_id":2}}}}"#).is_err());
    assert!(make(br#"{"play":{"serverbound":{"minecraft:a":{"protocol_id":1},"minecraft:b":{"protocol_id":1}}}}"#).is_err());
    assert!(make(br#"{"future":{"serverbound":{}}}"#).is_err());
    assert!(make(br#"{"play":{"sideways":{}}}"#).is_err());
}

#[test]
fn unsupported_unknown_and_unsupported_schema_versions_are_distinct() {
    let bytes = serialize_packet_schema(&generated("x")).unwrap();
    let registry = parse_packet_schema(&bytes).unwrap();
    assert!(matches!(
        registry.decode(ProtocolState::Play, PacketDirection::Serverbound, 9, &[]),
        Err(PacketSchemaError::UnsupportedPacket { .. })
    ));
    assert!(matches!(
        registry.decode(ProtocolState::Play, PacketDirection::Serverbound, 10, &[]),
        Err(PacketSchemaError::UnknownPacket { .. })
    ));
    let future = String::from_utf8(bytes)
        .unwrap()
        .replace("\"schema_version\": 1", "\"schema_version\": 99");
    assert!(matches!(
        parse_packet_schema(future.as_bytes()),
        Err(PacketSchemaError::UnsupportedSchemaVersion { found: 99, .. })
    ));
}

#[test]
fn bootstrap_overlap_cross_check_fails_loudly_on_disagreement() {
    let registry = PacketRegistry::new(generated("x")).unwrap();
    let matching = PacketIdentityCheck {
        state: ProtocolState::Status,
        direction: PacketDirection::Serverbound,
        identity: "minecraft:status_request",
        id: 0,
    };
    registry.cross_check(&[matching]).unwrap();
    assert!(
        registry
            .cross_check(&[PacketIdentityCheck { id: 7, ..matching }])
            .is_err()
    );
}

#[test]
fn two_exact_versions_keep_ids_and_layouts_isolated() {
    let mut first = generated("synthetic-a");
    let mut second = generated("synthetic-b");
    let first_packet = first
        .packets
        .iter_mut()
        .find(|packet| {
            packet.state == ProtocolState::Play && packet.direction == PacketDirection::Clientbound
        })
        .unwrap();
    first_packet.layout = PacketLayout::Fields {
        fields: vec![field("value", FieldCodec::VarInt)],
    };
    let second_packet = second
        .packets
        .iter_mut()
        .find(|packet| {
            packet.state == ProtocolState::Play && packet.direction == PacketDirection::Clientbound
        })
        .unwrap();
    second_packet.id = 101;
    second_packet.layout = PacketLayout::Fields {
        fields: vec![field(
            "value",
            FieldCodec::String {
                max_utf16_units: 8,
                max_bytes: 24,
            },
        )],
    };
    second.packets.sort_by(|a, b| {
        (a.state, a.direction, a.id, &a.identity).cmp(&(b.state, b.direction, b.id, &b.identity))
    });
    let first = PacketRegistry::new(first).unwrap();
    let second = PacketRegistry::new(second).unwrap();
    assert!(
        first
            .by_id(ProtocolState::Play, PacketDirection::Clientbound, 9)
            .is_some()
    );
    assert!(
        second
            .by_id(ProtocolState::Play, PacketDirection::Clientbound, 101)
            .is_some()
    );
    assert_ne!(
        first.artifact().minecraft_version,
        second.artifact().minecraft_version
    );
}

#[test]
fn composite_layout_round_trips_optional_nested_nbt_uuid_position_and_bitset() {
    let mut artifact = generated("codec-test");
    let packet = artifact
        .packets
        .iter_mut()
        .find(|packet| packet.identity.as_str() == "minecraft:sparse")
        .unwrap();
    packet.layout = PacketLayout::Fields {
        fields: vec![
            field("present", FieldCodec::Bool),
            field(
                "conditional",
                FieldCodec::Conditional {
                    field: "present".to_owned(),
                    equals: true,
                    value: Box::new(FieldCodec::String {
                        max_utf16_units: 16,
                        max_bytes: 48,
                    }),
                },
            ),
            field(
                "optional",
                FieldCodec::Optional {
                    value: Box::new(FieldCodec::VarLong),
                },
            ),
            field(
                "values",
                FieldCodec::List {
                    max_items: 4,
                    element: Box::new(FieldCodec::U16),
                },
            ),
            field(
                "nested",
                FieldCodec::Struct {
                    fields: vec![
                        field("uuid", FieldCodec::Uuid),
                        field("position", FieldCodec::Position),
                    ],
                },
            ),
            field(
                "flags",
                FieldCodec::BitSet {
                    max_words: 2,
                    max_bits: 128,
                },
            ),
            field("metadata", FieldCodec::Nbt),
            field("component", FieldCodec::TextComponent),
        ],
    };
    let registry = PacketRegistry::new(artifact).unwrap();
    let mut compound = NbtCompound::new();
    compound.insert(
        NbtString::from_utf16_units("name".encode_utf16().collect()),
        NbtTag::String(NbtString::from_utf16_units(
            "Cubic".encode_utf16().collect(),
        )),
    );
    let values = vec![
        value("present", PacketValue::Bool(true)),
        value("conditional", PacketValue::String("hello".to_owned())),
        value(
            "optional",
            PacketValue::Optional(Some(Box::new(PacketValue::VarLong(-9)))),
        ),
        value(
            "values",
            PacketValue::List(vec![PacketValue::U16(1), PacketValue::U16(65535)]),
        ),
        value(
            "nested",
            PacketValue::Struct(vec![
                value("uuid", PacketValue::Uuid(ProtocolUuid::from_u128(42))),
                value(
                    "position",
                    PacketValue::Position(BlockPosition::new(-4, 80, 9).unwrap()),
                ),
            ]),
        ),
        value(
            "flags",
            PacketValue::BitSet(BitSet::from_words(vec![5], BitSetLimits::new(2, 128)).unwrap()),
        ),
        value("metadata", PacketValue::Nbt(compound)),
        value(
            "component",
            PacketValue::NbtTag(NbtTag::String(NbtString::from_utf16_units(
                "hello".encode_utf16().collect(),
            ))),
        ),
    ];
    let body = registry
        .encode(
            ProtocolState::Play,
            PacketDirection::Serverbound,
            &MinecraftIdentifier::new("minecraft:sparse").unwrap(),
            &values,
        )
        .unwrap();
    assert_eq!(body[0], 100);
    let decoded = registry
        .decode(
            ProtocolState::Play,
            PacketDirection::Serverbound,
            100,
            &body[1..],
        )
        .unwrap();
    assert_eq!(decoded.fields, values);
}

#[test]
fn malformed_truncated_trailing_and_bounds_fail_structurally() {
    let mut artifact = generated("bounds");
    let packet = artifact
        .packets
        .iter_mut()
        .find(|packet| packet.identity.as_str() == "minecraft:sparse")
        .unwrap();
    packet.layout = PacketLayout::Fields {
        fields: vec![
            field(
                "text",
                FieldCodec::String {
                    max_utf16_units: 2,
                    max_bytes: 6,
                },
            ),
            field("bytes", FieldCodec::ByteArray { max_bytes: 2 }),
            field(
                "items",
                FieldCodec::List {
                    max_items: 1,
                    element: Box::new(FieldCodec::VarInt),
                },
            ),
        ],
    };
    let registry = PacketRegistry::new(artifact).unwrap();
    assert!(
        registry
            .decode(
                ProtocolState::Play,
                PacketDirection::Serverbound,
                100,
                &[5, b'a']
            )
            .is_err()
    );
    assert!(
        registry
            .decode(
                ProtocolState::Play,
                PacketDirection::Serverbound,
                100,
                &[1, b'a', 0, 0, 99]
            )
            .is_err()
    );
    let values = vec![
        value("text", PacketValue::String("abc".to_owned())),
        value("bytes", PacketValue::Bytes(vec![])),
        value("items", PacketValue::List(vec![])),
    ];
    assert!(
        registry
            .encode(
                ProtocolState::Play,
                PacketDirection::Serverbound,
                &MinecraftIdentifier::new("minecraft:sparse").unwrap(),
                &values
            )
            .is_err()
    );
}

#[test]
fn invalid_layouts_are_rejected_before_runtime_use() {
    let base = PacketSchemaArtifact {
        schema_version: PacketSchemaFormatVersion::CURRENT,
        minecraft_version: MinecraftVersionId::new("x").unwrap(),
        protocol_version: ProtocolVersion::new(1),
        provenance: PacketSchemaProvenance {
            official_report_sha1: HASH.parse().unwrap(),
            supplemental: None,
        },
        packets: vec![PacketDefinition {
            state: ProtocolState::Play,
            direction: PacketDirection::Clientbound,
            identity: MinecraftIdentifier::new("minecraft:test").unwrap(),
            id: 0,
            layout: PacketLayout::Unsupported {
                reason:
                    cubic_protocol::packet_schema::UnsupportedLayoutReason::NoStructuralSourceEntry,
            },
        }],
    };
    let mut invalid = base.clone();
    invalid.packets[0].layout = PacketLayout::Fields {
        fields: vec![field(
            "value",
            FieldCodec::Conditional {
                field: "missing".to_owned(),
                equals: true,
                value: Box::new(FieldCodec::I32),
            },
        )],
    };
    assert!(PacketRegistry::new(invalid).is_err());
    let mut invalid = base.clone();
    invalid.packets[0].layout = PacketLayout::Fields {
        fields: vec![
            field("same", FieldCodec::I32),
            field("same", FieldCodec::I64),
        ],
    };
    assert!(PacketRegistry::new(invalid).is_err());
    let mut invalid = base;
    invalid.packets[0].layout = PacketLayout::Fields {
        fields: vec![field(
            "kind",
            FieldCodec::Enum {
                values: BTreeMap::new(),
            },
        )],
    };
    assert!(PacketRegistry::new(invalid).is_err());
}

#[test]
fn supplemental_merge_preserves_field_order_and_translates_composites() {
    let bytes = protodef(
        "0x09",
        "chat_message",
        "packet_chat_message",
        r#"["container",[
          {"name":"message","type":"string"},
          {"name":"timestamp","type":"i64"},
          {"name":"signature","type":["option",["buffer",{"count":256}]]},
          {"name":"seen","type":["array",{"countType":"varint","type":"varint"}]},
          {"name":"kind","type":["mapper",{"type":"varint","mappings":{"0x00":"normal","0x01":"system"}}]},
          {"name":"nestedValue","type":"nested"},
          {"name":"metadata","type":"anonymousNbt"}
        ]]"#,
    );
    let aliases = [ProtoDefIdentityAlias {
        state: ProtocolState::Play,
        direction: PacketDirection::Serverbound,
        source: "chat_message",
        official: "minecraft:chat",
    }];
    let artifact =
        merge_protodef_layouts(generated("synthetic-a"), supplemental(&bytes, &aliases)).unwrap();
    let registry = PacketRegistry::new(artifact).unwrap();
    let packet = registry
        .by_identity(
            ProtocolState::Play,
            PacketDirection::Serverbound,
            &MinecraftIdentifier::new("minecraft:chat").unwrap(),
        )
        .unwrap();
    let PacketLayout::Fields { fields } = &packet.layout else {
        panic!("expected generated fields")
    };
    assert_eq!(
        fields
            .iter()
            .map(|field| field.name.as_str())
            .collect::<Vec<_>>(),
        vec![
            "message",
            "timestamp",
            "signature",
            "seen",
            "kind",
            "nested_value",
            "metadata"
        ]
    );
    assert!(matches!(fields[2].codec, FieldCodec::Optional { .. }));
    assert!(matches!(
        fields[3].codec,
        FieldCodec::List {
            max_items: 65_535,
            ..
        }
    ));
    assert!(matches!(fields[4].codec, FieldCodec::Enum { .. }));
    assert!(matches!(fields[5].codec, FieldCodec::Struct { .. }));
    assert_eq!(
        registry
            .artifact()
            .provenance
            .supplemental
            .as_ref()
            .unwrap()
            .license,
        "MIT"
    );
    let serialized = serialize_packet_schema(registry.artifact()).unwrap();
    parse_packet_schema(&serialized).unwrap();
}

#[test]
fn supplemental_merge_rejects_mojang_id_disagreement_and_duplicate_keys() {
    let aliases = [ProtoDefIdentityAlias {
        state: ProtocolState::Play,
        direction: PacketDirection::Serverbound,
        source: "chat_message",
        official: "minecraft:chat",
    }];
    let wrong_id = protodef(
        "0x08",
        "chat_message",
        "packet_chat_message",
        r#"["container",[]]"#,
    );
    assert!(merge_protodef_layouts(generated("x"), supplemental(&wrong_id, &aliases)).is_err());
    let duplicate = br#"{"types":{},"types":{}}"#;
    assert!(merge_protodef_layouts(generated("x"), supplemental(duplicate, &[])).is_err());
}

#[test]
fn unsupported_construct_and_ambiguous_identity_remain_explicit() {
    let unsupported = protodef(
        "0x09",
        "chat",
        "packet_chat",
        r#"["container",[{"name":"value","type":["switch",{"compareTo":"kind","fields":{}}]}]]"#,
    );
    let artifact = merge_protodef_layouts(generated("x"), supplemental(&unsupported, &[])).unwrap();
    let packet = artifact
        .packets
        .iter()
        .find(|packet| {
            packet.identity.as_str() == "minecraft:chat"
                && packet.direction == PacketDirection::Serverbound
        })
        .unwrap();
    assert!(matches!(
        packet.layout,
        PacketLayout::Unsupported {
            reason: UnsupportedLayoutReason::UnsupportedConditionalConstruct { .. }
        }
    ));

    let ambiguous = protodef(
        "0x09",
        "legacy_chat_name",
        "packet_chat",
        r#"["container",[]]"#,
    );
    let artifact = merge_protodef_layouts(generated("x"), supplemental(&ambiguous, &[])).unwrap();
    let packet = artifact
        .packets
        .iter()
        .find(|packet| {
            packet.identity.as_str() == "minecraft:chat"
                && packet.direction == PacketDirection::Serverbound
        })
        .unwrap();
    assert!(matches!(
        packet.layout,
        PacketLayout::Unsupported {
            reason: UnsupportedLayoutReason::AmbiguousIdentityMapping { .. }
        }
    ));
}

#[test]
fn supplemental_merge_is_deterministic_and_version_isolated() {
    let bytes = protodef(
        "0x09",
        "chat",
        "packet_chat",
        r#"["container",[{"name":"message","type":"string"}]]"#,
    );
    let first =
        merge_protodef_layouts(generated("synthetic-a"), supplemental(&bytes, &[])).unwrap();
    let second =
        merge_protodef_layouts(generated("synthetic-a"), supplemental(&bytes, &[])).unwrap();
    assert_eq!(
        serialize_packet_schema(&first).unwrap(),
        serialize_packet_schema(&second).unwrap()
    );
    let other = generated("synthetic-b");
    assert_ne!(first.minecraft_version, other.minecraft_version);
    assert!(matches!(
        other
            .packets
            .iter()
            .find(|packet| packet.identity.as_str() == "minecraft:chat")
            .unwrap()
            .layout,
        PacketLayout::Unsupported { .. }
    ));
}

#[test]
fn supplemental_boolean_conditionals_translate_and_round_trip() {
    let bytes = protodef(
        "0x09",
        "chat",
        "packet_chat",
        r#"["container",[
          {"name":"present","type":"bool"},
          {"name":"detail","type":["switch",{"compareTo":"present","fields":{"true":"varint","false":"void"}}]}
        ]]"#,
    );
    let artifact = merge_protodef_layouts(generated("x"), supplemental(&bytes, &[])).unwrap();
    let registry = PacketRegistry::new(artifact).unwrap();
    let identity = MinecraftIdentifier::new("minecraft:chat").unwrap();
    let values = vec![
        value("present", PacketValue::Bool(true)),
        value("detail", PacketValue::VarInt(42)),
    ];
    let encoded = registry
        .encode(
            ProtocolState::Play,
            PacketDirection::Serverbound,
            &identity,
            &values,
        )
        .unwrap();
    let decoded = registry
        .decode(
            ProtocolState::Play,
            PacketDirection::Serverbound,
            9,
            &encoded[1..],
        )
        .unwrap();
    assert_eq!(decoded.fields, values);
}

#[test]
fn supplemental_aliases_are_state_and_direction_scoped() {
    let bytes = protodef("0x09", "legacy", "packet_legacy", r#"["container",[]]"#);
    let aliases = [ProtoDefIdentityAlias {
        state: ProtocolState::Play,
        direction: PacketDirection::Clientbound,
        source: "legacy",
        official: "minecraft:sparse",
    }];
    assert!(merge_protodef_layouts(generated("x"), supplemental(&bytes, &aliases)).is_err());
}

proptest! {
    #[test]
    fn bounded_varint_lists_round_trip(values in proptest::collection::vec(any::<i32>(), 0..32)) {
        let mut artifact = generated("property");
        let packet = artifact.packets.iter_mut().find(|packet| packet.identity.as_str() == "minecraft:sparse").unwrap();
        packet.layout = PacketLayout::Fields { fields: vec![field("values", FieldCodec::List { max_items: 32, element: Box::new(FieldCodec::VarInt) })] };
        let registry = PacketRegistry::new(artifact).unwrap();
        let fields = vec![value("values", PacketValue::List(values.iter().copied().map(PacketValue::VarInt).collect()))];
        let body = registry.encode(ProtocolState::Play, PacketDirection::Serverbound, &MinecraftIdentifier::new("minecraft:sparse").unwrap(), &fields).unwrap();
        let decoded = registry.decode(ProtocolState::Play, PacketDirection::Serverbound, 100, &body[1..]).unwrap();
        prop_assert_eq!(decoded.fields, fields);
    }
}
