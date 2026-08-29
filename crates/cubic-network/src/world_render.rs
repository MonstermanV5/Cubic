use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use cubic_world::{
    AuthoritativeTransform, Chunk, ChunkCoordinate, ChunkRenderDelta, DimensionGeometry,
    WorldRenderUpdate,
};

#[derive(Default)]
struct Mailbox {
    generation: u64,
    dirty: bool,
    reset: bool,
    dimension: Option<String>,
    geometry: Option<DimensionGeometry>,
    pose: Option<AuthoritativeTransform>,
    chunks: BTreeMap<ChunkCoordinate, ChunkRenderDelta>,
}

/// Render-thread endpoint for a coalescing, bounded world-delta mailbox.
pub struct WorldRenderHandle(Arc<Mutex<Mailbox>>);

/// Network-thread endpoint. Lock hold times contain no I/O and no meshing work.
#[derive(Clone)]
pub struct WorldRenderRunner(Arc<Mutex<Mailbox>>);

impl WorldRenderHandle {
    #[must_use]
    pub fn new() -> (Self, WorldRenderRunner) {
        let mailbox = Arc::new(Mutex::new(Mailbox::default()));
        (Self(Arc::clone(&mailbox)), WorldRenderRunner(mailbox))
    }

    pub fn take_update(&mut self) -> Option<WorldRenderUpdate> {
        let mut mailbox = self.0.lock().ok()?;
        if !mailbox.dirty {
            return None;
        }
        mailbox.dirty = false;
        Some(WorldRenderUpdate {
            generation: mailbox.generation,
            reset: std::mem::take(&mut mailbox.reset),
            dimension: mailbox.dimension.clone(),
            geometry: mailbox.geometry,
            pose: mailbox.pose,
            chunks: std::mem::take(&mut mailbox.chunks).into_values().collect(),
        })
    }
}

impl WorldRenderRunner {
    pub fn reset(&self, dimension: String, geometry: DimensionGeometry) {
        if let Ok(mut mailbox) = self.0.lock() {
            mailbox.generation = mailbox.generation.wrapping_add(1);
            mailbox.reset = true;
            mailbox.dimension = Some(dimension);
            mailbox.geometry = Some(geometry);
            mailbox.pose = None;
            mailbox.chunks.clear();
            mailbox.dirty = true;
        }
    }

    pub fn pose(&self, pose: AuthoritativeTransform) {
        if let Ok(mut mailbox) = self.0.lock() {
            mailbox.pose = Some(pose);
            mailbox.dirty = true;
        }
    }

    pub fn load(&self, chunk: Arc<Chunk>) {
        if let Ok(mut mailbox) = self.0.lock() {
            mailbox
                .chunks
                .insert(chunk.coordinate, ChunkRenderDelta::Loaded(chunk));
            mailbox.dirty = true;
        }
    }

    pub fn unload(&self, coordinate: ChunkCoordinate) {
        if let Ok(mut mailbox) = self.0.lock() {
            mailbox
                .chunks
                .insert(coordinate, ChunkRenderDelta::Unloaded(coordinate));
            mailbox.dirty = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cubic_world::{
        ChunkLightSummary, ChunkSection, PalettedContainer, RuntimeBiomeId, RuntimeBlockStateId,
    };

    fn chunk(coordinate: ChunkCoordinate, state: u32) -> Arc<Chunk> {
        Arc::new(Chunk {
            coordinate,
            sections: vec![ChunkSection {
                non_empty_block_count: 1,
                fluid_count: 0,
                blocks: PalettedContainer::Single {
                    value: RuntimeBlockStateId(state),
                    entries: 4096,
                },
                biomes: PalettedContainer::Single {
                    value: RuntimeBiomeId(0),
                    entries: 64,
                },
            }],
            heightmaps: vec![],
            block_entities: vec![],
            light: ChunkLightSummary::default(),
        })
    }

    #[test]
    fn mailbox_coalesces_chunk_replacements_without_cloning_the_world() {
        let (mut handle, runner) = WorldRenderHandle::new();
        runner.reset(
            "test:world".to_owned(),
            DimensionGeometry {
                min_y: 0,
                height: 16,
            },
        );
        let coordinate = ChunkCoordinate::new(2, -3);
        runner.load(chunk(coordinate, 1));
        runner.load(chunk(coordinate, 2));
        let update = handle.take_update().unwrap();
        assert!(update.reset);
        assert_eq!(update.chunks.len(), 1);
        let ChunkRenderDelta::Loaded(chunk) = &update.chunks[0] else {
            panic!("expected load")
        };
        assert_eq!(
            chunk.sections[0].block(0, 0, 0),
            Some(RuntimeBlockStateId(2))
        );
        assert!(handle.take_update().is_none());
    }

    #[test]
    fn reset_discards_stale_chunk_deltas_and_pose() {
        let (mut handle, runner) = WorldRenderHandle::new();
        runner.load(chunk(ChunkCoordinate::new(0, 0), 1));
        runner.pose(AuthoritativeTransform {
            x: 1.0,
            y: 2.0,
            z: 3.0,
            yaw: 4.0,
            pitch: 5.0,
            teleport_id: 1,
        });
        runner.reset(
            "test:new".to_owned(),
            DimensionGeometry {
                min_y: -64,
                height: 384,
            },
        );
        let update = handle.take_update().unwrap();
        assert!(update.reset);
        assert!(update.pose.is_none());
        assert!(update.chunks.is_empty());
    }
}
