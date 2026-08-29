use std::{collections::BTreeSet, sync::Arc};

use cubic_version::{GameData, MinecraftIdentifier, VersionError};

use crate::{
    AuthoritativeTransform, Chunk, ChunkCoordinate, DimensionGeometry, RuntimeBlockStateId,
};

/// Version-selected semantic classification needed by the Phase 15 diagnostic renderer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockVisualProfile {
    air_states: BTreeSet<RuntimeBlockStateId>,
}

impl BlockVisualProfile {
    pub fn from_game_data(data: &GameData) -> Result<Self, VersionError> {
        let mut air_states = BTreeSet::new();
        for name in ["minecraft:air", "minecraft:cave_air", "minecraft:void_air"] {
            let identifier = MinecraftIdentifier::new(name)?;
            if let Some(block) = data.block(&identifier) {
                air_states.extend(
                    block
                        .states
                        .iter()
                        .map(|state| RuntimeBlockStateId(state.state_id)),
                );
            }
        }
        Ok(Self { air_states })
    }

    #[must_use]
    pub fn from_air_states(states: impl IntoIterator<Item = RuntimeBlockStateId>) -> Self {
        Self {
            air_states: states.into_iter().collect(),
        }
    }

    #[must_use]
    pub fn is_air(&self, state: RuntimeBlockStateId) -> bool {
        self.air_states.contains(&state)
    }
}

#[derive(Clone, Debug)]
pub enum ChunkRenderDelta {
    Loaded(Arc<Chunk>),
    Unloaded(ChunkCoordinate),
}

#[derive(Clone, Debug)]
pub struct WorldRenderUpdate {
    pub generation: u64,
    pub reset: bool,
    pub dimension: Option<String>,
    pub geometry: Option<DimensionGeometry>,
    pub pose: Option<AuthoritativeTransform>,
    pub chunks: Vec<ChunkRenderDelta>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn air_profile_is_explicit_and_unknown_states_remain_renderable() {
        let profile = BlockVisualProfile::from_air_states([
            RuntimeBlockStateId(0),
            RuntimeBlockStateId(17),
            RuntimeBlockStateId(23),
        ]);
        assert!(profile.is_air(RuntimeBlockStateId(17)));
        assert!(!profile.is_air(RuntimeBlockStateId(u32::MAX)));
    }
}
