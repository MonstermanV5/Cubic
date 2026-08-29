//! Translation from the temporary protocol-775 wire profile to stable world events.

use cubic_protocol::bootstrap::v775::{
    self, DefaultSpawnPosition, GlobalPosition, InitialPlayLogin, InitializeBorder, PlayerPosition,
    Respawn as WireRespawn, SpawnInfo, WorldTime as WireWorldTime,
};
use cubic_version::MinecraftIdentifier;
use cubic_world::{
    AuthoritativeRotation, BlockCoordinates, ClockState, Difficulty, DimensionTypeReference,
    EnterWorld, GameMode, LastDeathLocation, PlayerPositionUpdate, RelativeTransformFlags, Respawn,
    RespawnRotation, SpawnContext, SpawnPoint, WorldBorder, WorldEvent, WorldTime,
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
