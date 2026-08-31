//! Version-independent, connection-owned semantic world/session state.
//!
//! Wire packets are translated into [`WorldEvent`] values outside this crate.
//! Phase 17 adds bounded semantic collision data and deterministic local-player
//! prediction. Protocol packet construction remains outside this crate.

mod chunk;
mod collision_vanilla;
mod model;
mod movement;
mod render;
mod state;

pub use chunk::{
    BlockEntitySummary, Chunk, ChunkCoordinate, ChunkLightSummary, ChunkSection, ChunkStoreError,
    ChunkSummary, HeightmapData, LoadedChunks, MAX_BLOCK_ENTITIES_PER_CHUNK, MAX_CHUNK_SECTIONS,
    MAX_HEIGHTMAP_LONGS, MAX_HEIGHTMAPS, MAX_LOADED_CHUNKS, PaletteForm, PalettedContainer,
    RuntimeBiomeId, RuntimeBlockStateId, SECTION_BIOME_COUNT, SECTION_BLOCK_COUNT,
};

pub use model::{
    AuthoritativeRotation, AuthoritativeTransform, BlockCoordinates, BlockStateUpdate, ClockState,
    Difficulty, DimensionGeometry, DimensionTypeReference, EnterWorld, GameMode, LastDeathLocation,
    PlayerPositionUpdate, PlayerRotationUpdate, RelativeTransformFlags, Respawn, RespawnRotation,
    RuntimeDimensionType, RuntimeRegistrySnapshot, RuntimeRegistrySummary, SpawnContext,
    SpawnPoint, WeatherState, WorldBorder, WorldEvent, WorldSession, WorldTime,
};
pub use movement::{
    Aabb, BlockCollisionProfile, CollisionCandidate, CollisionDiagnostics, CollisionShape,
    CollisionShapeKind, LocalPlayerPose, MAX_COLLISION_BOXES_PER_STATE, MovementInput,
    PlayerDimensions, PlayerMovementState, PlayerPoseKind, SimulationError, SimulationResult,
    Vec3d,
};
pub use render::{
    BlockVisualProfile, ChunkRenderDelta, RenderLookSample, RenderPoseSample, WorldRenderUpdate,
};
pub use state::{
    BlockUpdateResult, MAX_BLOCK_UPDATES_PER_EVENT, MAX_KNOWN_DIMENSIONS, MAX_RUNTIME_REGISTRIES,
    MAX_RUNTIME_REGISTRY_ENTRIES, MAX_WORLD_CLOCKS, ResetScope, WorldError, WorldLifecycle,
    WorldState, WorldTransition,
};
