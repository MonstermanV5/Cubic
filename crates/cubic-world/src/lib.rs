//! Version-independent, connection-owned semantic world/session state.
//!
//! Wire packets are translated into [`WorldEvent`] values outside this crate.
//! Phase 14 adds bounded semantic chunk state while entity, movement,
//! collision, and rendering state remain deliberately absent.

mod chunk;
mod model;
mod state;

pub use chunk::{
    BlockEntitySummary, Chunk, ChunkCoordinate, ChunkLightSummary, ChunkSection, ChunkStoreError,
    ChunkSummary, HeightmapData, LoadedChunks, MAX_BLOCK_ENTITIES_PER_CHUNK, MAX_CHUNK_SECTIONS,
    MAX_HEIGHTMAP_LONGS, MAX_HEIGHTMAPS, MAX_LOADED_CHUNKS, PaletteForm, PalettedContainer,
    RuntimeBiomeId, RuntimeBlockStateId, SECTION_BIOME_COUNT, SECTION_BLOCK_COUNT,
};

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
