//! Connection-owned, version-independent remote entity state.
use std::collections::BTreeMap;

use cubic_core::StructuredText;
use thiserror::Error;

use crate::ItemStack;
use crate::Vec3d;

pub const MAX_REMOTE_ENTITIES: usize = 4_096;
pub const ENTITY_INTERPOLATION_MILLIS: u64 = 150;
pub const MAX_ENTITY_METADATA_RETAINED_BYTES: usize = 128 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EntityHandle {
    slot: u32,
    generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EntityTransform {
    pub position: Vec3d,
    pub yaw: f32,
    pub pitch: f32,
    pub head_yaw: f32,
}

impl EntityTransform {
    #[must_use]
    pub fn is_finite(self) -> bool {
        self.position.is_finite()
            && self.yaw.is_finite()
            && self.pitch.is_finite()
            && self.head_yaw.is_finite()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entity {
    pub id: i32,
    pub uuid: [u8; 16],
    pub entity_type: String,
    pub authoritative: EntityTransform,
    pub velocity: Vec3d,
    pub spawn_data: i32,
    pub metadata: BTreeMap<u8, EntityMetadataValue>,
    pub attributes: BTreeMap<String, EntityAttribute>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum EntityMetadataValue {
    Byte(i8),
    Integer(i32),
    Long(i64),
    Float(f32),
    String(String),
    Boolean(bool),
    Component {
        plain: String,
        structured: StructuredText,
        wire: Vec<u8>,
    },
    OptionalComponent(Option<(String, StructuredText, Vec<u8>)>),
    ItemStack(Option<ItemStack>),
    Rotations([f32; 3]),
    BlockPos([i32; 3]),
    OptionalBlockPos(Option<[i32; 3]>),
    Direction(u8),
    /// UUID identity, never an unsafe pointer or a recyclable protocol ID.
    LivingEntityReference(Option<[u8; 16]>),
    BlockState {
        id: u32,
        block: String,
        properties: BTreeMap<String, String>,
    },
    OptionalBlockState(Option<(u32, String, BTreeMap<String, String>)>),
    Particle(EntityMetadataParticle),
    Particles(Vec<EntityMetadataParticle>),
    VillagerData {
        kind: String,
        profession: String,
        level: i32,
    },
    OptionalUnsignedInt(Option<u32>),
    Pose {
        ordinal: u8,
        name: String,
    },
    RegistryHolder {
        serializer_id: u8,
        registry: String,
        identifier: String,
    },
    GlobalPos(Option<(String, [i32; 3])>),
    DirectPainting {
        width: i32,
        height: i32,
        asset: String,
        title: Option<(String, StructuredText, Vec<u8>)>,
        author: Option<(String, StructuredText, Vec<u8>)>,
    },
    EnumState {
        serializer_id: u8,
        ordinal: u8,
        name: String,
    },
    Vector3([f32; 3]),
    Quaternion([f32; 4]),
    ResolvableProfile(EntityMetadataProfile),
}

impl EntityMetadataValue {
    #[must_use]
    pub const fn serializer_id(&self) -> u8 {
        match self {
            Self::Byte(_) => 0,
            Self::Integer(_) => 1,
            Self::Long(_) => 2,
            Self::Float(_) => 3,
            Self::String(_) => 4,
            Self::Component { .. } => 5,
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

    fn retained_bytes(&self) -> usize {
        let extra = match self {
            Self::String(value) => value.len(),
            Self::Component { plain, wire, .. } => plain.len() + wire.len(),
            Self::OptionalComponent(value) => value
                .as_ref()
                .map_or(0, |(plain, _, wire)| plain.len() + wire.len()),
            Self::ItemStack(value) => value.as_ref().map_or(0, item_bytes),
            Self::BlockState {
                block, properties, ..
            } => {
                block.len()
                    + properties
                        .iter()
                        .map(|(key, value)| key.len() + value.len())
                        .sum::<usize>()
            }
            Self::OptionalBlockState(value) => {
                value.as_ref().map_or(0, |(_, block, properties)| {
                    block.len()
                        + properties
                            .iter()
                            .map(|(key, value)| key.len() + value.len())
                            .sum::<usize>()
                })
            }
            Self::Particle(particle) => particle_bytes(particle),
            Self::Particles(particles) => particles.iter().map(particle_bytes).sum(),
            Self::VillagerData {
                kind, profession, ..
            } => kind.len() + profession.len(),
            Self::Pose { name, .. } | Self::EnumState { name, .. } => name.len(),
            Self::RegistryHolder {
                registry,
                identifier,
                ..
            } => registry.len() + identifier.len(),
            Self::GlobalPos(value) => value.as_ref().map_or(0, |(dimension, _)| dimension.len()),
            Self::DirectPainting {
                asset,
                title,
                author,
                ..
            } => {
                asset.len()
                    + title
                        .as_ref()
                        .map_or(0, |(plain, _, wire)| plain.len() + wire.len())
                    + author
                        .as_ref()
                        .map_or(0, |(plain, _, wire)| plain.len() + wire.len())
            }
            Self::ResolvableProfile(profile) => {
                profile.name.as_ref().map_or(0, String::len)
                    + profile
                        .properties
                        .iter()
                        .map(|(name, value, signature)| {
                            name.len() + value.len() + signature.as_ref().map_or(0, String::len)
                        })
                        .sum::<usize>()
                    + profile
                        .skin
                        .iter()
                        .flatten()
                        .map(String::len)
                        .sum::<usize>()
            }
            _ => 0,
        };
        size_of::<Self>().saturating_add(extra)
    }
}

impl Entity {
    pub fn set_metadata(
        &mut self,
        accessor: u8,
        value: EntityMetadataValue,
    ) -> Result<(), EntityError> {
        let previous = self
            .metadata
            .get(&accessor)
            .map_or(0, EntityMetadataValue::retained_bytes);
        let current = self
            .metadata
            .values()
            .map(EntityMetadataValue::retained_bytes)
            .sum::<usize>();
        let next = current
            .saturating_sub(previous)
            .saturating_add(value.retained_bytes());
        if next > MAX_ENTITY_METADATA_RETAINED_BYTES {
            return Err(EntityError::MetadataCapacity {
                retained: next,
                max: MAX_ENTITY_METADATA_RETAINED_BYTES,
            });
        }
        self.metadata.insert(accessor, value);
        Ok(())
    }
}

fn item_bytes(stack: &ItemStack) -> usize {
    stack.item.as_str().len()
        + stack
            .components
            .added
            .iter()
            .map(|(identifier, value)| identifier.as_str().len() + value.retained_bytes())
            .sum::<usize>()
        + stack
            .components
            .removed
            .iter()
            .map(|identifier| identifier.as_str().len())
            .sum::<usize>()
}

fn particle_bytes(particle: &EntityMetadataParticle) -> usize {
    let extra = match &particle.data {
        EntityMetadataParticleData::BlockState {
            block, properties, ..
        } => {
            block.len()
                + properties
                    .iter()
                    .map(|(key, value)| key.len() + value.len())
                    .sum::<usize>()
        }
        EntityMetadataParticleData::Item(stack) => item_bytes(stack),
        _ => 0,
    };
    size_of::<EntityMetadataParticle>() + particle.kind.len() + extra
}

#[derive(Clone, Debug, PartialEq)]
pub struct EntityMetadataParticle {
    pub kind: String,
    pub data: EntityMetadataParticleData,
}

#[derive(Clone, Debug, PartialEq)]
pub enum EntityMetadataParticleData {
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
    Item(ItemStack),
    VibrationBlock {
        position: [i32; 3],
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
pub struct EntityMetadataProfile {
    pub name: Option<String>,
    pub uuid: Option<[u8; 16]>,
    pub properties: Vec<(String, String, Option<String>)>,
    pub skin: [Option<String>; 3],
    pub slim: Option<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EntityAttribute {
    pub base: f64,
    pub modifiers: BTreeMap<String, EntityAttributeModifier>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EntityAttributeModifier {
    pub amount: f64,
    pub operation: u8,
}

#[derive(Clone, Debug, PartialEq)]
struct EntitySlot {
    generation: u64,
    entity: Option<Entity>,
    previous: EntityTransform,
    target_at_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EntityStore {
    slots: Vec<EntitySlot>,
    by_id: BTreeMap<i32, EntityHandle>,
    free: Vec<u32>,
}

#[derive(Clone, Debug, Error, PartialEq)]
pub enum EntityError {
    #[error("remote entity cap {MAX_REMOTE_ENTITIES} exceeded")]
    Capacity,
    #[error("invalid entity transform or velocity")]
    NonFinite,
    #[error("remote entity type identifier is invalid")]
    InvalidType,
    #[error("entity metadata would retain {retained} bytes, exceeding {max}")]
    MetadataCapacity { retained: usize, max: usize },
}

impl EntityStore {
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    #[must_use]
    pub fn handle(&self, id: i32) -> Option<EntityHandle> {
        self.by_id.get(&id).copied()
    }

    #[must_use]
    pub fn get(&self, handle: EntityHandle) -> Option<&Entity> {
        let slot = self.slots.get(handle.slot as usize)?;
        if slot.generation != handle.generation {
            return None;
        }
        slot.entity.as_ref()
    }

    pub fn get_mut(&mut self, handle: EntityHandle) -> Option<&mut Entity> {
        let slot = self.slots.get_mut(handle.slot as usize)?;
        if slot.generation != handle.generation {
            return None;
        }
        slot.entity.as_mut()
    }

    pub fn spawn(&mut self, entity: Entity, at_ms: u64) -> Result<EntityHandle, EntityError> {
        if !entity.authoritative.is_finite() || !entity.velocity.is_finite() {
            return Err(EntityError::NonFinite);
        }
        if entity.entity_type.len() > 256 || !entity.entity_type.contains(':') {
            return Err(EntityError::InvalidType);
        }
        self.remove(entity.id);
        let slot_index = if let Some(index) = self.free.pop() {
            index
        } else {
            if self.slots.len() >= MAX_REMOTE_ENTITIES {
                return Err(EntityError::Capacity);
            }
            let index = u32::try_from(self.slots.len()).map_err(|_| EntityError::Capacity)?;
            self.slots.push(EntitySlot {
                generation: 0,
                previous: entity.authoritative,
                entity: None,
                target_at_ms: at_ms,
            });
            index
        };
        let slot = self
            .slots
            .get_mut(slot_index as usize)
            .ok_or(EntityError::Capacity)?;
        slot.previous = entity.authoritative;
        slot.target_at_ms = at_ms;
        slot.entity = Some(entity);
        let handle = EntityHandle {
            slot: slot_index,
            generation: slot.generation,
        };
        if let Some(entity) = slot.entity.as_ref() {
            self.by_id.insert(entity.id, handle);
        }
        Ok(handle)
    }

    pub fn remove(&mut self, id: i32) -> bool {
        let Some(handle) = self.by_id.remove(&id) else {
            return false;
        };
        if let Some(slot) = self.slots.get_mut(handle.slot as usize) {
            slot.entity = None;
            slot.generation = slot.generation.wrapping_add(1);
            self.free.push(handle.slot);
        }
        true
    }

    pub fn clear(&mut self) {
        let ids: Vec<i32> = self.by_id.keys().copied().collect();
        for id in ids {
            self.remove(id);
        }
    }

    pub fn update_transform(
        &mut self,
        id: i32,
        next: EntityTransform,
        at_ms: u64,
        teleport: bool,
    ) -> Result<bool, EntityError> {
        if !next.is_finite() {
            return Err(EntityError::NonFinite);
        }
        let Some(handle) = self.handle(id) else {
            return Ok(false);
        };
        let Some(slot) = self.slots.get_mut(handle.slot as usize) else {
            return Ok(false);
        };
        let Some(entity) = slot.entity.as_mut() else {
            return Ok(false);
        };
        let old = entity.authoritative;
        let dx = next.position.x - old.position.x;
        let dy = next.position.y - old.position.y;
        let dz = next.position.z - old.position.z;
        let snap = teleport || dx * dx + dy * dy + dz * dz > 64.0;
        slot.previous = if snap { next } else { old };
        slot.target_at_ms = at_ms;
        entity.authoritative = next;
        Ok(true)
    }

    #[must_use]
    pub fn render_transform(&self, handle: EntityHandle, now_ms: u64) -> Option<EntityTransform> {
        let slot = self.slots.get(handle.slot as usize)?;
        if slot.generation != handle.generation {
            return None;
        }
        let target = slot.entity.as_ref()?.authoritative;
        let t = (now_ms.saturating_sub(slot.target_at_ms) as f64
            / ENTITY_INTERPOLATION_MILLIS as f64)
            .clamp(0.0, 1.0);
        if t >= 1.0 {
            return Some(target);
        }
        let lerp = |a: f64, b: f64| a + (b - a) * t;
        let angle = |a: f32, b: f32| {
            let difference = (b - a + 180.0).rem_euclid(360.0) - 180.0;
            a + difference * t as f32
        };
        Some(EntityTransform {
            position: Vec3d::new(
                lerp(slot.previous.position.x, target.position.x),
                lerp(slot.previous.position.y, target.position.y),
                lerp(slot.previous.position.z, target.position.z),
            ),
            yaw: angle(slot.previous.yaw, target.yaw),
            pitch: angle(slot.previous.pitch, target.pitch),
            head_yaw: angle(slot.previous.head_yaw, target.head_yaw),
        })
    }

    #[must_use]
    pub fn snapshot(&self, now_ms: u64) -> Vec<(EntityHandle, &Entity, EntityTransform)> {
        self.by_id
            .values()
            .filter_map(|handle| {
                Some((
                    *handle,
                    self.get(*handle)?,
                    self.render_transform(*handle, now_ms)?,
                ))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entity(id: i32) -> Entity {
        Entity {
            id,
            uuid: [id as u8; 16],
            entity_type: "minecraft:zombie".into(),
            authoritative: EntityTransform {
                position: Vec3d::new(0.0, 0.0, 0.0),
                yaw: 359.0,
                pitch: 0.0,
                head_yaw: 0.0,
            },
            velocity: Vec3d::new(0.0, 0.0, 0.0),
            spawn_data: 0,
            metadata: BTreeMap::new(),
            attributes: BTreeMap::new(),
        }
    }

    #[test]
    fn stale_handles_and_protocol_id_reuse_do_not_alias() {
        let mut store = EntityStore::default();
        let first = store.spawn(entity(7), 0).unwrap();
        assert!(store.remove(7));
        let second = store.spawn(entity(7), 1).unwrap();
        assert_ne!(first, second);
        assert!(store.get(first).is_none());
        assert_eq!(store.get(second).unwrap().id, 7);
        store.clear();
        assert!(store.get(second).is_none());
    }

    #[test]
    fn authority_and_shortest_arc_render_interpolation_are_separate() {
        let mut store = EntityStore::default();
        let handle = store.spawn(entity(2), 0).unwrap();
        let next = EntityTransform {
            position: Vec3d::new(6.0, 0.0, 0.0),
            yaw: 1.0,
            pitch: 0.0,
            head_yaw: 0.0,
        };
        store.update_transform(2, next, 100, false).unwrap();
        assert_eq!(store.get(handle).unwrap().authoritative, next);
        let mid = store.render_transform(handle, 175).unwrap();
        assert_eq!(mid.position.x, 3.0);
        assert!((mid.yaw - 360.0).abs() < 0.001);
        assert_eq!(store.render_transform(handle, 300).unwrap(), next);
    }

    #[test]
    fn teleport_snaps_and_removal_eliminates_render_snapshot() {
        let mut store = EntityStore::default();
        let handle = store.spawn(entity(3), 0).unwrap();
        let next = EntityTransform {
            position: Vec3d::new(200.0, 80.0, 9.0),
            ..store.get(handle).unwrap().authoritative
        };
        store.update_transform(3, next, 10, true).unwrap();
        assert_eq!(store.render_transform(handle, 10).unwrap(), next);
        assert_eq!(store.snapshot(10).len(), 1);
        store.remove(3);
        assert!(store.snapshot(11).is_empty());
    }

    #[test]
    fn metadata_replaces_accessor_and_caps_retained_server_content() {
        let mut entity = entity(4);
        entity
            .set_metadata(1, EntityMetadataValue::Integer(10))
            .unwrap();
        entity
            .set_metadata(1, EntityMetadataValue::Integer(20))
            .unwrap();
        assert_eq!(
            entity.metadata.get(&1),
            Some(&EntityMetadataValue::Integer(20))
        );
        assert_eq!(entity.metadata.get(&1).unwrap().serializer_id(), 1);
        assert!(matches!(
            entity.set_metadata(
                2,
                EntityMetadataValue::String("x".repeat(MAX_ENTITY_METADATA_RETAINED_BYTES))
            ),
            Err(EntityError::MetadataCapacity { .. })
        ));
        assert!(!entity.metadata.contains_key(&2));
    }

    #[test]
    fn unresolved_living_reference_does_not_retarget_on_protocol_id_reuse() {
        let mut referenced_entity = entity(5);
        let identity = [0x42; 16];
        referenced_entity
            .set_metadata(
                3,
                EntityMetadataValue::LivingEntityReference(Some(identity)),
            )
            .unwrap();
        let mut store = EntityStore::default();
        store.spawn(referenced_entity, 0).unwrap();
        let old = store.spawn(entity(7), 0).unwrap();
        store.remove(7);
        let new = store.spawn(entity(7), 1).unwrap();
        assert_ne!(old, new);
        let owned = store.get(store.handle(5).unwrap()).unwrap();
        assert_eq!(
            owned.metadata.get(&3),
            Some(&EntityMetadataValue::LivingEntityReference(Some(identity)))
        );
    }
}
