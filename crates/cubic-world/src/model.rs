use cubic_version::MinecraftIdentifier;

use crate::{Chunk, ChunkCoordinate, ChunkLightSummary};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GameMode {
    Survival,
    Creative,
    Adventure,
    Spectator,
    Other(i32),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Difficulty {
    Peaceful,
    Easy,
    Normal,
    Hard,
    Other(u8),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DimensionTypeReference {
    pub registry: MinecraftIdentifier,
    pub raw_id: u32,
}

/// Authoritative vertical geometry supplied by the server's dimension-type registry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DimensionGeometry {
    pub min_y: i32,
    pub height: u32,
}

impl DimensionGeometry {
    #[must_use]
    pub const fn min_section_y(self) -> i32 {
        self.min_y / 16
    }

    #[must_use]
    pub const fn section_count(self) -> usize {
        (self.height / 16) as usize
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeDimensionType {
    pub raw_id: u32,
    pub identifier: MinecraftIdentifier,
    pub geometry: DimensionGeometry,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GrassColorModifier {
    None,
    DarkForest,
    Swamp,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeBiome {
    pub raw_id: u32,
    pub identifier: MinecraftIdentifier,
    pub temperature: f32,
    pub downfall: f32,
    pub water_color: u32,
    pub foliage_color: Option<u32>,
    pub dry_foliage_color: Option<u32>,
    pub grass_color: Option<u32>,
    pub grass_color_modifier: GrassColorModifier,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlockCoordinates {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlockStateUpdate {
    pub position: BlockCoordinates,
    pub state: crate::RuntimeBlockStateId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LastDeathLocation {
    pub dimension: MinecraftIdentifier,
    pub position: BlockCoordinates,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SpawnContext {
    pub dimension_type: DimensionTypeReference,
    pub dimension: MinecraftIdentifier,
    pub hashed_seed: i64,
    pub game_mode: GameMode,
    pub previous_game_mode: Option<GameMode>,
    pub debug_world: bool,
    pub flat_world: bool,
    pub last_death_location: Option<LastDeathLocation>,
    pub portal_cooldown_ticks: u32,
    pub sea_level: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EnterWorld {
    pub player_entity_id: i32,
    pub hardcore: bool,
    pub known_dimensions: Vec<MinecraftIdentifier>,
    pub max_players: u32,
    pub view_distance: u32,
    pub simulation_distance: u32,
    pub reduced_debug_info: bool,
    pub show_death_screen: bool,
    pub limited_crafting: bool,
    pub secure_chat_enforced: bool,
    pub spawn: SpawnContext,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Respawn {
    pub spawn: SpawnContext,
    pub keep_attribute_modifiers: bool,
    pub keep_entity_data: bool,
    pub rotation: RespawnRotation,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RespawnRotation {
    Preserve,
    Reset(AuthoritativeRotation),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RelativeTransformFlags {
    pub x: bool,
    pub y: bool,
    pub z: bool,
    pub yaw: bool,
    pub pitch: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerPositionUpdate {
    pub teleport_id: i32,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw: f32,
    pub pitch: f32,
    pub relative: RelativeTransformFlags,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerRotationUpdate {
    pub yaw: f32,
    pub pitch: f32,
    pub relative_yaw: bool,
    pub relative_pitch: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AuthoritativeTransform {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw: f32,
    pub pitch: f32,
    pub teleport_id: i32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AuthoritativeRotation {
    pub yaw: f32,
    pub pitch: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SpawnPoint {
    pub dimension: MinecraftIdentifier,
    pub position: BlockCoordinates,
    pub yaw: f32,
    pub pitch: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClockState {
    pub clock_type_raw_id: u32,
    pub total_ticks: i64,
    pub partial_tick: f32,
    pub rate: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorldTime {
    pub game_time: i64,
    pub clocks: Vec<ClockState>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WeatherState {
    pub raining: bool,
    pub rain_level: f32,
    pub thunder_level: f32,
}

impl Default for WeatherState {
    fn default() -> Self {
        Self {
            raining: false,
            rain_level: 0.0,
            thunder_level: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorldBorder {
    pub center_x: f64,
    pub center_z: f64,
    pub old_diameter: f64,
    pub new_diameter: f64,
    pub lerp_millis: u64,
    pub absolute_max_size: u32,
    pub warning_blocks: u32,
    pub warning_seconds: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeRegistrySummary {
    pub registry: MinecraftIdentifier,
    pub entry_count: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RuntimeRegistrySnapshot {
    pub registries: Vec<RuntimeRegistrySummary>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorldSession {
    pub player_entity_id: i32,
    pub hardcore: bool,
    pub known_dimensions: Vec<MinecraftIdentifier>,
    pub max_players: u32,
    pub view_distance: u32,
    pub simulation_distance: u32,
    pub reduced_debug_info: bool,
    pub show_death_screen: bool,
    pub limited_crafting: bool,
    pub secure_chat_enforced: bool,
    pub spawn_context: SpawnContext,
    pub dimension_geometry: DimensionGeometry,
    pub position: Option<AuthoritativeTransform>,
    pub rotation: Option<AuthoritativeRotation>,
    pub last_teleport_id: Option<i32>,
    pub spawn_point: Option<SpawnPoint>,
    pub time: Option<WorldTime>,
    pub difficulty: Option<(Difficulty, bool)>,
    pub weather: WeatherState,
    pub border: Option<WorldBorder>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum WorldEvent {
    BeginConfiguration,
    RuntimeRegistries(RuntimeRegistrySnapshot),
    RuntimeDimensionTypes(Vec<RuntimeDimensionType>),
    RuntimeBiomes(Vec<RuntimeBiome>),
    EnterWorld(EnterWorld),
    Respawn(Respawn),
    SynchronizePlayerPosition(PlayerPositionUpdate),
    SynchronizePlayerRotation(PlayerRotationUpdate),
    SetSpawn(SpawnPoint),
    SetTime(WorldTime),
    SetDifficulty {
        difficulty: Difficulty,
        locked: bool,
    },
    SetRaining(bool),
    SetRainLevel(f32),
    SetThunderLevel(f32),
    SetGameMode(GameMode),
    SetWorldBorder(WorldBorder),
    LoadChunk(Chunk),
    UnloadChunk(ChunkCoordinate),
    UpdateChunkLight {
        coordinate: ChunkCoordinate,
        light: ChunkLightSummary,
    },
    Disconnect,
}
