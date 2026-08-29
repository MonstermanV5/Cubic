use std::collections::{BTreeMap, BTreeSet};

use cubic_version::MinecraftIdentifier;
use serde::{Deserialize, de::MapAccess};

use super::{
    FieldCodec, PacketDirection, PacketField, PacketLayout, PacketSchemaArtifact,
    PacketSchemaError, ProtocolState, SupplementalPacketSchemaProvenance, UnsupportedLayoutReason,
    invalid, validate_artifact,
};

const MAX_SOURCE_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_STRING_UTF16_UNITS: usize = 32_767;
const DEFAULT_STRING_BYTES: usize = 131_068;
const DEFAULT_COLLECTION_ITEMS: usize = 65_535;
const DEFAULT_REMAINING_BYTES: usize = 2 * 1024 * 1024;
const MAX_SOURCE_STRING_BYTES: usize = 512;

#[derive(Clone, Copy, Debug)]
pub struct ProtoDefIdentityAlias {
    pub state: ProtocolState,
    pub direction: PacketDirection,
    pub source: &'static str,
    pub official: &'static str,
}

#[derive(Clone, Copy, Debug)]
pub struct ProtoDefSource<'a> {
    pub bytes: &'a [u8],
    pub source: &'a str,
    pub revision: &'a str,
    pub source_schema: &'a str,
    pub content_sha256: &'a str,
    pub license: &'a str,
    pub aliases: &'a [ProtoDefIdentityAlias],
}

pub fn merge_protodef_layouts(
    mut artifact: PacketSchemaArtifact,
    source: ProtoDefSource<'_>,
) -> Result<PacketSchemaArtifact, PacketSchemaError> {
    validate_source_metadata(&source)?;
    if source.bytes.len() > MAX_SOURCE_BYTES {
        return invalid("supplemental structural source exceeds the size limit");
    }
    let root: StrictJson = serde_json::from_slice(source.bytes).map_err(|error| {
        PacketSchemaError::MalformedJson(format!(
            "supplemental source line {}, column {}: {error}",
            error.line(),
            error.column()
        ))
    })?;
    let root = root.object("supplemental source root")?;
    let global_types = root
        .get("types")
        .ok_or_else(|| invalid_error("supplemental source is missing root types"))?
        .object("root types")?;

    let mut aliases = BTreeMap::new();
    for alias in source.aliases {
        let official = MinecraftIdentifier::new(alias.official)
            .map_err(|error| invalid_error(error.to_string()))?;
        if !artifact.packets.iter().any(|packet| {
            packet.state == alias.state
                && packet.direction == alias.direction
                && packet.identity == official
        }) {
            return invalid(format!(
                "supplemental alias {} has an official state/direction mismatch",
                alias.source
            ));
        }
        let key = (alias.state, alias.direction, alias.source);
        if aliases.insert(key, alias.official).is_some() {
            return invalid("duplicate supplemental identity alias");
        }
    }

    let mut attached = BTreeSet::new();
    for (state, source_state) in [
        (ProtocolState::Handshake, "handshaking"),
        (ProtocolState::Status, "status"),
        (ProtocolState::Login, "login"),
        (ProtocolState::Configuration, "configuration"),
        (ProtocolState::Play, "play"),
    ] {
        let Some(state_value) = root.get(source_state) else {
            continue;
        };
        let state_object = state_value.object("protocol state")?;
        for (direction, source_direction) in [
            (PacketDirection::Serverbound, "toServer"),
            (PacketDirection::Clientbound, "toClient"),
        ] {
            let Some(direction_value) = state_object.get(source_direction) else {
                continue;
            };
            let direction_object = direction_value.object("protocol direction")?;
            let local_types = direction_object
                .get("types")
                .ok_or_else(|| invalid_error("protocol direction is missing types"))?
                .object("direction types")?;
            let mappings = packet_mappings(local_types)?;
            for (id, source_identity, type_name) in mappings {
                let official_identity =
                    map_identity(&artifact, &aliases, state, direction, id, &source_identity)?;
                let Some(official_identity) = official_identity else {
                    if let Some(packet) = artifact.packets.iter_mut().find(|packet| {
                        packet.state == state && packet.direction == direction && packet.id == id
                    }) {
                        packet.layout = PacketLayout::Unsupported {
                            reason: UnsupportedLayoutReason::AmbiguousIdentityMapping {
                                source_identity: bounded_source_label(&source_identity),
                            },
                        };
                    }
                    continue;
                };
                if !attached.insert((state, direction, official_identity.clone())) {
                    return invalid("duplicate supplemental packet definition");
                }
                let layout = if type_name == "void" {
                    PacketLayout::Fields { fields: Vec::new() }
                } else {
                    let definition = local_types
                        .get(&type_name)
                        .or_else(|| global_types.get(&type_name))
                        .ok_or_else(|| {
                            invalid_error("packet mapping references an unknown type")
                        })?;
                    let mut resolver = Resolver {
                        global_types,
                        local_types,
                        active: BTreeSet::new(),
                    };
                    match resolver.packet_layout(definition) {
                        Ok(fields) => PacketLayout::Fields { fields },
                        Err(reason) => PacketLayout::Unsupported { reason },
                    }
                };
                let packet = artifact
                    .packets
                    .iter_mut()
                    .find(|packet| {
                        packet.state == state
                            && packet.direction == direction
                            && packet.identity == official_identity
                    })
                    .ok_or_else(|| invalid_error("mapped official packet disappeared"))?;
                if packet.id != id {
                    return invalid(format!(
                        "supplemental packet {source_identity} is ID {id}, but Mojang declares {}",
                        packet.id
                    ));
                }
                packet.layout = layout;
            }
        }
    }
    artifact.provenance.supplemental = Some(SupplementalPacketSchemaProvenance {
        source: source.source.to_owned(),
        revision: source.revision.to_owned(),
        source_schema: source.source_schema.to_owned(),
        content_sha256: source.content_sha256.to_owned(),
        license: source.license.to_owned(),
    });
    validate_artifact(&artifact)?;
    Ok(artifact)
}

fn validate_source_metadata(source: &ProtoDefSource<'_>) -> Result<(), PacketSchemaError> {
    for (name, value) in [
        ("source", source.source),
        ("revision", source.revision),
        ("source schema", source.source_schema),
        ("license", source.license),
    ] {
        if value.is_empty()
            || value.len() > MAX_SOURCE_STRING_BYTES
            || value.chars().any(char::is_control)
        {
            return invalid(format!("supplemental {name} is invalid"));
        }
    }
    if source.content_sha256.len() != 64
        || !source
            .content_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return invalid("supplemental content SHA-256 is invalid");
    }
    Ok(())
}

fn map_identity(
    artifact: &PacketSchemaArtifact,
    aliases: &BTreeMap<(ProtocolState, PacketDirection, &str), &str>,
    state: ProtocolState,
    direction: PacketDirection,
    source_id: u32,
    source_identity: &str,
) -> Result<Option<MinecraftIdentifier>, PacketSchemaError> {
    let exact = MinecraftIdentifier::new(format!("minecraft:{source_identity}"))
        .map_err(|error| invalid_error(error.to_string()))?;
    let mapped = if artifact.packets.iter().any(|packet| {
        packet.state == state && packet.direction == direction && packet.identity == exact
    }) {
        Some(exact)
    } else if let Some(alias) = aliases.get(&(state, direction, source_identity)) {
        Some(MinecraftIdentifier::new(*alias).map_err(|error| invalid_error(error.to_string()))?)
    } else {
        None
    };
    if let Some(identity) = &mapped {
        let packet = artifact
            .packets
            .iter()
            .find(|packet| {
                packet.state == state
                    && packet.direction == direction
                    && packet.identity == *identity
            })
            .ok_or_else(|| invalid_error("supplemental alias does not name an official packet"))?;
        if packet.id != source_id {
            return invalid(format!(
                "supplemental packet {source_identity} is ID {source_id}, but Mojang packet {identity} is ID {}",
                packet.id
            ));
        }
    }
    Ok(mapped)
}

fn packet_mappings(
    types: &BTreeMap<String, StrictJson>,
) -> Result<Vec<(u32, String, String)>, PacketSchemaError> {
    let packet = types
        .get("packet")
        .ok_or_else(|| invalid_error("direction types are missing packet dispatch"))?;
    let container = tagged_payload(packet, "container")?.array("packet dispatch container")?;
    let mut mapper = None;
    let mut switch = None;
    for field in container {
        let field = field.object("packet dispatch field")?;
        match field.get("name").and_then(StrictJson::as_str) {
            Some("name") => mapper = field.get("type"),
            Some("params") => switch = field.get("type"),
            _ => {}
        }
    }
    let mapper = tagged_payload(
        mapper.ok_or_else(|| invalid_error("packet dispatch is missing name mapper"))?,
        "mapper",
    )?
    .object("packet name mapper")?;
    if mapper.get("type").and_then(StrictJson::as_str) != Some("varint") {
        return invalid("packet ID mapper is not VarInt");
    }
    let id_mappings = mapper
        .get("mappings")
        .ok_or_else(|| invalid_error("packet mapper is missing mappings"))?
        .object("packet ID mappings")?;
    let switch = tagged_payload(
        switch.ok_or_else(|| invalid_error("packet dispatch is missing params switch"))?,
        "switch",
    )?
    .object("packet params switch")?;
    if switch.get("compareTo").and_then(StrictJson::as_str) != Some("name") {
        return invalid("packet params switch does not compare the packet name");
    }
    let fields = switch
        .get("fields")
        .ok_or_else(|| invalid_error("packet params switch is missing fields"))?
        .object("packet params mappings")?;
    let mut result = Vec::with_capacity(id_mappings.len());
    let mut names = BTreeSet::new();
    for (raw_id, identity) in id_mappings {
        let identity = identity
            .as_str()
            .ok_or_else(|| invalid_error("packet mapper identity is not a string"))?;
        if !names.insert(identity) {
            return invalid("duplicate source packet identity");
        }
        let id = parse_source_id(raw_id)?;
        let type_name = fields
            .get(identity)
            .and_then(StrictJson::as_str)
            .ok_or_else(|| invalid_error("packet mapper has no matching layout type"))?;
        if type_name == "void" {
            result.push((id, identity.to_owned(), "void".to_owned()));
        } else {
            result.push((id, identity.to_owned(), type_name.to_owned()));
        }
    }
    result.sort_by_key(|entry| entry.0);
    Ok(result)
}

fn parse_source_id(value: &str) -> Result<u32, PacketSchemaError> {
    let parsed = if let Some(hex) = value.strip_prefix("0x") {
        u32::from_str_radix(hex, 16)
    } else {
        value.parse()
    };
    parsed.map_err(|_| invalid_error("packet mapper contains an invalid numeric ID"))
}

struct Resolver<'a> {
    global_types: &'a BTreeMap<String, StrictJson>,
    local_types: &'a BTreeMap<String, StrictJson>,
    active: BTreeSet<String>,
}

impl Resolver<'_> {
    fn packet_layout(
        &mut self,
        value: &StrictJson,
    ) -> Result<Vec<PacketField>, UnsupportedLayoutReason> {
        if value.as_str() == Some("void") {
            return Ok(Vec::new());
        }
        let payload = tagged_payload_layout(value, "container")?;
        self.fields(payload.array_layout("packet container")?)
    }

    fn fields(
        &mut self,
        values: &[StrictJson],
    ) -> Result<Vec<PacketField>, UnsupportedLayoutReason> {
        let mut fields = Vec::with_capacity(values.len());
        let mut names = BTreeSet::new();
        for value in values {
            let object = value.object_layout("container field")?;
            let raw_name = object
                .get("name")
                .and_then(StrictJson::as_str)
                .ok_or_else(|| unsupported("field without a string name"))?;
            let name = snake_case(raw_name)?;
            if !names.insert(name.clone()) {
                return Err(unsupported("duplicate normalized field name"));
            }
            let codec = self.codec(
                object
                    .get("type")
                    .ok_or_else(|| unsupported("field without a type"))?,
            )?;
            fields.push(PacketField { name, codec });
        }
        Ok(fields)
    }

    fn codec(&mut self, value: &StrictJson) -> Result<FieldCodec, UnsupportedLayoutReason> {
        if let Some(name) = value.as_str() {
            return self.named_codec(name);
        }
        let tagged = value.array_layout("tagged codec")?;
        if tagged.len() != 2 {
            return Err(unsupported("malformed tagged codec"));
        }
        let tag = tagged[0]
            .as_str()
            .ok_or_else(|| unsupported("non-string codec tag"))?;
        let payload = &tagged[1];
        match tag {
            "container" => Ok(FieldCodec::Struct {
                fields: self.fields(payload.array_layout("nested container")?)?,
            }),
            "option" => Ok(FieldCodec::Optional {
                value: Box::new(self.codec(payload)?),
            }),
            "array" => self.array_codec(payload),
            "buffer" => self.buffer_codec(payload),
            "mapper" => self.mapper_codec(payload),
            "switch" => self.switch_codec(payload),
            other => Err(unsupported(other)),
        }
    }

    fn named_codec(&mut self, name: &str) -> Result<FieldCodec, UnsupportedLayoutReason> {
        let primitive = match name {
            "bool" => Some(FieldCodec::Bool),
            "i8" => Some(FieldCodec::I8),
            "u8" => Some(FieldCodec::U8),
            "i16" => Some(FieldCodec::I16),
            "u16" => Some(FieldCodec::U16),
            "i32" => Some(FieldCodec::I32),
            "u32" => Some(FieldCodec::U32),
            "i64" => Some(FieldCodec::I64),
            "u64" => Some(FieldCodec::U64),
            "f32" => Some(FieldCodec::F32),
            "f64" => Some(FieldCodec::F64),
            "varint" | "optvarint" => Some(FieldCodec::VarInt),
            "varlong" => Some(FieldCodec::VarLong),
            "string" | "pstring" => Some(FieldCodec::String {
                max_utf16_units: DEFAULT_STRING_UTF16_UNITS,
                max_bytes: DEFAULT_STRING_BYTES,
            }),
            "UUID" => Some(FieldCodec::Uuid),
            "position" => Some(FieldCodec::Position),
            "anonymousNbt" => Some(FieldCodec::NbtTag),
            "restBuffer" => Some(FieldCodec::RemainingBytes {
                max_bytes: DEFAULT_REMAINING_BYTES,
            }),
            "void" => return Err(unsupported("void field")),
            _ => None,
        };
        if let Some(codec) = primitive {
            return Ok(codec);
        }
        if !self.active.insert(name.to_owned()) {
            return Err(unsupported("recursive named codec"));
        }
        let definition = self
            .local_types
            .get(name)
            .or_else(|| self.global_types.get(name))
            .ok_or_else(|| unsupported(name))?
            .clone();
        let result = self.codec(&definition);
        self.active.remove(name);
        result
    }

    fn array_codec(&mut self, payload: &StrictJson) -> Result<FieldCodec, UnsupportedLayoutReason> {
        let options = payload.object_layout("array options")?;
        if options.get("countType").and_then(StrictJson::as_str) != Some("varint") {
            return Err(unsupported("non-VarInt array length"));
        }
        let element = options
            .get("type")
            .ok_or_else(|| unsupported("array without element type"))?;
        Ok(FieldCodec::List {
            max_items: DEFAULT_COLLECTION_ITEMS,
            element: Box::new(self.codec(element)?),
        })
    }

    fn buffer_codec(
        &mut self,
        payload: &StrictJson,
    ) -> Result<FieldCodec, UnsupportedLayoutReason> {
        let options = payload.object_layout("buffer options")?;
        if options.get("countType").and_then(StrictJson::as_str) == Some("varint") {
            return Ok(FieldCodec::ByteArray {
                max_bytes: DEFAULT_REMAINING_BYTES,
            });
        }
        if let Some(length) = options.get("count").and_then(StrictJson::as_u64) {
            return Ok(FieldCodec::FixedBytes {
                length: usize::try_from(length).map_err(|_| unsupported("fixed buffer length"))?,
            });
        }
        Err(unsupported("buffer length"))
    }

    fn mapper_codec(
        &mut self,
        payload: &StrictJson,
    ) -> Result<FieldCodec, UnsupportedLayoutReason> {
        let options = payload.object_layout("mapper options")?;
        if options.get("type").and_then(StrictJson::as_str) != Some("varint") {
            return Err(unsupported("non-VarInt mapper"));
        }
        let mappings = options
            .get("mappings")
            .ok_or_else(|| unsupported("mapper without mappings"))?
            .object_layout("mapper mappings")?;
        let mut values = BTreeMap::new();
        for (raw, label) in mappings {
            let discriminant = parse_i32(raw).map_err(|_| unsupported("mapper discriminant"))?;
            let label = snake_case(
                label
                    .as_str()
                    .ok_or_else(|| unsupported("non-string mapper label"))?,
            )?;
            if values.insert(discriminant, label).is_some() {
                return Err(unsupported("duplicate mapper discriminant"));
            }
        }
        if values.is_empty() {
            return Err(unsupported("empty mapper"));
        }
        Ok(FieldCodec::Enum { values })
    }

    fn switch_codec(
        &mut self,
        payload: &StrictJson,
    ) -> Result<FieldCodec, UnsupportedLayoutReason> {
        let options = payload.object_layout("switch options")?;
        if options.contains_key("default") {
            return Err(UnsupportedLayoutReason::UnsupportedConditionalConstruct {
                construct: bounded_source_label("switch with default"),
            });
        }
        let compared = options
            .get("compareTo")
            .and_then(StrictJson::as_str)
            .ok_or_else(
                || UnsupportedLayoutReason::UnsupportedConditionalConstruct {
                    construct: bounded_source_label("switch without compareTo"),
                },
            )?;
        let compared = compared.rsplit('/').next().unwrap_or(compared);
        let field = snake_case(compared)?;
        let cases = options
            .get("fields")
            .ok_or_else(
                || UnsupportedLayoutReason::UnsupportedConditionalConstruct {
                    construct: bounded_source_label("switch without fields"),
                },
            )?
            .object_layout("switch fields")?;
        let true_case = cases.get("true");
        let false_case = cases.get("false");
        let (equals, value) = match (true_case, false_case) {
            (Some(value), None) => (true, value),
            (Some(value), Some(StrictJson::String(name))) if name == "void" => (true, value),
            (None, Some(value)) => (false, value),
            (Some(StrictJson::String(name)), Some(value)) if name == "void" => (false, value),
            _ => {
                return Err(UnsupportedLayoutReason::UnsupportedConditionalConstruct {
                    construct: bounded_source_label("non-Boolean or two-valued switch"),
                });
            }
        };
        Ok(FieldCodec::Conditional {
            field,
            equals,
            value: Box::new(self.codec(value)?),
        })
    }
}

fn tagged_payload<'a>(
    value: &'a StrictJson,
    expected: &str,
) -> Result<&'a StrictJson, PacketSchemaError> {
    let tagged = value.array("tagged source codec")?;
    if tagged.len() != 2 || tagged[0].as_str() != Some(expected) {
        return invalid(format!("expected supplemental {expected} codec"));
    }
    Ok(&tagged[1])
}

fn tagged_payload_layout<'a>(
    value: &'a StrictJson,
    expected: &str,
) -> Result<&'a StrictJson, UnsupportedLayoutReason> {
    let tagged = value.array_layout("tagged source codec")?;
    if tagged.len() != 2 || tagged[0].as_str() != Some(expected) {
        return Err(unsupported(expected));
    }
    Ok(&tagged[1])
}

fn parse_i32(value: &str) -> Result<i32, ()> {
    if let Some(hex) = value.strip_prefix("0x") {
        i32::from_str_radix(hex, 16).map_err(|_| ())
    } else {
        value.parse().map_err(|_| ())
    }
}

fn snake_case(value: &str) -> Result<String, UnsupportedLayoutReason> {
    if value.is_empty() || value.len() > 128 || !value.is_ascii() {
        return Err(unsupported("field or enum name"));
    }
    let mut output = String::with_capacity(value.len() + 8);
    let bytes = value.as_bytes();
    for (index, byte) in bytes.iter().copied().enumerate() {
        if byte.is_ascii_uppercase() {
            let prior_is_lower_or_digit = index > 0
                && (bytes[index - 1].is_ascii_lowercase() || bytes[index - 1].is_ascii_digit());
            let acronym_boundary = index > 0
                && bytes[index - 1].is_ascii_uppercase()
                && bytes.get(index + 1).is_some_and(u8::is_ascii_lowercase);
            if (prior_is_lower_or_digit || acronym_boundary) && !output.ends_with('_') {
                output.push('_');
            }
            output.push(char::from(byte.to_ascii_lowercase()));
        } else if byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' {
            output.push(char::from(byte));
        } else {
            return Err(unsupported("field or enum name"));
        }
    }
    Ok(output)
}

fn bounded_source_label(value: &str) -> String {
    value.chars().take(128).collect()
}

fn unsupported(value: &str) -> UnsupportedLayoutReason {
    UnsupportedLayoutReason::UnsupportedCodecConstruct {
        construct: bounded_source_label(value),
    }
}

fn invalid_error(reason: impl Into<String>) -> PacketSchemaError {
    PacketSchemaError::InvalidSchema(reason.into())
}

#[derive(Clone, Debug)]
enum StrictJson {
    Null,
    Bool,
    Number(serde_json::Number),
    String(String),
    Array(Vec<Self>),
    Object(BTreeMap<String, Self>),
}

impl StrictJson {
    fn as_str(&self) -> Option<&str> {
        if let Self::String(value) = self {
            Some(value)
        } else {
            None
        }
    }
    fn as_u64(&self) -> Option<u64> {
        if let Self::Number(value) = self {
            value.as_u64()
        } else {
            None
        }
    }
    fn object(&self, context: &str) -> Result<&BTreeMap<String, Self>, PacketSchemaError> {
        if let Self::Object(value) = self {
            Ok(value)
        } else {
            invalid(format!("{context} is not an object"))
        }
    }
    fn array(&self, context: &str) -> Result<&[Self], PacketSchemaError> {
        if let Self::Array(value) = self {
            Ok(value)
        } else {
            invalid(format!("{context} is not an array"))
        }
    }
    fn object_layout(
        &self,
        context: &str,
    ) -> Result<&BTreeMap<String, Self>, UnsupportedLayoutReason> {
        if let Self::Object(value) = self {
            Ok(value)
        } else {
            Err(unsupported(context))
        }
    }
    fn array_layout(&self, context: &str) -> Result<&[Self], UnsupportedLayoutReason> {
        if let Self::Array(value) = self {
            Ok(value)
        } else {
            Err(unsupported(context))
        }
    }
}

impl<'de> Deserialize<'de> for StrictJson {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = StrictJson;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a JSON value with unique object keys")
            }
            fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
                Ok(StrictJson::Bool)
            }
            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(StrictJson::Number(value.into()))
            }
            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(StrictJson::Number(value.into()))
            }
            fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                serde_json::Number::from_f64(value)
                    .map(StrictJson::Number)
                    .ok_or_else(|| E::custom("non-finite JSON number"))
            }
            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                self.visit_string(value.to_owned())
            }
            fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                if value.len() > MAX_SOURCE_BYTES {
                    return Err(E::custom("source string exceeds limit"));
                }
                Ok(StrictJson::String(value))
            }
            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(StrictJson::Null)
            }
            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(StrictJson::Null)
            }
            fn visit_seq<A>(self, mut access: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                let mut values = Vec::new();
                while let Some(value) = access.next_element()? {
                    if values.len() >= super::MAX_PACKETS * super::MAX_FIELDS_PER_PACKET {
                        return Err(serde::de::Error::custom("source array exceeds item limit"));
                    }
                    values.push(value);
                }
                Ok(StrictJson::Array(values))
            }
            fn visit_map<A>(self, mut access: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut values = BTreeMap::new();
                while let Some((key, value)) = access.next_entry::<String, StrictJson>()? {
                    if key.len() > MAX_SOURCE_STRING_BYTES {
                        return Err(serde::de::Error::custom("source key exceeds limit"));
                    }
                    if values.insert(key.clone(), value).is_some() {
                        return Err(serde::de::Error::custom(format!("duplicate key {key}")));
                    }
                }
                Ok(StrictJson::Object(values))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}
