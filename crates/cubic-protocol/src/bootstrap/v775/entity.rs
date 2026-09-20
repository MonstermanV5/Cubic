//! Verified 26.1.2 clientbound entity transport subset. IDs are from Mojang's
//! `packets.json`; field order is checked against the installed client codecs.
use std::collections::BTreeMap;

use cubic_version::{GameData, MinecraftIdentifier};

use crate::{CodecError, CodecReader, split_raw_packet};

use super::{BootstrapProtocolError, decode_low_precision_vec3};

mod metadata;
pub use metadata::{
    EntityWireMetadataValue, EntityWireParticle, EntityWireParticleData, EntityWireProfile,
};

const ADD_ENTITY: i32 = 0x01;
const POSITION_SYNC: i32 = 0x23;
const MOVE_POS: i32 = 0x35;
const MOVE_POS_ROT: i32 = 0x36;
const MOVE_ROT: i32 = 0x38;
const REMOVE: i32 = 0x4d;
const HEAD_ROT: i32 = 0x53;
const ENTITY_DATA: i32 = 0x63;
const MOTION: i32 = 0x65;
const TELEPORT: i32 = 0x7d;
const UPDATE_ATTRIBUTES: i32 = 0x83;

pub const MAX_REMOVED_ENTITIES_PER_PACKET: usize = 4_096;
pub const MAX_ENTITY_METADATA_VALUE_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Default)]
pub struct EntityTypeProfile {
    types: BTreeMap<u32, String>,
    attributes: BTreeMap<u32, String>,
    particles: BTreeMap<u32, String>,
    block_states: BTreeMap<u32, (String, BTreeMap<String, String>)>,
    static_registries: BTreeMap<String, BTreeMap<u32, String>>,
    connection_registries: BTreeMap<String, Vec<MinecraftIdentifier>>,
}

impl EntityTypeProfile {
    #[must_use]
    pub fn from_game_data(data: &GameData) -> Self {
        let registry = |name| {
            data.artifact()
                .registries
                .iter()
                .find(|registry| registry.identifier.as_str() == name)
                .map_or(&[][..], |registry| registry.entries.as_slice())
                .iter()
                .map(|entry| (entry.raw_id, entry.identifier.as_str().to_owned()))
                .collect()
        };
        Self {
            types: registry("minecraft:entity_type"),
            attributes: registry("minecraft:attribute"),
            particles: registry("minecraft:particle_type"),
            static_registries: data
                .artifact()
                .registries
                .iter()
                .filter(|entry| {
                    matches!(
                        entry.identifier.as_str(),
                        "minecraft:villager_type" | "minecraft:villager_profession"
                    )
                })
                .map(|table| {
                    (
                        table.identifier.as_str().to_owned(),
                        table
                            .entries
                            .iter()
                            .map(|entry| (entry.raw_id, entry.identifier.as_str().to_owned()))
                            .collect(),
                    )
                })
                .collect(),
            block_states: data
                .artifact()
                .blocks
                .iter()
                .flat_map(|block| {
                    block.states.iter().map(|state| {
                        (
                            state.state_id,
                            (
                                block.identifier.as_str().to_owned(),
                                state.properties.clone(),
                            ),
                        )
                    })
                })
                .collect(),
            connection_registries: BTreeMap::new(),
        }
    }

    /// Installs the authoritative per-connection ordering sent during Configuration.
    pub fn install_connection_registries(
        &mut self,
        registries: BTreeMap<String, Vec<MinecraftIdentifier>>,
    ) {
        // Reconfiguration may resend only changed registries; a replacement
        // for one registry must not discard the other connection mappings.
        self.connection_registries.extend(registries);
    }

    fn registry_entry(
        &self,
        registry: &'static str,
        raw_id: u32,
    ) -> Result<String, BootstrapProtocolError> {
        if let Some(entries) = self.connection_registries.get(registry) {
            return entries
                .get(raw_id as usize)
                .map(ToString::to_string)
                .ok_or(BootstrapProtocolError::EntityRegistryEntryUnknown { registry, raw_id });
        }
        if let Some(entries) = self.static_registries.get(registry) {
            return entries
                .get(&raw_id)
                .cloned()
                .ok_or(BootstrapProtocolError::EntityRegistryEntryUnknown { registry, raw_id });
        }
        Err(BootstrapProtocolError::EntityRegistryUnavailable { registry })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct EntitySpawn {
    pub id: i32,
    pub uuid: [u8; 16],
    pub entity_type: String,
    pub position: [f64; 3],
    pub velocity: [f64; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub head_yaw: f32,
    pub data: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum EntityWireEvent {
    Spawn(EntitySpawn),
    Remove(Vec<i32>),
    RelativeMove {
        id: i32,
        delta: [f64; 3],
        yaw: Option<f32>,
        pitch: Option<f32>,
    },
    Rotate {
        id: i32,
        yaw: f32,
        pitch: f32,
    },
    HeadRotate {
        id: i32,
        head_yaw: f32,
    },
    PositionSync {
        id: i32,
        position: [f64; 3],
        velocity: [f64; 3],
        yaw: f32,
        pitch: f32,
    },
    Teleport {
        id: i32,
        position: [f64; 3],
        velocity: [f64; 3],
        yaw: f32,
        pitch: f32,
        relative_flags: u32,
    },
    Velocity {
        id: i32,
        velocity: [f64; 3],
    },
    Attributes {
        id: i32,
        values: Vec<EntityWireAttribute>,
    },
    Metadata {
        id: i32,
        values: Vec<(u8, EntityWireMetadataValue)>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct EntityWireAttribute {
    pub identifier: String,
    pub base: f64,
    pub modifiers: Vec<EntityWireModifier>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EntityWireModifier {
    pub identifier: String,
    pub amount: f64,
    pub operation: u8,
}

pub fn decode_entity_packet(
    body: &[u8],
    profile: &EntityTypeProfile,
    items: Option<&super::ItemStackProfile>,
) -> Result<Option<EntityWireEvent>, BootstrapProtocolError> {
    let packet = split_raw_packet(body)?;
    let mut reader = CodecReader::new(packet.payload);
    let event = match packet.id {
        ADD_ENTITY => {
            let id = reader.read_var_int()?;
            let uuid = reader.read_uuid()?.to_bytes();
            let raw_type = nonnegative(reader.read_var_int()?, "entity type raw ID")?;
            let entity_type = profile
                .types
                .get(&raw_type)
                .ok_or_else(|| CodecError::ValueOutOfRange {
                    context: "entity type raw ID",
                    value: i128::from(raw_type),
                    min: 0,
                    max: i128::from(u32::MAX),
                })?
                .to_owned();
            let position = vec3(&mut reader)?;
            let velocity = low_precision_vec3(&mut reader)?;
            let pitch = angle(reader.read_u8()?);
            let yaw = angle(reader.read_u8()?);
            let head_yaw = angle(reader.read_u8()?);
            let data = reader.read_var_int()?;
            EntityWireEvent::Spawn(EntitySpawn {
                id,
                uuid,
                entity_type,
                position,
                velocity,
                yaw,
                pitch,
                head_yaw,
                data,
            })
        }
        REMOVE => {
            let count = nonnegative(reader.read_var_int()?, "remove entities count")? as usize;
            if count > MAX_REMOVED_ENTITIES_PER_PACKET {
                return Err(CodecError::ValueOutOfRange {
                    context: "remove entities count",
                    value: count as i128,
                    min: 0,
                    max: MAX_REMOVED_ENTITIES_PER_PACKET as i128,
                }
                .into());
            }
            let mut ids = Vec::with_capacity(count);
            for _ in 0..count {
                ids.push(reader.read_var_int()?);
            }
            EntityWireEvent::Remove(ids)
        }
        MOVE_POS | MOVE_POS_ROT => {
            let id = reader.read_var_int()?;
            let delta = [
                f64::from(reader.read_i16()?) / 4096.0,
                f64::from(reader.read_i16()?) / 4096.0,
                f64::from(reader.read_i16()?) / 4096.0,
            ];
            let (yaw, pitch) = if packet.id == MOVE_POS_ROT {
                (
                    Some(angle(reader.read_u8()?)),
                    Some(angle(reader.read_u8()?)),
                )
            } else {
                (None, None)
            };
            let _on_ground = reader.read_bool()?;
            EntityWireEvent::RelativeMove {
                id,
                delta,
                yaw,
                pitch,
            }
        }
        MOVE_ROT => {
            let id = reader.read_var_int()?;
            let yaw = angle(reader.read_u8()?);
            let pitch = angle(reader.read_u8()?);
            let _on_ground = reader.read_bool()?;
            EntityWireEvent::Rotate { id, yaw, pitch }
        }
        HEAD_ROT => {
            let id = reader.read_var_int()?;
            EntityWireEvent::HeadRotate {
                id,
                head_yaw: angle(reader.read_u8()?),
            }
        }
        ENTITY_DATA => {
            let id = reader.read_var_int()?;
            let mut values = Vec::new();
            loop {
                let accessor = reader.read_u8()?;
                if accessor == 0xff {
                    break;
                }
                if values.len() >= 64 || values.iter().any(|(index, _)| *index == accessor) {
                    return Err(CodecError::ValueOutOfRange {
                        context: "entity metadata accessor count or duplicate",
                        value: i128::from(accessor),
                        min: 0,
                        max: 63,
                    }
                    .into());
                }
                let serializer = reader.read_var_int()?;
                let start = reader.position();
                let value = metadata::decode(&mut reader, serializer, profile, items)?;
                let length = reader.consumed_since(start)?.len();
                if length > MAX_ENTITY_METADATA_VALUE_BYTES {
                    return Err(BootstrapProtocolError::PayloadTooLarge {
                        context: "entity metadata value",
                        length,
                        max: MAX_ENTITY_METADATA_VALUE_BYTES,
                    });
                }
                values.push((accessor, value));
            }
            EntityWireEvent::Metadata { id, values }
        }
        MOTION => {
            let id = reader.read_var_int()?;
            EntityWireEvent::Velocity {
                id,
                velocity: low_precision_vec3(&mut reader)?,
            }
        }
        POSITION_SYNC | TELEPORT => {
            let id = reader.read_var_int()?;
            let position = vec3(&mut reader)?;
            let velocity = vec3(&mut reader)?;
            let yaw = reader.read_f32()?;
            let pitch = reader.read_f32()?;
            if !yaw.is_finite() || !pitch.is_finite() {
                return Err(super::BootstrapProtocolError::NonFinite {
                    context: "entity rotation",
                });
            }
            if packet.id == TELEPORT {
                let relative_flags = reader.read_u32()?;
                if relative_flags & !0x01ff != 0 {
                    return Err(CodecError::ValueOutOfRange {
                        context: "Teleport Entity relative flags",
                        value: i128::from(relative_flags),
                        min: 0,
                        max: 0x01ff,
                    }
                    .into());
                }
                let _on_ground = reader.read_bool()?;
                EntityWireEvent::Teleport {
                    id,
                    position,
                    velocity,
                    yaw,
                    pitch,
                    relative_flags,
                }
            } else {
                let _on_ground = reader.read_bool()?;
                EntityWireEvent::PositionSync {
                    id,
                    position,
                    velocity,
                    yaw,
                    pitch,
                }
            }
        }
        UPDATE_ATTRIBUTES => {
            let id = reader.read_var_int()?;
            let count = nonnegative(reader.read_var_int()?, "entity attribute count")? as usize;
            if count > 64 {
                return Err(CodecError::ValueOutOfRange {
                    context: "entity attribute count",
                    value: count as i128,
                    min: 0,
                    max: 64,
                }
                .into());
            }
            let mut values = Vec::with_capacity(count);
            let mut total_modifiers = 0_usize;
            for _ in 0..count {
                // Attribute.STREAM_CODEC is holderRegistry: zero-based raw
                // registry ID, never ByteBufCodecs.holder's direct/offset form.
                let raw_id = nonnegative(reader.read_var_int()?, "attribute registry holder")?;
                let identifier = profile
                    .attributes
                    .get(&raw_id)
                    .ok_or(CodecError::ValueOutOfRange {
                        context: "attribute registry ID",
                        value: i128::from(raw_id),
                        min: 0,
                        max: i128::from(u32::MAX),
                    })?
                    .clone();
                let base = reader.read_f64()?;
                if !base.is_finite() {
                    return Err(BootstrapProtocolError::NonFinite {
                        context: "entity attribute base",
                    });
                }
                let modifier_count =
                    nonnegative(reader.read_var_int()?, "attribute modifier count")? as usize;
                total_modifiers = total_modifiers.saturating_add(modifier_count);
                if modifier_count > 16 || total_modifiers > 128 {
                    return Err(CodecError::ValueOutOfRange {
                        context: "attribute modifier count",
                        value: modifier_count as i128,
                        min: 0,
                        max: 16,
                    }
                    .into());
                }
                let mut modifiers = Vec::with_capacity(modifier_count);
                for _ in 0..modifier_count {
                    let identifier = reader
                        .read_string(crate::StringLimits::new(256, 768))?
                        .to_owned();
                    cubic_version::MinecraftIdentifier::new(identifier.clone()).map_err(|_| {
                        CodecError::ValueOutOfRange {
                            context: "attribute modifier identifier",
                            value: 0,
                            min: 1,
                            max: 1,
                        }
                    })?;
                    let amount = reader.read_f64()?;
                    if !amount.is_finite() {
                        return Err(BootstrapProtocolError::NonFinite {
                            context: "attribute modifier amount",
                        });
                    }
                    let operation =
                        nonnegative(reader.read_var_int()?, "attribute modifier operation")?;
                    if operation > 2 {
                        return Err(CodecError::ValueOutOfRange {
                            context: "attribute modifier operation",
                            value: i128::from(operation),
                            min: 0,
                            max: 2,
                        }
                        .into());
                    }
                    modifiers.push(EntityWireModifier {
                        identifier,
                        amount,
                        operation: operation as u8,
                    });
                }
                values.push(EntityWireAttribute {
                    identifier,
                    base,
                    modifiers,
                });
            }
            EntityWireEvent::Attributes { id, values }
        }
        _ => return Ok(None),
    };
    if reader.remaining() != 0 {
        return Err(BootstrapProtocolError::TrailingData {
            context: "entity packet",
            remaining: reader.remaining(),
        });
    }
    Ok(Some(event))
}

fn nonnegative(value: i32, context: &'static str) -> Result<u32, CodecError> {
    u32::try_from(value).map_err(|_| CodecError::ValueOutOfRange {
        context,
        value: i128::from(value),
        min: 0,
        max: i128::from(i32::MAX),
    })
}

fn angle(value: u8) -> f32 {
    f32::from(value) * 360.0 / 256.0
}

fn vec3(reader: &mut CodecReader<'_>) -> Result<[f64; 3], BootstrapProtocolError> {
    let result = [reader.read_f64()?, reader.read_f64()?, reader.read_f64()?];
    if result.iter().any(|value| !value.is_finite()) {
        return Err(BootstrapProtocolError::NonFinite {
            context: "entity vector",
        });
    }
    Ok(result)
}

fn low_precision_vec3(reader: &mut CodecReader<'_>) -> Result<[f64; 3], BootstrapProtocolError> {
    let motion = decode_low_precision_vec3(reader)?;
    Ok([motion.delta_x, motion.delta_y, motion.delta_z])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_ids_and_relative_vector_have_independent_bytes() {
        assert_eq!(ADD_ENTITY, 1);
        assert_eq!(REMOVE, 77);
        assert_eq!(MOVE_POS, 53);
        assert_eq!(MOVE_POS_ROT, 54);
        assert_eq!(MOVE_ROT, 56);
        assert_eq!(HEAD_ROT, 83);
        assert_eq!(TELEPORT, 125);
        let profile = EntityTypeProfile {
            types: BTreeMap::new(),
            attributes: BTreeMap::new(),
            ..Default::default()
        };
        // Mojang MoveEntity.Pos: packet, VarInt entity, three signed i16
        // deltas scaled by 1/4096, then onGround.
        let raw = [0x35, 0x05, 0x10, 0x00, 0xf0, 0x00, 0, 0, 1];
        assert_eq!(
            decode_entity_packet(&raw, &profile, None).unwrap(),
            Some(EntityWireEvent::RelativeMove {
                id: 5,
                delta: [1.0, -1.0, 0.0],
                yaw: None,
                pitch: None
            })
        );
        assert!(decode_entity_packet(&raw[..raw.len() - 1], &profile, None).is_err());
        assert_eq!(
            decode_entity_packet(&[0x36, 5, 0, 0, 0, 0, 0, 0, 64, 192, 0], &profile, None).unwrap(),
            Some(EntityWireEvent::RelativeMove {
                id: 5,
                delta: [0.0; 3],
                yaw: Some(90.0),
                pitch: Some(270.0)
            })
        );
        assert_eq!(
            decode_entity_packet(&[0x4d, 2, 5, 6], &profile, None).unwrap(),
            Some(EntityWireEvent::Remove(vec![5, 6]))
        );
        assert!(
            decode_entity_packet(&[0x4d, 0xff, 0xff, 0xff, 0xff, 0x07], &profile, None).is_err()
        );
    }

    #[test]
    fn unified_spawn_uses_registry_identity_and_bounded_current_field_order() {
        let profile = EntityTypeProfile {
            types: BTreeMap::from([(2, "minecraft:zombie".to_owned())]),
            attributes: BTreeMap::new(),
            ..Default::default()
        };
        let mut raw = vec![0x01, 0x05];
        raw.extend([0x11; 16]);
        raw.push(2);
        raw.extend(1.5_f64.to_be_bytes());
        raw.extend((-2.0_f64).to_be_bytes());
        raw.extend(3.0_f64.to_be_bytes());
        raw.extend([0, 0, 64, 128, 0]);
        let Some(EntityWireEvent::Spawn(spawn)) =
            decode_entity_packet(&raw, &profile, None).unwrap()
        else {
            panic!("expected spawn")
        };
        assert_eq!(spawn.id, 5);
        assert_eq!(spawn.uuid, [0x11; 16]);
        assert_eq!(spawn.entity_type, "minecraft:zombie");
        assert_eq!(spawn.position, [1.5, -2.0, 3.0]);
        assert_eq!((spawn.pitch, spawn.yaw, spawn.head_yaw), (0.0, 90.0, 180.0));
        raw[18] = 9;
        assert!(decode_entity_packet(&raw, &profile, None).is_err());
    }

    #[test]
    fn attribute_holder_modifier_and_operation_are_typed_and_bounded() {
        let profile = EntityTypeProfile {
            types: BTreeMap::new(),
            attributes: BTreeMap::from([(1, "minecraft:max_health".to_owned())]),
            ..Default::default()
        };
        let mut raw = vec![0x83, 0x01, 5, 1, 1];
        raw.extend(20.0_f64.to_be_bytes());
        raw.push(1);
        raw.push(14);
        raw.extend(b"minecraft:test");
        raw.extend(2.0_f64.to_be_bytes());
        raw.push(1);
        let Some(EntityWireEvent::Attributes { id, values }) =
            decode_entity_packet(&raw, &profile, None).unwrap()
        else {
            panic!("expected attributes")
        };
        assert_eq!(id, 5);
        assert_eq!(values[0].identifier, "minecraft:max_health");
        assert_eq!(values[0].base, 20.0);
        assert_eq!(values[0].modifiers[0].identifier, "minecraft:test");
        assert_eq!(values[0].modifiers[0].amount, 2.0);
        assert_eq!(values[0].modifiers[0].operation, 1);
        raw.pop();
        raw.push(3);
        assert!(decode_entity_packet(&raw, &profile, None).is_err());
    }

    #[test]
    fn rotation_head_velocity_and_absolute_wire_vectors() {
        let profile = EntityTypeProfile {
            types: BTreeMap::new(),
            attributes: BTreeMap::new(),
            ..Default::default()
        };
        assert_eq!(
            decode_entity_packet(&[0x38, 5, 64, 192, 1], &profile, None).unwrap(),
            Some(EntityWireEvent::Rotate {
                id: 5,
                yaw: 90.0,
                pitch: 270.0
            })
        );
        assert_eq!(
            decode_entity_packet(&[0x53, 5, 128], &profile, None).unwrap(),
            Some(EntityWireEvent::HeadRotate {
                id: 5,
                head_yaw: 180.0
            })
        );
        assert_eq!(
            decode_entity_packet(&[0x65, 5, 0], &profile, None).unwrap(),
            Some(EntityWireEvent::Velocity {
                id: 5,
                velocity: [0.0; 3]
            })
        );
        let mut sync = vec![0x23, 5];
        for value in [1.0_f64, -2.0, 3.0, 0.25, 0.0, -0.25] {
            sync.extend(value.to_be_bytes());
        }
        sync.extend(90.0_f32.to_be_bytes());
        sync.extend(15.0_f32.to_be_bytes());
        sync.push(0);
        assert_eq!(
            decode_entity_packet(&sync, &profile, None).unwrap(),
            Some(EntityWireEvent::PositionSync {
                id: 5,
                position: [1.0, -2.0, 3.0],
                velocity: [0.25, 0.0, -0.25],
                yaw: 90.0,
                pitch: 15.0,
            })
        );
        let mut teleport = sync;
        teleport[0] = 0x7d;
        teleport.pop();
        teleport.extend(0x0000_0001_u32.to_be_bytes());
        teleport.push(1);
        assert_eq!(
            decode_entity_packet(&teleport, &profile, None).unwrap(),
            Some(EntityWireEvent::Teleport {
                id: 5,
                position: [1.0, -2.0, 3.0],
                velocity: [0.25, 0.0, -0.25],
                yaw: 90.0,
                pitch: 15.0,
                relative_flags: 1,
            })
        );
        let flags_at = teleport.len() - 5;
        teleport[flags_at] = 0x01;
        teleport[flags_at + 1] = 0x00;
        assert!(decode_entity_packet(&teleport, &profile, None).is_err());
    }

    #[test]
    fn metadata_primitives_replace_by_accessor_and_require_terminator() {
        let profile = EntityTypeProfile {
            types: BTreeMap::new(),
            attributes: BTreeMap::new(),
            ..Default::default()
        };
        // 0x63 Set Entity Data, entity 5, accessor 0 BYTE, 1 INT,
        // 2 BOOLEAN, followed by the 0xff list sentinel.
        let packet = [0x63, 5, 0, 0, 0x20, 1, 1, 0xac, 2, 2, 8, 1, 0xff];
        assert_eq!(
            decode_entity_packet(&packet, &profile, None).unwrap(),
            Some(EntityWireEvent::Metadata {
                id: 5,
                values: vec![
                    (0, EntityWireMetadataValue::Byte(0x20)),
                    (1, EntityWireMetadataValue::Integer(300)),
                    (2, EntityWireMetadataValue::Boolean(true)),
                ],
            })
        );
        assert!(decode_entity_packet(&packet[..packet.len() - 1], &profile, None).is_err());
        assert!(decode_entity_packet(&[0x63, 5, 0, 99, 0xff], &profile, None).is_err());
        // A truncated valid serializer is rejected, never silently skipped.
        assert!(decode_entity_packet(&[0x63, 5, 0, 5, 0xff], &profile, None).is_err());
    }
}
