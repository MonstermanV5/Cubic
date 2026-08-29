//! Version-independent, connection-owned semantic world/session state.
//!
//! Wire packets are translated into [`WorldEvent`] values outside this crate.
//! Chunk, entity, movement, collision, and rendering state are deliberately not
//! part of Phase 13.

mod model;
mod state;

pub use model::{
    AuthoritativeRotation, AuthoritativeTransform, BlockCoordinates, ClockState, Difficulty,
    DimensionTypeReference, EnterWorld, GameMode, LastDeathLocation, PlayerPositionUpdate,
    RelativeTransformFlags, Respawn, RespawnRotation, RuntimeRegistrySnapshot,
    RuntimeRegistrySummary, SpawnContext, SpawnPoint, WeatherState, WorldBorder, WorldEvent,
    WorldSession, WorldTime,
};
pub use state::{
    MAX_KNOWN_DIMENSIONS, MAX_RUNTIME_REGISTRIES, MAX_RUNTIME_REGISTRY_ENTRIES, MAX_WORLD_CLOCKS,
    ResetScope, WorldError, WorldLifecycle, WorldState, WorldTransition,
};
