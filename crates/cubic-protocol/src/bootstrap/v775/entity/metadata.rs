//! The 43 serializers registered by 26.1.2 `EntityDataSerializers`, in
//! registration order. This version-specific parser does not skip a valid
//! serializer: metadata values have no individual length prefix.
use std::collections::BTreeMap;

use crate::{BlockPosition, CodecError, CodecReader, StringLimits};

use super::{BootstrapProtocolError, EntityTypeProfile, nonnegative};
use crate::bootstrap::v775::{ItemStackProfile, TextComponent, WireItemStack};

const TEXT_LIMITS: StringLimits = StringLimits::new(32_767, 32_767);
const IDENTIFIER_LIMITS: StringLimits = StringLimits::new(256, 1_024);
const MAX_PARTICLES: usize = 256;

#[derive(Clone, Debug, PartialEq)]
pub struct EntityWireParticle {
    pub kind: String,
    pub data: EntityWireParticleData,
}

#[derive(Clone, Debug, PartialEq)]
pub enum EntityWireParticleData {
    None,
    BlockState {
        id: u32,
        block: String,
        properties: BTreeMap<String, String>,
    },
    Float(f32),
    Int(i32),
    ColorAndFloat {
        color: i32,
        amount: f32,
    },
    Dust {
        color: i32,
        scale: f32,
    },
    DustTransition {
        from: i32,
        to: i32,
        scale: f32,
    },
    Item(WireItemStack),
    VibrationBlock {
        position: BlockPosition,
        arrival_ticks: i32,
    },
    VibrationEntity {
        entity_id: i32,
        y_offset: f32,
        arrival_ticks: i32,
    },
    Trail {
        target: [f64; 3],
        color: i32,
        duration: i32,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct EntityWireProfile {
    pub name: Option<String>,
    pub uuid: Option<[u8; 16]>,
    pub properties: Vec<(String, String, Option<String>)>,
    pub skin: [Option<String>; 3],
    pub slim: Option<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum EntityWireMetadataValue {
    Byte(i8),
    Integer(i32),
    Long(i64),
    Float(f32),
    String(String),
    Component(TextComponent),
    OptionalComponent(Option<TextComponent>),
    ItemStack(Option<WireItemStack>),
    Boolean(bool),
    Rotations([f32; 3]),
    BlockPos(BlockPosition),
    OptionalBlockPos(Option<BlockPosition>),
    Direction(u8),
    LivingEntityReference(Option<[u8; 16]>),
    BlockState {
        id: u32,
        block: String,
        properties: BTreeMap<String, String>,
    },
    OptionalBlockState(Option<(u32, String, BTreeMap<String, String>)>),
    Particle(EntityWireParticle),
    Particles(Vec<EntityWireParticle>),
    VillagerData {
        kind: String,
        profession: String,
        level: i32,
    },
    OptionalUnsignedInt(Option<u32>),
    Pose {
        ordinal: u8,
        name: &'static str,
    },
    RegistryHolder {
        serializer_id: u8,
        registry: &'static str,
        identifier: String,
    },
    GlobalPos(Option<(String, BlockPosition)>),
    DirectPainting {
        width: i32,
        height: i32,
        asset: String,
        title: Option<TextComponent>,
        author: Option<TextComponent>,
    },
    EnumState {
        serializer_id: u8,
        ordinal: u8,
        name: &'static str,
    },
    Vector3([f32; 3]),
    Quaternion([f32; 4]),
    ResolvableProfile(EntityWireProfile),
}

impl EntityWireMetadataValue {
    #[must_use]
    pub const fn serializer_id(&self) -> u8 {
        match self {
            Self::Byte(_) => 0,
            Self::Integer(_) => 1,
            Self::Long(_) => 2,
            Self::Float(_) => 3,
            Self::String(_) => 4,
            Self::Component(_) => 5,
            Self::OptionalComponent(_) => 6,
            Self::ItemStack(_) => 7,
            Self::Boolean(_) => 8,
            Self::Rotations(_) => 9,
            Self::BlockPos(_) => 10,
            Self::OptionalBlockPos(_) => 11,
            Self::Direction(_) => 12,
            Self::LivingEntityReference(_) => 13,
            Self::BlockState { .. } => 14,
            Self::OptionalBlockState(_) => 15,
            Self::Particle(_) => 16,
            Self::Particles(_) => 17,
            Self::VillagerData { .. } => 18,
            Self::OptionalUnsignedInt(_) => 19,
            Self::Pose { .. } => 20,
            Self::RegistryHolder { serializer_id, .. } | Self::EnumState { serializer_id, .. } => {
                *serializer_id
            }
            Self::GlobalPos(_) => 33,
            Self::DirectPainting { .. } => 34,
            Self::Vector3(_) => 39,
            Self::Quaternion(_) => 40,
            Self::ResolvableProfile(_) => 41,
        }
    }
}

pub(super) fn decode(
    reader: &mut CodecReader<'_>,
    serializer: i32,
    profile: &EntityTypeProfile,
    items: Option<&ItemStackProfile>,
) -> Result<EntityWireMetadataValue, BootstrapProtocolError> {
    use EntityWireMetadataValue as Value;
    Ok(match serializer {
        0 => Value::Byte(reader.read_i8()?),
        1 => Value::Integer(reader.read_var_int()?),
        2 => Value::Long(reader.read_var_long()?),
        3 => Value::Float(finite(reader.read_f32()?, "entity metadata float")?),
        4 => Value::String(reader.read_string(TEXT_LIMITS)?.to_owned()),
        5 => Value::Component(super::super::decode_text_component(reader)?),
        6 => Value::OptionalComponent(optional(reader, super::super::decode_text_component)?),
        7 => Value::ItemStack(
            super::super::inventory::decode_item_stack(
                reader,
                items.ok_or(BootstrapProtocolError::EntityRegistryUnavailable {
                    registry: "minecraft:item",
                })?,
            )
            .map_err(|error| BootstrapProtocolError::Inventory(Box::new(error)))?,
        ),
        8 => Value::Boolean(reader.read_bool()?),
        9 => Value::Rotations(read_vec3(reader)?),
        10 => Value::BlockPos(reader.read_block_position()?),
        11 => Value::OptionalBlockPos(optional(reader, |r| Ok(r.read_block_position()?))?),
        12 => Value::Direction(read_ordinal(reader, 6, "entity direction")?),
        // EntityReference.STREAM_CODEC is UUIDUtil.STREAM_CODEC, not an entity ID.
        13 => Value::LivingEntityReference(optional(reader, |r| Ok(r.read_uuid()?.to_bytes()))?),
        14 => {
            let (id, block, properties) = block_state(reader, profile)?;
            Value::BlockState {
                id,
                block,
                properties,
            }
        }
        15 => {
            let encoded = nonnegative(reader.read_var_int()?, "optional block state")?;
            Value::OptionalBlockState(if encoded == 0 {
                None
            } else {
                // The optional-state codec uses zero as absence and passes
                // every nonzero number directly to Block.stateById.
                let id = encoded;
                let (block, properties) =
                    profile.block_states.get(&id).cloned().ok_or_else(|| {
                        invalid(
                            "optional block state ID",
                            i128::from(id),
                            0,
                            i128::from(u32::MAX),
                        )
                    })?;
                Some((id, block, properties))
            })
        }
        16 => Value::Particle(particle(reader, profile, items)?),
        17 => {
            let count = bounded_count(reader, MAX_PARTICLES, "entity particle list")?;
            let mut values = Vec::with_capacity(count);
            for _ in 0..count {
                values.push(particle(reader, profile, items)?);
            }
            Value::Particles(values)
        }
        18 => Value::VillagerData {
            kind: registry_raw(reader, profile, "minecraft:villager_type")?,
            profession: registry_raw(reader, profile, "minecraft:villager_profession")?,
            level: reader.read_var_int()?,
        },
        19 => {
            let encoded = nonnegative(reader.read_var_int()?, "optional unsigned integer")?;
            Value::OptionalUnsignedInt(encoded.checked_sub(1))
        }
        20 => {
            const NAMES: [&str; 18] = [
                "standing",
                "fall_flying",
                "sleeping",
                "swimming",
                "spin_attack",
                "crouching",
                "long_jumping",
                "dying",
                "croaking",
                "using_tongue",
                "sitting",
                "roaring",
                "sniffing",
                "emerging",
                "digging",
                "sliding",
                "shooting",
                "inhaling",
            ];
            let ordinal = read_ordinal(reader, NAMES.len() as u8, "entity pose")?;
            Value::Pose {
                ordinal,
                name: NAMES[ordinal as usize],
            }
        }
        21..=32 => {
            let registry = variant_registry(serializer)?;
            Value::RegistryHolder {
                serializer_id: serializer as u8,
                registry,
                identifier: registry_raw(reader, profile, registry)?,
            }
        }
        33 => Value::GlobalPos(optional(reader, |r| {
            let dimension = identifier(r)?;
            let position = r.read_block_position()?;
            Ok((dimension, position))
        })?),
        34 => {
            // PaintingVariant uses ByteBufCodecs.holder (direct value 0,
            // reference = registry raw ID + 1), unlike holderRegistry.
            let encoded = nonnegative(reader.read_var_int()?, "painting variant holder")?;
            if encoded == 0 {
                let width = reader.read_var_int()?;
                let height = reader.read_var_int()?;
                if width <= 0 || height <= 0 || width > 64 || height > 64 {
                    return Err(invalid(
                        "direct painting dimensions",
                        i128::from(width.max(height)),
                        1,
                        64,
                    ));
                }
                Value::DirectPainting {
                    width,
                    height,
                    asset: identifier(reader)?,
                    title: optional(reader, super::super::decode_text_component)?,
                    author: optional(reader, super::super::decode_text_component)?,
                }
            } else {
                Value::RegistryHolder {
                    serializer_id: 34,
                    registry: "minecraft:painting_variant",
                    identifier: profile
                        .registry_entry("minecraft:painting_variant", encoded - 1)?,
                }
            }
        }
        35 => enum_state(
            reader,
            35,
            &[
                "idling",
                "feeling_happy",
                "scenting",
                "sniffing",
                "searching",
                "digging",
                "rising",
            ],
        )?,
        36 => enum_state(reader, 36, &["idle", "rolling", "scared", "unrolling"])?,
        37 => enum_state(
            reader,
            37,
            &[
                "idle",
                "getting_item",
                "getting_no_item",
                "dropping_item",
                "dropping_no_item",
            ],
        )?,
        38 => enum_state(
            reader,
            38,
            &["unaffected", "exposed", "weathered", "oxidized"],
        )?,
        39 => Value::Vector3(read_vec3(reader)?),
        40 => Value::Quaternion([
            finite(reader.read_f32()?, "quaternion x")?,
            finite(reader.read_f32()?, "quaternion y")?,
            finite(reader.read_f32()?, "quaternion z")?,
            finite(reader.read_f32()?, "quaternion w")?,
        ]),
        41 => Value::ResolvableProfile(resolvable_profile(reader)?),
        42 => enum_state(reader, 42, &["left", "right"])?,
        _ => {
            return Err(invalid(
                "entity metadata serializer ID",
                i128::from(serializer),
                0,
                42,
            ));
        }
    })
}

fn invalid(context: &'static str, value: i128, min: i128, max: i128) -> BootstrapProtocolError {
    CodecError::ValueOutOfRange {
        context,
        value,
        min,
        max,
    }
    .into()
}

fn finite(value: f32, context: &'static str) -> Result<f32, BootstrapProtocolError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(BootstrapProtocolError::NonFinite { context })
    }
}

fn read_vec3(reader: &mut CodecReader<'_>) -> Result<[f32; 3], BootstrapProtocolError> {
    Ok([
        finite(reader.read_f32()?, "metadata vector x")?,
        finite(reader.read_f32()?, "metadata vector y")?,
        finite(reader.read_f32()?, "metadata vector z")?,
    ])
}

fn optional<T>(
    reader: &mut CodecReader<'_>,
    read: impl FnOnce(&mut CodecReader<'_>) -> Result<T, BootstrapProtocolError>,
) -> Result<Option<T>, BootstrapProtocolError> {
    if reader.read_bool()? {
        read(reader).map(Some)
    } else {
        Ok(None)
    }
}

fn bounded_count(
    reader: &mut CodecReader<'_>,
    max: usize,
    context: &'static str,
) -> Result<usize, BootstrapProtocolError> {
    let count = nonnegative(reader.read_var_int()?, context)? as usize;
    if count > max {
        Err(invalid(context, count as i128, 0, max as i128))
    } else {
        Ok(count)
    }
}

fn read_ordinal(
    reader: &mut CodecReader<'_>,
    count: u8,
    context: &'static str,
) -> Result<u8, BootstrapProtocolError> {
    let value = reader.read_var_int()?;
    if value < 0 || value >= i32::from(count) {
        Err(invalid(
            context,
            i128::from(value),
            0,
            i128::from(count - 1),
        ))
    } else {
        Ok(value as u8)
    }
}

fn identifier(reader: &mut CodecReader<'_>) -> Result<String, BootstrapProtocolError> {
    let value = reader.read_string(IDENTIFIER_LIMITS)?.to_owned();
    cubic_version::MinecraftIdentifier::new(value.clone())
        .map_err(|_| invalid("metadata identifier", 0, 1, 1))?;
    Ok(value)
}

fn block_state(
    reader: &mut CodecReader<'_>,
    profile: &EntityTypeProfile,
) -> Result<(u32, String, BTreeMap<String, String>), BootstrapProtocolError> {
    let id = nonnegative(reader.read_var_int()?, "block state ID")?;
    let (block, properties) = profile
        .block_states
        .get(&id)
        .cloned()
        .ok_or_else(|| invalid("block state ID", i128::from(id), 0, i128::from(u32::MAX)))?;
    Ok((id, block, properties))
}

fn registry_raw(
    reader: &mut CodecReader<'_>,
    profile: &EntityTypeProfile,
    registry: &'static str,
) -> Result<String, BootstrapProtocolError> {
    let raw = nonnegative(reader.read_var_int()?, "metadata registry raw ID")?;
    profile.registry_entry(registry, raw)
}

fn variant_registry(serializer: i32) -> Result<&'static str, BootstrapProtocolError> {
    Ok(match serializer {
        21 => "minecraft:cat_variant",
        22 => "minecraft:cat_sound_variant",
        23 => "minecraft:cow_variant",
        24 => "minecraft:cow_sound_variant",
        25 => "minecraft:wolf_variant",
        26 => "minecraft:wolf_sound_variant",
        27 => "minecraft:frog_variant",
        28 => "minecraft:pig_variant",
        29 => "minecraft:pig_sound_variant",
        30 => "minecraft:chicken_variant",
        31 => "minecraft:chicken_sound_variant",
        32 => "minecraft:zombie_nautilus_variant",
        _ => {
            return Err(invalid(
                "variant serializer",
                i128::from(serializer),
                21,
                32,
            ));
        }
    })
}

fn enum_state(
    reader: &mut CodecReader<'_>,
    serializer_id: u8,
    names: &'static [&'static str],
) -> Result<EntityWireMetadataValue, BootstrapProtocolError> {
    let ordinal = read_ordinal(reader, names.len() as u8, "entity metadata state")?;
    Ok(EntityWireMetadataValue::EnumState {
        serializer_id,
        ordinal,
        name: names[ordinal as usize],
    })
}

fn particle(
    reader: &mut CodecReader<'_>,
    profile: &EntityTypeProfile,
    items: Option<&ItemStackProfile>,
) -> Result<EntityWireParticle, BootstrapProtocolError> {
    let raw = nonnegative(reader.read_var_int()?, "particle type ID")?;
    let kind = profile
        .particles
        .get(&raw)
        .cloned()
        .ok_or_else(|| invalid("particle type ID", i128::from(raw), 0, i128::from(u32::MAX)))?;
    use EntityWireParticleData as Data;
    let data = match kind.as_str() {
        "minecraft:block"
        | "minecraft:block_marker"
        | "minecraft:falling_dust"
        | "minecraft:dust_pillar"
        | "minecraft:block_crumble" => {
            let (id, block, properties) = block_state(reader, profile)?;
            Data::BlockState {
                id,
                block,
                properties,
            }
        }
        "minecraft:dragon_breath" | "minecraft:sculk_charge" => {
            Data::Float(finite(reader.read_f32()?, "particle power")?)
        }
        "minecraft:dust" => Data::Dust {
            color: reader.read_i32()?,
            scale: finite(reader.read_f32()?, "dust scale")?,
        },
        "minecraft:dust_color_transition" => Data::DustTransition {
            from: reader.read_i32()?,
            to: reader.read_i32()?,
            scale: finite(reader.read_f32()?, "dust transition scale")?,
        },
        "minecraft:effect" | "minecraft:instant_effect" => Data::ColorAndFloat {
            color: reader.read_i32()?,
            amount: finite(reader.read_f32()?, "spell particle amount")?,
        },
        "minecraft:entity_effect" | "minecraft:tinted_leaves" | "minecraft:flash" => {
            Data::Int(reader.read_i32()?)
        }
        "minecraft:item" => Data::Item(
            super::super::inventory::decode_item_stack_template(
                reader,
                items.ok_or(BootstrapProtocolError::EntityRegistryUnavailable {
                    registry: "minecraft:item",
                })?,
            )
            .map_err(|error| BootstrapProtocolError::Inventory(Box::new(error)))?,
        ),
        "minecraft:shriek" => Data::Int(reader.read_var_int()?),
        "minecraft:trail" => {
            let target = [reader.read_f64()?, reader.read_f64()?, reader.read_f64()?];
            if target.iter().any(|coordinate| !coordinate.is_finite()) {
                return Err(BootstrapProtocolError::NonFinite {
                    context: "trail target",
                });
            }
            Data::Trail {
                target,
                color: reader.read_i32()?,
                duration: reader.read_var_int()?,
            }
        }
        "minecraft:vibration" => {
            // PositionSource.STREAM_CODEC dispatches the built-in
            // position_source_type registry: block=0, entity=1.
            let source = reader.read_var_int()?;
            match source {
                0 => {
                    let position = reader.read_block_position()?;
                    Data::VibrationBlock {
                        position,
                        arrival_ticks: reader.read_var_int()?,
                    }
                }
                1 => {
                    let entity_id = reader.read_var_int()?;
                    let y_offset = finite(reader.read_f32()?, "vibration y offset")?;
                    Data::VibrationEntity {
                        entity_id,
                        y_offset,
                        arrival_ticks: reader.read_var_int()?,
                    }
                }
                _ => {
                    return Err(invalid(
                        "vibration position source",
                        i128::from(source),
                        0,
                        1,
                    ));
                }
            }
        }
        // Every other registered ParticleType is SimpleParticleType in the
        // official 26.1.2 registration. Its stream codec has no option bytes.
        _ => Data::None,
    };
    Ok(EntityWireParticle { kind, data })
}

fn resolvable_profile(
    reader: &mut CodecReader<'_>,
) -> Result<EntityWireProfile, BootstrapProtocolError> {
    // ByteBufCodecs.either(GAME_PROFILE, Partial): true selects the complete
    // GameProfile; false selects the partial three-field codec.
    let complete = reader.read_bool()?;
    let (name, uuid) = if complete {
        let uuid = reader.read_uuid()?.to_bytes();
        (Some(read_player_name(reader)?), Some(uuid))
    } else {
        (
            optional(reader, read_player_name)?,
            optional(reader, |r| Ok(r.read_uuid()?.to_bytes()))?,
        )
    };
    let count = bounded_count(reader, 16, "profile property count")?;
    let mut properties = Vec::with_capacity(count);
    for _ in 0..count {
        let key = reader.read_string(StringLimits::new(64, 192))?.to_owned();
        let value = reader.read_string(TEXT_LIMITS)?.to_owned();
        let signature = optional(reader, |r| {
            Ok(r.read_string(StringLimits::new(1_024, 3_072))?.to_owned())
        })?;
        properties.push((key, value, signature));
    }
    // PlayerSkin.Patch: optional body/cape/elytra ResourceTexture asset IDs
    // followed by optional PlayerModelType (BOOL: slim=true, wide=false).
    let mut skin = [None, None, None];
    for texture in &mut skin {
        *texture = optional(reader, identifier)?;
    }
    let slim = optional(reader, |r| Ok(r.read_bool()?))?;
    Ok(EntityWireProfile {
        name,
        uuid,
        properties,
        skin,
        slim,
    })
}

fn read_player_name(reader: &mut CodecReader<'_>) -> Result<String, BootstrapProtocolError> {
    Ok(reader.read_string(StringLimits::new(16, 48))?.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootstrap::v775::{EntityWireEvent, decode_entity_packet};
    use cubic_version::MinecraftIdentifier;

    fn profile() -> EntityTypeProfile {
        let mut profile = EntityTypeProfile {
            particles: BTreeMap::from([
                (0, "minecraft:poof".to_owned()),
                (1, "minecraft:dust".to_owned()),
                (2, "minecraft:block".to_owned()),
                (3, "minecraft:vibration".to_owned()),
            ]),
            block_states: BTreeMap::from([
                (0, ("minecraft:air".to_owned(), BTreeMap::new())),
                (1, ("minecraft:stone".to_owned(), BTreeMap::new())),
            ]),
            static_registries: BTreeMap::from([
                (
                    "minecraft:villager_type".to_owned(),
                    BTreeMap::from([(0, "minecraft:plains".to_owned())]),
                ),
                (
                    "minecraft:villager_profession".to_owned(),
                    BTreeMap::from([(0, "minecraft:none".to_owned())]),
                ),
            ]),
            ..Default::default()
        };
        let registries = (21..=32)
            .chain(std::iter::once(34))
            .map(|serializer| {
                (
                    variant_registry(serializer)
                        .unwrap_or("minecraft:painting_variant")
                        .to_owned(),
                    vec![MinecraftIdentifier::new("minecraft:example").unwrap()],
                )
            })
            .collect();
        profile.install_connection_registries(registries);
        profile
    }

    fn item_profile() -> ItemStackProfile {
        ItemStackProfile::synthetic([(1, "minecraft:stone")], [(0, "minecraft:custom_name")], [])
    }

    fn identifier_bytes(value: &str) -> Vec<u8> {
        let mut bytes = vec![u8::try_from(value.len()).unwrap()];
        bytes.extend(value.as_bytes());
        bytes
    }

    fn fixtures() -> Vec<(u8, Vec<u8>)> {
        let mut cases = Vec::with_capacity(43);
        cases.push((0, vec![0x7f]));
        cases.push((1, vec![0xac, 2]));
        cases.push((2, vec![0]));
        cases.push((3, 1.25_f32.to_be_bytes().to_vec()));
        cases.push((4, vec![1, b'x']));
        // Unnamed network TAG_String("x") used by the trusted Component codec.
        cases.push((5, vec![8, 0, 1, b'x']));
        cases.push((6, vec![0]));
        // Nontrivial stack: count 1, stone raw ID 1, one custom_name
        // component containing TAG_String("x"), no removed components.
        cases.push((7, vec![1, 1, 1, 0, 0, 8, 0, 1, b'x']));
        cases.push((8, vec![1]));
        cases.push((9, vec![0; 12]));
        cases.push((10, vec![0; 8]));
        cases.push((11, vec![0]));
        cases.push((12, vec![0]));
        cases.push((13, vec![0]));
        cases.push((14, vec![1]));
        cases.push((15, vec![0]));
        cases.push((16, vec![0]));
        cases.push((17, vec![2, 0, 0]));
        cases.push((18, vec![0, 0, 1]));
        cases.push((19, vec![0]));
        cases.push((20, vec![0]));
        for serializer in 21..=32 {
            cases.push((serializer, vec![0]));
        }
        cases.push((33, vec![0]));
        cases.push((34, vec![1]));
        for serializer in 35..=38 {
            cases.push((serializer, vec![0]));
        }
        cases.push((39, vec![0; 12]));
        cases.push((40, vec![0; 16]));
        // Partial profile, no name/UUID/properties/skin overrides.
        cases.push((41, vec![0; 8]));
        cases.push((42, vec![0]));
        cases
    }

    #[test]
    fn all_43_registered_serializers_consume_exact_payload_boundaries() {
        let profile = profile();
        let items = item_profile();
        let fixtures = fixtures();
        assert_eq!(fixtures.len(), 43);
        let mut packet = vec![0x63, 5];
        for (accessor, payload) in &fixtures {
            packet.extend([*accessor, *accessor]);
            packet.extend(payload);
        }
        packet.push(0xff);
        let Some(EntityWireEvent::Metadata { id, values }) =
            decode_entity_packet(&packet, &profile, Some(&items)).unwrap()
        else {
            panic!("expected metadata");
        };
        assert_eq!(id, 5);
        assert_eq!(values.len(), 43);
        for (index, value) in values {
            assert_eq!(index, value.serializer_id());
        }
        packet.push(0);
        assert!(decode_entity_packet(&packet, &profile, Some(&items)).is_err());
    }

    #[test]
    fn boolean_is_eight_and_block_position_is_ten() {
        let profile = profile();
        let packet = [0x63, 5, 0, 8, 1, 1, 10, 0, 0, 0, 0, 0, 0, 0, 0, 0xff];
        let Some(EntityWireEvent::Metadata { values, .. }) =
            decode_entity_packet(&packet, &profile, None).unwrap()
        else {
            panic!("expected metadata");
        };
        assert_eq!(values[0], (0, EntityWireMetadataValue::Boolean(true)));
        assert_eq!(
            values[1],
            (
                1,
                EntityWireMetadataValue::BlockPos(BlockPosition::new(0, 0, 0).unwrap())
            )
        );
    }

    #[test]
    fn holder_reference_direct_painting_and_missing_registry_are_distinct() {
        let profile = profile();
        assert!(
            matches!(decode(&mut CodecReader::new(&[0]), 21, &profile, None).unwrap(),
            EntityWireMetadataValue::RegistryHolder { identifier, .. } if identifier == "minecraft:example")
        );
        let mut direct = vec![0, 1, 2];
        direct.extend(identifier_bytes("minecraft:test"));
        direct.extend([0, 0]);
        assert!(matches!(
            decode(&mut CodecReader::new(&direct), 34, &profile, None).unwrap(),
            EntityWireMetadataValue::DirectPainting {
                width: 1,
                height: 2,
                ..
            }
        ));
        assert!(decode(&mut CodecReader::new(&[2]), 21, &profile, None).is_err());
        let missing = EntityTypeProfile::default();
        assert!(matches!(
            decode(&mut CodecReader::new(&[0]), 21, &missing, None),
            Err(BootstrapProtocolError::EntityRegistryUnavailable { .. })
        ));
    }

    #[test]
    fn particle_option_families_and_profile_fields_are_bounded() {
        let profile = profile();
        let mut dust = vec![1];
        dust.extend(0x00ff_00ff_i32.to_be_bytes());
        dust.extend(0.5_f32.to_be_bytes());
        assert!(matches!(
            decode(&mut CodecReader::new(&dust), 16, &profile, None).unwrap(),
            EntityWireMetadataValue::Particle(EntityWireParticle {
                data: EntityWireParticleData::Dust { .. },
                ..
            })
        ));
        assert!(matches!(
            decode(&mut CodecReader::new(&[2, 1]), 16, &profile, None).unwrap(),
            EntityWireMetadataValue::Particle(EntityWireParticle {
                data: EntityWireParticleData::BlockState { id: 1, .. },
                ..
            })
        ));
        let mut global = vec![1];
        global.extend(identifier_bytes("minecraft:overworld"));
        global.extend([0; 8]);
        assert!(
            matches!(decode(&mut CodecReader::new(&global), 33, &profile, None).unwrap(),
            EntityWireMetadataValue::GlobalPos(Some((dimension, _))) if dimension == "minecraft:overworld")
        );
        assert!(
            decode(
                &mut CodecReader::new(&[0xff, 0xff, 0x7f]),
                17,
                &profile,
                None
            )
            .is_err()
        );
        assert!(decode(&mut CodecReader::new(&[0; 7]), 41, &profile, None).is_err());
    }

    #[test]
    fn block_state_optional_and_registry_lifecycle_are_exact() {
        let mut profile = profile();
        profile.block_states.insert(
            42,
            (
                "minecraft:oak_stairs".to_owned(),
                BTreeMap::from([("facing".to_owned(), "north".to_owned())]),
            ),
        );
        assert!(matches!(
            decode(&mut CodecReader::new(&[42]), 14, &profile, None).unwrap(),
            EntityWireMetadataValue::BlockState { id: 42, block, properties }
                if block == "minecraft:oak_stairs" && properties["facing"] == "north"
        ));
        assert_eq!(
            decode(&mut CodecReader::new(&[0]), 15, &profile, None).unwrap(),
            EntityWireMetadataValue::OptionalBlockState(None)
        );
        assert!(matches!(
            decode(&mut CodecReader::new(&[42]), 15, &profile, None).unwrap(),
            EntityWireMetadataValue::OptionalBlockState(Some((42, ..)))
        ));
        assert!(decode(&mut CodecReader::new(&[43]), 14, &profile, None).is_err());
        assert!(decode(&mut CodecReader::new(&[43]), 15, &profile, None).is_err());

        let mut pending = EntityTypeProfile::default();
        assert!(matches!(
            decode(&mut CodecReader::new(&[0]), 21, &pending, None),
            Err(BootstrapProtocolError::EntityRegistryUnavailable { .. })
        ));
        pending.install_connection_registries(BTreeMap::from([(
            "minecraft:cat_variant".to_owned(),
            vec![MinecraftIdentifier::new("minecraft:tabby").unwrap()],
        )]));
        assert!(matches!(
            decode(&mut CodecReader::new(&[0]), 21, &pending, None).unwrap(),
            EntityWireMetadataValue::RegistryHolder { identifier, .. }
                if identifier == "minecraft:tabby"
        ));
        assert!(matches!(
            decode(&mut CodecReader::new(&[1]), 21, &pending, None),
            Err(BootstrapProtocolError::EntityRegistryEntryUnknown { .. })
        ));
    }

    #[test]
    fn enum_ordinals_and_optional_reference_are_bounded() {
        let profile = profile();
        for (serializer, max) in [(20, 18), (35, 7), (36, 4), (37, 5), (38, 4), (42, 2)] {
            for ordinal in 0..max {
                let value = decode(
                    &mut CodecReader::new(&[ordinal]),
                    serializer,
                    &profile,
                    None,
                )
                .unwrap();
                assert_eq!(value.serializer_id(), serializer as u8);
            }
            assert!(decode(&mut CodecReader::new(&[max]), serializer, &profile, None).is_err());
        }
        assert_eq!(
            decode(&mut CodecReader::new(&[0]), 13, &profile, None).unwrap(),
            EntityWireMetadataValue::LivingEntityReference(None)
        );
        let mut reference = vec![1];
        reference.extend([0xab; 16]);
        assert_eq!(
            decode(&mut CodecReader::new(&reference), 13, &profile, None).unwrap(),
            EntityWireMetadataValue::LivingEntityReference(Some([0xab; 16]))
        );
        assert!(decode(&mut CodecReader::new(&[1, 0]), 13, &profile, None).is_err());
    }

    #[test]
    fn malformed_payloads_fail_without_metadata_fallback() {
        let profile = profile();
        let items = item_profile();
        for (serializer, payload) in [
            (5, vec![8, 0]),
            (7, vec![1, 1, 1]),
            (16, vec![3, 1]),
            (34, vec![0, 1]),
            (41, vec![1, 0]),
        ] {
            assert!(
                decode(
                    &mut CodecReader::new(&payload),
                    serializer,
                    &profile,
                    Some(&items)
                )
                .is_err()
            );
        }
        assert!(decode(&mut CodecReader::new(&[]), 43, &profile, Some(&items)).is_err());
        assert!(decode(&mut CodecReader::new(&[0]), 41, &profile, Some(&items)).is_err());
    }

    #[test]
    fn non_simple_particle_families_consume_their_exact_payloads() {
        let mut profile = profile();
        let mut cases = Vec::new();
        cases.push(("minecraft:dragon_breath", 1_f32.to_be_bytes().to_vec()));
        cases.push(("minecraft:dust_color_transition", {
            let mut bytes = Vec::new();
            bytes.extend(0x112233_i32.to_be_bytes());
            bytes.extend(0x445566_i32.to_be_bytes());
            bytes.extend(1.5_f32.to_be_bytes());
            bytes
        }));
        for kind in ["minecraft:effect", "minecraft:instant_effect"] {
            let mut bytes = 0x112233_i32.to_be_bytes().to_vec();
            bytes.extend(0.75_f32.to_be_bytes());
            cases.push((kind, bytes));
        }
        for kind in [
            "minecraft:entity_effect",
            "minecraft:tinted_leaves",
            "minecraft:flash",
        ] {
            cases.push((kind, 0x112233_i32.to_be_bytes().to_vec()));
        }
        cases.push(("minecraft:sculk_charge", 0.5_f32.to_be_bytes().to_vec()));
        cases.push(("minecraft:item", vec![1, 1, 0, 0]));
        cases.push(("minecraft:vibration", vec![0, 0, 0, 0, 0, 0, 0, 0, 0, 3]));
        cases.push(("minecraft:trail", {
            let mut bytes = Vec::new();
            for coordinate in [1.0_f64, -2.0, 3.0] {
                bytes.extend(coordinate.to_be_bytes());
            }
            bytes.extend(0x112233_i32.to_be_bytes());
            bytes.push(5);
            bytes
        }));
        cases.push(("minecraft:shriek", vec![3]));
        for kind in [
            "minecraft:block_marker",
            "minecraft:falling_dust",
            "minecraft:dust_pillar",
            "minecraft:block_crumble",
        ] {
            cases.push((kind, vec![1]));
        }
        for (index, (kind, payload)) in cases.into_iter().enumerate() {
            let raw = 10 + index as u32;
            profile.particles.insert(raw, kind.to_owned());
            let mut bytes = vec![raw as u8];
            bytes.extend(payload);
            let mut reader = CodecReader::new(&bytes);
            let decoded = decode(&mut reader, 16, &profile, Some(&item_profile())).unwrap();
            assert_eq!(reader.position(), bytes.len(), "{kind}");
            assert!(
                matches!(decoded, EntityWireMetadataValue::Particle(particle) if particle.kind == kind)
            );
        }
    }
}
