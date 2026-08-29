use std::collections::BTreeSet;

use thiserror::Error;

use crate::{
    AuthoritativeRotation, AuthoritativeTransform, EnterWorld, LoadedChunks, PlayerPositionUpdate,
    RespawnRotation, RuntimeRegistrySnapshot, SpawnPoint, WorldBorder, WorldEvent, WorldSession,
    WorldTime,
};

pub const MAX_KNOWN_DIMENSIONS: usize = 1_024;
pub const MAX_RUNTIME_REGISTRIES: usize = 512;
pub const MAX_RUNTIME_REGISTRY_ENTRIES: usize = 1_048_576;
pub const MAX_WORLD_CLOCKS: usize = 64;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WorldLifecycle {
    #[default]
    Disconnected,
    Configuring,
    Active,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ResetScope {
    #[default]
    None,
    WorldContents,
    Connection,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorldTransition {
    pub revision: u64,
    pub reset: ResetScope,
    pub dimension_changed: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct WorldState {
    lifecycle: WorldLifecycle,
    revision: u64,
    registries: RuntimeRegistrySnapshot,
    session: Option<WorldSession>,
    chunks: LoadedChunks,
}

#[derive(Clone, Debug, Error, PartialEq)]
pub enum WorldError {
    #[error("world event {event} is invalid while lifecycle is {lifecycle:?}")]
    InvalidLifecycle {
        event: &'static str,
        lifecycle: WorldLifecycle,
    },
    #[error("known dimension count {count} exceeds limit {max}")]
    TooManyDimensions { count: usize, max: usize },
    #[error("known dimensions contain a duplicate identifier")]
    DuplicateDimension,
    #[error("current dimension is absent from the server's known-dimension set")]
    CurrentDimensionUnknown,
    #[error("runtime registry count {count} exceeds limit {max}")]
    TooManyRegistries { count: usize, max: usize },
    #[error("runtime registry {registry} has {count} entries, exceeding limit {max}")]
    TooManyRegistryEntries {
        registry: String,
        count: usize,
        max: usize,
    },
    #[error("runtime registry summaries contain a duplicate identifier")]
    DuplicateRegistry,
    #[error("world clock updates contain a duplicate runtime clock ID")]
    DuplicateWorldClock,
    #[error("world-state revision counter is exhausted")]
    RevisionOverflow,
    #[error("{field} must be finite and within its permitted range")]
    InvalidNumber { field: &'static str },
    #[error("teleport ID {value} is negative")]
    NegativeTeleportId { value: i32 },
    #[error("teleport ID {received} is not newer than applied ID {current}")]
    StaleTeleport { current: i32, received: i32 },
    #[error("relative {field} update requires a prior authoritative position")]
    RelativeWithoutBaseline { field: &'static str },
    #[error(transparent)]
    ChunkStore(#[from] crate::ChunkStoreError),
}

impl WorldState {
    #[must_use]
    pub const fn lifecycle(&self) -> WorldLifecycle {
        self.lifecycle
    }

    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    #[must_use]
    pub fn session(&self) -> Option<&WorldSession> {
        self.session.as_ref()
    }

    #[must_use]
    pub const fn runtime_registries(&self) -> &RuntimeRegistrySnapshot {
        &self.registries
    }

    #[must_use]
    pub const fn loaded_chunks(&self) -> &LoadedChunks {
        &self.chunks
    }

    pub fn apply(&mut self, event: WorldEvent) -> Result<WorldTransition, WorldError> {
        let next_revision = self
            .revision
            .checked_add(1)
            .ok_or(WorldError::RevisionOverflow)?;
        let (reset, dimension_changed) = match event {
            WorldEvent::BeginConfiguration => {
                self.lifecycle = WorldLifecycle::Configuring;
                self.registries = RuntimeRegistrySnapshot::default();
                self.session = None;
                self.chunks.clear();
                (ResetScope::Connection, false)
            }
            WorldEvent::RuntimeRegistries(mut registries) => {
                self.require(WorldLifecycle::Configuring, "RuntimeRegistries")?;
                validate_registries(&registries)?;
                registries
                    .registries
                    .sort_by(|left, right| left.registry.cmp(&right.registry));
                self.registries = registries;
                (ResetScope::None, false)
            }
            WorldEvent::EnterWorld(enter) => {
                self.require(WorldLifecycle::Configuring, "EnterWorld")?;
                validate_enter_world(&enter)?;
                self.session = Some(session_from_enter(enter));
                self.lifecycle = WorldLifecycle::Active;
                self.chunks.clear();
                (ResetScope::WorldContents, true)
            }
            WorldEvent::Respawn(respawn) => {
                if let RespawnRotation::Reset(rotation) = respawn.rotation {
                    validate_rotation(rotation)?;
                }
                let session = self.active_mut("Respawn")?;
                let dimension_changed = session.spawn_context.dimension != respawn.spawn.dimension;
                session.spawn_context = respawn.spawn;
                session.position = None;
                if let RespawnRotation::Reset(rotation) = respawn.rotation {
                    session.rotation = Some(rotation);
                }
                if dimension_changed {
                    session.spawn_point = None;
                    session.time = None;
                    session.weather = Default::default();
                    session.border = None;
                }
                self.chunks.clear();
                (ResetScope::WorldContents, dimension_changed)
            }
            WorldEvent::SynchronizePlayerPosition(update) => {
                let session = self.active_mut("SynchronizePlayerPosition")?;
                let position = apply_position(
                    session.position,
                    session.rotation,
                    session.last_teleport_id,
                    update,
                )?;
                session.rotation = Some(AuthoritativeRotation {
                    yaw: position.yaw,
                    pitch: position.pitch,
                });
                session.last_teleport_id = Some(position.teleport_id);
                session.position = Some(position);
                (ResetScope::None, false)
            }
            WorldEvent::SetSpawn(spawn) => {
                validate_spawn(&spawn)?;
                self.active_mut("SetSpawn")?.spawn_point = Some(spawn);
                (ResetScope::None, false)
            }
            WorldEvent::SetTime(time) => {
                validate_time(&time)?;
                self.active_mut("SetTime")?.time = Some(time);
                (ResetScope::None, false)
            }
            WorldEvent::SetDifficulty { difficulty, locked } => {
                self.active_mut("SetDifficulty")?.difficulty = Some((difficulty, locked));
                (ResetScope::None, false)
            }
            WorldEvent::SetRaining(raining) => {
                self.active_mut("SetRaining")?.weather.raining = raining;
                (ResetScope::None, false)
            }
            WorldEvent::SetRainLevel(level) => {
                validate_level(level, "rain level")?;
                self.active_mut("SetRainLevel")?.weather.rain_level = level;
                (ResetScope::None, false)
            }
            WorldEvent::SetThunderLevel(level) => {
                validate_level(level, "thunder level")?;
                self.active_mut("SetThunderLevel")?.weather.thunder_level = level;
                (ResetScope::None, false)
            }
            WorldEvent::SetGameMode(mode) => {
                self.active_mut("SetGameMode")?.spawn_context.game_mode = mode;
                (ResetScope::None, false)
            }
            WorldEvent::SetWorldBorder(border) => {
                validate_border(&border)?;
                self.active_mut("SetWorldBorder")?.border = Some(border);
                (ResetScope::None, false)
            }
            WorldEvent::LoadChunk(chunk) => {
                self.require(WorldLifecycle::Active, "LoadChunk")?;
                self.chunks.insert(chunk)?;
                (ResetScope::None, false)
            }
            WorldEvent::UnloadChunk(coordinate) => {
                self.require(WorldLifecycle::Active, "UnloadChunk")?;
                self.chunks.remove(coordinate);
                (ResetScope::None, false)
            }
            WorldEvent::UpdateChunkLight { coordinate, light } => {
                self.require(WorldLifecycle::Active, "UpdateChunkLight")?;
                self.chunks.update_light(coordinate, light);
                (ResetScope::None, false)
            }
            WorldEvent::Disconnect => {
                self.lifecycle = WorldLifecycle::Disconnected;
                self.registries = RuntimeRegistrySnapshot::default();
                self.session = None;
                self.chunks.clear();
                (ResetScope::Connection, false)
            }
        };
        self.revision = next_revision;
        Ok(WorldTransition {
            revision: self.revision,
            reset,
            dimension_changed,
        })
    }

    #[must_use]
    pub fn summary(&self) -> String {
        let Some(session) = &self.session else {
            return format!("lifecycle={:?}", self.lifecycle);
        };
        let position = session.position.map_or_else(
            || "unavailable".to_owned(),
            |value| {
                format!(
                    "{:.3},{:.3},{:.3} yaw={:.2} pitch={:.2} teleport={}",
                    value.x, value.y, value.z, value.yaw, value.pitch, value.teleport_id
                )
            },
        );
        format!(
            "lifecycle=Active dimension={} dimension_type_raw={} entity={} game_mode={:?} position={} known_dimensions={} runtime_registries={} loaded_chunks={}",
            session.spawn_context.dimension,
            session.spawn_context.dimension_type.raw_id,
            session.player_entity_id,
            session.spawn_context.game_mode,
            position,
            session.known_dimensions.len(),
            self.registries.registries.len(),
            self.chunks.len()
        )
    }

    fn require(&self, lifecycle: WorldLifecycle, event: &'static str) -> Result<(), WorldError> {
        if self.lifecycle != lifecycle {
            return Err(WorldError::InvalidLifecycle {
                event,
                lifecycle: self.lifecycle,
            });
        }
        Ok(())
    }

    fn active_mut(&mut self, event: &'static str) -> Result<&mut WorldSession, WorldError> {
        if self.lifecycle != WorldLifecycle::Active {
            return Err(WorldError::InvalidLifecycle {
                event,
                lifecycle: self.lifecycle,
            });
        }
        self.session.as_mut().ok_or(WorldError::InvalidLifecycle {
            event,
            lifecycle: self.lifecycle,
        })
    }
}

fn session_from_enter(enter: EnterWorld) -> WorldSession {
    let mut known_dimensions = enter.known_dimensions;
    known_dimensions.sort();
    WorldSession {
        player_entity_id: enter.player_entity_id,
        hardcore: enter.hardcore,
        known_dimensions,
        max_players: enter.max_players,
        view_distance: enter.view_distance,
        simulation_distance: enter.simulation_distance,
        reduced_debug_info: enter.reduced_debug_info,
        show_death_screen: enter.show_death_screen,
        limited_crafting: enter.limited_crafting,
        secure_chat_enforced: enter.secure_chat_enforced,
        spawn_context: enter.spawn,
        position: None,
        rotation: None,
        last_teleport_id: None,
        spawn_point: None,
        time: None,
        difficulty: None,
        weather: Default::default(),
        border: None,
    }
}

fn validate_enter_world(enter: &EnterWorld) -> Result<(), WorldError> {
    if enter.known_dimensions.len() > MAX_KNOWN_DIMENSIONS {
        return Err(WorldError::TooManyDimensions {
            count: enter.known_dimensions.len(),
            max: MAX_KNOWN_DIMENSIONS,
        });
    }
    let unique: BTreeSet<_> = enter.known_dimensions.iter().collect();
    if unique.len() != enter.known_dimensions.len() {
        return Err(WorldError::DuplicateDimension);
    }
    if !unique.contains(&enter.spawn.dimension) {
        return Err(WorldError::CurrentDimensionUnknown);
    }
    Ok(())
}

fn validate_registries(registries: &RuntimeRegistrySnapshot) -> Result<(), WorldError> {
    if registries.registries.len() > MAX_RUNTIME_REGISTRIES {
        return Err(WorldError::TooManyRegistries {
            count: registries.registries.len(),
            max: MAX_RUNTIME_REGISTRIES,
        });
    }
    let mut unique = BTreeSet::new();
    for registry in &registries.registries {
        if !unique.insert(&registry.registry) {
            return Err(WorldError::DuplicateRegistry);
        }
        if registry.entry_count > MAX_RUNTIME_REGISTRY_ENTRIES {
            return Err(WorldError::TooManyRegistryEntries {
                registry: registry.registry.to_string(),
                count: registry.entry_count,
                max: MAX_RUNTIME_REGISTRY_ENTRIES,
            });
        }
    }
    Ok(())
}

fn apply_position(
    current: Option<AuthoritativeTransform>,
    rotation: Option<AuthoritativeRotation>,
    last_teleport_id: Option<i32>,
    update: PlayerPositionUpdate,
) -> Result<AuthoritativeTransform, WorldError> {
    if update.teleport_id < 0 {
        return Err(WorldError::NegativeTeleportId {
            value: update.teleport_id,
        });
    }
    if let Some(current) = last_teleport_id
        && update.teleport_id <= current
    {
        return Err(WorldError::StaleTeleport {
            current,
            received: update.teleport_id,
        });
    }
    for (field, finite) in [
        ("x", update.x.is_finite()),
        ("y", update.y.is_finite()),
        ("z", update.z.is_finite()),
        ("yaw", update.yaw.is_finite()),
        ("pitch", update.pitch.is_finite()),
    ] {
        if !finite {
            return Err(WorldError::InvalidNumber { field });
        }
    }
    let resolve_f64 = |value: f64, relative: bool, prior: Option<f64>, field| {
        if relative {
            prior
                .map(|base| base + value)
                .ok_or(WorldError::RelativeWithoutBaseline { field })
        } else {
            Ok(value)
        }
    };
    let resolve_f32 = |value: f32, relative: bool, prior: Option<f32>, field| {
        if relative {
            prior
                .map(|base| base + value)
                .ok_or(WorldError::RelativeWithoutBaseline { field })
        } else {
            Ok(value)
        }
    };
    let result = AuthoritativeTransform {
        x: resolve_f64(
            update.x,
            update.relative.x,
            current.map(|value| value.x),
            "x",
        )?,
        y: resolve_f64(
            update.y,
            update.relative.y,
            current.map(|value| value.y),
            "y",
        )?,
        z: resolve_f64(
            update.z,
            update.relative.z,
            current.map(|value| value.z),
            "z",
        )?,
        yaw: resolve_f32(
            update.yaw,
            update.relative.yaw,
            rotation.map(|value| value.yaw),
            "yaw",
        )?,
        pitch: resolve_f32(
            update.pitch,
            update.relative.pitch,
            rotation.map(|value| value.pitch),
            "pitch",
        )?,
        teleport_id: update.teleport_id,
    };
    if !result.x.is_finite()
        || !result.y.is_finite()
        || !result.z.is_finite()
        || !result.yaw.is_finite()
        || !result.pitch.is_finite()
    {
        return Err(WorldError::InvalidNumber {
            field: "resolved player transform",
        });
    }
    Ok(result)
}

fn validate_rotation(rotation: AuthoritativeRotation) -> Result<(), WorldError> {
    if !rotation.yaw.is_finite() {
        return Err(WorldError::InvalidNumber { field: "yaw" });
    }
    if !rotation.pitch.is_finite() {
        return Err(WorldError::InvalidNumber { field: "pitch" });
    }
    Ok(())
}

fn validate_spawn(spawn: &SpawnPoint) -> Result<(), WorldError> {
    if !spawn.yaw.is_finite() {
        return Err(WorldError::InvalidNumber { field: "spawn yaw" });
    }
    if !spawn.pitch.is_finite() {
        return Err(WorldError::InvalidNumber {
            field: "spawn pitch",
        });
    }
    Ok(())
}

fn validate_time(time: &WorldTime) -> Result<(), WorldError> {
    if time.clocks.len() > MAX_WORLD_CLOCKS {
        return Err(WorldError::TooManyRegistryEntries {
            registry: "minecraft:world_clock".to_owned(),
            count: time.clocks.len(),
            max: MAX_WORLD_CLOCKS,
        });
    }
    let mut clock_ids = BTreeSet::new();
    for clock in &time.clocks {
        if !clock_ids.insert(clock.clock_type_raw_id) {
            return Err(WorldError::DuplicateWorldClock);
        }
        if !clock.partial_tick.is_finite() {
            return Err(WorldError::InvalidNumber {
                field: "clock partial tick",
            });
        }
        if !clock.rate.is_finite() {
            return Err(WorldError::InvalidNumber {
                field: "clock rate",
            });
        }
    }
    Ok(())
}

fn validate_level(level: f32, field: &'static str) -> Result<(), WorldError> {
    if !level.is_finite() || !(0.0..=1.0).contains(&level) {
        return Err(WorldError::InvalidNumber { field });
    }
    Ok(())
}

fn validate_border(border: &WorldBorder) -> Result<(), WorldError> {
    for (field, value) in [
        ("border center x", border.center_x),
        ("border center z", border.center_z),
        ("border old diameter", border.old_diameter),
        ("border new diameter", border.new_diameter),
    ] {
        if !value.is_finite() || (field.contains("diameter") && value < 0.0) {
            return Err(WorldError::InvalidNumber { field });
        }
    }
    Ok(())
}
