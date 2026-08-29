use cubic_version::MinecraftIdentifier;
use cubic_world::{
    AuthoritativeRotation, BlockCoordinates, Chunk, ChunkCoordinate, ChunkLightSummary,
    ChunkSection, ClockState, Difficulty, DimensionTypeReference, EnterWorld, GameMode,
    MAX_KNOWN_DIMENSIONS, MAX_RUNTIME_REGISTRIES, MAX_RUNTIME_REGISTRY_ENTRIES, MAX_WORLD_CLOCKS,
    PalettedContainer, PlayerPositionUpdate, RelativeTransformFlags, ResetScope, Respawn,
    RespawnRotation, RuntimeBiomeId, RuntimeBlockStateId, RuntimeRegistrySnapshot,
    RuntimeRegistrySummary, SpawnContext, SpawnPoint, WorldBorder, WorldError, WorldEvent,
    WorldLifecycle, WorldState, WorldTime,
};

fn id(value: &str) -> MinecraftIdentifier {
    MinecraftIdentifier::new(value).unwrap()
}

fn spawn(dimension: &str) -> SpawnContext {
    SpawnContext {
        dimension_type: DimensionTypeReference {
            registry: id("minecraft:dimension_type"),
            raw_id: 7,
        },
        dimension: id(dimension),
        hashed_seed: 42,
        game_mode: GameMode::Survival,
        previous_game_mode: None,
        debug_world: false,
        flat_world: false,
        last_death_location: None,
        portal_cooldown_ticks: 0,
        sea_level: 63,
    }
}

fn enter(dimension: &str) -> EnterWorld {
    let mut known_dimensions = vec![id(dimension)];
    if dimension != "other:sky" {
        known_dimensions.push(id("other:sky"));
    }
    EnterWorld {
        player_entity_id: 19,
        hardcore: true,
        known_dimensions,
        max_players: 20,
        view_distance: 10,
        simulation_distance: 8,
        reduced_debug_info: false,
        show_death_screen: true,
        limited_crafting: false,
        secure_chat_enforced: true,
        spawn: spawn(dimension),
    }
}

fn active() -> WorldState {
    let mut state = WorldState::default();
    state.apply(WorldEvent::BeginConfiguration).unwrap();
    state
        .apply(WorldEvent::EnterWorld(enter("example:moon")))
        .unwrap();
    state
}

fn absolute_position(id: i32) -> PlayerPositionUpdate {
    PlayerPositionUpdate {
        teleport_id: id,
        x: 1.5,
        y: 70.0,
        z: -2.5,
        yaw: 90.0,
        pitch: -12.0,
        relative: RelativeTransformFlags::default(),
    }
}

fn sample_chunk() -> Chunk {
    Chunk {
        coordinate: ChunkCoordinate::new(-3, 4),
        sections: vec![ChunkSection {
            non_empty_block_count: 0,
            fluid_count: 0,
            blocks: PalettedContainer::Single {
                value: RuntimeBlockStateId(0),
                entries: 4_096,
            },
            biomes: PalettedContainer::Single {
                value: RuntimeBiomeId(1),
                entries: 64,
            },
        }],
        heightmaps: Vec::new(),
        block_entities: Vec::new(),
        light: ChunkLightSummary::default(),
    }
}

#[test]
fn world_contents_and_connection_resets_clear_loaded_chunks() {
    let mut state = active();
    state.apply(WorldEvent::LoadChunk(sample_chunk())).unwrap();
    assert_eq!(state.loaded_chunks().len(), 1);
    state
        .apply(WorldEvent::Respawn(Respawn {
            spawn: spawn("other:sky"),
            keep_attribute_modifiers: false,
            keep_entity_data: true,
            rotation: RespawnRotation::Preserve,
        }))
        .unwrap();
    assert!(state.loaded_chunks().is_empty());

    state.apply(WorldEvent::LoadChunk(sample_chunk())).unwrap();
    state.apply(WorldEvent::BeginConfiguration).unwrap();
    assert!(state.loaded_chunks().is_empty());
    state
        .apply(WorldEvent::EnterWorld(enter("example:moon")))
        .unwrap();
    state.apply(WorldEvent::LoadChunk(sample_chunk())).unwrap();
    state.apply(WorldEvent::Disconnect).unwrap();
    assert!(state.loaded_chunks().is_empty());
}

#[test]
fn disconnected_configuration_and_enter_world_are_explicit() {
    let mut state = WorldState::default();
    assert_eq!(state.lifecycle(), WorldLifecycle::Disconnected);
    assert!(matches!(
        state.apply(WorldEvent::EnterWorld(enter("example:moon"))),
        Err(WorldError::InvalidLifecycle { .. })
    ));
    let transition = state.apply(WorldEvent::BeginConfiguration).unwrap();
    assert_eq!(transition.reset, ResetScope::Connection);
    state
        .apply(WorldEvent::EnterWorld(enter("example:moon")))
        .unwrap();
    let session = state.session().unwrap();
    assert_eq!(state.lifecycle(), WorldLifecycle::Active);
    assert_eq!(session.player_entity_id, 19);
    assert!(session.hardcore);
    assert_eq!(session.spawn_context.dimension, id("example:moon"));
    assert_eq!(session.spawn_context.dimension_type.raw_id, 7);
    assert_eq!(session.spawn_context.game_mode, GameMode::Survival);
}

#[test]
fn arbitrary_namespaces_and_known_dimensions_are_deterministic() {
    let state = active();
    assert_eq!(
        state.session().unwrap().known_dimensions,
        vec![id("example:moon"), id("other:sky")]
    );
    assert!(MinecraftIdentifier::new("minecraft:../bad").is_err());
}

#[test]
fn known_dimensions_are_bounded_unique_and_include_current() {
    let mut state = WorldState::default();
    state.apply(WorldEvent::BeginConfiguration).unwrap();
    let mut duplicate = enter("example:moon");
    duplicate.known_dimensions.push(id("example:moon"));
    assert_eq!(
        state.apply(WorldEvent::EnterWorld(duplicate)),
        Err(WorldError::DuplicateDimension)
    );

    let mut state = WorldState::default();
    state.apply(WorldEvent::BeginConfiguration).unwrap();
    let mut missing = enter("example:moon");
    missing.known_dimensions = vec![id("other:sky")];
    assert_eq!(
        state.apply(WorldEvent::EnterWorld(missing)),
        Err(WorldError::CurrentDimensionUnknown)
    );

    let mut state = WorldState::default();
    state.apply(WorldEvent::BeginConfiguration).unwrap();
    let mut oversized = enter("example:moon");
    oversized.known_dimensions = (0..=MAX_KNOWN_DIMENSIONS)
        .map(|index| id(&format!("test:d{index}")))
        .collect();
    oversized.known_dimensions[0] = id("example:moon");
    assert!(matches!(
        state.apply(WorldEvent::EnterWorld(oversized)),
        Err(WorldError::TooManyDimensions { .. })
    ));
}

#[test]
fn authoritative_position_applies_absolute_and_relative_updates() {
    let mut state = active();
    state
        .apply(WorldEvent::SynchronizePlayerPosition(absolute_position(1)))
        .unwrap();
    state
        .apply(WorldEvent::SynchronizePlayerPosition(
            PlayerPositionUpdate {
                teleport_id: 2,
                x: 2.0,
                y: 3.0,
                z: 4.0,
                yaw: 5.0,
                pitch: 6.0,
                relative: RelativeTransformFlags {
                    x: true,
                    y: true,
                    z: true,
                    yaw: true,
                    pitch: true,
                },
            },
        ))
        .unwrap();
    let position = state.session().unwrap().position.unwrap();
    assert_eq!((position.x, position.y, position.z), (3.5, 73.0, 1.5));
    assert_eq!((position.yaw, position.pitch), (95.0, -6.0));
}

#[test]
fn malformed_stale_and_baseless_positions_are_rejected_without_mutation() {
    let mut state = active();
    let mut relative = absolute_position(1);
    relative.relative.x = true;
    assert!(matches!(
        state.apply(WorldEvent::SynchronizePlayerPosition(relative)),
        Err(WorldError::RelativeWithoutBaseline { field: "x" })
    ));
    let mut invalid = absolute_position(1);
    invalid.x = f64::NAN;
    assert!(matches!(
        state.apply(WorldEvent::SynchronizePlayerPosition(invalid)),
        Err(WorldError::InvalidNumber { field: "x" })
    ));
    state
        .apply(WorldEvent::SynchronizePlayerPosition(absolute_position(2)))
        .unwrap();
    assert!(matches!(
        state.apply(WorldEvent::SynchronizePlayerPosition(absolute_position(2))),
        Err(WorldError::StaleTeleport { .. })
    ));
    assert_eq!(state.session().unwrap().position.unwrap().teleport_id, 2);
}

#[test]
fn spawn_time_difficulty_weather_and_border_are_authoritative() {
    let mut state = active();
    state
        .apply(WorldEvent::SetSpawn(SpawnPoint {
            dimension: id("example:moon"),
            position: BlockCoordinates { x: 1, y: 80, z: 2 },
            yaw: 45.0,
            pitch: 0.0,
        }))
        .unwrap();
    state
        .apply(WorldEvent::SetTime(WorldTime {
            game_time: 12_345,
            clocks: vec![ClockState {
                clock_type_raw_id: 0,
                total_ticks: 6_000,
                partial_tick: 0.5,
                rate: 1.0,
            }],
        }))
        .unwrap();
    state
        .apply(WorldEvent::SetDifficulty {
            difficulty: Difficulty::Hard,
            locked: true,
        })
        .unwrap();
    state.apply(WorldEvent::SetRaining(true)).unwrap();
    state.apply(WorldEvent::SetRainLevel(0.75)).unwrap();
    state.apply(WorldEvent::SetThunderLevel(0.25)).unwrap();
    state
        .apply(WorldEvent::SetWorldBorder(WorldBorder {
            center_x: 0.0,
            center_z: 0.0,
            old_diameter: 1_000.0,
            new_diameter: 500.0,
            lerp_millis: 10_000,
            absolute_max_size: 29_999_984,
            warning_blocks: 5,
            warning_seconds: 15,
        }))
        .unwrap();
    let session = state.session().unwrap();
    assert_eq!(session.time.as_ref().unwrap().game_time, 12_345);
    assert_eq!(session.difficulty, Some((Difficulty::Hard, true)));
    assert_eq!(session.weather.rain_level, 0.75);
    assert!(session.border.is_some());
}

#[test]
fn respawn_same_dimension_invalidates_position_but_preserves_world_metadata() {
    let mut state = active();
    state
        .apply(WorldEvent::SynchronizePlayerPosition(absolute_position(1)))
        .unwrap();
    state
        .apply(WorldEvent::SetTime(WorldTime {
            game_time: 1,
            clocks: Vec::new(),
        }))
        .unwrap();
    let transition = state
        .apply(WorldEvent::Respawn(Respawn {
            spawn: spawn("example:moon"),
            keep_attribute_modifiers: true,
            keep_entity_data: true,
            rotation: RespawnRotation::Preserve,
        }))
        .unwrap();
    assert_eq!(transition.reset, ResetScope::WorldContents);
    assert!(!transition.dimension_changed);
    assert!(state.session().unwrap().position.is_none());
    assert!(state.session().unwrap().time.is_some());
}

#[test]
fn dimension_transition_clears_dimension_scoped_state_and_exposes_reset_hook() {
    let mut state = active();
    state
        .apply(WorldEvent::SynchronizePlayerPosition(absolute_position(1)))
        .unwrap();
    state.apply(WorldEvent::SetRaining(true)).unwrap();
    let transition = state
        .apply(WorldEvent::Respawn(Respawn {
            spawn: spawn("other:sky"),
            keep_attribute_modifiers: false,
            keep_entity_data: false,
            rotation: RespawnRotation::Reset(AuthoritativeRotation {
                yaw: -180.0,
                pitch: 0.0,
            }),
        }))
        .unwrap();
    assert!(transition.dimension_changed);
    assert_eq!(transition.reset, ResetScope::WorldContents);
    let session = state.session().unwrap();
    assert_eq!(session.spawn_context.dimension, id("other:sky"));
    assert!(session.position.is_none());
    assert!(session.spawn_point.is_none());
    assert!(!session.weather.raining);

    state
        .apply(WorldEvent::SynchronizePlayerPosition(
            PlayerPositionUpdate {
                teleport_id: 2,
                x: 8.0,
                y: 75.0,
                z: -4.0,
                yaw: 5.0,
                pitch: 6.0,
                relative: RelativeTransformFlags {
                    yaw: true,
                    pitch: true,
                    ..Default::default()
                },
            },
        ))
        .unwrap();
    let position = state.session().unwrap().position.unwrap();
    assert_eq!((position.yaw, position.pitch), (-175.0, 6.0));
}

#[test]
fn dimension_respawn_preserves_rotation_baseline_for_relative_sync() {
    let mut state = active();
    state
        .apply(WorldEvent::SynchronizePlayerPosition(absolute_position(1)))
        .unwrap();
    state
        .apply(WorldEvent::Respawn(Respawn {
            spawn: spawn("other:sky"),
            keep_attribute_modifiers: true,
            keep_entity_data: true,
            rotation: RespawnRotation::Preserve,
        }))
        .unwrap();

    let session = state.session().unwrap();
    assert!(session.position.is_none());
    assert_eq!(
        session.rotation,
        Some(AuthoritativeRotation {
            yaw: 90.0,
            pitch: -12.0,
        })
    );

    state
        .apply(WorldEvent::SynchronizePlayerPosition(
            PlayerPositionUpdate {
                teleport_id: 2,
                x: 8.0,
                y: 75.0,
                z: -4.0,
                yaw: 5.0,
                pitch: 6.0,
                relative: RelativeTransformFlags {
                    yaw: true,
                    pitch: true,
                    ..Default::default()
                },
            },
        ))
        .unwrap();

    let position = state.session().unwrap().position.unwrap();
    assert_eq!((position.x, position.y, position.z), (8.0, 75.0, -4.0));
    assert_eq!((position.yaw, position.pitch), (95.0, -6.0));
}

#[test]
fn disconnect_and_new_configuration_never_reuse_old_session_state() {
    let mut state = active();
    state
        .apply(WorldEvent::SynchronizePlayerPosition(absolute_position(1)))
        .unwrap();
    state.apply(WorldEvent::Disconnect).unwrap();
    assert_eq!(state.lifecycle(), WorldLifecycle::Disconnected);
    assert!(state.session().is_none());
    state.apply(WorldEvent::BeginConfiguration).unwrap();
    state
        .apply(WorldEvent::EnterWorld(enter("other:sky")))
        .unwrap();
    assert!(state.session().unwrap().position.is_none());
}

#[test]
fn runtime_registry_boundary_is_bounded_sorted_and_server_owned() {
    let mut state = WorldState::default();
    state.apply(WorldEvent::BeginConfiguration).unwrap();
    state
        .apply(WorldEvent::RuntimeRegistries(RuntimeRegistrySnapshot {
            registries: vec![
                RuntimeRegistrySummary {
                    registry: id("other:custom"),
                    entry_count: 3,
                },
                RuntimeRegistrySummary {
                    registry: id("minecraft:dimension_type"),
                    entry_count: 4,
                },
            ],
        }))
        .unwrap();
    assert_eq!(
        state.runtime_registries().registries[0].registry,
        id("minecraft:dimension_type")
    );

    let mut duplicate = WorldState::default();
    duplicate.apply(WorldEvent::BeginConfiguration).unwrap();
    assert_eq!(
        duplicate.apply(WorldEvent::RuntimeRegistries(RuntimeRegistrySnapshot {
            registries: vec![
                RuntimeRegistrySummary {
                    registry: id("other:custom"),
                    entry_count: 1,
                },
                RuntimeRegistrySummary {
                    registry: id("other:custom"),
                    entry_count: 2,
                },
            ],
        })),
        Err(WorldError::DuplicateRegistry)
    );
}

#[test]
fn registry_and_clock_caps_reject_pathological_inputs() {
    let mut state = WorldState::default();
    state.apply(WorldEvent::BeginConfiguration).unwrap();
    let registries = (0..=MAX_RUNTIME_REGISTRIES)
        .map(|index| RuntimeRegistrySummary {
            registry: id(&format!("test:r{index}")),
            entry_count: 0,
        })
        .collect();
    assert!(matches!(
        state.apply(WorldEvent::RuntimeRegistries(RuntimeRegistrySnapshot {
            registries
        })),
        Err(WorldError::TooManyRegistries { .. })
    ));

    let mut state = WorldState::default();
    state.apply(WorldEvent::BeginConfiguration).unwrap();
    assert!(matches!(
        state.apply(WorldEvent::RuntimeRegistries(RuntimeRegistrySnapshot {
            registries: vec![RuntimeRegistrySummary {
                registry: id("test:large"),
                entry_count: MAX_RUNTIME_REGISTRY_ENTRIES + 1,
            }],
        })),
        Err(WorldError::TooManyRegistryEntries { .. })
    ));

    let mut state = active();
    let clocks = (0..=MAX_WORLD_CLOCKS)
        .map(|index| ClockState {
            clock_type_raw_id: index as u32,
            total_ticks: 0,
            partial_tick: 0.0,
            rate: 1.0,
        })
        .collect();
    assert!(matches!(
        state.apply(WorldEvent::SetTime(WorldTime {
            game_time: 0,
            clocks,
        })),
        Err(WorldError::TooManyRegistryEntries { .. })
    ));
}

#[test]
fn unknown_semantic_game_modes_remain_forward_compatible() {
    let mut state = active();
    state
        .apply(WorldEvent::SetGameMode(GameMode::Other(99)))
        .unwrap();
    assert_eq!(
        state.session().unwrap().spawn_context.game_mode,
        GameMode::Other(99)
    );
    assert!(state.summary().contains("dimension=example:moon"));
}
