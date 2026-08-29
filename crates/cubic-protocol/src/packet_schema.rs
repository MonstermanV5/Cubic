//! Exact-version packet identities and bounded, data-driven wire layouts.
//!
//! Mojang's Data Generator packet report supplies packet identities and IDs,
//! but not field layouts. An imported report therefore produces
//! [`PacketLayout::Unsupported`] entries. Layouts may be attached later only
//! from a separately validated and provenance-tracked source.

use std::collections::{BTreeMap, BTreeSet};

use cubic_version::{MinecraftIdentifier, MinecraftVersionId, ProtocolVersion, Sha1Digest};
use serde::{Deserialize, Serialize, de::MapAccess};
use thiserror::Error;

use crate::{
    BitSet, BitSetLimits, BlockPosition, CodecError, CodecReader, CodecWriter, ProtocolUuid,
    StringLimits,
    nbt::{
        NbtCompound, NbtError, NbtLimits, NbtTag, decode_unnamed_network_root,
        decode_unnamed_network_tag, encode_unnamed_network_root, encode_unnamed_network_tag,
    },
};

mod protodef;
pub use protodef::{ProtoDefIdentityAlias, ProtoDefSource, merge_protodef_layouts};

pub const MAX_PACKET_SCHEMA_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_PACKETS: usize = 4_096;
pub const MAX_FIELDS_PER_PACKET: usize = 256;
pub const MAX_FIELD_NESTING: usize = 32;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct PacketSchemaFormatVersion(u32);

impl PacketSchemaFormatVersion {
    pub const CURRENT: Self = Self(1);

    #[must_use]
    pub const fn value(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolState {
    Handshake,
    Status,
    Login,
    Configuration,
    Play,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PacketDirection {
    Serverbound,
    Clientbound,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PacketSchemaProvenance {
    pub official_report_sha1: Sha1Digest,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supplemental: Option<SupplementalPacketSchemaProvenance>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SupplementalPacketSchemaProvenance {
    pub source: String,
    pub revision: String,
    pub source_schema: String,
    pub content_sha256: String,
    pub license: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PacketSchemaArtifact {
    pub schema_version: PacketSchemaFormatVersion,
    pub minecraft_version: MinecraftVersionId,
    pub protocol_version: ProtocolVersion,
    pub provenance: PacketSchemaProvenance,
    pub packets: Vec<PacketDefinition>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PacketDefinition {
    pub state: ProtocolState,
    pub direction: PacketDirection,
    pub identity: MinecraftIdentifier,
    pub id: u32,
    pub layout: PacketLayout,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PacketIdentityCheck {
    pub state: ProtocolState,
    pub direction: PacketDirection,
    pub identity: &'static str,
    pub id: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "availability", rename_all = "snake_case")]
pub enum PacketLayout {
    Unsupported { reason: UnsupportedLayoutReason },
    Fields { fields: Vec<PacketField> },
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum UnsupportedLayoutReason {
    NoStructuralSourceEntry,
    AmbiguousIdentityMapping { source_identity: String },
    UnsupportedCodecConstruct { construct: String },
    UnsupportedConditionalConstruct { construct: String },
}

impl UnsupportedLayoutReason {
    #[must_use]
    pub const fn category(&self) -> &'static str {
        match self {
            Self::NoStructuralSourceEntry => "no_structural_source_entry",
            Self::AmbiguousIdentityMapping { .. } => "ambiguous_identity_mapping",
            Self::UnsupportedCodecConstruct { .. } => "unsupported_codec_construct",
            Self::UnsupportedConditionalConstruct { .. } => "unsupported_conditional_construct",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PacketField {
    pub name: String,
    pub codec: FieldCodec,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FieldCodec {
    Bool,
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    F32,
    F64,
    VarInt,
    VarLong,
    String {
        max_utf16_units: usize,
        max_bytes: usize,
    },
    Identifier {
        max_utf16_units: usize,
        max_bytes: usize,
    },
    Uuid,
    Position,
    ByteArray {
        max_bytes: usize,
    },
    FixedBytes {
        length: usize,
    },
    BitSet {
        max_words: usize,
        max_bits: usize,
    },
    List {
        max_items: usize,
        element: Box<FieldCodec>,
    },
    Optional {
        value: Box<FieldCodec>,
    },
    Enum {
        #[serde(with = "i32_key_map")]
        values: BTreeMap<i32, String>,
    },
    Struct {
        fields: Vec<PacketField>,
    },
    Conditional {
        field: String,
        equals: bool,
        value: Box<FieldCodec>,
    },
    Nbt,
    NbtTag,
    TextComponent,
    RemainingBytes {
        max_bytes: usize,
    },
}

mod i32_key_map {
    use std::collections::BTreeMap;

    use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error};

    pub fn serialize<S>(values: &BTreeMap<i32, String>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        values
            .iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect::<BTreeMap<_, _>>()
            .serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<BTreeMap<i32, String>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = BTreeMap::<String, String>::deserialize(deserializer)?;
        raw.into_iter()
            .map(|(key, value)| {
                key.parse::<i32>()
                    .map(|key| (key, value))
                    .map_err(D::Error::custom)
            })
            .collect()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamedPacketValue {
    pub name: String,
    pub value: PacketValue,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PacketValue {
    Bool(bool),
    I8(i8),
    U8(u8),
    I16(i16),
    U16(u16),
    I32(i32),
    U32(u32),
    I64(i64),
    U64(u64),
    F32Bits(u32),
    F64Bits(u64),
    VarInt(i32),
    VarLong(i64),
    String(String),
    Identifier(String),
    Uuid(ProtocolUuid),
    Position(BlockPosition),
    Bytes(Vec<u8>),
    BitSet(BitSet),
    List(Vec<PacketValue>),
    Optional(Option<Box<PacketValue>>),
    Enum(i32),
    Struct(Vec<NamedPacketValue>),
    Nbt(NbtCompound),
    NbtTag(NbtTag),
    Absent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedPacket<'a> {
    pub definition: &'a PacketDefinition,
    pub fields: Vec<NamedPacketValue>,
}

#[derive(Debug, Error)]
pub enum PacketSchemaError {
    #[error("packet schema JSON is malformed: {0}")]
    MalformedJson(String),
    #[error("unsupported packet schema version {found}; supported version is {supported}")]
    UnsupportedSchemaVersion { found: u32, supported: u32 },
    #[error("invalid packet schema: {0}")]
    InvalidSchema(String),
    #[error("packet {state:?}/{direction:?} ID {id} is unknown")]
    UnknownPacket {
        state: ProtocolState,
        direction: PacketDirection,
        id: u32,
    },
    #[error("packet {identity} is known but has no generated field layout")]
    UnsupportedPacket { identity: MinecraftIdentifier },
    #[error("packet value mismatch at {field}: expected {expected}")]
    ValueMismatch {
        field: String,
        expected: &'static str,
    },
    #[error("packet payload has {remaining} trailing byte(s)")]
    TrailingBytes { remaining: usize },
    #[error("packet codec failed at {field}: {source}")]
    Codec {
        field: String,
        #[source]
        source: CodecError,
    },
    #[error("NBT codec failed at {field}: {source}")]
    Nbt {
        field: String,
        #[source]
        source: NbtError,
    },
}

#[derive(Clone, Debug)]
pub struct PacketRegistry {
    artifact: PacketSchemaArtifact,
    by_id: BTreeMap<(ProtocolState, PacketDirection, u32), usize>,
    by_identity: BTreeMap<(ProtocolState, PacketDirection, MinecraftIdentifier), usize>,
}

impl PacketRegistry {
    pub fn new(artifact: PacketSchemaArtifact) -> Result<Self, PacketSchemaError> {
        validate_artifact(&artifact)?;
        let mut by_id = BTreeMap::new();
        let mut by_identity = BTreeMap::new();
        for (index, packet) in artifact.packets.iter().enumerate() {
            by_id.insert((packet.state, packet.direction, packet.id), index);
            by_identity.insert(
                (packet.state, packet.direction, packet.identity.clone()),
                index,
            );
        }
        Ok(Self {
            artifact,
            by_id,
            by_identity,
        })
    }

    #[must_use]
    pub const fn artifact(&self) -> &PacketSchemaArtifact {
        &self.artifact
    }

    #[must_use]
    pub fn by_id(
        &self,
        state: ProtocolState,
        direction: PacketDirection,
        id: u32,
    ) -> Option<&PacketDefinition> {
        self.by_id
            .get(&(state, direction, id))
            .map(|index| &self.artifact.packets[*index])
    }

    #[must_use]
    pub fn by_identity(
        &self,
        state: ProtocolState,
        direction: PacketDirection,
        identity: &MinecraftIdentifier,
    ) -> Option<&PacketDefinition> {
        self.by_identity
            .get(&(state, direction, identity.clone()))
            .map(|index| &self.artifact.packets[*index])
    }

    pub fn decode(
        &self,
        state: ProtocolState,
        direction: PacketDirection,
        id: u32,
        payload: &[u8],
    ) -> Result<DecodedPacket<'_>, PacketSchemaError> {
        let definition =
            self.by_id(state, direction, id)
                .ok_or(PacketSchemaError::UnknownPacket {
                    state,
                    direction,
                    id,
                })?;
        let PacketLayout::Fields { fields } = &definition.layout else {
            return Err(PacketSchemaError::UnsupportedPacket {
                identity: definition.identity.clone(),
            });
        };
        let mut reader = CodecReader::new(payload);
        let decoded = decode_fields(fields, &mut reader, 0)?;
        if reader.remaining() != 0 {
            return Err(PacketSchemaError::TrailingBytes {
                remaining: reader.remaining(),
            });
        }
        Ok(DecodedPacket {
            definition,
            fields: decoded,
        })
    }

    pub fn encode(
        &self,
        state: ProtocolState,
        direction: PacketDirection,
        identity: &MinecraftIdentifier,
        fields: &[NamedPacketValue],
    ) -> Result<Vec<u8>, PacketSchemaError> {
        let definition = self
            .by_identity(state, direction, identity)
            .ok_or_else(|| {
                PacketSchemaError::InvalidSchema(format!(
                    "unknown packet identity {identity} in {state:?}/{direction:?}"
                ))
            })?;
        let PacketLayout::Fields {
            fields: definitions,
        } = &definition.layout
        else {
            return Err(PacketSchemaError::UnsupportedPacket {
                identity: definition.identity.clone(),
            });
        };
        let mut writer = CodecWriter::new();
        writer.write_var_int(i32::try_from(definition.id).map_err(|_| {
            PacketSchemaError::InvalidSchema("packet ID exceeds VarInt range".to_owned())
        })?);
        encode_fields(definitions, fields, &mut writer, 0)?;
        Ok(writer.into_inner())
    }

    pub fn cross_check(&self, checks: &[PacketIdentityCheck]) -> Result<(), PacketSchemaError> {
        for check in checks {
            let identity = MinecraftIdentifier::new(check.identity)
                .map_err(|error| PacketSchemaError::InvalidSchema(error.to_string()))?;
            let definition = self
                .by_identity(check.state, check.direction, &identity)
                .ok_or_else(|| {
                    PacketSchemaError::InvalidSchema(format!(
                        "official report is missing bootstrap packet {} in {:?}/{:?}",
                        check.identity, check.state, check.direction
                    ))
                })?;
            if definition.id != check.id {
                return invalid(format!(
                    "bootstrap packet {} is ID {}, but the official report declares {}",
                    check.identity, check.id, definition.id
                ));
            }
        }
        Ok(())
    }
}

pub fn generate_packet_schema_from_report(
    minecraft_version: MinecraftVersionId,
    protocol_version: ProtocolVersion,
    official_report_sha1: Sha1Digest,
    report: &[u8],
) -> Result<PacketSchemaArtifact, PacketSchemaError> {
    if report.len() > MAX_PACKET_SCHEMA_BYTES {
        return invalid("official packet report exceeds the size limit");
    }
    let raw: StrictMap<String, StrictMap<String, StrictMap<String, OfficialPacket>>> =
        serde_json::from_slice(report).map_err(|error| {
            PacketSchemaError::MalformedJson(format!(
                "line {}, column {}: {error}",
                error.line(),
                error.column()
            ))
        })?;
    let mut packets = Vec::new();
    for (state, directions) in raw.0 {
        let state = parse_state(&state)?;
        for (direction, definitions) in directions.0 {
            let direction = parse_direction(&direction)?;
            for (identity, packet) in definitions.0 {
                packets.push(PacketDefinition {
                    state,
                    direction,
                    identity: MinecraftIdentifier::new(identity)
                        .map_err(|error| PacketSchemaError::InvalidSchema(error.to_string()))?,
                    id: u32::try_from(packet.protocol_id).map_err(|_| {
                        PacketSchemaError::InvalidSchema(
                            "packet ID is negative or too large".to_owned(),
                        )
                    })?,
                    layout: PacketLayout::Unsupported {
                        reason: UnsupportedLayoutReason::NoStructuralSourceEntry,
                    },
                });
            }
        }
    }
    packets.sort_by(|left, right| {
        (left.state, left.direction, left.id, &left.identity).cmp(&(
            right.state,
            right.direction,
            right.id,
            &right.identity,
        ))
    });
    let artifact = PacketSchemaArtifact {
        schema_version: PacketSchemaFormatVersion::CURRENT,
        minecraft_version,
        protocol_version,
        provenance: PacketSchemaProvenance {
            official_report_sha1,
            supplemental: None,
        },
        packets,
    };
    validate_artifact(&artifact)?;
    Ok(artifact)
}

pub fn serialize_packet_schema(
    artifact: &PacketSchemaArtifact,
) -> Result<Vec<u8>, PacketSchemaError> {
    validate_artifact(artifact)?;
    let mut bytes = serde_json::to_vec_pretty(artifact)
        .map_err(|error| PacketSchemaError::MalformedJson(error.to_string()))?;
    bytes.push(b'\n');
    Ok(bytes)
}

pub fn parse_packet_schema(bytes: &[u8]) -> Result<PacketRegistry, PacketSchemaError> {
    if bytes.len() > MAX_PACKET_SCHEMA_BYTES {
        return invalid("packet schema artifact exceeds the size limit");
    }
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|error| {
        PacketSchemaError::MalformedJson(format!(
            "line {}, column {}: {error}",
            error.line(),
            error.column()
        ))
    })?;
    let found = value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| {
            PacketSchemaError::InvalidSchema("missing or invalid schema_version".to_owned())
        })?;
    if found != PacketSchemaFormatVersion::CURRENT.value() {
        return Err(PacketSchemaError::UnsupportedSchemaVersion {
            found,
            supported: PacketSchemaFormatVersion::CURRENT.value(),
        });
    }
    let artifact = serde_json::from_value(value).map_err(|error| {
        PacketSchemaError::InvalidSchema(format!("artifact shape is invalid: {error}"))
    })?;
    PacketRegistry::new(artifact)
}

fn validate_artifact(artifact: &PacketSchemaArtifact) -> Result<(), PacketSchemaError> {
    if artifact.schema_version != PacketSchemaFormatVersion::CURRENT {
        return Err(PacketSchemaError::UnsupportedSchemaVersion {
            found: artifact.schema_version.value(),
            supported: PacketSchemaFormatVersion::CURRENT.value(),
        });
    }
    if artifact.packets.len() > MAX_PACKETS {
        return invalid("artifact has too many packet definitions");
    }
    if let Some(source) = &artifact.provenance.supplemental {
        for value in [
            &source.source,
            &source.revision,
            &source.source_schema,
            &source.license,
        ] {
            if value.is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
                return invalid("supplemental provenance contains invalid text");
            }
        }
        if source.content_sha256.len() != 64
            || !source
                .content_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return invalid("supplemental provenance SHA-256 is invalid");
        }
    }
    let mut ids = BTreeSet::new();
    let mut identities = BTreeSet::new();
    let mut previous = None;
    for packet in &artifact.packets {
        let key = (packet.state, packet.direction, packet.id, &packet.identity);
        if previous.is_some_and(|prior| prior >= key) {
            return invalid("packet definitions are not in deterministic order");
        }
        previous = Some(key);
        if packet.id > i32::MAX as u32 {
            return invalid("packet ID exceeds non-negative VarInt range");
        }
        if !ids.insert((packet.state, packet.direction, packet.id)) {
            return invalid("duplicate packet ID in one state/direction");
        }
        if !identities.insert((packet.state, packet.direction, &packet.identity)) {
            return invalid("duplicate packet identity in one state/direction");
        }
        match &packet.layout {
            PacketLayout::Fields { fields } => validate_fields(fields, 0)?,
            PacketLayout::Unsupported { reason } => validate_unsupported_reason(reason)?,
        }
    }
    Ok(())
}

fn validate_unsupported_reason(reason: &UnsupportedLayoutReason) -> Result<(), PacketSchemaError> {
    let detail = match reason {
        UnsupportedLayoutReason::NoStructuralSourceEntry => return Ok(()),
        UnsupportedLayoutReason::AmbiguousIdentityMapping { source_identity } => source_identity,
        UnsupportedLayoutReason::UnsupportedCodecConstruct { construct }
        | UnsupportedLayoutReason::UnsupportedConditionalConstruct { construct } => construct,
    };
    if detail.is_empty() || detail.len() > 128 || detail.chars().any(char::is_control) {
        return invalid("unsupported-layout reason contains invalid detail");
    }
    Ok(())
}

fn validate_fields(fields: &[PacketField], depth: usize) -> Result<(), PacketSchemaError> {
    if depth > MAX_FIELD_NESTING {
        return invalid("field schema nesting exceeds the limit");
    }
    if fields.len() > MAX_FIELDS_PER_PACKET {
        return invalid("packet/structure has too many fields");
    }
    let mut names = BTreeSet::new();
    let mut prior_bools = BTreeSet::new();
    for field in fields {
        validate_field_name(&field.name)?;
        if !names.insert(field.name.as_str()) {
            return invalid("duplicate field name");
        }
        validate_codec(&field.codec, depth + 1, &prior_bools)?;
        if field.codec == FieldCodec::Bool {
            prior_bools.insert(field.name.as_str());
        }
    }
    Ok(())
}

fn validate_codec(
    codec: &FieldCodec,
    depth: usize,
    prior_bools: &BTreeSet<&str>,
) -> Result<(), PacketSchemaError> {
    if depth > MAX_FIELD_NESTING {
        return invalid("field schema nesting exceeds the limit");
    }
    match codec {
        FieldCodec::String {
            max_utf16_units,
            max_bytes,
        }
        | FieldCodec::Identifier {
            max_utf16_units,
            max_bytes,
        } => {
            if *max_utf16_units == 0 || *max_bytes == 0 || *max_bytes > MAX_PACKET_SCHEMA_BYTES {
                return invalid("string/identifier requires finite non-zero bounds");
            }
        }
        FieldCodec::ByteArray { max_bytes } | FieldCodec::RemainingBytes { max_bytes } => {
            if *max_bytes > MAX_PACKET_SCHEMA_BYTES {
                return invalid("byte field bound exceeds packet-schema limit");
            }
        }
        FieldCodec::FixedBytes { length } => {
            if *length > MAX_PACKET_SCHEMA_BYTES {
                return invalid("fixed byte field is too large");
            }
        }
        FieldCodec::BitSet {
            max_words,
            max_bits,
        } => {
            if *max_words == 0 || *max_bits == 0 || *max_words > MAX_PACKET_SCHEMA_BYTES / 8 {
                return invalid("BitSet requires finite non-zero bounds");
            }
        }
        FieldCodec::List { max_items, element } => {
            if *max_items > 1_048_576 {
                return invalid("list bound exceeds the schema safety limit");
            }
            validate_codec(element, depth + 1, &BTreeSet::new())?;
        }
        FieldCodec::Optional { value } => validate_codec(value, depth + 1, &BTreeSet::new())?,
        FieldCodec::Enum { values } => {
            if values.is_empty() || values.len() > 4_096 {
                return invalid("enum has an invalid discriminant count");
            }
            let mut labels = BTreeSet::new();
            if values
                .values()
                .any(|value| !valid_field_name(value) || !labels.insert(value))
            {
                return invalid("enum labels are invalid or duplicated");
            }
        }
        FieldCodec::Struct { fields } => validate_fields(fields, depth + 1)?,
        FieldCodec::Conditional { field, value, .. } => {
            if !prior_bools.contains(field.as_str()) {
                return invalid(
                    "conditional field must reference a preceding bool field in the same structure",
                );
            }
            validate_codec(value, depth + 1, &BTreeSet::new())?;
        }
        _ => {}
    }
    Ok(())
}

fn validate_field_name(name: &str) -> Result<(), PacketSchemaError> {
    if valid_field_name(name) {
        Ok(())
    } else {
        invalid("field name is empty, overlong, or contains unsupported characters")
    }
}

fn valid_field_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn decode_fields(
    fields: &[PacketField],
    reader: &mut CodecReader<'_>,
    depth: usize,
) -> Result<Vec<NamedPacketValue>, PacketSchemaError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(fields.len())
        .map_err(|_| PacketSchemaError::InvalidSchema("field allocation failed".to_owned()))?;
    for field in fields {
        let value = decode_value(&field.codec, reader, &values, &field.name, depth + 1)?;
        values.push(NamedPacketValue {
            name: field.name.clone(),
            value,
        });
    }
    Ok(values)
}

fn decode_value(
    codec: &FieldCodec,
    reader: &mut CodecReader<'_>,
    prior: &[NamedPacketValue],
    field: &str,
    depth: usize,
) -> Result<PacketValue, PacketSchemaError> {
    if depth > MAX_FIELD_NESTING {
        return invalid("runtime field nesting exceeds the limit");
    }
    let codec_error = |source| PacketSchemaError::Codec {
        field: field.to_owned(),
        source,
    };
    Ok(match codec {
        FieldCodec::Bool => PacketValue::Bool(reader.read_bool().map_err(codec_error)?),
        FieldCodec::I8 => PacketValue::I8(reader.read_i8().map_err(codec_error)?),
        FieldCodec::U8 => PacketValue::U8(reader.read_u8().map_err(codec_error)?),
        FieldCodec::I16 => PacketValue::I16(reader.read_i16().map_err(codec_error)?),
        FieldCodec::U16 => PacketValue::U16(reader.read_u16().map_err(codec_error)?),
        FieldCodec::I32 => PacketValue::I32(reader.read_i32().map_err(codec_error)?),
        FieldCodec::U32 => PacketValue::U32(reader.read_u32().map_err(codec_error)?),
        FieldCodec::I64 => PacketValue::I64(reader.read_i64().map_err(codec_error)?),
        FieldCodec::U64 => PacketValue::U64(reader.read_u64().map_err(codec_error)?),
        FieldCodec::F32 => PacketValue::F32Bits(reader.read_f32().map_err(codec_error)?.to_bits()),
        FieldCodec::F64 => PacketValue::F64Bits(reader.read_f64().map_err(codec_error)?.to_bits()),
        FieldCodec::VarInt => PacketValue::VarInt(reader.read_var_int().map_err(codec_error)?),
        FieldCodec::VarLong => PacketValue::VarLong(reader.read_var_long().map_err(codec_error)?),
        FieldCodec::String {
            max_utf16_units,
            max_bytes,
        } => PacketValue::String(
            reader
                .read_string(StringLimits::new(*max_utf16_units, *max_bytes))
                .map_err(codec_error)?
                .to_owned(),
        ),
        FieldCodec::Identifier {
            max_utf16_units,
            max_bytes,
        } => {
            let text = reader
                .read_string(StringLimits::new(*max_utf16_units, *max_bytes))
                .map_err(codec_error)?;
            MinecraftIdentifier::new(text).map_err(|error| {
                PacketSchemaError::InvalidSchema(format!(
                    "field {field} contains an invalid identifier: {error}"
                ))
            })?;
            PacketValue::Identifier(text.to_owned())
        }
        FieldCodec::Uuid => PacketValue::Uuid(reader.read_uuid().map_err(codec_error)?),
        FieldCodec::Position => {
            PacketValue::Position(reader.read_block_position().map_err(codec_error)?)
        }
        FieldCodec::ByteArray { max_bytes } => PacketValue::Bytes(
            reader
                .read_byte_array(*max_bytes)
                .map_err(codec_error)?
                .to_vec(),
        ),
        FieldCodec::FixedBytes { length } => PacketValue::Bytes(
            reader
                .read_bytes(*length, "generated fixed bytes")
                .map_err(codec_error)?
                .to_vec(),
        ),
        FieldCodec::BitSet {
            max_words,
            max_bits,
        } => PacketValue::BitSet(
            reader
                .read_bitset(BitSetLimits::new(*max_words, *max_bits))
                .map_err(codec_error)?,
        ),
        FieldCodec::List { max_items, element } => {
            let count = read_count(reader, *max_items, field)?;
            let mut list = Vec::new();
            list.try_reserve_exact(count).map_err(|_| {
                PacketSchemaError::InvalidSchema("list allocation failed".to_owned())
            })?;
            for _ in 0..count {
                list.push(decode_value(element, reader, &[], field, depth + 1)?);
            }
            PacketValue::List(list)
        }
        FieldCodec::Optional { value } => {
            if reader.read_bool().map_err(codec_error)? {
                PacketValue::Optional(Some(Box::new(decode_value(
                    value,
                    reader,
                    &[],
                    field,
                    depth + 1,
                )?)))
            } else {
                PacketValue::Optional(None)
            }
        }
        FieldCodec::Enum { values } => {
            let value = reader.read_var_int().map_err(codec_error)?;
            if !values.contains_key(&value) {
                return Err(PacketSchemaError::ValueMismatch {
                    field: field.to_owned(),
                    expected: "declared enum discriminant",
                });
            }
            PacketValue::Enum(value)
        }
        FieldCodec::Struct { fields } => {
            PacketValue::Struct(decode_fields(fields, reader, depth + 1)?)
        }
        FieldCodec::Conditional {
            field: reference,
            equals,
            value,
        } => {
            if prior_bool(prior, reference, field)? == *equals {
                decode_value(value, reader, &[], field, depth + 1)?
            } else {
                PacketValue::Absent
            }
        }
        FieldCodec::Nbt => PacketValue::Nbt(
            decode_unnamed_network_root(reader, NbtLimits::default()).map_err(|source| {
                PacketSchemaError::Nbt {
                    field: field.to_owned(),
                    source,
                }
            })?,
        ),
        FieldCodec::NbtTag | FieldCodec::TextComponent => PacketValue::NbtTag(
            decode_unnamed_network_tag(reader, NbtLimits::default()).map_err(|source| {
                PacketSchemaError::Nbt {
                    field: field.to_owned(),
                    source,
                }
            })?,
        ),
        FieldCodec::RemainingBytes { max_bytes } => {
            if reader.remaining() > *max_bytes {
                return Err(PacketSchemaError::ValueMismatch {
                    field: field.to_owned(),
                    expected: "bounded remaining payload",
                });
            }
            PacketValue::Bytes(reader.read_remaining().to_vec())
        }
    })
}

fn encode_fields(
    definitions: &[PacketField],
    values: &[NamedPacketValue],
    writer: &mut CodecWriter,
    depth: usize,
) -> Result<(), PacketSchemaError> {
    if definitions.len() != values.len() {
        return Err(PacketSchemaError::ValueMismatch {
            field: "packet".to_owned(),
            expected: "one ordered value per field",
        });
    }
    for (definition, value) in definitions.iter().zip(values) {
        if definition.name != value.name {
            return Err(PacketSchemaError::ValueMismatch {
                field: value.name.clone(),
                expected: "schema field name and order",
            });
        }
        encode_value(
            &definition.codec,
            &value.value,
            values,
            &definition.name,
            writer,
            depth + 1,
        )?;
    }
    Ok(())
}

fn encode_value(
    codec: &FieldCodec,
    value: &PacketValue,
    prior: &[NamedPacketValue],
    field: &str,
    writer: &mut CodecWriter,
    depth: usize,
) -> Result<(), PacketSchemaError> {
    if depth > MAX_FIELD_NESTING {
        return invalid("runtime field nesting exceeds the limit");
    }
    let mismatch = || PacketSchemaError::ValueMismatch {
        field: field.to_owned(),
        expected: codec_name(codec),
    };
    let codec_error = |source| PacketSchemaError::Codec {
        field: field.to_owned(),
        source,
    };
    match (codec, value) {
        (FieldCodec::Bool, PacketValue::Bool(v)) => writer.write_bool(*v),
        (FieldCodec::I8, PacketValue::I8(v)) => writer.write_i8(*v),
        (FieldCodec::U8, PacketValue::U8(v)) => writer.write_u8(*v),
        (FieldCodec::I16, PacketValue::I16(v)) => writer.write_i16(*v),
        (FieldCodec::U16, PacketValue::U16(v)) => writer.write_u16(*v),
        (FieldCodec::I32, PacketValue::I32(v)) => writer.write_i32(*v),
        (FieldCodec::U32, PacketValue::U32(v)) => writer.write_u32(*v),
        (FieldCodec::I64, PacketValue::I64(v)) => writer.write_i64(*v),
        (FieldCodec::U64, PacketValue::U64(v)) => writer.write_u64(*v),
        (FieldCodec::F32, PacketValue::F32Bits(v)) => writer.write_u32(*v),
        (FieldCodec::F64, PacketValue::F64Bits(v)) => writer.write_u64(*v),
        (FieldCodec::VarInt, PacketValue::VarInt(v)) => writer.write_var_int(*v),
        (FieldCodec::VarLong, PacketValue::VarLong(v)) => writer.write_var_long(*v),
        (
            FieldCodec::String {
                max_utf16_units,
                max_bytes,
            },
            PacketValue::String(v),
        ) => writer
            .write_string(v, StringLimits::new(*max_utf16_units, *max_bytes))
            .map_err(codec_error)?,
        (
            FieldCodec::Identifier {
                max_utf16_units,
                max_bytes,
            },
            PacketValue::Identifier(v),
        ) => {
            MinecraftIdentifier::new(v).map_err(|error| {
                PacketSchemaError::InvalidSchema(format!(
                    "field {field} contains an invalid identifier: {error}"
                ))
            })?;
            writer
                .write_string(v, StringLimits::new(*max_utf16_units, *max_bytes))
                .map_err(codec_error)?;
        }
        (FieldCodec::Uuid, PacketValue::Uuid(v)) => writer.write_uuid(*v),
        (FieldCodec::Position, PacketValue::Position(v)) => writer
            .write_block_position(v.x(), v.y(), v.z())
            .map_err(codec_error)?,
        (FieldCodec::ByteArray { max_bytes }, PacketValue::Bytes(v)) => writer
            .write_byte_array(v, *max_bytes)
            .map_err(codec_error)?,
        (FieldCodec::FixedBytes { length }, PacketValue::Bytes(v)) if v.len() == *length => {
            writer.write_bytes(v)
        }
        (
            FieldCodec::BitSet {
                max_words,
                max_bits,
            },
            PacketValue::BitSet(v),
        ) => writer
            .write_bitset(v, BitSetLimits::new(*max_words, *max_bits))
            .map_err(codec_error)?,
        (FieldCodec::List { max_items, element }, PacketValue::List(v))
            if v.len() <= *max_items =>
        {
            write_count(writer, v.len(), field)?;
            for item in v {
                encode_value(element, item, &[], field, writer, depth + 1)?;
            }
        }
        (FieldCodec::Optional { value: inner }, PacketValue::Optional(v)) => {
            writer.write_bool(v.is_some());
            if let Some(v) = v {
                encode_value(inner, v, &[], field, writer, depth + 1)?;
            }
        }
        (FieldCodec::Enum { values }, PacketValue::Enum(v)) if values.contains_key(v) => {
            writer.write_var_int(*v)
        }
        (FieldCodec::Struct { fields }, PacketValue::Struct(v)) => {
            encode_fields(fields, v, writer, depth + 1)?
        }
        (
            FieldCodec::Conditional {
                field: reference,
                equals,
                value: inner,
            },
            PacketValue::Absent,
        ) if prior_bool(prior, reference, field)? != *equals => {}
        (
            FieldCodec::Conditional {
                field: reference,
                equals,
                value: inner,
            },
            value,
        ) if prior_bool(prior, reference, field)? == *equals => {
            encode_value(inner, value, &[], field, writer, depth + 1)?
        }
        (FieldCodec::Nbt, PacketValue::Nbt(v)) => writer.write_bytes(
            &encode_unnamed_network_root(v, NbtLimits::default()).map_err(|source| {
                PacketSchemaError::Nbt {
                    field: field.to_owned(),
                    source,
                }
            })?,
        ),
        (FieldCodec::NbtTag | FieldCodec::TextComponent, PacketValue::NbtTag(v)) => writer
            .write_bytes(
                &encode_unnamed_network_tag(v, NbtLimits::default()).map_err(|source| {
                    PacketSchemaError::Nbt {
                        field: field.to_owned(),
                        source,
                    }
                })?,
            ),
        (FieldCodec::RemainingBytes { max_bytes }, PacketValue::Bytes(v))
            if v.len() <= *max_bytes =>
        {
            writer.write_bytes(v)
        }
        _ => return Err(mismatch()),
    }
    Ok(())
}

fn read_count(
    reader: &mut CodecReader<'_>,
    max: usize,
    field: &str,
) -> Result<usize, PacketSchemaError> {
    let value = reader
        .read_var_int()
        .map_err(|source| PacketSchemaError::Codec {
            field: field.to_owned(),
            source,
        })?;
    let count = usize::try_from(value).map_err(|_| PacketSchemaError::ValueMismatch {
        field: field.to_owned(),
        expected: "non-negative bounded list length",
    })?;
    if count > max {
        return Err(PacketSchemaError::ValueMismatch {
            field: field.to_owned(),
            expected: "bounded list length",
        });
    }
    Ok(count)
}

fn write_count(
    writer: &mut CodecWriter,
    count: usize,
    field: &str,
) -> Result<(), PacketSchemaError> {
    writer.write_var_int(
        i32::try_from(count).map_err(|_| PacketSchemaError::ValueMismatch {
            field: field.to_owned(),
            expected: "VarInt-sized list length",
        })?,
    );
    Ok(())
}

fn prior_bool(
    values: &[NamedPacketValue],
    reference: &str,
    field: &str,
) -> Result<bool, PacketSchemaError> {
    values
        .iter()
        .find(|value| value.name == reference)
        .and_then(|value| match value.value {
            PacketValue::Bool(value) => Some(value),
            _ => None,
        })
        .ok_or_else(|| PacketSchemaError::ValueMismatch {
            field: field.to_owned(),
            expected: "preceding bool condition",
        })
}

fn codec_name(codec: &FieldCodec) -> &'static str {
    match codec {
        FieldCodec::Bool => "bool",
        FieldCodec::I8 => "i8",
        FieldCodec::U8 => "u8",
        FieldCodec::I16 => "i16",
        FieldCodec::U16 => "u16",
        FieldCodec::I32 => "i32",
        FieldCodec::U32 => "u32",
        FieldCodec::I64 => "i64",
        FieldCodec::U64 => "u64",
        FieldCodec::F32 => "f32 bits",
        FieldCodec::F64 => "f64 bits",
        FieldCodec::VarInt => "VarInt",
        FieldCodec::VarLong => "VarLong",
        FieldCodec::String { .. } => "string",
        FieldCodec::Identifier { .. } => "identifier",
        FieldCodec::Uuid => "UUID",
        FieldCodec::Position => "Position",
        FieldCodec::ByteArray { .. }
        | FieldCodec::FixedBytes { .. }
        | FieldCodec::RemainingBytes { .. } => "bytes",
        FieldCodec::BitSet { .. } => "BitSet",
        FieldCodec::List { .. } => "list",
        FieldCodec::Optional { .. } => "optional",
        FieldCodec::Enum { .. } => "enum",
        FieldCodec::Struct { .. } => "structure",
        FieldCodec::Conditional { .. } => "conditional value",
        FieldCodec::Nbt => "NBT",
        FieldCodec::NbtTag => "generic NBT",
        FieldCodec::TextComponent => "text component NBT",
    }
}

fn parse_state(value: &str) -> Result<ProtocolState, PacketSchemaError> {
    match value {
        "handshake" => Ok(ProtocolState::Handshake),
        "status" => Ok(ProtocolState::Status),
        "login" => Ok(ProtocolState::Login),
        "configuration" => Ok(ProtocolState::Configuration),
        "play" => Ok(ProtocolState::Play),
        _ => invalid(format!("unsupported protocol state {value}")),
    }
}
fn parse_direction(value: &str) -> Result<PacketDirection, PacketSchemaError> {
    match value {
        "serverbound" => Ok(PacketDirection::Serverbound),
        "clientbound" => Ok(PacketDirection::Clientbound),
        _ => invalid(format!("unsupported packet direction {value}")),
    }
}
fn invalid<T>(reason: impl Into<String>) -> Result<T, PacketSchemaError> {
    Err(PacketSchemaError::InvalidSchema(reason.into()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OfficialPacket {
    protocol_id: i64,
}

struct StrictMap<K, V>(BTreeMap<K, V>);
impl<'de, K, V> Deserialize<'de> for StrictMap<K, V>
where
    K: Deserialize<'de> + Ord + Clone + std::fmt::Display,
    V: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct Visitor<K, V>(std::marker::PhantomData<(K, V)>);
        impl<'de, K, V> serde::de::Visitor<'de> for Visitor<K, V>
        where
            K: Deserialize<'de> + Ord + Clone + std::fmt::Display,
            V: Deserialize<'de>,
        {
            type Value = StrictMap<K, V>;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an object with unique keys")
            }
            fn visit_map<A>(self, mut access: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut values = BTreeMap::new();
                while let Some((key, value)) = access.next_entry::<K, V>()? {
                    if values.insert(key.clone(), value).is_some() {
                        return Err(serde::de::Error::custom(format!("duplicate key {key}")));
                    }
                }
                Ok(StrictMap(values))
            }
        }
        deserializer.deserialize_map(Visitor(std::marker::PhantomData))
    }
}
