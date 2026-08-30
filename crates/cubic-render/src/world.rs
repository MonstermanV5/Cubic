use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use bytemuck::{Pod, Zeroable};
use cubic_world::{
    Chunk, ChunkCoordinate, ChunkRenderDelta, DimensionGeometry, LocalPlayerPose, RenderLookSample,
    RenderPoseSample, WorldRenderUpdate,
};
use wgpu::{
    util::{BufferInitDescriptor, DeviceExt},
    *,
};

use crate::{
    BlockResources,
    mesher::{ChunkMesh, MeshStatistics, TerrainVertex, mesh_chunk},
};

const DEPTH_FORMAT: TextureFormat = TextureFormat::Depth32Float;
const MAX_PENDING_MESH_JOBS: usize = 32;
const MAX_MESH_RESULTS_PER_PREPARE: usize = 16;
const MAX_MESH_DISPATCHES_PER_PREPARE: usize = 8;
const MESH_INTEGRATION_TIME_BUDGET: Duration = Duration::from_millis(4);
const _: () = assert!(MAX_MESH_RESULTS_PER_PREPARE < MAX_PENDING_MESH_JOBS);
const SIMULATION_TICK: Duration = Duration::from_millis(50);
const TERRAIN_CULL_MODE: Option<Face> = Some(Face::Back);

pub(crate) struct WorldRenderer {
    pipeline: RenderPipeline,
    camera_buffer: Buffer,
    camera_bind_group: BindGroup,
    atlas_bind_group: BindGroup,
    _atlas_texture: Texture,
    depth: DepthTarget,
    meshes: BTreeMap<ChunkCoordinate, GpuChunkMesh>,
    chunks: BTreeMap<ChunkCoordinate, Arc<Chunk>>,
    dirty: BTreeMap<ChunkCoordinate, u64>,
    pending: BTreeSet<ChunkCoordinate>,
    generation: u64,
    revision: u64,
    geometry: Option<DimensionGeometry>,
    dimension: Option<String>,
    pose: Option<PosePresentation>,
    worker: Option<MeshWorker>,
    progress: MeshProgress,
}

impl WorldRenderer {
    pub(crate) fn new(
        device: &Device,
        queue: &Queue,
        surface_format: TextureFormat,
        width: u32,
        height: u32,
        mut resources: BlockResources,
    ) -> Self {
        let shader = device.create_shader_module(ShaderModuleDescriptor {
            label: Some("Cubic diagnostic terrain shader"),
            source: ShaderSource::Wgsl(include_str!("world.wgsl").into()),
        });
        let camera_layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
            label: Some("Cubic world camera layout"),
            entries: &[BindGroupLayoutEntry {
                binding: 0,
                visibility: ShaderStages::VERTEX,
                ty: BindingType::Buffer {
                    ty: BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let camera_buffer = device.create_buffer_init(&BufferInitDescriptor {
            label: Some("Cubic world camera"),
            contents: bytemuck::bytes_of(&CameraUniform::identity()),
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
        });
        let camera_bind_group = device.create_bind_group(&BindGroupDescriptor {
            label: Some("Cubic world camera bind group"),
            layout: &camera_layout,
            entries: &[BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });
        let atlas_texture = device.create_texture(&TextureDescriptor {
            label: Some("Cubic vanilla block atlas"),
            size: Extent3d {
                width: resources.atlas.width,
                height: resources.atlas.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: TextureFormat::Rgba8UnormSrgb,
            usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            TexelCopyTextureInfo {
                texture: &atlas_texture,
                mip_level: 0,
                origin: Origin3d::ZERO,
                aspect: TextureAspect::All,
            },
            &resources.atlas.rgba,
            TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(resources.atlas.width * 4),
                rows_per_image: Some(resources.atlas.height),
            },
            Extent3d {
                width: resources.atlas.width,
                height: resources.atlas.height,
                depth_or_array_layers: 1,
            },
        );
        resources.atlas.rgba.clear();
        resources.atlas.rgba.shrink_to_fit();
        let atlas_view = atlas_texture.create_view(&TextureViewDescriptor::default());
        let atlas_sampler = device.create_sampler(&SamplerDescriptor {
            label: Some("Cubic nearest block sampler"),
            mag_filter: FilterMode::Nearest,
            min_filter: FilterMode::Nearest,
            mipmap_filter: MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let atlas_layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
            label: Some("Cubic block atlas layout"),
            entries: &[
                BindGroupLayoutEntry {
                    binding: 0,
                    visibility: ShaderStages::FRAGMENT,
                    ty: BindingType::Texture {
                        sample_type: TextureSampleType::Float { filterable: true },
                        view_dimension: TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                BindGroupLayoutEntry {
                    binding: 1,
                    visibility: ShaderStages::FRAGMENT,
                    ty: BindingType::Sampler(SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let atlas_bind_group = device.create_bind_group(&BindGroupDescriptor {
            label: Some("Cubic block atlas bind group"),
            layout: &atlas_layout,
            entries: &[
                BindGroupEntry {
                    binding: 0,
                    resource: BindingResource::TextureView(&atlas_view),
                },
                BindGroupEntry {
                    binding: 1,
                    resource: BindingResource::Sampler(&atlas_sampler),
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
            label: Some("Cubic world pipeline layout"),
            bind_group_layouts: &[Some(&camera_layout), Some(&atlas_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("Cubic diagnostic terrain pipeline"),
            layout: Some(&pipeline_layout),
            vertex: VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: PipelineCompilationOptions::default(),
                buffers: &[Some(VertexBufferLayout {
                    array_stride: size_of::<TerrainVertex>() as BufferAddress,
                    step_mode: VertexStepMode::Vertex,
                    attributes: &vertex_attr_array![0 => Float32x3, 1 => Float32x2, 2 => Float32x3, 3 => Uint32],
                })],
            },
            fragment: Some(FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: PipelineCompilationOptions::default(),
                targets: &[Some(ColorTargetState {
                    format: surface_format,
                    blend: Some(BlendState::REPLACE),
                    write_mask: ColorWrites::ALL,
                })],
            }),
            primitive: PrimitiveState {
                topology: PrimitiveTopology::TriangleList,
                // Minecraft zero-thickness cross models deliberately contain
                // opposite-winding faces on the same plane. Back-face culling
                // selects exactly one for a view instead of depth-testing two
                // coplanar fragments against each other.
                cull_mode: TERRAIN_CULL_MODE,
                front_face: FrontFace::Ccw,
                ..Default::default()
            },
            depth_stencil: Some(DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(CompareFunction::Less),
                stencil: StencilState::default(),
                bias: DepthBiasState::default(),
            }),
            multisample: MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        Self {
            pipeline,
            camera_buffer,
            camera_bind_group,
            atlas_bind_group,
            _atlas_texture: atlas_texture,
            depth: DepthTarget::new(device, width, height),
            meshes: BTreeMap::new(),
            chunks: BTreeMap::new(),
            dirty: BTreeMap::new(),
            pending: BTreeSet::new(),
            generation: 0,
            revision: 0,
            geometry: None,
            dimension: None,
            pose: None,
            worker: MeshWorker::new(resources, device.clone()),
            progress: MeshProgress::new(),
        }
    }

    pub(crate) fn resize(&mut self, device: &Device, width: u32, height: u32) {
        self.depth = DepthTarget::new(device, width, height);
    }

    pub(crate) fn apply(&mut self, update: WorldRenderUpdate) {
        if update.reset || update.generation != self.generation {
            self.log_mesh_progress(true);
            tracing::info!(
                generation = update.generation,
                dimension = update.dimension.as_deref().unwrap_or("unknown"),
                geometry = ?update.geometry,
                "reset diagnostic world renderer"
            );
            self.generation = update.generation;
            self.revision = self.revision.wrapping_add(1);
            self.chunks.clear();
            self.meshes.clear();
            self.dirty.clear();
            self.pending.clear();
            self.pose = None;
            self.progress = MeshProgress::new();
        }
        self.dimension = update.dimension.or_else(|| self.dimension.take());
        self.geometry = update.geometry.or(self.geometry);
        if let Some(sample) = update.pose {
            if let Some(presentation) = &mut self.pose {
                presentation.apply(sample);
            } else {
                self.pose = Some(PosePresentation::new(sample));
            }
        }
        for delta in update.chunks {
            match delta {
                ChunkRenderDelta::Loaded(chunk) => {
                    let coordinate = chunk.coordinate;
                    self.chunks.insert(coordinate, chunk);
                    self.mark_with_neighbors(coordinate);
                }
                ChunkRenderDelta::Unloaded(coordinate) => {
                    self.chunks.remove(&coordinate);
                    self.meshes.remove(&coordinate);
                    self.mark_with_neighbors(coordinate);
                }
            }
        }
    }

    pub(crate) fn preview_look(&mut self, sequence: u64, yaw_delta: f32, pitch_delta: f32) {
        if let Some(pose) = &mut self.pose {
            pose.preview_look(sequence, yaw_delta, pitch_delta);
        }
    }

    fn mark_with_neighbors(&mut self, coordinate: ChunkCoordinate) {
        self.revision = self.revision.wrapping_add(1);
        mark_dirty_with_neighbors(&mut self.dirty, coordinate, self.revision);
    }

    pub(crate) fn prepare(&mut self, _device: &Device, queue: &Queue, width: u32, height: u32) {
        let started = Instant::now();
        let integration_started = Instant::now();
        let mut integrated = 0;
        while mesh_integration_permitted(integrated, started.elapsed()) {
            let Some(result) = self
                .worker
                .as_ref()
                .and_then(|worker| worker.results.try_recv().ok())
            else {
                break;
            };
            integrated += 1;
            self.progress.record(&result.mesh);
            self.pending.remove(&result.coordinate);
            if result.generation != self.generation
                || self.dirty.get(&result.coordinate) != Some(&result.revision)
            {
                continue;
            }
            self.dirty.remove(&result.coordinate);
            match result.mesh {
                Ok(PreparedChunkMesh::Empty { .. }) => {
                    self.meshes.remove(&result.coordinate);
                }
                Ok(PreparedChunkMesh::Ready { mesh, .. }) => {
                    self.meshes.insert(result.coordinate, mesh);
                }
                Err(error) => {
                    self.meshes.remove(&result.coordinate);
                    tracing::warn!(x = result.coordinate.x, z = result.coordinate.z, %error, "bounded chunk meshing failed");
                }
            }
        }
        let integration_elapsed = integration_started.elapsed();
        let dispatch_dirty_before = self.dirty.len();
        let dispatch_pending_before = self.pending.len();
        let dispatch_started = Instant::now();
        let dispatched = self.dispatch_jobs(started);
        let dispatch_elapsed = dispatch_started.elapsed();
        let progress_started = Instant::now();
        self.log_mesh_progress(false);
        let progress_elapsed = progress_started.elapsed();
        let camera_started = Instant::now();
        if let Some(pose) = self.pose.map(|pose| pose.display(Instant::now())) {
            let camera = CameraUniform::from_pose(pose, width, height);
            queue.write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&camera));
        }
        let camera_elapsed = camera_started.elapsed();
        let elapsed = started.elapsed();
        if elapsed > Duration::from_millis(50) {
            tracing::debug!(target: "movement::latency", ?elapsed, ?integration_elapsed, ?dispatch_elapsed, ?progress_elapsed, ?camera_elapsed, integrated, dispatched, dispatch_dirty_before, dispatch_pending_before, dispatch_capacity = MAX_PENDING_MESH_JOBS, time_budget_ms = MESH_INTEGRATION_TIME_BUDGET.as_millis(), "world render preparation delayed event-loop service");
        }
    }

    fn dispatch_jobs(&mut self, prepare_started: Instant) -> usize {
        let Some(geometry) = self.geometry else {
            return 0;
        };
        let Some(worker) = &mut self.worker else {
            return 0;
        };
        let priority_origin = self.pose.map(|pose| player_chunk(pose.current));
        let mut dispatched = 0;
        while mesh_dispatch_permitted(dispatched, prepare_started.elapsed()) {
            // `pending` accounts for every accepted worker job until its result
            // is integrated. Once that bounded capacity is occupied, scanning
            // the dirty map and constructing a neighbor snapshot can only end
            // in a failed `try_send`. Avoid doing that work on winit's event
            // thread while terrain streaming is already at capacity.
            if !mesh_dispatch_capacity_available(self.pending.len()) {
                break;
            }
            let Some((coordinate, revision)) =
                next_mesh_candidate(&self.dirty, &self.pending, priority_origin)
            else {
                break;
            };
            if !self.chunks.contains_key(&coordinate) {
                self.dirty.remove(&coordinate);
                self.meshes.remove(&coordinate);
                continue;
            }
            let local_chunks = neighbors_including(coordinate)
                .into_iter()
                .filter_map(|key| self.chunks.get(&key).map(|chunk| (key, Arc::clone(chunk))))
                .collect();
            let job = MeshJob {
                coordinate,
                chunks: local_chunks,
                geometry,
                generation: self.generation,
                revision,
            };
            if worker.try_send(job) {
                self.pending.insert(coordinate);
                dispatched += 1;
            } else {
                break;
            }
        }
        dispatched
    }

    pub(crate) fn has_pending_work(&self) -> bool {
        (self.worker.is_some() && (!self.dirty.is_empty() || !self.pending.is_empty()))
            || self
                .pose
                .is_some_and(|pose| pose.is_interpolating(Instant::now()))
    }

    pub(crate) fn draw<'a>(&'a self, pass: &mut RenderPass<'a>) {
        if self.pose.is_none() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.camera_bind_group, &[]);
        pass.set_bind_group(1, &self.atlas_bind_group, &[]);
        for mesh in self.meshes.values() {
            pass.set_vertex_buffer(0, mesh.vertices.slice(..));
            pass.set_index_buffer(mesh.indices.slice(..), IndexFormat::Uint32);
            pass.draw_indexed(0..mesh.index_count, 0, 0..1);
        }
    }

    pub(crate) fn depth_view(&self) -> &TextureView {
        &self.depth.view
    }

    pub(crate) fn stats(&self) -> super::WorldRenderStats {
        super::WorldRenderStats {
            dimension: self.dimension.clone(),
            geometry: self.geometry,
            pose: self.pose.map(|pose| pose.display(Instant::now())),
            loaded_chunks: self.chunks.len(),
            meshed_chunks: self.meshes.len(),
            pending_meshes: self.dirty.len(),
        }
    }

    fn log_mesh_progress(&mut self, final_summary: bool) {
        if final_summary && self.progress.completed == 0 && self.chunks.is_empty() {
            return;
        }
        let active = self.worker.as_ref().map_or(0, MeshWorker::active_jobs);
        let now = Instant::now();
        let (info_due, debug_due) = self.progress.report_due(now, final_summary);
        if !info_due && !debug_due {
            return;
        }
        let snapshot = self.progress.snapshot(
            self.chunks.len(),
            self.meshes.len(),
            self.dirty.len(),
            self.pending.len(),
            active,
            now,
        );
        if info_due {
            tracing::info!(
                loaded = snapshot.loaded,
                meshed = snapshot.meshed,
                pending = snapshot.pending,
                queued = snapshot.queued,
                active = snapshot.active,
                completed = snapshot.completed,
                meshes_per_sec = format_args!("{:.2}", snapshot.meshes_per_second),
                overall_meshes_per_sec = format_args!("{:.2}", snapshot.overall_meshes_per_second),
                average_mesh_ms = format_args!("{:.2}", snapshot.average_mesh_ms),
                maximum_mesh_ms = format_args!("{:.2}", snapshot.maximum_mesh_ms),
                final_summary,
                "world render mesh progress"
            );
            self.progress.last_info = now;
            self.progress.last_info_completed = self.progress.completed;
        }
        if debug_due || final_summary {
            tracing::debug!(
                loaded = snapshot.loaded,
                meshed = snapshot.meshed,
                pending = snapshot.pending,
                queued = snapshot.queued,
                active = snapshot.active,
                completed = snapshot.completed,
                positions_visited = snapshot.statistics.positions_visited,
                air_skipped = snapshot.statistics.air_skipped,
                non_air = snapshot.statistics.non_air_blocks,
                occluded_fast_rejected = snapshot.statistics.fully_occluded_fast_rejected,
                model_processed = snapshot.statistics.model_processed,
                geometry_emitting = snapshot.statistics.geometry_emitting,
                neighbor_checks = snapshot.statistics.neighbor_checks,
                model_selections = snapshot.statistics.model_selections,
                quads = snapshot.statistics.quads_emitted,
                "world render mesh detail"
            );
            self.progress.last_debug = now;
        }
    }
}

fn mesh_integration_permitted(completed: usize, elapsed: Duration) -> bool {
    completed < MAX_MESH_RESULTS_PER_PREPARE
        && (completed == 0 || elapsed < MESH_INTEGRATION_TIME_BUDGET)
}

fn mesh_dispatch_permitted(dispatched: usize, elapsed: Duration) -> bool {
    dispatched < MAX_MESH_DISPATCHES_PER_PREPARE && elapsed < MESH_INTEGRATION_TIME_BUDGET
}

fn mesh_dispatch_capacity_available(pending: usize) -> bool {
    pending < MAX_PENDING_MESH_JOBS
}

#[derive(Clone, Copy, Debug)]
struct PosePresentation {
    previous: LocalPlayerPose,
    current: LocalPlayerPose,
    tick_at: Instant,
    acknowledged_look: RenderLookSample,
    produced_look: RenderLookSample,
    display_yaw: f32,
    display_pitch: f32,
}

impl PosePresentation {
    fn new(sample: RenderPoseSample) -> Self {
        Self {
            previous: sample.pose,
            current: sample.pose,
            tick_at: sample.tick_at,
            acknowledged_look: sample.look,
            produced_look: sample.look,
            display_yaw: sample.pose.yaw.rem_euclid(360.0),
            display_pitch: sample.pose.pitch.clamp(-90.0, 90.0),
        }
    }

    fn apply(&mut self, sample: RenderPoseSample) {
        if sample.discontinuity {
            if sample.look.sequence > self.produced_look.sequence {
                self.produced_look = sample.look;
            }
            let pending_yaw = self.produced_look.yaw_total - sample.look.yaw_total;
            let pending_pitch = self.produced_look.pitch_total - sample.look.pitch_total;
            self.display_yaw = (sample.pose.yaw + pending_yaw as f32).rem_euclid(360.0);
            self.display_pitch = (sample.pose.pitch + pending_pitch as f32).clamp(-90.0, 90.0);
        } else if sample.look.sequence > self.produced_look.sequence {
            // The renderer may be recreated after suspension and legitimately
            // miss platform look events that the simulation already consumed.
            self.produced_look = sample.look;
            self.display_yaw = sample.pose.yaw.rem_euclid(360.0);
            self.display_pitch = sample.pose.pitch.clamp(-90.0, 90.0);
        }
        self.previous = if sample.discontinuity {
            sample.pose
        } else {
            self.current
        };
        self.current = sample.pose;
        self.tick_at = sample.tick_at;
        if sample.look.sequence >= self.acknowledged_look.sequence {
            self.acknowledged_look = sample.look;
        }
        tracing::trace!(target: "movement::look", new_simulated_pose = true, discontinuity = sample.discontinuity, ?sample.tick_at, simulation_yaw = sample.pose.yaw, simulation_pitch = sample.pose.pitch, acknowledged_sequence = self.acknowledged_look.sequence, acknowledged_yaw = self.acknowledged_look.yaw_total, acknowledged_pitch = self.acknowledged_look.pitch_total, produced_sequence = self.produced_look.sequence, produced_yaw = self.produced_look.yaw_total, produced_pitch = self.produced_look.pitch_total, outstanding_yaw = self.produced_look.yaw_total - self.acknowledged_look.yaw_total, outstanding_pitch = self.produced_look.pitch_total - self.acknowledged_look.pitch_total, display_yaw = self.display_yaw, display_pitch = self.display_pitch, "applied simulation look sample to render presentation");
    }

    fn preview_look(&mut self, sequence: u64, yaw_delta: f32, pitch_delta: f32) {
        if sequence <= self.produced_look.sequence
            || !yaw_delta.is_finite()
            || !pitch_delta.is_finite()
        {
            return;
        }
        self.produced_look.sequence = sequence;
        self.produced_look.yaw_total += f64::from(yaw_delta);
        self.produced_look.pitch_total += f64::from(pitch_delta);
        self.display_yaw = (self.display_yaw + yaw_delta).rem_euclid(360.0);
        self.display_pitch = (self.display_pitch + pitch_delta).clamp(-90.0, 90.0);
        tracing::trace!(target: "movement::look", sequence, yaw_delta, pitch_delta, produced_yaw = self.produced_look.yaw_total, produced_pitch = self.produced_look.pitch_total, acknowledged_sequence = self.acknowledged_look.sequence, acknowledged_yaw = self.acknowledged_look.yaw_total, acknowledged_pitch = self.acknowledged_look.pitch_total, outstanding_yaw = self.produced_look.yaw_total - self.acknowledged_look.yaw_total, outstanding_pitch = self.produced_look.pitch_total - self.acknowledged_look.pitch_total, display_yaw = self.display_yaw, display_pitch = self.display_pitch, "integrated raw mouse event into persistent display orientation");
    }

    fn display(self, now: Instant) -> LocalPlayerPose {
        let alpha = now.saturating_duration_since(self.tick_at).as_secs_f64()
            / SIMULATION_TICK.as_secs_f64();
        let alpha = alpha.clamp(0.0, 1.0);
        let lerp = |old: f64, current: f64| old + (current - old) * alpha;
        let mut pose = self.current;
        pose.x = lerp(self.previous.x, self.current.x);
        pose.y = lerp(self.previous.y, self.current.y);
        pose.z = lerp(self.previous.z, self.current.z);
        pose.eye_height = lerp(self.previous.eye_height, self.current.eye_height);
        pose.yaw = self.display_yaw;
        pose.pitch = self.display_pitch;
        tracing::trace!(target: "movement::look", ?now, new_simulated_pose = false, simulation_yaw = self.current.yaw, simulation_pitch = self.current.pitch, acknowledged_sequence = self.acknowledged_look.sequence, produced_sequence = self.produced_look.sequence, outstanding_yaw = self.produced_look.yaw_total - self.acknowledged_look.yaw_total, outstanding_pitch = self.produced_look.pitch_total - self.acknowledged_look.pitch_total, display_yaw = pose.yaw, display_pitch = pose.pitch, "sampled persistent display orientation for render frame");
        pose
    }

    fn is_interpolating(self, now: Instant) -> bool {
        now.saturating_duration_since(self.tick_at) < SIMULATION_TICK
            && (self.previous.x != self.current.x
                || self.previous.y != self.current.y
                || self.previous.z != self.current.z
                || self.previous.eye_height != self.current.eye_height)
    }
}

impl Drop for WorldRenderer {
    fn drop(&mut self) {
        self.log_mesh_progress(true);
    }
}

struct MeshWorker {
    jobs: Vec<mpsc::SyncSender<MeshJob>>,
    results: mpsc::Receiver<MeshResult>,
    active: Arc<AtomicUsize>,
    next_lane: usize,
}

impl MeshWorker {
    fn new(resources: BlockResources, device: Device) -> Option<Self> {
        let (result_tx, results) = mpsc::sync_channel(MAX_PENDING_MESH_JOBS);
        let available = thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
        let (worker_count, queue_per_worker) = worker_layout(available);
        let resources = Arc::new(resources);
        let active = Arc::new(AtomicUsize::new(0));
        let mut jobs = Vec::with_capacity(worker_count);
        for index in 0..worker_count {
            let (job_tx, job_rx) = mpsc::sync_channel::<MeshJob>(queue_per_worker);
            let result_tx = result_tx.clone();
            let resources = Arc::clone(&resources);
            let active = Arc::clone(&active);
            let device = device.clone();
            let spawn = thread::Builder::new()
                .name(format!("cubic-chunk-mesher-{index}"))
                .spawn(move || {
                    while let Ok(job) = job_rx.recv() {
                        active.fetch_add(1, Ordering::AcqRel);
                        let mesh =
                            mesh_chunk(job.coordinate, &job.chunks, job.geometry, &resources).map(
                                |mesh| {
                                    let statistics = mesh.statistics;
                                    if mesh.indices.is_empty() {
                                        PreparedChunkMesh::Empty { statistics }
                                    } else {
                                        PreparedChunkMesh::Ready {
                                            mesh: GpuChunkMesh::new(&device, &mesh),
                                            statistics,
                                        }
                                    }
                                },
                            );
                        active.fetch_sub(1, Ordering::AcqRel);
                        if result_tx
                            .send(MeshResult {
                                coordinate: job.coordinate,
                                generation: job.generation,
                                revision: job.revision,
                                mesh,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                });
            match spawn {
                Ok(_worker) => jobs.push(job_tx),
                Err(error) => {
                    tracing::error!(worker = index, %error, "could not start bounded chunk-meshing worker");
                    break;
                }
            }
        }
        if jobs.is_empty() {
            return None;
        }
        tracing::info!(
            workers = jobs.len(),
            maximum_in_flight = MAX_PENDING_MESH_JOBS,
            "started bounded chunk-meshing workers"
        );
        Some(Self {
            jobs,
            results,
            active,
            next_lane: 0,
        })
    }

    fn try_send(&mut self, job: MeshJob) -> bool {
        let mut job = Some(job);
        for offset in 0..self.jobs.len() {
            let lane = (self.next_lane + offset) % self.jobs.len();
            let Some(candidate) = job.take() else {
                return false;
            };
            match self.jobs[lane].try_send(candidate) {
                Ok(()) => {
                    self.next_lane = (lane + 1) % self.jobs.len();
                    return true;
                }
                Err(mpsc::TrySendError::Full(returned))
                | Err(mpsc::TrySendError::Disconnected(returned)) => job = Some(returned),
            }
        }
        false
    }

    fn active_jobs(&self) -> usize {
        self.active.load(Ordering::Acquire).min(self.jobs.len())
    }
}

fn worker_layout(available_parallelism: usize) -> (usize, usize) {
    let workers = available_parallelism.clamp(1, 4);
    let queued_per_worker = ((MAX_PENDING_MESH_JOBS - workers) / workers).max(1);
    (workers, queued_per_worker)
}

#[derive(Clone, Copy, Debug)]
struct MeshProgressSnapshot {
    loaded: usize,
    meshed: usize,
    pending: usize,
    queued: usize,
    active: usize,
    completed: u64,
    meshes_per_second: f64,
    overall_meshes_per_second: f64,
    average_mesh_ms: f64,
    maximum_mesh_ms: f64,
    statistics: MeshStatistics,
}

struct MeshProgress {
    started: Instant,
    last_info: Instant,
    last_debug: Instant,
    last_info_completed: u64,
    completed: u64,
    total_cpu_time: Duration,
    maximum_cpu_time: Duration,
    statistics: MeshStatistics,
}

impl MeshProgress {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            started: now,
            last_info: now,
            last_debug: now,
            last_info_completed: 0,
            completed: 0,
            total_cpu_time: Duration::ZERO,
            maximum_cpu_time: Duration::ZERO,
            statistics: MeshStatistics::default(),
        }
    }

    fn record(&mut self, result: &Result<PreparedChunkMesh, crate::mesher::MeshError>) {
        self.completed = self.completed.saturating_add(1);
        let Ok(mesh) = result else { return };
        let statistics = mesh.statistics();
        self.total_cpu_time = self.total_cpu_time.saturating_add(statistics.cpu_time);
        self.maximum_cpu_time = self.maximum_cpu_time.max(statistics.cpu_time);
        self.statistics.accumulate(statistics);
    }

    fn report_due(&self, now: Instant, final_summary: bool) -> (bool, bool) {
        (
            final_summary || now.duration_since(self.last_info) >= Duration::from_secs(10),
            final_summary || now.duration_since(self.last_debug) >= Duration::from_secs(2),
        )
    }

    fn snapshot(
        &self,
        loaded: usize,
        meshed: usize,
        pending: usize,
        in_flight: usize,
        active: usize,
        now: Instant,
    ) -> MeshProgressSnapshot {
        let interval = now.duration_since(self.last_info).as_secs_f64().max(0.001);
        let interval_completed = self.completed.saturating_sub(self.last_info_completed);
        MeshProgressSnapshot {
            loaded,
            meshed,
            pending,
            queued: in_flight.saturating_sub(active),
            active,
            completed: self.completed,
            meshes_per_second: interval_completed as f64 / interval,
            overall_meshes_per_second: self.completed as f64
                / now.duration_since(self.started).as_secs_f64().max(0.001),
            average_mesh_ms: if self.completed == 0 {
                0.0
            } else {
                self.total_cpu_time.as_secs_f64() * 1000.0 / self.completed as f64
            },
            maximum_mesh_ms: self.maximum_cpu_time.as_secs_f64() * 1000.0,
            statistics: self.statistics,
        }
    }
}

struct MeshJob {
    coordinate: ChunkCoordinate,
    chunks: BTreeMap<ChunkCoordinate, Arc<Chunk>>,
    geometry: DimensionGeometry,
    generation: u64,
    revision: u64,
}

struct MeshResult {
    coordinate: ChunkCoordinate,
    generation: u64,
    revision: u64,
    mesh: Result<PreparedChunkMesh, crate::mesher::MeshError>,
}

enum PreparedChunkMesh {
    Empty {
        statistics: MeshStatistics,
    },
    Ready {
        mesh: GpuChunkMesh,
        statistics: MeshStatistics,
    },
}

impl PreparedChunkMesh {
    const fn statistics(&self) -> MeshStatistics {
        match self {
            Self::Empty { statistics } | Self::Ready { statistics, .. } => *statistics,
        }
    }
}

struct GpuChunkMesh {
    vertices: Buffer,
    indices: Buffer,
    index_count: u32,
}
impl GpuChunkMesh {
    fn new(device: &Device, mesh: &ChunkMesh) -> Self {
        Self {
            vertices: device.create_buffer_init(&BufferInitDescriptor {
                label: Some("Cubic chunk vertices"),
                contents: bytemuck::cast_slice(&mesh.vertices),
                usage: BufferUsages::VERTEX,
            }),
            indices: device.create_buffer_init(&BufferInitDescriptor {
                label: Some("Cubic chunk indices"),
                contents: bytemuck::cast_slice(&mesh.indices),
                usage: BufferUsages::INDEX,
            }),
            index_count: u32::try_from(mesh.indices.len()).unwrap_or(u32::MAX),
        }
    }
}

struct DepthTarget {
    _texture: Texture,
    view: TextureView,
}
impl DepthTarget {
    fn new(device: &Device, width: u32, height: u32) -> Self {
        let texture = device.create_texture(&TextureDescriptor {
            label: Some("Cubic world depth"),
            size: Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&TextureViewDescriptor::default());
        Self {
            _texture: texture,
            view,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CameraUniform {
    view_projection: [[f32; 4]; 4],
}
impl CameraUniform {
    const fn identity() -> Self {
        Self {
            view_projection: [
                [1., 0., 0., 0.],
                [0., 1., 0., 0.],
                [0., 0., 1., 0.],
                [0., 0., 0., 1.],
            ],
        }
    }
    fn from_pose(pose: LocalPlayerPose, width: u32, height: u32) -> Self {
        let aspect = width.max(1) as f32 / height.max(1) as f32;
        let (forward, right, up) = camera_basis(pose.yaw, pose.pitch);
        let eye = [
            pose.x as f32,
            (pose.y + pose.eye_height) as f32,
            pose.z as f32,
        ];
        Self {
            view_projection: multiply(
                perspective(70_f32.to_radians(), aspect, 0.05, 2048.0),
                look_to_basis(eye, forward, right, up),
            ),
        }
    }
}

fn neighbors_including(c: ChunkCoordinate) -> [ChunkCoordinate; 5] {
    [
        c,
        ChunkCoordinate::new(c.x - 1, c.z),
        ChunkCoordinate::new(c.x + 1, c.z),
        ChunkCoordinate::new(c.x, c.z - 1),
        ChunkCoordinate::new(c.x, c.z + 1),
    ]
}

fn mark_dirty_with_neighbors(
    dirty: &mut BTreeMap<ChunkCoordinate, u64>,
    coordinate: ChunkCoordinate,
    revision: u64,
) {
    for coordinate in neighbors_including(coordinate) {
        dirty.insert(coordinate, revision);
    }
}

fn player_chunk(pose: LocalPlayerPose) -> ChunkCoordinate {
    ChunkCoordinate::new(
        (pose.x / 16.0).floor() as i32,
        (pose.z / 16.0).floor() as i32,
    )
}

fn next_mesh_candidate(
    dirty: &BTreeMap<ChunkCoordinate, u64>,
    pending: &BTreeSet<ChunkCoordinate>,
    origin: Option<ChunkCoordinate>,
) -> Option<(ChunkCoordinate, u64)> {
    dirty
        .iter()
        .filter(|(coordinate, _)| !pending.contains(coordinate))
        .min_by_key(|(coordinate, _)| mesh_priority(**coordinate, origin))
        .map(|(coordinate, revision)| (*coordinate, *revision))
}

fn mesh_priority(coordinate: ChunkCoordinate, origin: Option<ChunkCoordinate>) -> (u128, i32, i32) {
    let distance_squared = origin.map_or(0, |origin| {
        let dx = i128::from(coordinate.x) - i128::from(origin.x);
        let dz = i128::from(coordinate.z) - i128::from(origin.z);
        (dx * dx + dz * dz) as u128
    });
    (distance_squared, coordinate.x, coordinate.z)
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn normalize(v: [f32; 3]) -> [f32; 3] {
    let n = dot(v, v).sqrt().max(f32::EPSILON);
    [v[0] / n, v[1] / n, v[2] / n]
}
fn camera_basis(yaw_degrees: f32, pitch_degrees: f32) -> ([f32; 3], [f32; 3], [f32; 3]) {
    let yaw = yaw_degrees.to_radians();
    let pitch = pitch_degrees.to_radians();
    let forward = normalize([
        -yaw.sin() * pitch.cos(),
        -pitch.sin(),
        yaw.cos() * pitch.cos(),
    ]);
    // Derive right directly from yaw. Crossing forward with fixed world-up is
    // singular at Minecraft's legal +/-90-degree pitch poles and causes the
    // view basis to flip even though the eye position remains unchanged.
    let right = normalize([-yaw.cos(), 0.0, -yaw.sin()]);
    let up = normalize(cross(right, forward));
    (forward, right, up)
}
fn look_to_basis(eye: [f32; 3], forward: [f32; 3], right: [f32; 3], up: [f32; 3]) -> [[f32; 4]; 4] {
    let f = normalize(forward);
    let r = normalize(right);
    let u = normalize(up);
    [
        [r[0], u[0], -f[0], 0.],
        [r[1], u[1], -f[1], 0.],
        [r[2], u[2], -f[2], 0.],
        [-dot(r, eye), -dot(u, eye), dot(f, eye), 1.],
    ]
}
fn perspective(fovy: f32, aspect: f32, near: f32, far: f32) -> [[f32; 4]; 4] {
    let f = 1.0 / (fovy / 2.0).tan();
    let depth = far / (near - far);
    [
        [f / aspect, 0., 0., 0.],
        [0., f, 0., 0.],
        [0., 0., depth, -1.],
        [0., 0., near * depth, 0.],
    ]
}
fn multiply(a: [[f32; 4]; 4], b: [[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut o = [[0.; 4]; 4];
    for c in 0..4 {
        for r in 0..4 {
            o[c][r] = a[0][r] * b[c][0] + a[1][r] * b[c][1] + a[2][r] * b[c][2] + a[3][r] * b[c][3];
        }
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy)]
    enum MouseTrace {
        Horizontal,
        Vertical,
        Diagonal,
        ClockwiseCircle,
        CounterClockwiseCircle,
        TightFastCircle,
        SlowLargeCircle,
        FigureEight,
        HorizontalReversal,
        VerticalReversal,
    }

    fn trace_deltas(trace: MouseTrace, count: usize) -> Vec<(f32, f32)> {
        let point = |index: usize| {
            let phase = index as f32 / count as f32 * std::f32::consts::TAU;
            match trace {
                MouseTrace::Horizontal => (index as f32 * 0.35, 0.0),
                MouseTrace::Vertical => (0.0, index as f32 * 0.35),
                MouseTrace::Diagonal => (index as f32 * 0.25, index as f32 * 0.2),
                MouseTrace::ClockwiseCircle => (20.0 * phase.sin(), 20.0 * (1.0 - phase.cos())),
                MouseTrace::CounterClockwiseCircle => {
                    (-20.0 * phase.sin(), 20.0 * (1.0 - phase.cos()))
                }
                MouseTrace::TightFastCircle => (
                    8.0 * (phase * 30.0).sin(),
                    8.0 * (1.0 - (phase * 30.0).cos()),
                ),
                MouseTrace::SlowLargeCircle => (40.0 * phase.sin(), 40.0 * (1.0 - phase.cos())),
                MouseTrace::FigureEight => (30.0 * phase.sin(), 18.0 * (phase * 2.0).sin()),
                MouseTrace::HorizontalReversal => (18.0 * (phase * 30.0).sin(), 0.0),
                MouseTrace::VerticalReversal => (0.0, 18.0 * (phase * 30.0).sin()),
            }
        };
        let mut previous = point(0);
        (1..=count)
            .map(|index| {
                let current = point(index);
                let delta = (current.0 - previous.0, current.1 - previous.1);
                previous = current;
                delta
            })
            .collect()
    }

    fn angle_error(actual: f32, expected: f32) -> f32 {
        ((actual - expected + 180.0).rem_euclid(360.0) - 180.0).abs()
    }

    fn assert_trace_follows_raw_path(trace: MouseTrace) -> (f32, f32) {
        let start = Instant::now();
        // Starting near the upper pitch limit exercises the nonlinear clamp
        // that a circular gesture crosses in the real camera.
        let initial = LocalPlayerPose::new(0.0, 64.0, 0.0, 15.0, 80.0);
        let mut presentation = PosePresentation::new(RenderPoseSample {
            pose: initial,
            tick_at: start,
            look: RenderLookSample::default(),
            discontinuity: true,
        });
        let mut reference_yaw = initial.yaw;
        let mut reference_pitch = initial.pitch;
        let mut produced = RenderLookSample::default();
        let mut acknowledged = RenderLookSample::default();
        let mut simulated_yaw = initial.yaw;
        let mut simulated_pitch = initial.pitch;
        let mut persistent_max_error = 0.0_f32;
        let mut legacy_max_error = 0.0_f32;

        for (index, (yaw, pitch)) in trace_deltas(trace, 240).into_iter().enumerate() {
            let sequence = index as u64 + 1;
            produced.sequence = sequence;
            produced.yaw_total += f64::from(yaw);
            produced.pitch_total += f64::from(pitch);
            reference_yaw = (reference_yaw + yaw).rem_euclid(360.0);
            reference_pitch = (reference_pitch + pitch).clamp(-90.0, 90.0);
            presentation.preview_look(sequence, yaw, pitch);

            // Four or more direction reversals can occur between these 20 Hz
            // samples in the fast traces.
            let new_simulated_pose = sequence.is_multiple_of(12);
            if new_simulated_pose {
                simulated_yaw = reference_yaw;
                simulated_pitch = reference_pitch;
                acknowledged = produced;
                presentation.apply(RenderPoseSample {
                    pose: LocalPlayerPose::new(
                        sequence as f64 / 100.0,
                        64.0,
                        0.0,
                        simulated_yaw,
                        simulated_pitch,
                    ),
                    tick_at: start + Duration::from_millis(sequence),
                    look: acknowledged,
                    discontinuity: false,
                });
            }

            let displayed = presentation.display(start + Duration::from_millis(sequence));
            persistent_max_error = persistent_max_error
                .max(angle_error(displayed.yaw, reference_yaw))
                .max((displayed.pitch - reference_pitch).abs());

            let legacy_yaw = (simulated_yaw + (produced.yaw_total - acknowledged.yaw_total) as f32)
                .rem_euclid(360.0);
            let legacy_pitch = (simulated_pitch
                + (produced.pitch_total - acknowledged.pitch_total) as f32)
                .clamp(-90.0, 90.0);
            legacy_max_error = legacy_max_error
                .max(angle_error(legacy_yaw, reference_yaw))
                .max((legacy_pitch - reference_pitch).abs());
        }
        assert!(
            persistent_max_error < 1.0e-4,
            "trajectory error {persistent_max_error}"
        );
        (legacy_max_error, persistent_max_error)
    }

    fn assert_vector_close(actual: [f32; 3], expected: [f32; 3]) {
        for (actual, expected) in actual.into_iter().zip(expected) {
            assert!((actual - expected).abs() < 1.0e-5, "{actual} != {expected}");
        }
    }

    fn transform(matrix: [[f32; 4]; 4], point: [f32; 4]) -> [f32; 4] {
        let mut result = [0.0; 4];
        for row in 0..4 {
            result[row] = matrix[0][row] * point[0]
                + matrix[1][row] * point[1]
                + matrix[2][row] * point[2]
                + matrix[3][row] * point[3];
        }
        result
    }

    #[test]
    fn camera_matrix_is_finite_and_aspect_sensitive() {
        let pose = LocalPlayerPose {
            x: 1.,
            y: 64.,
            z: -2.,
            yaw: 30.,
            pitch: -10.,
            eye_height: 1.62,
        };
        let a = CameraUniform::from_pose(pose, 1280, 720);
        let b = CameraUniform::from_pose(pose, 720, 720);
        assert!(a.view_projection.iter().flatten().all(|v| v.is_finite()));
        assert_ne!(a.view_projection, b.view_projection);
    }

    #[test]
    fn render_only_look_preview_wraps_yaw_and_clamps_pitch() {
        let now = Instant::now();
        let pose = LocalPlayerPose::new(1.0, 64.0, 2.0, 350.0, 80.0);
        let mut presentation = PosePresentation::new(RenderPoseSample {
            pose,
            tick_at: now,
            look: RenderLookSample::default(),
            discontinuity: true,
        });
        presentation.preview_look(1, 20.0, 30.0);
        let displayed = presentation.display(now);
        assert_eq!((displayed.yaw, displayed.pitch), (10.0, 90.0));
        presentation.preview_look(2, f32::NAN, -20.0);
        let displayed = presentation.display(now);
        assert_eq!((displayed.yaw, displayed.pitch), (10.0, 90.0));
    }

    #[test]
    fn partial_tick_presentation_interpolates_translation_and_eye_height_only() {
        let start = Instant::now();
        let previous = LocalPlayerPose {
            x: 0.0,
            y: 64.0,
            z: 0.0,
            yaw: 10.0,
            pitch: 5.0,
            eye_height: 1.62,
        };
        let current = LocalPlayerPose {
            x: 1.0,
            y: 65.0,
            z: -2.0,
            yaw: 30.0,
            pitch: 15.0,
            eye_height: 1.27,
        };
        let mut presentation = PosePresentation::new(RenderPoseSample {
            pose: previous,
            tick_at: start - SIMULATION_TICK,
            look: RenderLookSample::default(),
            discontinuity: true,
        });
        presentation.apply(RenderPoseSample {
            pose: current,
            tick_at: start,
            look: RenderLookSample::default(),
            discontinuity: false,
        });

        let halfway = presentation.display(start + SIMULATION_TICK / 2);
        assert_eq!((halfway.x, halfway.y, halfway.z), (0.5, 64.5, -1.0));
        assert!((halfway.eye_height - 1.445).abs() < 1.0e-9);
        // Ordinary 20 Hz samples update translation but cannot replace the
        // persistent display-rate orientation without a mouse event.
        assert_eq!((halfway.yaw, halfway.pitch), (10.0, 5.0));
        let at_tick = presentation.display(start + SIMULATION_TICK);
        assert_eq!((at_tick.x, at_tick.y, at_tick.z), (1.0, 65.0, -2.0));
        assert_eq!((at_tick.yaw, at_tick.pitch), (10.0, 5.0));
    }

    #[test]
    fn a_new_jump_pose_starts_moving_on_the_first_post_tick_frame() {
        let start = Instant::now();
        let grounded = LocalPlayerPose::new(0.0, 64.0, 0.0, 0.0, 0.0);
        let airborne = LocalPlayerPose::new(0.0, 64.42, 0.0, 0.0, 0.0);
        let mut presentation = PosePresentation::new(RenderPoseSample {
            pose: grounded,
            tick_at: start - SIMULATION_TICK,
            look: RenderLookSample::default(),
            discontinuity: true,
        });
        presentation.apply(RenderPoseSample {
            pose: airborne,
            tick_at: start,
            look: RenderLookSample::default(),
            discontinuity: false,
        });

        let first_frame = presentation.display(start + Duration::from_millis(2));
        assert!(first_frame.y > grounded.y);
        assert!(first_frame.y < airborne.y);
        assert!((first_frame.y - 64.0168).abs() < 1.0e-9);
    }

    #[test]
    fn corrections_snap_and_cumulative_look_ack_never_builds_a_backlog() {
        let start = Instant::now();
        let initial = LocalPlayerPose::new(0.0, 64.0, 0.0, 0.0, 0.0);
        let mut presentation = PosePresentation::new(RenderPoseSample {
            pose: initial,
            tick_at: start,
            look: RenderLookSample::default(),
            discontinuity: true,
        });
        let mut yaw_total = 0.0_f64;
        let mut pitch_total = 0.0_f64;
        for sequence in 1..=10_000_u64 {
            let yaw = 0.125;
            let pitch = if sequence % 2 == 0 { 0.025 } else { -0.025 };
            presentation.preview_look(sequence, yaw, pitch);
            yaw_total += f64::from(yaw);
            pitch_total += f64::from(pitch);
            if sequence % 10 == 0 {
                let pose = LocalPlayerPose::new(
                    f64::from(sequence as u32) / 100.0,
                    64.0,
                    0.0,
                    yaw_total as f32,
                    pitch_total as f32,
                );
                presentation.apply(RenderPoseSample {
                    pose,
                    tick_at: start + Duration::from_millis(sequence / 10 * 50),
                    look: RenderLookSample {
                        sequence,
                        yaw_total,
                        pitch_total,
                    },
                    discontinuity: false,
                });
                let displayed = presentation.display(presentation.tick_at);
                assert!((displayed.yaw - pose.yaw.rem_euclid(360.0)).abs() < 1.0e-4);
                assert!((displayed.pitch - pose.pitch).abs() < 1.0e-4);
                assert_eq!(presentation.produced_look, presentation.acknowledged_look);
            }
        }

        let corrected = LocalPlayerPose::new(100.0, 70.0, -30.0, 45.0, 10.0);
        presentation.apply(RenderPoseSample {
            pose: corrected,
            tick_at: start + Duration::from_secs(60),
            look: presentation.acknowledged_look,
            discontinuity: true,
        });
        assert_eq!(
            presentation.display(start + Duration::from_secs(60)),
            corrected
        );
    }

    #[test]
    fn frequent_movement_pose_updates_never_drop_pending_mouse_preview() {
        let start = Instant::now();
        let initial = LocalPlayerPose::new(0.0, 64.0, 0.0, 0.0, 0.0);
        let mut presentation = PosePresentation::new(RenderPoseSample {
            pose: initial,
            tick_at: start,
            look: RenderLookSample::default(),
            discontinuity: true,
        });
        let mut produced_yaw = 0.0_f64;
        let mut reference_yaw = 0.0_f32;

        for sequence in 1..=2_000_u64 {
            let yaw_delta = if sequence % 3 == 0 { -0.075 } else { 0.125 };
            presentation.preview_look(sequence, yaw_delta, 0.0);
            produced_yaw += f64::from(yaw_delta);
            reference_yaw = (reference_yaw + yaw_delta).rem_euclid(360.0);

            // Model the extra fixed-tick pose publications seen while held or
            // rapidly changing movement keys. They may acknowledge only the
            // mouse input actually consumed by that simulation tick.
            if sequence % 7 == 0 {
                let acknowledged_yaw = produced_yaw;
                presentation.apply(RenderPoseSample {
                    pose: LocalPlayerPose::new(
                        sequence as f64 / 100.0,
                        64.0,
                        0.0,
                        acknowledged_yaw as f32,
                        0.0,
                    ),
                    tick_at: start + Duration::from_millis(sequence),
                    look: RenderLookSample {
                        sequence,
                        yaw_total: acknowledged_yaw,
                        pitch_total: 0.0,
                    },
                    discontinuity: false,
                });
            }

            let displayed = presentation.display(start + Duration::from_millis(sequence));
            assert!(angle_error(displayed.yaw, reference_yaw) < 1.0e-6);
        }
    }

    #[test]
    fn persistent_display_look_matches_straight_circle_reversal_and_figure_eight_paths() {
        let straight = assert_trace_follows_raw_path(MouseTrace::Horizontal);
        let vertical = assert_trace_follows_raw_path(MouseTrace::Vertical);
        let diagonal = assert_trace_follows_raw_path(MouseTrace::Diagonal);
        let clockwise = assert_trace_follows_raw_path(MouseTrace::ClockwiseCircle);
        let counter = assert_trace_follows_raw_path(MouseTrace::CounterClockwiseCircle);
        let tight = assert_trace_follows_raw_path(MouseTrace::TightFastCircle);
        let large = assert_trace_follows_raw_path(MouseTrace::SlowLargeCircle);
        let figure_eight = assert_trace_follows_raw_path(MouseTrace::FigureEight);
        let horizontal_reversal = assert_trace_follows_raw_path(MouseTrace::HorizontalReversal);
        let vertical_reversal = assert_trace_follows_raw_path(MouseTrace::VerticalReversal);

        assert!(straight.0 < 1.0e-4 && horizontal_reversal.0 < 1.0e-4);
        assert!(vertical.0 < 1.0e-4);
        assert!(
            vertical_reversal.0 > 1.0,
            "vertical reversal: {vertical_reversal:?}"
        );
        assert!(clockwise.0 < 1.0e-4 && counter.0 < 1.0e-4);
        assert!(tight.0 > 1.0 && figure_eight.0 > 0.5);
        assert!(large.0 < 1.0e-4);
        assert!(diagonal.1 < 1.0e-4);
    }

    #[test]
    fn local_tick_acknowledgement_does_not_move_display_but_correction_rebases_it() {
        let start = Instant::now();
        let mut presentation = PosePresentation::new(RenderPoseSample {
            pose: LocalPlayerPose::new(0.0, 64.0, 0.0, 10.0, 5.0),
            tick_at: start,
            look: RenderLookSample::default(),
            discontinuity: true,
        });
        presentation.preview_look(1, 20.0, 10.0);
        presentation.preview_look(2, -3.0, 4.0);
        let before_ack = presentation.display(start);
        presentation.apply(RenderPoseSample {
            pose: LocalPlayerPose::new(1.0, 64.0, 0.0, 27.0, 19.0),
            tick_at: start + Duration::from_millis(50),
            look: RenderLookSample {
                sequence: 2,
                yaw_total: 17.0,
                pitch_total: 14.0,
            },
            discontinuity: false,
        });
        let after_ack = presentation.display(start + Duration::from_millis(50));
        assert_eq!((before_ack.yaw, before_ack.pitch), (27.0, 19.0));
        assert_eq!((after_ack.yaw, after_ack.pitch), (27.0, 19.0));

        presentation.preview_look(3, 2.0, -1.0);
        presentation.apply(RenderPoseSample {
            pose: LocalPlayerPose::new(20.0, 70.0, -5.0, 90.0, -20.0),
            tick_at: start + Duration::from_millis(75),
            look: RenderLookSample {
                sequence: 2,
                yaw_total: 17.0,
                pitch_total: 14.0,
            },
            discontinuity: true,
        });
        let corrected = presentation.display(start + Duration::from_millis(75));
        assert_eq!((corrected.yaw, corrected.pitch), (92.0, -21.0));
        assert_eq!((corrected.x, corrected.y, corrected.z), (20.0, 70.0, -5.0));
    }

    #[test]
    fn minecraft_cardinal_camera_basis_is_not_mirrored() {
        let cases = [
            (0.0, [0.0, 0.0, 1.0], [-1.0, 0.0, 0.0]),
            (90.0, [-1.0, 0.0, 0.0], [0.0, 0.0, -1.0]),
            (-90.0, [1.0, 0.0, 0.0], [0.0, -0.0, 1.0]),
            (180.0, [0.0, 0.0, -1.0], [1.0, 0.0, 0.0]),
        ];
        for (yaw, expected_forward, expected_right) in cases {
            let (forward, right, up) = camera_basis(yaw, 0.0);
            assert_vector_close(forward, expected_forward);
            assert_vector_close(right, expected_right);
            assert_vector_close(up, [0.0, 1.0, 0.0]);
        }

        let (downward, _, _) = camera_basis(0.0, 30.0);
        assert!(downward[1] < 0.0, "positive Minecraft pitch must look down");
    }

    #[test]
    fn camera_basis_is_finite_and_continuous_at_pitch_poles() {
        for yaw in [0.0, 90.0, 180.0, 270.0, 37.25] {
            let mut previous_right = None;
            for pitch in [-90.0, -89.999, -89.0, 0.0, 89.0, 89.999, 90.0] {
                let (forward, right, up) = camera_basis(yaw, pitch);
                for vector in [forward, right, up] {
                    assert!(vector.into_iter().all(f32::is_finite));
                    assert!((dot(vector, vector) - 1.0).abs() < 1.0e-5);
                }
                assert!(dot(forward, right).abs() < 1.0e-5);
                assert!(dot(forward, up).abs() < 1.0e-5);
                assert!(dot(right, up).abs() < 1.0e-5);
                if let Some(previous) = previous_right {
                    assert!(dot(previous, right) > 0.99999);
                }
                previous_right = Some(right);
            }
        }
    }

    #[test]
    fn exact_pitch_poles_keep_a_finite_view_at_the_eye_origin() {
        for yaw in [0.0, 90.0, 180.0, 270.0, 123.5] {
            for pitch in [-90.0, 90.0] {
                let pose = LocalPlayerPose::new(4.0, 70.0, -3.0, yaw, pitch);
                let (forward, right, up) = camera_basis(yaw, pitch);
                let eye = [
                    pose.x as f32,
                    (pose.y + pose.eye_height) as f32,
                    pose.z as f32,
                ];
                let view = look_to_basis(eye, forward, right, up);
                assert!(view.into_iter().flatten().all(f32::is_finite));
                assert_eq!(eye, [4.0, 71.62, -3.0]);
            }
        }
    }

    #[test]
    fn view_projection_places_east_on_left_when_facing_south() {
        let pose = LocalPlayerPose {
            x: 0.0,
            y: 64.0,
            z: 0.0,
            yaw: 0.0,
            pitch: 0.0,
            eye_height: 1.62,
        };
        let camera = CameraUniform::from_pose(pose, 1280, 720);
        let east = transform(camera.view_projection, [1.0, 65.62, 10.0, 1.0]);
        let west = transform(camera.view_projection, [-1.0, 65.62, 10.0, 1.0]);

        assert!(east[3] > 0.0 && west[3] > 0.0);
        assert!(
            east[0] / east[3] < 0.0,
            "east must appear left when facing south"
        );
        assert!(
            west[0] / west[3] > 0.0,
            "west must appear right when facing south"
        );
    }

    #[test]
    fn mesh_candidates_prioritize_near_chunks_and_coalesce_revisions() {
        let origin = ChunkCoordinate::new(0, 0);
        let near = ChunkCoordinate::new(1, 0);
        let far = ChunkCoordinate::new(8, 3);
        let mut dirty = BTreeMap::from([(far, 1), (near, 2)]);
        dirty.insert(near, 3);

        assert_eq!(dirty.len(), 2);
        assert_eq!(
            next_mesh_candidate(&dirty, &BTreeSet::new(), Some(origin)),
            Some((near, 3))
        );
    }

    #[test]
    fn equal_distance_mesh_priority_is_deterministic() {
        let origin = ChunkCoordinate::new(0, 0);
        let dirty = BTreeMap::from([
            (ChunkCoordinate::new(1, 0), 1),
            (ChunkCoordinate::new(0, 1), 2),
            (ChunkCoordinate::new(0, -1), 3),
            (ChunkCoordinate::new(-1, 0), 4),
        ]);
        let mut pending = BTreeSet::new();
        let mut selected = Vec::new();
        while let Some((coordinate, _)) = next_mesh_candidate(&dirty, &pending, Some(origin)) {
            selected.push(coordinate);
            pending.insert(coordinate);
        }

        assert_eq!(
            selected,
            [
                ChunkCoordinate::new(-1, 0),
                ChunkCoordinate::new(0, -1),
                ChunkCoordinate::new(0, 1),
                ChunkCoordinate::new(1, 0),
            ]
        );
    }

    #[test]
    fn mesh_priority_tracks_authoritative_chunk_and_reset() {
        let west = ChunkCoordinate::new(-5, 0);
        let east = ChunkCoordinate::new(5, 0);
        let dirty = BTreeMap::from([(west, 1), (east, 2)]);
        let pending = BTreeSet::new();
        let pose = |x| LocalPlayerPose {
            x,
            y: 64.0,
            z: 0.0,
            yaw: 0.0,
            pitch: 0.0,
            eye_height: 1.62,
        };

        assert_eq!(player_chunk(pose(-0.1)), ChunkCoordinate::new(-1, 0));
        assert_eq!(
            next_mesh_candidate(&dirty, &pending, Some(player_chunk(pose(-80.0)))),
            Some((west, 1))
        );
        assert_eq!(
            next_mesh_candidate(&dirty, &pending, Some(player_chunk(pose(80.0)))),
            Some((east, 2))
        );
        assert_eq!(next_mesh_candidate(&dirty, &pending, None), Some((west, 1)));
    }

    #[test]
    fn mesh_job_channel_bound_remains_explicit() {
        let (sender, _receiver) = mpsc::sync_channel::<()>(MAX_PENDING_MESH_JOBS);
        for _ in 0..MAX_PENDING_MESH_JOBS {
            sender.try_send(()).expect("bounded test slot");
        }
        assert!(matches!(
            sender.try_send(()),
            Err(mpsc::TrySendError::Full(()))
        ));
    }

    #[test]
    fn mesh_integration_allows_initial_progress_and_dispatch_respects_deadline() {
        assert!(mesh_integration_permitted(0, Duration::from_secs(1)));
        assert!(mesh_integration_permitted(1, Duration::from_millis(3)));
        assert!(!mesh_integration_permitted(1, Duration::from_millis(4)));
        assert!(!mesh_integration_permitted(
            MAX_MESH_RESULTS_PER_PREPARE,
            Duration::ZERO
        ));
        assert!(!mesh_dispatch_permitted(0, Duration::from_secs(1)));
        assert!(mesh_dispatch_permitted(0, Duration::from_millis(3)));
        assert!(mesh_dispatch_permitted(1, Duration::from_millis(3)));
        assert!(!mesh_dispatch_permitted(1, Duration::from_millis(4)));
        assert!(!mesh_dispatch_permitted(
            MAX_MESH_DISPATCHES_PER_PREPARE,
            Duration::ZERO
        ));
        assert!(mesh_dispatch_capacity_available(MAX_PENDING_MESH_JOBS - 1));
        assert!(!mesh_dispatch_capacity_available(MAX_PENDING_MESH_JOBS));
    }

    #[test]
    fn worker_layout_uses_modest_parallelism_without_exceeding_the_job_bound() {
        assert_eq!(worker_layout(1), (1, 31));
        assert_eq!(worker_layout(4), (4, 7));
        assert_eq!(worker_layout(128), (4, 7));
        for available in 1..=128 {
            let (workers, queued_per_worker) = worker_layout(available);
            assert!(workers * (queued_per_worker + 1) <= MAX_PENDING_MESH_JOBS);
        }
    }

    #[test]
    fn progress_snapshot_distinguishes_loaded_meshed_pending_queued_and_active() {
        let mut progress = MeshProgress::new();
        progress.completed = 116;
        progress.last_info_completed = 100;
        progress.total_cpu_time = Duration::from_millis(5_800);
        progress.maximum_cpu_time = Duration::from_millis(120);
        let now = progress.last_info + Duration::from_secs(2);
        let snapshot = progress.snapshot(329, 116, 213, 32, 4, now);
        assert_eq!(snapshot.loaded, 329);
        assert_eq!(snapshot.meshed, 116);
        assert_eq!(snapshot.pending, 213);
        assert_eq!(snapshot.queued, 28);
        assert_eq!(snapshot.active, 4);
        assert_eq!(snapshot.completed, 116);
        assert_eq!(snapshot.meshes_per_second, 8.0);
        assert_eq!(snapshot.average_mesh_ms, 50.0);
    }

    #[test]
    fn progress_reporting_is_periodic_not_per_mesh() {
        let progress = MeshProgress::new();
        assert_eq!(
            progress.report_due(progress.last_debug + Duration::from_millis(500), false),
            (false, false)
        );
        assert_eq!(
            progress.report_due(progress.last_debug + Duration::from_secs(2), false),
            (false, true)
        );
        assert_eq!(
            progress.report_due(progress.last_info + Duration::from_secs(10), false),
            (true, true)
        );
        assert_eq!(progress.report_due(progress.last_info, true), (true, true));
    }

    #[test]
    fn neighbor_set_includes_four_horizontal_boundaries() {
        let c = ChunkCoordinate::new(2, -3);
        assert_eq!(
            neighbors_including(c),
            [
                c,
                ChunkCoordinate::new(1, -3),
                ChunkCoordinate::new(3, -3),
                ChunkCoordinate::new(2, -4),
                ChunkCoordinate::new(2, -2)
            ]
        );
    }

    #[test]
    fn neighbor_arrival_replacement_and_removal_keep_both_chunk_sides_dirty() {
        let center = ChunkCoordinate::new(0, 0);
        let east = ChunkCoordinate::new(1, 0);
        let mut dirty = BTreeMap::new();
        mark_dirty_with_neighbors(&mut dirty, center, 1);
        assert_eq!(dirty.get(&center), Some(&1));
        assert_eq!(dirty.get(&east), Some(&1));

        mark_dirty_with_neighbors(&mut dirty, east, 2);
        assert_eq!(dirty.get(&center), Some(&2));
        assert_eq!(dirty.get(&east), Some(&2));

        mark_dirty_with_neighbors(&mut dirty, east, 3);
        assert_eq!(dirty.get(&center), Some(&3));
        assert_eq!(dirty.get(&east), Some(&3));
    }

    #[test]
    fn terrain_backface_culling_prevents_coplanar_cross_face_fighting() {
        assert_eq!(TERRAIN_CULL_MODE, Some(Face::Back));
    }
}
