use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, mpsc},
    thread,
};

use bytemuck::{Pod, Zeroable};
use cubic_world::{
    AuthoritativeTransform, BlockVisualProfile, Chunk, ChunkCoordinate, ChunkRenderDelta,
    DimensionGeometry, WorldRenderUpdate,
};
use wgpu::{
    util::{BufferInitDescriptor, DeviceExt},
    *,
};

use crate::mesher::{ChunkMesh, TerrainVertex, mesh_chunk};

const DEPTH_FORMAT: TextureFormat = TextureFormat::Depth32Float;
const MAX_PENDING_MESH_JOBS: usize = 32;

pub(crate) struct WorldRenderer {
    pipeline: RenderPipeline,
    camera_buffer: Buffer,
    camera_bind_group: BindGroup,
    depth: DepthTarget,
    meshes: BTreeMap<ChunkCoordinate, GpuChunkMesh>,
    chunks: BTreeMap<ChunkCoordinate, Arc<Chunk>>,
    dirty: BTreeMap<ChunkCoordinate, u64>,
    pending: BTreeSet<ChunkCoordinate>,
    generation: u64,
    revision: u64,
    geometry: Option<DimensionGeometry>,
    dimension: Option<String>,
    pose: Option<AuthoritativeTransform>,
    worker: Option<MeshWorker>,
}

impl WorldRenderer {
    pub(crate) fn new(
        device: &Device,
        surface_format: TextureFormat,
        width: u32,
        height: u32,
        visual: BlockVisualProfile,
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
        let pipeline_layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
            label: Some("Cubic world pipeline layout"),
            bind_group_layouts: &[Some(&camera_layout)],
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
                    attributes: &vertex_attr_array![0 => Float32x3, 1 => Float32x3],
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
                cull_mode: None,
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
            worker: MeshWorker::new(visual),
        }
    }

    pub(crate) fn resize(&mut self, device: &Device, width: u32, height: u32) {
        self.depth = DepthTarget::new(device, width, height);
    }

    pub(crate) fn apply(&mut self, update: WorldRenderUpdate) {
        if update.reset || update.generation != self.generation {
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
        }
        self.dimension = update.dimension.or_else(|| self.dimension.take());
        self.geometry = update.geometry.or(self.geometry);
        self.pose = update.pose.or(self.pose);
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

    fn mark_with_neighbors(&mut self, coordinate: ChunkCoordinate) {
        self.revision = self.revision.wrapping_add(1);
        for coordinate in neighbors_including(coordinate) {
            self.dirty.insert(coordinate, self.revision);
        }
    }

    pub(crate) fn prepare(&mut self, device: &Device, queue: &Queue, width: u32, height: u32) {
        while let Some(result) = self
            .worker
            .as_ref()
            .and_then(|worker| worker.results.try_recv().ok())
        {
            self.pending.remove(&result.coordinate);
            if result.generation != self.generation
                || self.dirty.get(&result.coordinate) != Some(&result.revision)
            {
                continue;
            }
            self.dirty.remove(&result.coordinate);
            match result.mesh {
                Ok(mesh) if mesh.indices.is_empty() => {
                    self.meshes.remove(&result.coordinate);
                }
                Ok(mesh) => {
                    self.meshes
                        .insert(result.coordinate, GpuChunkMesh::new(device, &mesh));
                }
                Err(error) => {
                    self.meshes.remove(&result.coordinate);
                    tracing::warn!(x = result.coordinate.x, z = result.coordinate.z, %error, "bounded chunk meshing failed");
                }
            }
        }
        self.dispatch_jobs();
        if let Some(pose) = self.pose {
            let camera = CameraUniform::from_pose(pose, width, height);
            queue.write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&camera));
        }
    }

    fn dispatch_jobs(&mut self) {
        let Some(geometry) = self.geometry else {
            return;
        };
        let Some(worker) = &self.worker else {
            return;
        };
        self.dirty
            .retain(|coordinate, _| self.chunks.contains_key(coordinate));
        let priority_origin = self.pose.map(player_chunk);
        while let Some((coordinate, revision)) =
            next_mesh_candidate(&self.dirty, &self.pending, priority_origin)
        {
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
            match worker.jobs.try_send(job) {
                Ok(()) => {
                    self.pending.insert(coordinate);
                }
                Err(mpsc::TrySendError::Full(_)) => break,
                Err(mpsc::TrySendError::Disconnected(_)) => break,
            }
        }
    }

    pub(crate) fn has_pending_work(&self) -> bool {
        self.worker.is_some() && (!self.dirty.is_empty() || !self.pending.is_empty())
    }

    pub(crate) fn draw<'a>(&'a self, pass: &mut RenderPass<'a>) {
        if self.pose.is_none() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.camera_bind_group, &[]);
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
            pose: self.pose,
            loaded_chunks: self.chunks.len(),
            meshed_chunks: self.meshes.len(),
            pending_meshes: self.dirty.len() + self.pending.len(),
        }
    }
}

struct MeshWorker {
    jobs: mpsc::SyncSender<MeshJob>,
    results: mpsc::Receiver<MeshResult>,
}

impl MeshWorker {
    fn new(visual: BlockVisualProfile) -> Option<Self> {
        let (jobs, job_rx) = mpsc::sync_channel::<MeshJob>(MAX_PENDING_MESH_JOBS);
        let (result_tx, results) = mpsc::sync_channel(MAX_PENDING_MESH_JOBS);
        let spawn = thread::Builder::new()
            .name("cubic-chunk-mesher".to_owned())
            .spawn(move || {
                while let Ok(job) = job_rx.recv() {
                    let mesh = mesh_chunk(job.coordinate, &job.chunks, job.geometry, &visual);
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
            Ok(_worker) => Some(Self { jobs, results }),
            Err(error) => {
                tracing::error!(%error, "could not start bounded chunk-meshing worker");
                None
            }
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
    mesh: Result<ChunkMesh, crate::mesher::MeshError>,
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
    fn from_pose(pose: AuthoritativeTransform, width: u32, height: u32) -> Self {
        let aspect = width.max(1) as f32 / height.max(1) as f32;
        let (forward, _, _) = camera_basis(pose.yaw, pose.pitch);
        let eye = [pose.x as f32, pose.y as f32 + 1.62, pose.z as f32];
        Self {
            view_projection: multiply(
                perspective(70_f32.to_radians(), aspect, 0.05, 2048.0),
                look_to(eye, forward),
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

fn player_chunk(pose: AuthoritativeTransform) -> ChunkCoordinate {
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
    let right = normalize(cross(forward, [0., 1., 0.]));
    let up = cross(right, forward);
    (forward, right, up)
}
fn look_to(eye: [f32; 3], forward: [f32; 3]) -> [[f32; 4]; 4] {
    let f = normalize(forward);
    let r = normalize(cross(f, [0., 1., 0.]));
    let u = cross(r, f);
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
        let pose = AuthoritativeTransform {
            x: 1.,
            y: 64.,
            z: -2.,
            yaw: 30.,
            pitch: -10.,
            teleport_id: 1,
        };
        let a = CameraUniform::from_pose(pose, 1280, 720);
        let b = CameraUniform::from_pose(pose, 720, 720);
        assert!(a.view_projection.iter().flatten().all(|v| v.is_finite()));
        assert_ne!(a.view_projection, b.view_projection);
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
    fn view_projection_places_east_on_left_when_facing_south() {
        let pose = AuthoritativeTransform {
            x: 0.0,
            y: 64.0,
            z: 0.0,
            yaw: 0.0,
            pitch: 0.0,
            teleport_id: 1,
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
        let pose = |x| AuthoritativeTransform {
            x,
            y: 64.0,
            z: 0.0,
            yaw: 0.0,
            pitch: 0.0,
            teleport_id: 1,
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
}
