use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use cubic_world::{
    Chunk, ChunkCoordinate, ChunkRenderDelta, DimensionGeometry, LocalPlayerPose, RenderLookSample,
    RenderPoseSample, WorldRenderUpdate,
};

#[derive(Default)]
struct Mailbox {
    generation: u64,
    dirty: bool,
    reset: bool,
    dimension: Option<String>,
    geometry: Option<DimensionGeometry>,
    biomes: Option<Arc<[cubic_world::RuntimeBiome]>>,
    pose: Option<RenderPoseSample>,
    pose_published_at: Option<Instant>,
    pose_contains_jump: bool,
    chunks: BTreeMap<ChunkCoordinate, ChunkRenderDelta>,
    waker: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// Render-thread endpoint for a coalescing, bounded world-delta mailbox.
pub struct WorldRenderHandle(Arc<Mutex<Mailbox>>);

/// Network-thread endpoint. Lock hold times contain no I/O and no meshing work.
#[derive(Clone)]
pub struct WorldRenderRunner(Arc<Mutex<Mailbox>>);

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct RenderPublishTiming {
    pub lock_wait: Duration,
    pub bookkeeping: Duration,
}

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
        let generation = mailbox.generation;
        let reset = std::mem::take(&mut mailbox.reset);
        let dimension = mailbox.dimension.clone();
        let geometry = mailbox.geometry;
        let biomes = mailbox.biomes.clone();
        let pose = mailbox.pose.take();
        let pose_published_at = mailbox.pose_published_at.take();
        let pose_contains_jump = std::mem::take(&mut mailbox.pose_contains_jump);
        let chunks = std::mem::take(&mut mailbox.chunks);
        drop(mailbox);
        Some(WorldRenderUpdate {
            generation,
            reset,
            dimension,
            geometry,
            biomes,
            pose,
            pose_published_at,
            pose_contains_jump,
            chunks: chunks.into_values().collect(),
        })
    }

    /// Connects latency-sensitive pose publication to the native event loop.
    /// Chunk deltas remain pull-driven so bulk terrain streaming cannot flood
    /// the platform event queue.
    pub fn set_waker(&self, waker: Arc<dyn Fn() + Send + Sync>) {
        if let Ok(mut mailbox) = self.0.lock() {
            mailbox.waker = Some(waker);
        }
    }
}

impl WorldRenderRunner {
    pub fn reset(
        &self,
        dimension: String,
        geometry: DimensionGeometry,
        biomes: Arc<[cubic_world::RuntimeBiome]>,
    ) {
        let waker = if let Ok(mut mailbox) = self.0.lock() {
            mailbox.generation = mailbox.generation.wrapping_add(1);
            mailbox.reset = true;
            mailbox.dimension = Some(dimension);
            mailbox.geometry = Some(geometry);
            mailbox.biomes = Some(biomes);
            mailbox.pose = None;
            mailbox.pose_published_at = None;
            mailbox.pose_contains_jump = false;
            mailbox.chunks.clear();
            mailbox.dirty = true;
            mailbox.waker.clone()
        } else {
            None
        };
        if let Some(waker) = waker {
            waker();
        }
    }

    pub fn pose(&self, pose: LocalPlayerPose) {
        self.pose_discontinuity(pose, Instant::now(), RenderLookSample::default());
    }

    pub fn pose_tick(
        &self,
        pose: LocalPlayerPose,
        tick_at: Instant,
        look: RenderLookSample,
        jumped: bool,
    ) {
        self.set_pose(
            RenderPoseSample {
                pose,
                tick_at,
                look,
                discontinuity: false,
            },
            jumped,
        );
    }

    pub fn pose_discontinuity(
        &self,
        pose: LocalPlayerPose,
        tick_at: Instant,
        look: RenderLookSample,
    ) {
        self.set_pose(
            RenderPoseSample {
                pose,
                tick_at,
                look,
                discontinuity: true,
            },
            false,
        );
    }

    fn set_pose(&self, mut pose: RenderPoseSample, jumped: bool) {
        let published_at = Instant::now();
        let waker = if let Ok(mut mailbox) = self.0.lock() {
            pose.discontinuity |= mailbox.pose.is_some_and(|pending| pending.discontinuity);
            mailbox.pose = Some(pose);
            if jumped || !mailbox.pose_contains_jump {
                mailbox.pose_published_at = Some(published_at);
            }
            mailbox.pose_contains_jump |= jumped;
            mailbox.dirty = true;
            mailbox.waker.clone()
        } else {
            None
        };
        if let Some(waker) = waker {
            waker();
        }
        if jumped {
            tracing::debug!(target: "movement::latency", ?published_at, "published predicted jump pose to render mailbox");
        }
    }

    pub(crate) fn load(&self, chunk: Arc<Chunk>) -> RenderPublishTiming {
        let started = Instant::now();
        if let Ok(mut mailbox) = self.0.lock() {
            let lock_wait = started.elapsed();
            let bookkeeping_started = Instant::now();
            mailbox
                .chunks
                .insert(chunk.coordinate, ChunkRenderDelta::Loaded(chunk));
            mailbox.dirty = true;
            return RenderPublishTiming {
                lock_wait,
                bookkeeping: bookkeeping_started.elapsed(),
            };
        }
        RenderPublishTiming {
            lock_wait: started.elapsed(),
            bookkeeping: Duration::ZERO,
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
    use std::sync::atomic::{AtomicUsize, Ordering};

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
            Arc::from([]),
        );
        let coordinate = ChunkCoordinate::new(2, -3);
        runner.load(chunk(coordinate, 1));
        let latest = chunk(coordinate, 2);
        runner.load(Arc::clone(&latest));
        let update = handle.take_update().unwrap();
        assert!(update.reset);
        assert_eq!(update.chunks.len(), 1);
        let ChunkRenderDelta::Loaded(chunk) = &update.chunks[0] else {
            panic!("expected load")
        };
        assert!(Arc::ptr_eq(chunk, &latest));
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
        runner.pose(LocalPlayerPose {
            x: 1.0,
            y: 2.0,
            z: 3.0,
            yaw: 4.0,
            pitch: 5.0,
            eye_height: 1.62,
        });
        runner.reset(
            "test:new".to_owned(),
            DimensionGeometry {
                min_y: -64,
                height: 384,
            },
            Arc::from([]),
        );
        let update = handle.take_update().unwrap();
        assert!(update.reset);
        assert!(update.pose.is_none());
        assert!(update.chunks.is_empty());
    }

    #[test]
    fn pose_is_a_consumed_delta_not_republished_by_unrelated_chunk_updates() {
        let (mut handle, runner) = WorldRenderHandle::new();
        let pose = LocalPlayerPose::new(4.0, 65.0, -2.0, 90.0, 10.0);
        runner.pose(pose);
        assert_eq!(
            handle.take_update().unwrap().pose.map(|sample| sample.pose),
            Some(pose)
        );

        runner.load(chunk(ChunkCoordinate::new(0, 0), 1));
        assert!(handle.take_update().unwrap().pose.is_none());
    }

    #[test]
    fn correction_discontinuity_survives_pose_coalescing() {
        let (mut handle, runner) = WorldRenderHandle::new();
        let corrected = LocalPlayerPose::new(100.0, 70.0, -20.0, 45.0, 5.0);
        let look = RenderLookSample::default();
        runner.pose_discontinuity(corrected, Instant::now(), look);
        runner.pose_tick(
            LocalPlayerPose::new(100.1, 70.0, -20.0, 45.0, 5.0),
            Instant::now(),
            look,
            false,
        );
        assert!(handle.take_update().unwrap().pose.unwrap().discontinuity);
    }

    #[test]
    fn pose_publication_wakes_the_platform_without_waking_for_chunk_floods() {
        let (mut handle, runner) = WorldRenderHandle::new();
        let wakes = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&wakes);
        handle.set_waker(Arc::new(move || {
            observed.fetch_add(1, Ordering::Relaxed);
        }));

        runner.load(chunk(ChunkCoordinate::new(0, 0), 1));
        runner.load(chunk(ChunkCoordinate::new(1, 0), 1));
        assert_eq!(wakes.load(Ordering::Relaxed), 0);

        runner.pose_tick(
            LocalPlayerPose::new(0.0, 64.42, 0.0, 0.0, 0.0),
            Instant::now(),
            RenderLookSample::default(),
            true,
        );
        assert_eq!(wakes.load(Ordering::Relaxed), 1);
        let update = handle.take_update().unwrap();
        assert!(update.pose_contains_jump);

        runner.reset(
            "test:reset".to_owned(),
            DimensionGeometry {
                min_y: 0,
                height: 16,
            },
            Arc::from([]),
        );
        assert_eq!(wakes.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn jump_timing_marker_survives_pose_coalescing_until_render_observes_it() {
        let (mut handle, runner) = WorldRenderHandle::new();
        let look = RenderLookSample::default();
        runner.pose_tick(
            LocalPlayerPose::new(0.0, 64.42, 0.0, 0.0, 0.0),
            Instant::now(),
            look,
            true,
        );
        let newest = LocalPlayerPose::new(0.0, 64.75, 0.0, 0.0, 0.0);
        runner.pose_tick(newest, Instant::now(), look, false);

        let update = handle.take_update().unwrap();
        assert!(update.pose_contains_jump);
        assert_eq!(update.pose.map(|sample| sample.pose), Some(newest));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stalled_renderer_does_not_backpressure_ticks_and_latest_revision_wins() {
        let (mut handle, runner) = WorldRenderHandle::new();
        let coordinate = ChunkCoordinate::new(4, -7);
        let started = tokio::time::Instant::now();
        let mut ticks = tokio::time::interval(Duration::from_millis(50));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut serviced = 0_u32;
        let revisions = [chunk(coordinate, 1), chunk(coordinate, 2)];
        let mut latest = Arc::clone(&revisions[0]);
        let mut maximum_lateness = Duration::ZERO;

        // Model a renderer which consumes no updates for 500 ms. Authoritative
        // producers continue publishing replacements and the control timer is
        // serviced on the same executor throughout the stall.
        while started.elapsed() < Duration::from_millis(500) {
            let scheduled = ticks.tick().await;
            maximum_lateness = maximum_lateness
                .max(tokio::time::Instant::now().saturating_duration_since(scheduled));
            serviced += 1;
            for revision in 0..128_u32 {
                latest = Arc::clone(&revisions[(revision as usize) & 1]);
                let timing = runner.load(Arc::clone(&latest));
                assert!(timing.lock_wait + timing.bookkeeping < Duration::from_millis(25));
            }
        }
        assert!(serviced >= 10);
        assert!(maximum_lateness < Duration::from_millis(75));

        let update = handle.take_update().unwrap();
        assert_eq!(
            update.chunks.len(),
            1,
            "superseded render work must coalesce"
        );
        let ChunkRenderDelta::Loaded(published) = &update.chunks[0] else {
            panic!("expected latest loaded chunk")
        };
        assert!(Arc::ptr_eq(published, &latest));
        assert!(handle.take_update().is_none());
    }
}
