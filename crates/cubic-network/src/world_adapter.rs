//! Translation from the temporary protocol-775 wire profile to stable world events.

use cubic_protocol::bootstrap::v775::{
    self, DefaultSpawnPosition, GlobalPosition, InitialPlayLogin, InitializeBorder, PlayerPosition,
    Respawn as WireRespawn, SpawnInfo, WorldTime as WireWorldTime,
};
use cubic_version::MinecraftIdentifier;
use cubic_world::{
    AuthoritativeRotation, BlockCoordinates, BlockEntitySummary, BlockStateUpdate, Chunk,
    ChunkCoordinate, ChunkLightSummary, ChunkSection, ClockState, Difficulty, DimensionGeometry,
    DimensionTypeReference, EnterWorld, GameMode, GrassColorModifier, HeightmapData,
    LastDeathLocation, LightLayerData, PalettedContainer, PlayerPositionUpdate,
    RelativeTransformFlags, Respawn, RespawnRotation, RuntimeBiome, RuntimeBiomeId,
    RuntimeBlockStateId, RuntimeDimensionType, SpawnContext, SpawnPoint, WorldBorder, WorldEvent,
    WorldTime,
};
use thiserror::Error;

const KEEP_ATTRIBUTE_MODIFIERS: u8 = 0x01;
const KEEP_ENTITY_DATA: u8 = 0x02;

#[derive(Debug, Error)]
pub(crate) enum WorldAdapterError {
    #[error("protocol-775 world identifier is invalid: {0}")]
    Identifier(String),
    #[error("protocol-775 {field} value {value} is negative or unsupported")]
    InvalidInteger { field: &'static str, value: i64 },
    #[error("protocol-775 game mode {0} is invalid")]
    InvalidGameMode(i32),
    #[error("protocol-775 difficulty {0} is invalid")]
    InvalidDifficulty(i32),
    #[error("protocol-775 {field} value must be finite")]
    NonFiniteFloat { field: &'static str },
    #[error("dimension-type registry entry {entry} is missing its data")]
    MissingDimensionData { entry: String },
    #[error("dimension-type registry entry {entry} does not contain compound NBT")]
    InvalidDimensionData { entry: String },
    #[error("dimension-type registry entry {entry} is missing integer field {field}")]
    MissingDimensionField { entry: String, field: &'static str },
    #[error("invalid {registry} registry entry {entry} at {path}: {reason}")]
    InvalidBiomeField {
        registry: &'static str,
        entry: String,
        path: &'static str,
        reason: &'static str,
    },
}

pub(crate) fn biomes(
    entries: Vec<v775::ConfigurationRegistryEntry<'_>>,
) -> Result<Vec<RuntimeBiome>, WorldAdapterError> {
    const REGISTRY: &str = "minecraft:worldgen/biome";
    let biomes = entries
        .into_iter()
        .enumerate()
        .map(|(raw_id, entry)| {
            let identifier = identifier(entry.identifier.to_owned())?;
            let data = entry
                .data
                .ok_or_else(|| WorldAdapterError::InvalidBiomeField {
                    registry: REGISTRY,
                    entry: identifier.to_string(),
                    path: "entry data",
                    reason: "required compound is absent",
                })?;
            let cubic_protocol::nbt::NbtTag::Compound(compound) = data else {
                return Err(WorldAdapterError::InvalidBiomeField {
                    registry: REGISTRY,
                    entry: identifier.to_string(),
                    path: "entry data",
                    reason: "expected compound NBT",
                });
            };
            let temperature = nbt_float(&compound, "temperature").ok_or_else(|| {
                WorldAdapterError::InvalidBiomeField {
                    registry: REGISTRY,
                    entry: identifier.to_string(),
                    path: "temperature",
                    reason: "required finite NBT float is absent or has the wrong type",
                }
            })?;
            let downfall = nbt_float(&compound, "downfall").ok_or_else(|| {
                WorldAdapterError::InvalidBiomeField {
                    registry: REGISTRY,
                    entry: identifier.to_string(),
                    path: "downfall",
                    reason: "required finite NBT float is absent or has the wrong type",
                }
            })?;
            let effects = match compound.get_str("effects") {
                Some(cubic_protocol::nbt::NbtTag::Compound(effects)) => effects,
                _ => {
                    return Err(WorldAdapterError::InvalidBiomeField {
                        registry: REGISTRY,
                        entry: identifier.to_string(),
                        path: "effects",
                        reason: "required compound is absent or has the wrong NBT type",
                    });
                }
            };
            let water_color = biome_color(
                effects,
                "water_color",
                "effects.water_color",
                REGISTRY,
                &identifier,
                true,
            )?
            .ok_or_else(|| WorldAdapterError::InvalidBiomeField {
                registry: REGISTRY,
                entry: identifier.to_string(),
                path: "effects.water_color",
                reason: "required hexadecimal NBT string is absent",
            })?;
            let foliage_color = biome_color(
                effects,
                "foliage_color",
                "effects.foliage_color",
                REGISTRY,
                &identifier,
                false,
            )?;
            let dry_foliage_color = biome_color(
                effects,
                "dry_foliage_color",
                "effects.dry_foliage_color",
                REGISTRY,
                &identifier,
                false,
            )?;
            let grass_color = biome_color(
                effects,
                "grass_color",
                "effects.grass_color",
                REGISTRY,
                &identifier,
                false,
            )?;
            let grass_color_modifier = biome_grass_modifier(effects, REGISTRY, &identifier)?;
            Ok(RuntimeBiome {
                raw_id: u32::try_from(raw_id).map_err(|_| WorldAdapterError::InvalidInteger {
                    field: "biome raw ID",
                    value: i64::MAX,
                })?,
                identifier,
                temperature,
                downfall,
                water_color,
                foliage_color,
                dry_foliage_color,
                grass_color,
                grass_color_modifier,
            })
        })
        .collect::<Result<Vec<_>, WorldAdapterError>>()?;
    tracing::debug!(
        entries = biomes.len(),
        grass_overrides = biomes
            .iter()
            .filter(|biome| biome.grass_color.is_some())
            .count(),
        foliage_overrides = biomes
            .iter()
            .filter(|biome| biome.foliage_color.is_some())
            .count(),
        grass_modifiers = biomes
            .iter()
            .filter(|biome| biome.grass_color_modifier != GrassColorModifier::None)
            .count(),
        "decoded protocol-775 runtime biome registry"
    );
    Ok(biomes)
}

fn biome_color(
    effects: &cubic_protocol::nbt::NbtCompound,
    field: &str,
    path: &'static str,
    registry: &'static str,
    identifier: &MinecraftIdentifier,
    required: bool,
) -> Result<Option<u32>, WorldAdapterError> {
    let Some(tag) = effects.get_str(field) else {
        return if required {
            Err(WorldAdapterError::InvalidBiomeField {
                registry,
                entry: identifier.to_string(),
                path,
                reason: "required hexadecimal NBT string is absent",
            })
        } else {
            Ok(None)
        };
    };
    let cubic_protocol::nbt::NbtTag::String(value) = tag else {
        return Err(WorldAdapterError::InvalidBiomeField {
            registry,
            entry: identifier.to_string(),
            path,
            reason: "expected hexadecimal NBT string",
        });
    };
    let value = value.to_string_lossy();
    let Some(hex) = value.strip_prefix('#').filter(|hex| hex.len() == 6) else {
        return Err(WorldAdapterError::InvalidBiomeField {
            registry,
            entry: identifier.to_string(),
            path,
            reason: "expected color in #RRGGBB form",
        });
    };
    let color = u32::from_str_radix(hex, 16).map_err(|_| WorldAdapterError::InvalidBiomeField {
        registry,
        entry: identifier.to_string(),
        path,
        reason: "color contains non-hexadecimal digits",
    })?;
    Ok(Some(color))
}

fn biome_grass_modifier(
    effects: &cubic_protocol::nbt::NbtCompound,
    registry: &'static str,
    identifier: &MinecraftIdentifier,
) -> Result<GrassColorModifier, WorldAdapterError> {
    let Some(tag) = effects.get_str("grass_color_modifier") else {
        return Ok(GrassColorModifier::None);
    };
    let cubic_protocol::nbt::NbtTag::String(value) = tag else {
        return Err(WorldAdapterError::InvalidBiomeField {
            registry,
            entry: identifier.to_string(),
            path: "effects.grass_color_modifier",
            reason: "expected NBT string",
        });
    };
    match value.to_string_lossy().as_str() {
        "none" => Ok(GrassColorModifier::None),
        "dark_forest" => Ok(GrassColorModifier::DarkForest),
        "swamp" => Ok(GrassColorModifier::Swamp),
        _ => Err(WorldAdapterError::InvalidBiomeField {
            registry,
            entry: identifier.to_string(),
            path: "effects.grass_color_modifier",
            reason: "unknown modifier",
        }),
    }
}

fn nbt_float(compound: &cubic_protocol::nbt::NbtCompound, name: &str) -> Option<f32> {
    match compound.get_str(name) {
        Some(cubic_protocol::nbt::NbtTag::Float(value)) if value.is_finite() => Some(*value),
        _ => None,
    }
}

pub(crate) fn dimension_types(
    entries: Vec<v775::ConfigurationRegistryEntry<'_>>,
) -> Result<Vec<RuntimeDimensionType>, WorldAdapterError> {
    entries
        .into_iter()
        .enumerate()
        .map(|(raw_id, entry)| {
            let identifier = identifier(entry.identifier.to_owned())?;
            let data = entry
                .data
                .ok_or_else(|| WorldAdapterError::MissingDimensionData {
                    entry: identifier.to_string(),
                })?;
            let cubic_protocol::nbt::NbtTag::Compound(compound) = data else {
                return Err(WorldAdapterError::InvalidDimensionData {
                    entry: identifier.to_string(),
                });
            };
            let min_y = compound.get_int("min_y").ok_or_else(|| {
                WorldAdapterError::MissingDimensionField {
                    entry: identifier.to_string(),
                    field: "min_y",
                }
            })?;
            let height = compound.get_int("height").ok_or_else(|| {
                WorldAdapterError::MissingDimensionField {
                    entry: identifier.to_string(),
                    field: "height",
                }
            })?;
            Ok(RuntimeDimensionType {
                raw_id: u32::try_from(raw_id).map_err(|_| WorldAdapterError::InvalidInteger {
                    field: "dimension type raw ID",
                    value: i64::MAX,
                })?,
                identifier,
                geometry: DimensionGeometry {
                    min_y,
                    height: u32::try_from(height).map_err(|_| {
                        WorldAdapterError::InvalidInteger {
                            field: "dimension height",
                            value: i64::from(height),
                        }
                    })?,
                },
            })
        })
        .collect()
}

pub(crate) enum ChunkAdaptation {
    Load(Chunk),
    Unload(ChunkCoordinate),
    Light {
        coordinate: ChunkCoordinate,
        light: ChunkLightSummary,
    },
    Blocks(Vec<BlockStateUpdate>),
    Other(v775::PlayClientbound),
}

pub(crate) fn adapt_chunk_packet(packet: v775::PlayClientbound) -> ChunkAdaptation {
    match packet {
        v775::PlayClientbound::LevelChunkWithLight(chunk) => {
            ChunkAdaptation::Load(semantic_chunk(chunk))
        }
        v775::PlayClientbound::ForgetLevelChunk { x, z } => {
            ChunkAdaptation::Unload(ChunkCoordinate::new(x, z))
        }
        v775::PlayClientbound::LightUpdate(update) => ChunkAdaptation::Light {
            coordinate: ChunkCoordinate::new(update.x, update.z),
            light: semantic_light(update.light),
        },
        v775::PlayClientbound::BlockUpdate(update) => {
            ChunkAdaptation::Blocks(vec![BlockStateUpdate {
                position: BlockCoordinates {
                    x: update.x,
                    y: update.y,
                    z: update.z,
                },
                state: RuntimeBlockStateId(update.state_id),
            }])
        }
        v775::PlayClientbound::SectionBlocksUpdate(update) => {
            let base_x = update.section_x * 16;
            let base_y = update.section_y * 16;
            let base_z = update.section_z * 16;
            ChunkAdaptation::Blocks(
                update
                    .updates
                    .into_iter()
                    .map(|entry| BlockStateUpdate {
                        position: BlockCoordinates {
                            x: base_x + i32::from(entry.local_x),
                            y: base_y + i32::from(entry.local_y),
                            z: base_z + i32::from(entry.local_z),
                        },
                        state: RuntimeBlockStateId(entry.state_id),
                    })
                    .collect(),
            )
        }
        other => ChunkAdaptation::Other(other),
    }
}

pub(crate) fn initial_world_event(
    login: InitialPlayLogin,
) -> Result<WorldEvent, WorldAdapterError> {
    let known_dimensions = login
        .known_dimensions
        .into_iter()
        .map(identifier)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(WorldEvent::EnterWorld(EnterWorld {
        player_entity_id: login.player_entity_id,
        hardcore: login.hardcore,
        known_dimensions,
        max_players: nonnegative(login.max_players, "maximum players")?,
        view_distance: nonnegative(login.view_distance, "view distance")?,
        simulation_distance: nonnegative(login.simulation_distance, "simulation distance")?,
        reduced_debug_info: login.reduced_debug_info,
        show_death_screen: login.show_death_screen,
        limited_crafting: login.limited_crafting,
        secure_chat_enforced: login.secure_chat_enforced,
        spawn: spawn_context(login.spawn)?,
    }))
}

pub(crate) fn play_world_event(
    packet: &v775::PlayClientbound,
) -> Result<Option<WorldEvent>, WorldAdapterError> {
    let event = match packet {
        v775::PlayClientbound::PlayerPosition(position) => Some(
            WorldEvent::SynchronizePlayerPosition(player_position(*position)),
        ),
        v775::PlayClientbound::PlayerRotation(rotation) => Some(
            WorldEvent::SynchronizePlayerRotation(cubic_world::PlayerRotationUpdate {
                yaw: rotation.yaw,
                pitch: rotation.pitch,
                relative_yaw: rotation.relative_yaw,
                relative_pitch: rotation.relative_pitch,
            }),
        ),
        v775::PlayClientbound::Respawn(respawn) => {
            Some(WorldEvent::Respawn(respawn_event(respawn)?))
        }
        v775::PlayClientbound::SetDefaultSpawnPosition(spawn) => {
            Some(WorldEvent::SetSpawn(spawn_point(spawn)?))
        }
        v775::PlayClientbound::SetTime(time) => Some(WorldEvent::SetTime(world_time(time)?)),
        v775::PlayClientbound::ChangeDifficulty { difficulty, locked } => {
            Some(WorldEvent::SetDifficulty {
                difficulty: difficulty_value(*difficulty)?,
                locked: *locked,
            })
        }
        v775::PlayClientbound::GameEvent { event, value } => game_event(*event, *value)?,
        v775::PlayClientbound::InitializeBorder(border) => {
            Some(WorldEvent::SetWorldBorder(world_border(*border)?))
        }
        _ => None,
    };
    Ok(event)
}

fn semantic_chunk(chunk: v775::LevelChunkWithLight) -> Chunk {
    Chunk {
        coordinate: ChunkCoordinate::new(chunk.x, chunk.z),
        sections: chunk.sections.into_iter().map(semantic_section).collect(),
        heightmaps: chunk
            .heightmaps
            .into_iter()
            .map(|heightmap| HeightmapData {
                kind_raw_id: heightmap.kind_raw_id,
                data: heightmap.data,
            })
            .collect(),
        block_entities: chunk
            .block_entities
            .into_iter()
            .map(|entity| BlockEntitySummary {
                local_x: entity.local_x,
                y: entity.y,
                local_z: entity.local_z,
                type_raw_id: entity.type_raw_id,
                has_data: entity.has_data,
            })
            .collect(),
        light: semantic_light(chunk.light),
    }
}

fn semantic_section(section: v775::WireChunkSection) -> ChunkSection {
    ChunkSection {
        non_empty_block_count: section.non_empty_block_count,
        fluid_count: section.fluid_count,
        blocks: map_palette(section.blocks, RuntimeBlockStateId),
        biomes: map_palette(section.biomes, RuntimeBiomeId),
    }
}

fn map_palette<T, F>(palette: v775::WirePalettedContainer, map: F) -> PalettedContainer<T>
where
    F: Fn(u32) -> T + Copy,
{
    match palette {
        v775::WirePalettedContainer::Single { value, entries } => PalettedContainer::Single {
            value: map(value),
            entries,
        },
        v775::WirePalettedContainer::Indirect { palette, indices } => PalettedContainer::Indirect {
            palette: palette.into_iter().map(map).collect(),
            indices,
        },
        v775::WirePalettedContainer::Direct { values } => PalettedContainer::Direct {
            values: values.into_iter().map(map).collect(),
        },
    }
}

fn semantic_light(light: v775::WireLightData) -> ChunkLightSummary {
    ChunkLightSummary {
        sky_mask: light.sky_mask,
        block_mask: light.block_mask,
        empty_sky_mask: light.empty_sky_mask,
        empty_block_mask: light.empty_block_mask,
        sky_layer_count: light.sky_layer_count,
        block_layer_count: light.block_layer_count,
        sky_layers: light
            .sky_layers
            .into_iter()
            .map(|layer| LightLayerData {
                mask_index: layer.mask_index,
                data: layer.data,
            })
            .collect(),
        block_layers: light
            .block_layers
            .into_iter()
            .map(|layer| LightLayerData {
                mask_index: layer.mask_index,
                data: layer.data,
            })
            .collect(),
    }
}

fn spawn_context(spawn: SpawnInfo) -> Result<SpawnContext, WorldAdapterError> {
    Ok(SpawnContext {
        dimension_type: DimensionTypeReference {
            registry: identifier("minecraft:dimension_type".to_owned())?,
            raw_id: nonnegative(spawn.dimension_type_raw_id, "dimension type raw ID")?,
        },
        dimension: identifier(spawn.dimension)?,
        hashed_seed: spawn.hashed_seed,
        game_mode: game_mode(i32::from(spawn.game_mode))?,
        previous_game_mode: if spawn.previous_game_mode == u8::MAX {
            None
        } else {
            Some(game_mode(i32::from(spawn.previous_game_mode))?)
        },
        debug_world: spawn.debug_world,
        flat_world: spawn.flat_world,
        last_death_location: spawn.last_death_location.map(last_death).transpose()?,
        portal_cooldown_ticks: nonnegative(spawn.portal_cooldown_ticks, "portal cooldown")?,
        sea_level: spawn.sea_level,
    })
}

fn last_death(position: GlobalPosition) -> Result<LastDeathLocation, WorldAdapterError> {
    Ok(LastDeathLocation {
        dimension: identifier(position.dimension)?,
        position: block_coordinates(position.position),
    })
}

fn respawn_event(respawn: &WireRespawn) -> Result<Respawn, WorldAdapterError> {
    let keep_entity_data = respawn.data_to_keep & KEEP_ENTITY_DATA != 0;
    Ok(Respawn {
        spawn: spawn_context(respawn.spawn.clone())?,
        keep_attribute_modifiers: respawn.data_to_keep & KEEP_ATTRIBUTE_MODIFIERS != 0,
        keep_entity_data,
        rotation: if keep_entity_data {
            RespawnRotation::Preserve
        } else {
            RespawnRotation::Reset(AuthoritativeRotation {
                yaw: -180.0,
                pitch: 0.0,
            })
        },
    })
}

fn player_position(position: PlayerPosition) -> PlayerPositionUpdate {
    PlayerPositionUpdate {
        teleport_id: position.teleport_id,
        x: position.x,
        y: position.y,
        z: position.z,
        yaw: position.yaw,
        pitch: position.pitch,
        relative: RelativeTransformFlags {
            x: position.relative_flags & 0x01 != 0,
            y: position.relative_flags & 0x02 != 0,
            z: position.relative_flags & 0x04 != 0,
            yaw: position.relative_flags & 0x08 != 0,
            pitch: position.relative_flags & 0x10 != 0,
        },
    }
}

fn spawn_point(spawn: &DefaultSpawnPosition) -> Result<SpawnPoint, WorldAdapterError> {
    Ok(SpawnPoint {
        dimension: identifier(spawn.position.dimension.clone())?,
        position: block_coordinates(spawn.position.position),
        yaw: spawn.yaw,
        pitch: spawn.pitch,
    })
}

fn world_time(time: &WireWorldTime) -> Result<WorldTime, WorldAdapterError> {
    let clocks = time
        .clocks
        .iter()
        .map(|clock| {
            Ok(ClockState {
                clock_type_raw_id: nonnegative(clock.clock_type_raw_id, "world clock raw ID")?,
                total_ticks: clock.total_ticks,
                partial_tick: clock.partial_tick,
                rate: clock.rate,
            })
        })
        .collect::<Result<Vec<_>, WorldAdapterError>>()?;
    Ok(WorldTime {
        game_time: time.game_time,
        clocks,
    })
}

fn game_event(event: u8, value: f32) -> Result<Option<WorldEvent>, WorldAdapterError> {
    let result = match event {
        1 => Some(WorldEvent::SetRaining(true)),
        2 => Some(WorldEvent::SetRaining(false)),
        3 => {
            if !value.is_finite() {
                return Err(WorldAdapterError::NonFiniteFloat { field: "game mode" });
            }
            if value.fract() != 0.0 {
                return Err(WorldAdapterError::InvalidGameMode(value as i32));
            }
            Some(WorldEvent::SetGameMode(game_mode(value as i32)?))
        }
        7 => Some(WorldEvent::SetRainLevel(value)),
        8 => Some(WorldEvent::SetThunderLevel(value)),
        _ => None,
    };
    Ok(result)
}

fn world_border(border: InitializeBorder) -> Result<WorldBorder, WorldAdapterError> {
    Ok(WorldBorder {
        center_x: border.center_x,
        center_z: border.center_z,
        old_diameter: border.old_diameter,
        new_diameter: border.new_diameter,
        lerp_millis: nonnegative_u64(border.lerp_millis, "border lerp time")?,
        absolute_max_size: nonnegative(border.absolute_max_size, "border maximum size")?,
        warning_blocks: nonnegative(border.warning_blocks, "border warning blocks")?,
        warning_seconds: nonnegative(border.warning_seconds, "border warning seconds")?,
    })
}

fn difficulty_value(value: i32) -> Result<Difficulty, WorldAdapterError> {
    match value {
        0 => Ok(Difficulty::Peaceful),
        1 => Ok(Difficulty::Easy),
        2 => Ok(Difficulty::Normal),
        3 => Ok(Difficulty::Hard),
        _ => Err(WorldAdapterError::InvalidDifficulty(value)),
    }
}

fn game_mode(value: i32) -> Result<GameMode, WorldAdapterError> {
    match value {
        0 => Ok(GameMode::Survival),
        1 => Ok(GameMode::Creative),
        2 => Ok(GameMode::Adventure),
        3 => Ok(GameMode::Spectator),
        _ => Err(WorldAdapterError::InvalidGameMode(value)),
    }
}

fn identifier(value: String) -> Result<MinecraftIdentifier, WorldAdapterError> {
    MinecraftIdentifier::new(value)
        .map_err(|error| WorldAdapterError::Identifier(error.to_string()))
}

fn nonnegative<T>(value: T, field: &'static str) -> Result<u32, WorldAdapterError>
where
    T: TryInto<u32> + Copy + Into<i64>,
{
    value
        .try_into()
        .map_err(|_| WorldAdapterError::InvalidInteger {
            field,
            value: value.into(),
        })
}

fn nonnegative_u64(value: i64, field: &'static str) -> Result<u64, WorldAdapterError> {
    u64::try_from(value).map_err(|_| WorldAdapterError::InvalidInteger { field, value })
}

fn block_coordinates(position: cubic_protocol::BlockPosition) -> BlockCoordinates {
    BlockCoordinates {
        x: position.x(),
        y: position.y(),
        z: position.z(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nbt_string(value: &str) -> cubic_protocol::nbt::NbtTag {
        cubic_protocol::nbt::NbtTag::String(value.into())
    }

    fn biome_entry(
        identifier: &'static str,
        temperature: f32,
        downfall: f32,
        effects: impl IntoIterator<Item = (&'static str, cubic_protocol::nbt::NbtTag)>,
    ) -> v775::ConfigurationRegistryEntry<'static> {
        let mut effect_compound = cubic_protocol::nbt::NbtCompound::new();
        for (name, value) in effects {
            effect_compound.insert(name.into(), value);
        }
        let mut compound = cubic_protocol::nbt::NbtCompound::new();
        compound.insert(
            "temperature".into(),
            cubic_protocol::nbt::NbtTag::Float(temperature),
        );
        compound.insert(
            "downfall".into(),
            cubic_protocol::nbt::NbtTag::Float(downfall),
        );
        compound.insert(
            "effects".into(),
            cubic_protocol::nbt::NbtTag::Compound(effect_compound),
        );
        v775::ConfigurationRegistryEntry {
            identifier,
            data: Some(cubic_protocol::nbt::NbtTag::Compound(compound)),
        }
    }

    fn spawn(dimension: &str) -> SpawnInfo {
        SpawnInfo {
            dimension_type_raw_id: 5,
            dimension: dimension.to_owned(),
            hashed_seed: 9,
            game_mode: 2,
            previous_game_mode: 1,
            debug_world: true,
            flat_world: false,
            last_death_location: None,
            portal_cooldown_ticks: 4,
            sea_level: 64,
        }
    }

    #[test]
    fn current_biome_registry_decodes_required_strings_and_optional_overrides() {
        let entries = vec![
            biome_entry(
                "minecraft:plains",
                0.8,
                0.4,
                [
                    ("water_color", nbt_string("#3f76e4")),
                    ("future_visual_field", nbt_string("ignored")),
                ],
            ),
            biome_entry(
                "minecraft:badlands",
                2.0,
                0.0,
                [
                    ("water_color", nbt_string("#3f76e4")),
                    ("foliage_color", nbt_string("#9e814d")),
                    ("grass_color", nbt_string("#90814d")),
                ],
            ),
            biome_entry(
                "minecraft:swamp",
                0.8,
                0.9,
                [
                    ("water_color", nbt_string("#617b64")),
                    ("foliage_color", nbt_string("#6a7039")),
                    ("grass_color_modifier", nbt_string("swamp")),
                ],
            ),
            biome_entry(
                "minecraft:dark_forest",
                0.7,
                0.8,
                [
                    ("water_color", nbt_string("#3f76e4")),
                    ("grass_color_modifier", nbt_string("dark_forest")),
                ],
            ),
        ];

        let decoded = biomes(entries).expect("current biome registry");
        assert_eq!(decoded.len(), 4);
        assert_eq!(decoded[0].water_color, 0x3f76e4);
        assert_eq!((decoded[0].temperature, decoded[0].downfall), (0.8, 0.4));
        assert_eq!(
            (decoded[0].grass_color, decoded[0].foliage_color),
            (None, None)
        );
        assert_eq!(decoded[1].grass_color, Some(0x90814d));
        assert_eq!(decoded[1].foliage_color, Some(0x9e814d));
        assert_eq!(decoded[2].water_color, 0x617b64);
        assert_eq!(decoded[2].grass_color_modifier, GrassColorModifier::Swamp);
        assert_eq!(
            decoded[3].grass_color_modifier,
            GrassColorModifier::DarkForest
        );
    }

    #[test]
    fn biome_registry_rejects_missing_and_wrong_required_fields_with_paths() {
        let missing = biome_entry("minecraft:test", 0.5, 0.5, []);
        let error = biomes(vec![missing]).expect_err("water color is required");
        let message = error.to_string();
        assert!(message.contains("minecraft:worldgen/biome"));
        assert!(message.contains("minecraft:test"));
        assert!(message.contains("effects.water_color"));

        let wrong_color = biome_entry(
            "minecraft:test",
            0.5,
            0.5,
            [("water_color", cubic_protocol::nbt::NbtTag::Int(0x3f76e4))],
        );
        assert!(
            biomes(vec![wrong_color])
                .expect_err("integer color is obsolete in 26.1.2")
                .to_string()
                .contains("expected hexadecimal NBT string")
        );

        let mut wrong_temperature = biome_entry(
            "minecraft:test",
            0.5,
            0.5,
            [("water_color", nbt_string("#3f76e4"))],
        );
        let Some(cubic_protocol::nbt::NbtTag::Compound(compound)) = wrong_temperature.data.as_mut()
        else {
            panic!("test biome compound")
        };
        compound.insert(
            "temperature".into(),
            cubic_protocol::nbt::NbtTag::Double(0.5),
        );
        assert!(
            biomes(vec![wrong_temperature])
                .expect_err("temperature must use the current float type")
                .to_string()
                .contains("temperature")
        );
    }

    #[test]
    fn malformed_present_optional_biome_fields_are_not_treated_as_absent() {
        for (field, value) in [
            ("grass_color", nbt_string("90814d")),
            ("foliage_color", cubic_protocol::nbt::NbtTag::Int(1)),
            ("grass_color_modifier", cubic_protocol::nbt::NbtTag::Byte(1)),
        ] {
            let entry = biome_entry(
                "minecraft:test",
                0.5,
                0.5,
                [("water_color", nbt_string("#3f76e4")), (field, value)],
            );
            assert!(
                biomes(vec![entry]).is_err(),
                "field {field} must be validated"
            );
        }
    }

    #[test]
    fn protocol_login_becomes_version_independent_enter_world() {
        let event = initial_world_event(InitialPlayLogin {
            player_entity_id: 12,
            hardcore: false,
            known_dimensions: vec!["custom:moon".to_owned()],
            max_players: 10,
            view_distance: 8,
            simulation_distance: 6,
            reduced_debug_info: false,
            show_death_screen: true,
            limited_crafting: false,
            spawn: spawn("custom:moon"),
            secure_chat_enforced: true,
        })
        .unwrap();
        let WorldEvent::EnterWorld(enter) = event else {
            panic!("expected EnterWorld")
        };
        assert_eq!(enter.spawn.dimension.as_str(), "custom:moon");
        assert_eq!(enter.spawn.dimension_type.raw_id, 5);
        assert_eq!(enter.spawn.game_mode, GameMode::Adventure);
    }

    #[test]
    fn wire_discriminants_are_validated_at_the_version_boundary() {
        let packet = v775::PlayClientbound::ChangeDifficulty {
            difficulty: 99,
            locked: false,
        };
        assert!(matches!(
            play_world_event(&packet),
            Err(WorldAdapterError::InvalidDifficulty(99))
        ));
        assert!(
            initial_world_event(InitialPlayLogin {
                player_entity_id: 1,
                hardcore: false,
                known_dimensions: vec!["Invalid:dimension".to_owned()],
                max_players: 1,
                view_distance: 1,
                simulation_distance: 1,
                reduced_debug_info: false,
                show_death_screen: true,
                limited_crafting: false,
                spawn: spawn("custom:moon"),
                secure_chat_enforced: false,
            })
            .is_err()
        );
    }

    #[test]
    fn chunk_wire_data_becomes_typed_semantic_state_without_packet_ids() {
        let packet = v775::PlayClientbound::LevelChunkWithLight(v775::LevelChunkWithLight {
            x: -8,
            z: 3,
            sections: vec![v775::WireChunkSection {
                non_empty_block_count: 1,
                fluid_count: 0,
                blocks: v775::WirePalettedContainer::Single {
                    value: 17,
                    entries: 4_096,
                },
                biomes: v775::WirePalettedContainer::Single {
                    value: 4,
                    entries: 64,
                },
            }],
            heightmaps: Vec::new(),
            block_entities: Vec::new(),
            light: v775::WireLightData::default(),
        });
        let ChunkAdaptation::Load(chunk) = adapt_chunk_packet(packet) else {
            panic!("expected semantic chunk")
        };
        assert_eq!(chunk.coordinate, ChunkCoordinate::new(-8, 3));
        assert_eq!(
            chunk.sections[0].block(0, 0, 0),
            Some(RuntimeBlockStateId(17))
        );
        assert_eq!(chunk.sections[0].biome(0, 0, 0), Some(RuntimeBiomeId(4)));
    }

    #[test]
    fn both_v775_live_block_update_families_become_semantic_updates() {
        let ChunkAdaptation::Blocks(single) =
            adapt_chunk_packet(v775::PlayClientbound::BlockUpdate(v775::BlockUpdate {
                x: -17,
                y: 63,
                z: 32,
                state_id: 91,
            }))
        else {
            panic!("expected semantic block update")
        };
        assert_eq!(
            single,
            vec![BlockStateUpdate {
                position: BlockCoordinates {
                    x: -17,
                    y: 63,
                    z: 32
                },
                state: RuntimeBlockStateId(91)
            }]
        );

        let ChunkAdaptation::Blocks(section) = adapt_chunk_packet(
            v775::PlayClientbound::SectionBlocksUpdate(v775::SectionBlocksUpdate {
                section_x: -2,
                section_y: -4,
                section_z: 3,
                updates: vec![
                    v775::SectionBlockUpdate {
                        local_x: 15,
                        local_y: 0,
                        local_z: 1,
                        state_id: 1,
                    },
                    v775::SectionBlockUpdate {
                        local_x: 0,
                        local_y: 15,
                        local_z: 14,
                        state_id: 2,
                    },
                ],
            }),
        ) else {
            panic!("expected semantic section updates")
        };
        assert_eq!(
            section[0].position,
            BlockCoordinates {
                x: -17,
                y: -64,
                z: 49
            }
        );
        assert_eq!(
            section[1].position,
            BlockCoordinates {
                x: -32,
                y: -49,
                z: 62
            }
        );
        assert_eq!(section[1].state, RuntimeBlockStateId(2));
    }

    #[test]
    fn position_and_weather_packets_map_without_packet_ids_in_world_state() {
        let position = v775::PlayClientbound::PlayerPosition(PlayerPosition {
            teleport_id: 7,
            x: 1.0,
            y: 2.0,
            z: 3.0,
            delta_x: 0.0,
            delta_y: 0.0,
            delta_z: 0.0,
            yaw: 4.0,
            pitch: 5.0,
            relative_flags: 0x09,
        });
        let Some(WorldEvent::SynchronizePlayerPosition(update)) =
            play_world_event(&position).unwrap()
        else {
            panic!("expected position event")
        };
        assert!(update.relative.x);
        assert!(update.relative.yaw);
        assert!(!update.relative.pitch);

        assert_eq!(
            play_world_event(&v775::PlayClientbound::GameEvent {
                event: 1,
                value: 0.0,
            })
            .unwrap(),
            Some(WorldEvent::SetRaining(true))
        );
    }

    #[test]
    fn respawn_keep_data_controls_the_version_specific_rotation_baseline() {
        let preserving = v775::PlayClientbound::Respawn(WireRespawn {
            spawn: spawn("custom:moon"),
            data_to_keep: KEEP_ENTITY_DATA,
        });
        let Some(WorldEvent::Respawn(preserving)) = play_world_event(&preserving).unwrap() else {
            panic!("expected Respawn")
        };
        assert_eq!(preserving.rotation, RespawnRotation::Preserve);

        let resetting = v775::PlayClientbound::Respawn(WireRespawn {
            spawn: spawn("custom:moon"),
            data_to_keep: 0,
        });
        let Some(WorldEvent::Respawn(resetting)) = play_world_event(&resetting).unwrap() else {
            panic!("expected Respawn")
        };
        assert_eq!(
            resetting.rotation,
            RespawnRotation::Reset(AuthoritativeRotation {
                yaw: -180.0,
                pitch: 0.0,
            })
        );
    }
}
