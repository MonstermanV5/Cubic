use std::{collections::BTreeSet, sync::Arc, time::Instant};

use cubic_version::{GameData, MinecraftIdentifier, VersionError};

use crate::{
    Chunk, ChunkCoordinate, DimensionGeometry, LocalPlayerPose, RuntimeBiome, RuntimeBlockStateId,
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

/// Cumulative render-side acknowledgement for event-driven mouse look.
/// Totals make preview rebasing constant-size instead of retaining raw events.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RenderLookSample {
    pub sequence: u64,
    pub yaw_total: f64,
    pub pitch_total: f64,
}

/// One fixed-tick pose sample for display-rate presentation.
#[derive(Clone, Copy, Debug)]
pub struct RenderPoseSample {
    pub pose: LocalPlayerPose,
    pub tick_at: Instant,
    pub look: RenderLookSample,
    /// Corrections and world changes snap instead of interpolating through
    /// authoritative geometry.
    pub discontinuity: bool,
}

#[derive(Clone, Debug)]
pub struct WorldRenderUpdate {
    pub generation: u64,
    pub reset: bool,
    pub dimension: Option<String>,
    pub geometry: Option<DimensionGeometry>,
    pub biomes: Option<Arc<[RuntimeBiome]>>,
    pub pose: Option<RenderPoseSample>,
    /// Publication time used only for bounded input-to-frame diagnostics.
    pub pose_published_at: Option<Instant>,
    /// Coalesced low-frequency marker for a grounded jump pose.
    pub pose_contains_jump: bool,
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
