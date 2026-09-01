use std::{
    collections::BTreeMap,
    sync::{Arc, atomic::AtomicUsize},
    time::Duration,
};

use bytemuck::{Pod, Zeroable};
use cubic_world::{
    Chunk, ChunkCoordinate, DimensionGeometry, FluidKind, RuntimeBiome, RuntimeBiomeId,
    RuntimeBlockStateId,
};
use thiserror::Error;

use crate::block_resources::{
    BlockResources, Direction, ModelApplication, ModelOffset, RenderLayer, TintKind,
    rotate_blockstate_corner, rotate_blockstate_direction, uvlock_uvs,
};

pub const MAX_CHUNK_MESH_FACES: usize = 1_000_000;
const MAX_FLUID_DEBUG_CELLS_PER_MESH: usize = 32;
const MAX_FLUID_DEBUG_QUADS_PER_CELL: usize = 128;
pub(crate) const FLUID_DEBUG_LOG_CELL_BUDGET: usize = 128;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct FluidDebugOptions {
    pub face_colors: bool,
    pub mesh_log: bool,
    pub radius: u8,
}

impl FluidDebugOptions {
    pub(crate) fn from_environment() -> Self {
        Self::from_values(
            std::env::var_os("CUBIC_DEBUG_FLUID_FACES").as_deref(),
            std::env::var_os("CUBIC_DEBUG_FLUID_MESH").as_deref(),
            std::env::var_os("CUBIC_DEBUG_FLUID_RADIUS").as_deref(),
        )
    }

    fn from_values(
        faces: Option<&std::ffi::OsStr>,
        mesh: Option<&std::ffi::OsStr>,
        radius: Option<&std::ffi::OsStr>,
    ) -> Self {
        let enabled = |value: Option<&std::ffi::OsStr>| {
            value.is_some_and(|value| {
                value
                    .to_str()
                    .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
            })
        };
        let radius = radius
            .and_then(std::ffi::OsStr::to_str)
            .and_then(|value| value.parse::<u8>().ok())
            .unwrap_or(6)
            .clamp(1, 32);
        Self {
            face_colors: enabled(faces),
            mesh_log: enabled(mesh),
            radius,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub(crate) struct TerrainVertex {
    pub position: [f32; 3],
    pub uv: [f32; 2],
    pub tint: [f32; 3],
    pub layer: u32,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ChunkMesh {
    pub vertices: Vec<TerrainVertex>,
    pub indices: Vec<u32>,
    pub translucent_indices: Vec<u32>,
    pub layered_translucent_indices: Vec<u32>,
    pub statistics: MeshStatistics,
    fluid_debug: Vec<FluidCellDiagnostic>,
    active_fluid_debug: Option<FluidCellDiagnostic>,
    debug_face_colors: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FluidSideDecision {
    Emitted { subquads: usize },
    SameFluid,
    NeighborOcclusion,
    RemovedByClipping,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FluidDebugBatch {
    Opaque,
    Translucent,
    LayeredTranslucent,
}

#[derive(Clone, Debug)]
struct FluidQuadDiagnostic {
    direction: Direction,
    clipped: bool,
    positions: [[f32; 3]; 4],
    uvs: [[f32; 2]; 4],
    base_vertex: u32,
    forward_indices: [u32; 6],
    reverse_indices: Option<[u32; 6]>,
    batch: FluidDebugBatch,
    invariant: Result<(), FluidQuadInvariantViolation>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FluidQuadInvariantViolation {
    NonFinite,
    NonPlanar,
    InvertedHeight,
    ZeroArea,
    ForeignIndex,
}

#[derive(Clone, Debug)]
struct FluidCellDiagnostic {
    coordinate: [i32; 3],
    runtime_state: RuntimeBlockStateId,
    kind: FluidKind,
    level: u8,
    falling: bool,
    source: bool,
    heights: [f64; 4],
    sides: [Option<FluidSideDecision>; 4],
    quads: Vec<FluidQuadDiagnostic>,
    quads_truncated: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct MeshStatistics {
    pub positions_visited: u64,
    pub air_skipped: u64,
    pub non_air_blocks: u64,
    pub fully_occluded_fast_rejected: u64,
    pub model_processed: u64,
    pub geometry_emitting: u64,
    pub neighbor_checks: u64,
    pub model_selections: u64,
    pub quads_emitted: u64,
    pub cpu_time: Duration,
}

impl MeshStatistics {
    pub(crate) fn accumulate(&mut self, other: Self) {
        self.positions_visited = self
            .positions_visited
            .saturating_add(other.positions_visited);
        self.air_skipped = self.air_skipped.saturating_add(other.air_skipped);
        self.non_air_blocks = self.non_air_blocks.saturating_add(other.non_air_blocks);
        self.fully_occluded_fast_rejected = self
            .fully_occluded_fast_rejected
            .saturating_add(other.fully_occluded_fast_rejected);
        self.model_processed = self.model_processed.saturating_add(other.model_processed);
        self.geometry_emitting = self
            .geometry_emitting
            .saturating_add(other.geometry_emitting);
        self.neighbor_checks = self.neighbor_checks.saturating_add(other.neighbor_checks);
        self.model_selections = self.model_selections.saturating_add(other.model_selections);
        self.quads_emitted = self.quads_emitted.saturating_add(other.quads_emitted);
        self.cpu_time = self.cpu_time.saturating_add(other.cpu_time);
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub(crate) enum MeshError {
    #[error("chunk has {actual} sections but dimension geometry requires {expected}")]
    SectionCount { actual: usize, expected: usize },
    #[error("chunk mesh exceeds the bounded face limit {max}")]
    FaceLimit { max: usize },
    #[error("chunk mesh index space overflowed")]
    IndexOverflow,
}

#[cfg(test)]
pub(crate) fn mesh_chunk(
    coordinate: ChunkCoordinate,
    chunks: &BTreeMap<ChunkCoordinate, Arc<Chunk>>,
    geometry: DimensionGeometry,
    resources: &BlockResources,
) -> Result<ChunkMesh, MeshError> {
    mesh_chunk_with_biomes(coordinate, chunks, geometry, resources, &[])
}

#[cfg(test)]
pub(crate) fn mesh_chunk_with_biomes(
    coordinate: ChunkCoordinate,
    chunks: &BTreeMap<ChunkCoordinate, Arc<Chunk>>,
    geometry: DimensionGeometry,
    resources: &BlockResources,
    biomes: &[RuntimeBiome],
) -> Result<ChunkMesh, MeshError> {
    mesh_chunk_with_debug(
        coordinate,
        chunks,
        geometry,
        resources,
        biomes,
        FluidDebugOptions::default(),
        None,
    )
}

pub(crate) fn mesh_chunk_with_debug(
    coordinate: ChunkCoordinate,
    chunks: &BTreeMap<ChunkCoordinate, Arc<Chunk>>,
    geometry: DimensionGeometry,
    resources: &BlockResources,
    biomes: &[RuntimeBiome],
    debug: FluidDebugOptions,
    debug_origin: Option<[i32; 3]>,
) -> Result<ChunkMesh, MeshError> {
    let started = std::time::Instant::now();
    let Some(chunk) = chunks.get(&coordinate) else {
        return Ok(ChunkMesh::default());
    };
    if chunk.sections.len() != geometry.section_count() {
        return Err(MeshError::SectionCount {
            actual: chunk.sections.len(),
            expected: geometry.section_count(),
        });
    }
    let mut mesh = ChunkMesh {
        vertices: Vec::with_capacity(16 * 1024),
        indices: Vec::with_capacity(24 * 1024),
        translucent_indices: Vec::with_capacity(4 * 1024),
        layered_translucent_indices: Vec::with_capacity(1024),
        statistics: MeshStatistics::default(),
        fluid_debug: Vec::new(),
        active_fluid_debug: None,
        debug_face_colors: debug.face_colors,
    };
    for (section_index, section) in chunk.sections.iter().enumerate() {
        if section.non_empty_block_count == 0 {
            continue;
        }
        for y in 0_u8..16 {
            for z in 0_u8..16 {
                for x in 0_u8..16 {
                    let Some(state) = section.block(x, y, z) else {
                        continue;
                    };
                    mesh.statistics.positions_visited =
                        mesh.statistics.positions_visited.saturating_add(1);
                    let models = resources.state(state);
                    if models.parts.is_empty() && models.fluid.is_none() {
                        mesh.statistics.air_skipped = mesh.statistics.air_skipped.saturating_add(1);
                        continue;
                    }
                    mesh.statistics.non_air_blocks =
                        mesh.statistics.non_air_blocks.saturating_add(1);
                    let world_x = coordinate.x.saturating_mul(16) + i32::from(x);
                    let world_y = geometry.min_y
                        + i32::try_from(section_index)
                            .unwrap_or(i32::MAX)
                            .saturating_mul(16)
                        + i32::from(y);
                    let world_z = coordinate.z.saturating_mul(16) + i32::from(z);
                    let full_cube_occlusion = if models.full_opaque_cube {
                        let mut occluded = [false; 6];
                        for direction in Direction::ALL {
                            mesh.statistics.neighbor_checks =
                                mesh.statistics.neighbor_checks.saturating_add(1);
                            occluded[direction.index()] = neighbor_fully_occludes(
                                chunks, geometry, resources, world_x, world_y, world_z, direction,
                            );
                        }
                        Some(occluded)
                    } else {
                        None
                    };
                    if full_cube_occlusion.is_some_and(|occluded| occluded.into_iter().all(|v| v)) {
                        mesh.statistics.fully_occluded_fast_rejected = mesh
                            .statistics
                            .fully_occluded_fast_rejected
                            .saturating_add(1);
                        continue;
                    }
                    mesh.statistics.model_processed =
                        mesh.statistics.model_processed.saturating_add(1);
                    let quads_before = mesh.statistics.quads_emitted;
                    let mut variant_random =
                        ModelVariantRandom::at_position(world_x, world_y, world_z);
                    let mut selected_models = Vec::with_capacity(models.parts.len());
                    for part in &models.parts {
                        mesh.statistics.model_selections =
                            mesh.statistics.model_selections.saturating_add(1);
                        let Some(model) = select_model(part, &mut variant_random) else {
                            continue;
                        };
                        selected_models.push(model);
                    }
                    if let Some(fluid) = models.fluid {
                        if debug.mesh_log
                            && mesh.fluid_debug.len() < MAX_FLUID_DEBUG_CELLS_PER_MESH
                            && debug_origin.is_some_and(|origin| {
                                world_x.abs_diff(origin[0]) <= u32::from(debug.radius)
                                    && world_y.abs_diff(origin[1]) <= u32::from(debug.radius)
                                    && world_z.abs_diff(origin[2]) <= u32::from(debug.radius)
                            })
                        {
                            mesh.active_fluid_debug = Some(FluidCellDiagnostic {
                                coordinate: [world_x, world_y, world_z],
                                runtime_state: state,
                                kind: fluid.kind,
                                level: fluid.level,
                                falling: fluid.falling,
                                source: fluid.level == 0 && !fluid.falling,
                                heights: fluid_surface_heights(
                                    chunks, geometry, resources, fluid.kind, world_x, world_y,
                                    world_z,
                                ),
                                sides: [None; 4],
                                quads: Vec::new(),
                                quads_truncated: false,
                            });
                        }
                        push_fluid(
                            &mut mesh,
                            chunks,
                            geometry,
                            resources,
                            biomes,
                            world_x,
                            world_y,
                            world_z,
                            fluid,
                            &selected_models,
                        )?;
                        if let Some(record) = mesh.active_fluid_debug.take() {
                            mesh.fluid_debug.push(record);
                        }
                    }
                    for model in selected_models {
                        for face in &model.faces {
                            let cullface = face.cullface.map(|direction| {
                                rotate_blockstate_direction(
                                    direction,
                                    model.x_rotation,
                                    model.y_rotation,
                                )
                            });
                            let occluded = cullface.is_some_and(|direction| {
                                full_cube_occlusion.map_or_else(
                                    || {
                                        mesh.statistics.neighbor_checks =
                                            mesh.statistics.neighbor_checks.saturating_add(1);
                                        neighbor_fully_occludes(
                                            chunks, geometry, resources, world_x, world_y, world_z,
                                            direction,
                                        )
                                    },
                                    |occluded| occluded[direction.index()],
                                )
                            });
                            if occluded {
                                continue;
                            }
                            push_model_face(
                                &mut mesh, chunks, geometry, resources, biomes, world_x, world_y,
                                world_z, model, face,
                            )?;
                        }
                    }
                    if mesh.statistics.quads_emitted > quads_before {
                        mesh.statistics.geometry_emitting =
                            mesh.statistics.geometry_emitting.saturating_add(1);
                    }
                }
            }
        }
    }
    mesh.statistics.cpu_time = started.elapsed();
    Ok(mesh)
}

pub(crate) fn log_fluid_mesh_diagnostics(
    mesh: &ChunkMesh,
    remaining_cells: &AtomicUsize,
    chunk: ChunkCoordinate,
    generation: u64,
    revision: u64,
) -> usize {
    let mut logged = 0;
    for record in &mesh.fluid_debug {
        if remaining_cells
            .fetch_update(
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
                |remaining| remaining.checked_sub(1),
            )
            .is_err()
        {
            break;
        }
        logged += 1;
        tracing::debug!(
            target: "render::fluid_mesh",
            chunk_x = chunk.x,
            chunk_z = chunk.z,
            generation,
            revision,
            coordinate = ?record.coordinate,
            runtime_state = record.runtime_state.0,
            kind = ?record.kind,
            level = record.level,
            falling = record.falling,
            source = record.source,
            heights = ?record.heights,
            north = ?record.sides[0],
            south = ?record.sides[1],
            east = ?record.sides[2],
            west = ?record.sides[3],
            emitted_quads = record.quads.len(),
            quads_truncated = record.quads_truncated,
            final_vertex_count = mesh.vertices.len(),
            final_opaque_indices = mesh.indices.len(),
            final_translucent_indices = mesh.translucent_indices.len(),
            final_layered_translucent_indices = mesh.layered_translucent_indices.len(),
            "bounded final fluid mesh diagnostic"
        );
        for quad in &record.quads {
            tracing::debug!(
                target: "render::fluid_mesh",
                coordinate = ?record.coordinate,
                direction = ?quad.direction,
                clipped = quad.clipped,
                positions = ?quad.positions,
                uvs = ?quad.uvs,
                base_vertex = quad.base_vertex,
                forward_indices = ?quad.forward_indices,
                reverse_indices = ?quad.reverse_indices,
                batch = ?quad.batch,
                invariant = ?quad.invariant,
                "final fluid quad selected for chunk batch upload"
            );
            if let Err(violation) = quad.invariant {
                tracing::warn!(
                    target: "render::fluid_mesh",
                    coordinate = ?record.coordinate,
                    ?quad.direction,
                    ?violation,
                    "fluid final-mesh invariant failed"
                );
            }
        }
    }
    logged
}

fn neighbor_fully_occludes(
    chunks: &BTreeMap<ChunkCoordinate, Arc<Chunk>>,
    geometry: DimensionGeometry,
    resources: &BlockResources,
    x: i32,
    y: i32,
    z: i32,
    direction: Direction,
) -> bool {
    let offset = direction.offset();
    block_at(
        chunks,
        geometry,
        x + offset[0],
        y + offset[1],
        z + offset[2],
    )
    .is_some_and(|neighbor| resources.state(neighbor).full_opaque_cube)
}

fn block_at(
    chunks: &BTreeMap<ChunkCoordinate, Arc<Chunk>>,
    geometry: DimensionGeometry,
    x: i32,
    y: i32,
    z: i32,
) -> Option<RuntimeBlockStateId> {
    let relative_y = y.checked_sub(geometry.min_y)?;
    if relative_y < 0 || u32::try_from(relative_y).ok()? >= geometry.height {
        return None;
    }
    let coordinate = ChunkCoordinate::new(x.div_euclid(16), z.div_euclid(16));
    let chunk = chunks.get(&coordinate)?;
    let section = chunk.sections.get(usize::try_from(relative_y / 16).ok()?)?;
    section.block(
        u8::try_from(x.rem_euclid(16)).ok()?,
        u8::try_from(relative_y.rem_euclid(16)).ok()?,
        u8::try_from(z.rem_euclid(16)).ok()?,
    )
}

#[allow(clippy::too_many_arguments)]
fn push_model_face(
    mesh: &mut ChunkMesh,
    chunks: &BTreeMap<ChunkCoordinate, Arc<Chunk>>,
    geometry: DimensionGeometry,
    resources: &BlockResources,
    biomes: &[RuntimeBiome],
    x: i32,
    y: i32,
    z: i32,
    model: &ModelApplication,
    face: &crate::block_resources::ModelFace,
) -> Result<(), MeshError> {
    if (mesh.indices.len()
        + mesh.translucent_indices.len()
        + mesh.layered_translucent_indices.len())
        / 6
        >= MAX_CHUNK_MESH_FACES
    {
        return Err(MeshError::FaceLimit {
            max: MAX_CHUNK_MESH_FACES,
        });
    }
    let base = u32::try_from(mesh.vertices.len()).map_err(|_| MeshError::IndexOverflow)?;
    let region = face.atlas_region;
    let tint_base = tint_at(resources, biomes, chunks, geometry, face.tint_kind, x, y, z);
    let emissive = resources
        .state(block_at(chunks, geometry, x, y, z).unwrap_or(RuntimeBlockStateId(u32::MAX)))
        .emissive;
    let mut uvs = face.uv;
    if model.uvlock {
        uvs = uvlock_uvs(uvs, face.direction, model.x_rotation, model.y_rotation);
    }
    let state = block_at(chunks, geometry, x, y, z).unwrap_or(RuntimeBlockStateId(u32::MAX));
    let offset = model_offset(resources.state(state).model_offset, x, z);
    for (corner, uv) in face.corners.into_iter().zip(uvs) {
        let corner = rotate_blockstate_corner(corner, model.x_rotation, model.y_rotation);
        let ambient_occlusion = if emissive || !model.ambient_occlusion {
            1.0
        } else {
            vertex_ambient_occlusion(chunks, geometry, resources, x, y, z, face.direction, corner)
        };
        let light = if emissive {
            1.0
        } else if face.cullface.is_none() {
            // Unculled/internal model quads (including shade=false crossed
            // vegetation) use the block's own packed light. Sampling the
            // nominal face neighbour made the two sides of one zero-thickness
            // plane pick unrelated light cells.
            sample_light(chunks, geometry, x, y, z)
        } else {
            vertex_face_light(chunks, geometry, x, y, z, face.direction, corner)
        };
        mesh.vertices.push(TerrainVertex {
            position: [
                x as f32 + corner[0] + offset[0],
                y as f32 + corner[1] + offset[1],
                z as f32 + corner[2] + offset[2],
            ],
            uv: [
                region.min[0] + (region.max[0] - region.min[0]) * uv[0],
                region.min[1] + (region.max[1] - region.min[1]) * uv[1],
            ],
            tint: tint_base.map(|value| value * face.shade * light * ambient_occlusion),
            layer: match face.render_layer {
                RenderLayer::Opaque => 0,
                RenderLayer::Cutout => 1,
                RenderLayer::Translucent | RenderLayer::LayeredTranslucent => 2,
            },
        });
    }
    let target = match face.render_layer {
        RenderLayer::Opaque | RenderLayer::Cutout => &mut mesh.indices,
        RenderLayer::Translucent => &mut mesh.translucent_indices,
        RenderLayer::LayeredTranslucent => &mut mesh.layered_translucent_indices,
    };
    target.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    mesh.statistics.quads_emitted = mesh.statistics.quads_emitted.saturating_add(1);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn vertex_ambient_occlusion(
    chunks: &BTreeMap<ChunkCoordinate, Arc<Chunk>>,
    geometry: DimensionGeometry,
    resources: &BlockResources,
    x: i32,
    y: i32,
    z: i32,
    direction: Direction,
    corner: [f32; 3],
) -> f32 {
    let normal = direction.offset();
    let sign = |value: f32| if value < 0.5 { -1 } else { 1 };
    let (first, second) = match direction {
        Direction::Up | Direction::Down => ([sign(corner[0]), 0, 0], [0, 0, sign(corner[2])]),
        Direction::North | Direction::South => ([sign(corner[0]), 0, 0], [0, sign(corner[1]), 0]),
        Direction::East | Direction::West => ([0, 0, sign(corner[2])], [0, sign(corner[1]), 0]),
    };
    let occupied = |offset: [i32; 3]| {
        block_at(
            chunks,
            geometry,
            x + offset[0],
            y + offset[1],
            z + offset[2],
        )
        .is_some_and(|state| resources.state(state).full_opaque_cube)
    };
    let first_occupied = occupied([
        normal[0] + first[0],
        normal[1] + first[1],
        normal[2] + first[2],
    ]);
    let second_occupied = occupied([
        normal[0] + second[0],
        normal[1] + second[1],
        normal[2] + second[2],
    ]);
    let corner_occupied = occupied([
        normal[0] + first[0] + second[0],
        normal[1] + first[1] + second[1],
        normal[2] + first[2] + second[2],
    ]);
    ambient_occlusion_factor(first_occupied, second_occupied, corner_occupied)
}

fn ambient_occlusion_factor(first: bool, second: bool, corner: bool) -> f32 {
    if first && second {
        0.55
    } else {
        1.0 - 0.15 * f32::from(u8::from(first) + u8::from(second) + u8::from(corner))
    }
}

const BIOME_BLEND_RADIUS: i32 = 2;

#[allow(clippy::too_many_arguments)]
fn tint_at(
    resources: &BlockResources,
    biomes: &[RuntimeBiome],
    chunks: &BTreeMap<ChunkCoordinate, Arc<Chunk>>,
    geometry: DimensionGeometry,
    kind: TintKind,
    x: i32,
    y: i32,
    z: i32,
) -> [f32; 3] {
    if kind == TintKind::None {
        return [1.0; 3];
    }
    if let TintKind::Fixed(color) = kind {
        return rgb(color);
    }
    let mut total = [0_u64; 3];
    let mut samples = 0_u64;
    for dz in -BIOME_BLEND_RADIUS..=BIOME_BLEND_RADIUS {
        for dx in -BIOME_BLEND_RADIUS..=BIOME_BLEND_RADIUS {
            let Some(raw_id) = biome_at(chunks, geometry, x + dx, y, z + dz) else {
                continue;
            };
            let Some(biome) = usize::try_from(raw_id.0)
                .ok()
                .and_then(|index| biomes.get(index))
                .filter(|biome| biome.raw_id == raw_id.0)
            else {
                continue;
            };
            let color = match kind {
                TintKind::Grass => grass_color(resources, biome, x + dx, z + dz),
                TintKind::Foliage => biome.foliage_color.unwrap_or_else(|| {
                    climate_color(
                        &resources.foliage_colormap,
                        biome.temperature,
                        biome.downfall,
                    )
                }),
                TintKind::DryFoliage => biome.dry_foliage_color.unwrap_or_else(|| {
                    climate_color(
                        &resources.dry_foliage_colormap,
                        biome.temperature,
                        biome.downfall,
                    )
                }),
                TintKind::Water => biome.water_color,
                TintKind::None | TintKind::Fixed(_) => 0x00ff_ffff,
            };
            total[0] += u64::from((color >> 16) & 0xff);
            total[1] += u64::from((color >> 8) & 0xff);
            total[2] += u64::from(color & 0xff);
            samples += 1;
        }
    }
    if samples == 0 {
        return match kind {
            TintKind::Water => rgb(0x3f76e4),
            TintKind::Foliage => rgb(0x48b518),
            TintKind::DryFoliage => rgb(0x9e814d),
            TintKind::Grass => rgb(0x7fb238),
            TintKind::None => [1.0; 3],
            TintKind::Fixed(color) => rgb(color),
        };
    }
    let averaged =
        ((total[0] / samples) << 16) | ((total[1] / samples) << 8) | (total[2] / samples);
    rgb(u32::try_from(averaged).unwrap_or(0x00ff_ffff))
}

fn grass_color(resources: &BlockResources, biome: &RuntimeBiome, x: i32, z: i32) -> u32 {
    let base = biome.grass_color.unwrap_or_else(|| {
        climate_color(&resources.grass_colormap, biome.temperature, biome.downfall)
    });
    match biome.grass_color_modifier {
        cubic_world::GrassColorModifier::None => base,
        cubic_world::GrassColorModifier::DarkForest => ((base & 0x00fe_fefe) + 0x0028_3e16) >> 1,
        cubic_world::GrassColorModifier::Swamp => {
            if position_noise(x, z) < 0 {
                0x4c763c
            } else {
                0x6a7039
            }
        }
    }
}

fn position_noise(x: i32, z: i32) -> i64 {
    let seed = i64::from(x).wrapping_mul(3_129_871) ^ i64::from(z).wrapping_mul(116_129_781);
    seed.wrapping_mul(seed.wrapping_mul(42_317_861) + 11)
}

fn climate_color(colormap: &[u32], temperature: f32, downfall: f32) -> u32 {
    let temperature = temperature.clamp(0.0, 1.0);
    let humidity = downfall.clamp(0.0, 1.0) * temperature;
    let x = ((1.0 - temperature) * 255.0).round() as usize;
    let y = ((1.0 - humidity) * 255.0).round() as usize;
    colormap.get(y * 256 + x).copied().unwrap_or(0x7fb238)
}

fn rgb(color: u32) -> [f32; 3] {
    [
        ((color >> 16) & 0xff) as f32 / 255.0,
        ((color >> 8) & 0xff) as f32 / 255.0,
        (color & 0xff) as f32 / 255.0,
    ]
}

fn biome_at(
    chunks: &BTreeMap<ChunkCoordinate, Arc<Chunk>>,
    geometry: DimensionGeometry,
    x: i32,
    y: i32,
    z: i32,
) -> Option<RuntimeBiomeId> {
    let relative_y = y.checked_sub(geometry.min_y)?;
    if relative_y < 0 || u32::try_from(relative_y).ok()? >= geometry.height {
        return None;
    }
    let coordinate = ChunkCoordinate::new(x.div_euclid(16), z.div_euclid(16));
    let chunk = chunks.get(&coordinate)?;
    let section = chunk.sections.get(usize::try_from(relative_y / 16).ok()?)?;
    section.biome(
        u8::try_from(x.rem_euclid(16) / 4).ok()?,
        u8::try_from(relative_y.rem_euclid(16) / 4).ok()?,
        u8::try_from(z.rem_euclid(16) / 4).ok()?,
    )
}

#[allow(clippy::too_many_arguments)]
fn vertex_face_light(
    chunks: &BTreeMap<ChunkCoordinate, Arc<Chunk>>,
    geometry: DimensionGeometry,
    x: i32,
    y: i32,
    z: i32,
    direction: Direction,
    corner: [f32; 3],
) -> f32 {
    let normal = direction.offset();
    let sign = |value: f32| if value < 0.5 { -1 } else { 1 };
    let (first, second) = match direction {
        Direction::Up | Direction::Down => ([sign(corner[0]), 0, 0], [0, 0, sign(corner[2])]),
        Direction::North | Direction::South => ([sign(corner[0]), 0, 0], [0, sign(corner[1]), 0]),
        Direction::East | Direction::West => ([0, 0, sign(corner[2])], [0, sign(corner[1]), 0]),
    };
    [
        normal,
        [
            normal[0] + first[0],
            normal[1] + first[1],
            normal[2] + first[2],
        ],
        [
            normal[0] + second[0],
            normal[1] + second[1],
            normal[2] + second[2],
        ],
        [
            normal[0] + first[0] + second[0],
            normal[1] + first[1] + second[1],
            normal[2] + first[2] + second[2],
        ],
    ]
    .into_iter()
    .map(|offset| {
        sample_light(
            chunks,
            geometry,
            x + offset[0],
            y + offset[1],
            z + offset[2],
        )
    })
    .fold(0.0, f32::max)
}

fn sample_light(
    chunks: &BTreeMap<ChunkCoordinate, Arc<Chunk>>,
    geometry: DimensionGeometry,
    x: i32,
    y: i32,
    z: i32,
) -> f32 {
    let Some(relative_y) = y.checked_sub(geometry.min_y) else {
        return 1.0;
    };
    if relative_y < 0
        || u32::try_from(relative_y)
            .ok()
            .is_none_or(|value| value >= geometry.height)
    {
        return 1.0;
    }
    let coordinate = ChunkCoordinate::new(x.div_euclid(16), z.div_euclid(16));
    let Some(chunk) = chunks.get(&coordinate) else {
        return 1.0;
    };
    let section = usize::try_from(relative_y / 16).unwrap_or(0);
    let local_x = u8::try_from(x.rem_euclid(16)).unwrap_or(0);
    let local_y = u8::try_from(relative_y.rem_euclid(16)).unwrap_or(0);
    let local_z = u8::try_from(z.rem_euclid(16)).unwrap_or(0);
    let sky = chunk.light.sky(section, local_x, local_y, local_z);
    let block = chunk.light.block(section, local_x, local_y, local_z);
    match (sky, block) {
        (None, None) => 1.0,
        _ => f32::from(sky.unwrap_or(0).max(block.unwrap_or(0))) / 15.0 * 0.85 + 0.15,
    }
}

#[allow(clippy::too_many_arguments)]
fn push_fluid(
    mesh: &mut ChunkMesh,
    chunks: &BTreeMap<ChunkCoordinate, Arc<Chunk>>,
    geometry: DimensionGeometry,
    resources: &BlockResources,
    biomes: &[RuntimeBiome],
    x: i32,
    y: i32,
    z: i32,
    fluid: cubic_world::FluidState,
    selected_models: &[&ModelApplication],
) -> Result<(), MeshError> {
    let prefix = match fluid.kind {
        FluidKind::Water => "water",
        FluidKind::Lava => "lava",
    };
    let still = resources
        .atlas
        .region(&format!("minecraft:block/{prefix}_still"));
    let flowing = resources
        .atlas
        .region(&format!("minecraft:block/{prefix}_flow"));
    let layer = if fluid.kind == FluidKind::Water {
        RenderLayer::Translucent
    } else {
        RenderLayer::Opaque
    };
    let tint_kind = if fluid.kind == FluidKind::Water {
        TintKind::Water
    } else {
        TintKind::None
    };
    let heights = fluid_surface_heights(chunks, geometry, resources, fluid.kind, x, y, z);
    let flow = fluid_surface_flow(chunks, geometry, resources, fluid.kind, x, y, z);
    let flowing_surface = fluid_top_uses_flowing_texture(flow);
    let render_bottom = !same_fluid(chunks, geometry, resources, fluid.kind, x, y - 1, z);
    // FluidRenderer raises the shared lower edge by the same epsilon as its
    // bottom face only when that face is emitted. Keeping the bottom at Y=0
    // while emitting an independently offset bottom quad leaves a visible
    // sliver under an oblique/sloped side.
    let side_bottom = if render_bottom {
        FLUID_FACE_EPSILON
    } else {
        0.0
    };
    if !same_fluid(chunks, geometry, resources, fluid.kind, x, y + 1, z) {
        let top_uvs = if flowing_surface {
            flowing_top_uvs(flow)
        } else {
            FLUID_QUAD_UVS
        };
        push_clipped_fluid_top(
            mesh,
            chunks,
            geometry,
            resources,
            biomes,
            x,
            y,
            z,
            heights,
            if flowing_surface { flowing } else { still },
            top_uvs,
            tint_kind,
            layer,
            fluid.kind == FluidKind::Lava,
            selected_models,
        )?;
    }
    let sides = [
        (Direction::North, [0, 0, -1]),
        (Direction::South, [0, 0, 1]),
        (Direction::West, [-1, 0, 0]),
        (Direction::East, [1, 0, 0]),
    ];
    for (direction, offset) in sides {
        let shared_with_neighbor = same_fluid(
            chunks,
            geometry,
            resources,
            fluid.kind,
            x + offset[0],
            y,
            z + offset[2],
        );
        let [left, right] = fluid_side_height_indices(direction);
        let neighbor_occludes = !shared_with_neighbor
            && neighbor_fully_occludes_fluid_side(
                chunks,
                geometry,
                resources,
                x,
                y,
                z,
                direction,
                heights[left].max(heights[right]),
            );
        let quads_before = mesh
            .active_fluid_debug
            .as_ref()
            .map_or(0, |record| record.quads.len());
        push_clipped_fluid_side(
            mesh,
            chunks,
            geometry,
            resources,
            biomes,
            x,
            y,
            z,
            direction,
            heights.map(|height| height as f32),
            side_bottom,
            !shared_with_neighbor && !neighbor_occludes,
            flowing,
            tint_kind,
            layer,
            fluid.kind == FluidKind::Lava,
            selected_models,
        )?;
        let emitted = mesh
            .active_fluid_debug
            .as_ref()
            .map_or(0, |record| record.quads.len().saturating_sub(quads_before));
        set_fluid_side_decision(
            mesh,
            direction,
            if emitted == 0 {
                if shared_with_neighbor {
                    FluidSideDecision::SameFluid
                } else if neighbor_occludes {
                    FluidSideDecision::NeighborOcclusion
                } else {
                    FluidSideDecision::RemovedByClipping
                }
            } else {
                FluidSideDecision::Emitted { subquads: emitted }
            },
        );
    }
    if render_bottom {
        push_fluid_quad(
            mesh,
            chunks,
            geometry,
            resources,
            biomes,
            x,
            y,
            z,
            Direction::Down,
            [
                [0.0, FLUID_FACE_EPSILON, 1.0],
                [0.0, FLUID_FACE_EPSILON, 0.0],
                [1.0, FLUID_FACE_EPSILON, 0.0],
                [1.0, FLUID_FACE_EPSILON, 1.0],
            ],
            still,
            FLUID_QUAD_UVS,
            tint_kind,
            layer,
            fluid.kind == FluidKind::Lava,
            false,
            false,
        )?;
    }
    Ok(())
}

fn set_fluid_side_decision(
    mesh: &mut ChunkMesh,
    direction: Direction,
    decision: FluidSideDecision,
) {
    let index = match direction {
        Direction::North => 0,
        Direction::South => 1,
        Direction::East => 2,
        Direction::West => 3,
        Direction::Down | Direction::Up => return,
    };
    if let Some(record) = &mut mesh.active_fluid_debug {
        record.sides[index] = Some(decision);
    }
}

#[cfg(test)]
fn fluid_side_corners(direction: Direction, heights: [f32; 4], bottom: f32) -> [[f32; 3]; 4] {
    let [left, right] = fluid_side_height_indices(direction);
    // FluidRenderer supplies each side to its QUADS consumer in the literal
    // order top-left, top-right, bottom-right, bottom-left. This ordering is
    // significant: the fixed quad index expansion chooses the top-left to
    // bottom-right diagonal for the outward face and the opposite diagonal
    // for the reverse face. A cyclically shifted vertex order preserves the
    // outline and winding but swaps those diagonals on a sloped trapezoid.
    match direction {
        Direction::North => [
            [0.0, heights[left], FLUID_FACE_EPSILON],
            [1.0, heights[right], FLUID_FACE_EPSILON],
            [1.0, bottom, FLUID_FACE_EPSILON],
            [0.0, bottom, FLUID_FACE_EPSILON],
        ],
        Direction::South => [
            [1.0, heights[right], 1.0 - FLUID_FACE_EPSILON],
            [0.0, heights[left], 1.0 - FLUID_FACE_EPSILON],
            [0.0, bottom, 1.0 - FLUID_FACE_EPSILON],
            [1.0, bottom, 1.0 - FLUID_FACE_EPSILON],
        ],
        Direction::West => [
            [FLUID_FACE_EPSILON, heights[right], 1.0],
            [FLUID_FACE_EPSILON, heights[left], 0.0],
            [FLUID_FACE_EPSILON, bottom, 0.0],
            [FLUID_FACE_EPSILON, bottom, 1.0],
        ],
        Direction::East => [
            [1.0 - FLUID_FACE_EPSILON, heights[right], 0.0],
            [1.0 - FLUID_FACE_EPSILON, heights[left], 1.0],
            [1.0 - FLUID_FACE_EPSILON, bottom, 1.0],
            [1.0 - FLUID_FACE_EPSILON, bottom, 0.0],
        ],
        Direction::Down | Direction::Up => [[0.0; 3]; 4],
    }
}

fn fluid_side_height_indices(direction: Direction) -> [usize; 2] {
    // Left/right are expressed in the emitted quad's visible orientation.
    match direction {
        Direction::North => [0, 3],
        Direction::South => [1, 2],
        Direction::West => [0, 1],
        Direction::East => [2, 3],
        Direction::Down | Direction::Up => [0, 0],
    }
}

#[allow(clippy::too_many_arguments)]
fn neighbor_fully_occludes_fluid_side(
    chunks: &BTreeMap<ChunkCoordinate, Arc<Chunk>>,
    geometry: DimensionGeometry,
    resources: &BlockResources,
    x: i32,
    y: i32,
    z: i32,
    direction: Direction,
    maximum_height: f64,
) -> bool {
    if maximum_height <= 0.0 {
        return true;
    }
    let offset = direction.offset();
    let neighbor_x = x + offset[0];
    let neighbor_y = y + offset[1];
    let neighbor_z = z + offset[2];
    let Some(state) = block_at(chunks, geometry, neighbor_x, neighbor_y, neighbor_z) else {
        return false;
    };
    let models = resources.state(state);
    if models.full_opaque_cube {
        return true;
    }

    let opposite = match direction {
        Direction::North => Direction::South,
        Direction::South => Direction::North,
        Direction::West => Direction::East,
        Direction::East => Direction::West,
        Direction::Down => Direction::Up,
        Direction::Up => Direction::Down,
    };
    let mut random = ModelVariantRandom::at_position(neighbor_x, neighbor_y, neighbor_z);
    let mut covering_boxes = Vec::new();
    for part in &models.parts {
        let Some(model) = select_model(part, &mut random) else {
            continue;
        };
        if !model
            .faces
            .iter()
            .any(|face| face.direction == opposite && face.render_layer == RenderLayer::Opaque)
        {
            continue;
        }
        covering_boxes.extend(
            model
                .solid_boxes
                .iter()
                .copied()
                .filter(|bounds| match direction {
                    Direction::North => bounds[1][2] >= 1.0 - 1.0e-6,
                    Direction::South => bounds[0][2] <= 1.0e-6,
                    Direction::West => bounds[1][0] >= 1.0 - 1.0e-6,
                    Direction::East => bounds[0][0] <= 1.0e-6,
                    Direction::Down | Direction::Up => false,
                }),
        );
    }
    fluid_side_rectangle_is_covered(&covering_boxes, direction, maximum_height as f32)
}

fn fluid_side_rectangle_is_covered(
    boxes: &[[[f32; 3]; 2]],
    direction: Direction,
    maximum_height: f32,
) -> bool {
    let mut horizontal = vec![0.0_f32, 1.0];
    let mut vertical = vec![0.0_f32, maximum_height.clamp(0.0, 1.0)];
    let rectangle = |bounds: &[[f32; 3]; 2]| {
        let [u0, u1] = fluid_side_horizontal_bounds(*bounds, direction);
        [u0, bounds[0][1], u1, bounds[1][1]]
    };
    for bounds in boxes {
        let [u0, y0, u1, y1] = rectangle(bounds);
        horizontal.extend([u0.clamp(0.0, 1.0), u1.clamp(0.0, 1.0)]);
        vertical.extend([y0.clamp(0.0, maximum_height), y1.clamp(0.0, maximum_height)]);
    }
    for values in [&mut horizontal, &mut vertical] {
        values.sort_by(f32::total_cmp);
        values.dedup_by(|left, right| (*left - *right).abs() < 1.0e-6);
    }
    horizontal.windows(2).all(|u| {
        vertical.windows(2).all(|y| {
            let center_u = (u[0] + u[1]) * 0.5;
            let center_y = (y[0] + y[1]) * 0.5;
            boxes.iter().any(|bounds| {
                let [u0, y0, u1, y1] = rectangle(bounds);
                center_u >= u0 - 1.0e-6
                    && center_u <= u1 + 1.0e-6
                    && center_y >= y0 - 1.0e-6
                    && center_y <= y1 + 1.0e-6
            })
        })
    }) && !boxes.is_empty()
}

#[allow(clippy::too_many_arguments)]
fn push_clipped_fluid_side(
    mesh: &mut ChunkMesh,
    chunks: &BTreeMap<ChunkCoordinate, Arc<Chunk>>,
    geometry: DimensionGeometry,
    resources: &BlockResources,
    biomes: &[RuntimeBiome],
    x: i32,
    y: i32,
    z: i32,
    direction: Direction,
    heights: [f32; 4],
    lower: f32,
    emit_outer_boundary: bool,
    region: crate::block_resources::AtlasRegion,
    tint_kind: TintKind,
    layer: RenderLayer,
    emissive: bool,
    models: &[&ModelApplication],
) -> Result<(), MeshError> {
    let boxes = selected_solid_boxes(models);
    let mut horizontal = vec![0.0_f32, 1.0];
    let mut normal = vec![0.0_f32, 1.0];
    let mut vertical = vec![lower, 1.0];
    for bounds in &boxes {
        horizontal.extend(fluid_side_horizontal_bounds(*bounds, direction));
        normal.extend(fluid_side_normal_bounds(*bounds, direction));
        vertical.extend([bounds[0][1], bounds[1][1]]);
    }
    for (values, minimum) in [
        (&mut horizontal, 0.0),
        (&mut normal, 0.0),
        (&mut vertical, lower),
    ] {
        values
            .iter_mut()
            .for_each(|value| *value = value.clamp(minimum, 1.0));
        values.sort_by(f32::total_cmp);
        values.dedup_by(|left, right| (*left - *right).abs() < 1.0e-6);
    }

    for t_pair in horizontal.windows(2) {
        for y_pair in vertical.windows(2) {
            let (t0, t1) = (t_pair[0], t_pair[1]);
            let (y0, y1) = (y_pair[0], y_pair[1]);
            let center_t = (t0 + t1) * 0.5;
            let center_y = (y0 + y1) * 0.5;
            for boundary_index in 0..normal.len() {
                let boundary = normal[boundary_index];
                let Some(inward_normal) =
                    fluid_side_inward_interval_center(&normal, direction, boundary_index)
                else {
                    continue;
                };
                if !fluid_side_sample_is_open(
                    &boxes,
                    direction,
                    heights,
                    center_t,
                    center_y,
                    inward_normal,
                ) {
                    continue;
                }
                let outward =
                    fluid_side_outward_interval_center(&normal, direction, boundary_index);
                let boundary_is_visible = outward.map_or(emit_outer_boundary, |outward_normal| {
                    fluid_side_sample_is_solid(
                        &boxes,
                        direction,
                        center_t,
                        center_y,
                        outward_normal,
                    )
                });
                if !boundary_is_visible {
                    continue;
                }

                let top0 = fluid_side_top_at(direction, heights, t0, boundary).min(y1);
                let top1 = fluid_side_top_at(direction, heights, t1, boundary).min(y1);
                if top0 <= y0 && top1 <= y0 {
                    continue;
                }
                let render_boundary = fluid_side_inset_boundary(direction, boundary);
                let corners = [
                    fluid_side_point(direction, t0, top0.max(y0), render_boundary),
                    fluid_side_point(direction, t1, top1.max(y0), render_boundary),
                    fluid_side_point(direction, t1, y0, render_boundary),
                    fluid_side_point(direction, t0, y0, render_boundary),
                ];
                // 26.1.2 maps only the first half of the 32px flowing sprite to
                // one block side. V is derived from physical height, so partial
                // and falling sides retain the same texel density.
                let uvs = fluid_side_uvs(t0, t1, y0, top0, top1);
                push_fluid_quad(
                    mesh,
                    chunks,
                    geometry,
                    resources,
                    biomes,
                    x,
                    y,
                    z,
                    direction,
                    corners,
                    region,
                    uvs,
                    tint_kind,
                    layer,
                    emissive,
                    true,
                    !boxes.is_empty(),
                )?;
            }
        }
    }
    Ok(())
}

fn fluid_side_inward_interval_center(
    boundaries: &[f32],
    direction: Direction,
    boundary_index: usize,
) -> Option<f32> {
    if fluid_side_outward_is_positive(direction) {
        boundary_index
            .checked_sub(1)
            .map(|index| (boundaries[index] + boundaries[boundary_index]) * 0.5)
    } else {
        boundaries
            .get(boundary_index + 1)
            .map(|next| (boundaries[boundary_index] + *next) * 0.5)
    }
}

fn fluid_side_outward_interval_center(
    boundaries: &[f32],
    direction: Direction,
    boundary_index: usize,
) -> Option<f32> {
    if fluid_side_outward_is_positive(direction) {
        boundaries
            .get(boundary_index + 1)
            .map(|next| (boundaries[boundary_index] + *next) * 0.5)
    } else {
        boundary_index
            .checked_sub(1)
            .map(|index| (boundaries[index] + boundaries[boundary_index]) * 0.5)
    }
}

fn fluid_side_outward_is_positive(direction: Direction) -> bool {
    matches!(direction, Direction::South | Direction::East)
}

fn fluid_side_sample_is_open(
    boxes: &[[[f32; 3]; 2]],
    direction: Direction,
    heights: [f32; 4],
    t: f32,
    y: f32,
    normal: f32,
) -> bool {
    y < fluid_side_top_at(direction, heights, t, normal)
        && !fluid_side_sample_is_solid(boxes, direction, t, y, normal)
}

fn fluid_side_sample_is_solid(
    boxes: &[[[f32; 3]; 2]],
    direction: Direction,
    t: f32,
    y: f32,
    normal: f32,
) -> bool {
    let point = fluid_side_point(direction, t, y, normal);
    boxes.iter().any(|bounds| {
        (0..3).all(|axis| {
            point[axis] > bounds[0][axis] + 1.0e-6 && point[axis] < bounds[1][axis] - 1.0e-6
        })
    })
}

fn fluid_side_top_at(direction: Direction, heights: [f32; 4], t: f32, normal: f32) -> f32 {
    let point = fluid_side_point(direction, t, 0.0, normal);
    fluid_height_at(heights.map(f64::from), point[0], point[2])
}

fn fluid_side_point(direction: Direction, t: f32, y: f32, normal: f32) -> [f32; 3] {
    match direction {
        Direction::North | Direction::South => {
            let x = if direction == Direction::North {
                t
            } else {
                1.0 - t
            };
            [x, y, normal]
        }
        Direction::West | Direction::East => {
            let z = if direction == Direction::East {
                t
            } else {
                1.0 - t
            };
            [normal, y, z]
        }
        Direction::Down | Direction::Up => [0.0, y, 0.0],
    }
}

fn fluid_side_inset_boundary(direction: Direction, boundary: f32) -> f32 {
    if fluid_side_outward_is_positive(direction) {
        (boundary - FLUID_FACE_EPSILON).max(0.0)
    } else {
        (boundary + FLUID_FACE_EPSILON).min(1.0)
    }
}

fn fluid_side_uvs(t0: f32, t1: f32, bottom: f32, top0: f32, top1: f32) -> [[f32; 2]; 4] {
    [
        [t0 * 0.5, (1.0 - top0) * 0.5],
        [t1 * 0.5, (1.0 - top1) * 0.5],
        [t1 * 0.5, (1.0 - bottom) * 0.5],
        [t0 * 0.5, (1.0 - bottom) * 0.5],
    ]
}

fn fluid_side_horizontal_bounds(bounds: [[f32; 3]; 2], direction: Direction) -> [f32; 2] {
    // `t` follows the literal top-left -> top-right order supplied to
    // FluidRenderer's quad consumer. Keep subdivision and occupancy tests in
    // that same basis; reflecting this range independently makes asymmetric
    // waterlogged geometry suppress the open half of a side instead of the
    // part occupied by the model.
    match direction {
        Direction::North => [bounds[0][0], bounds[1][0]],
        Direction::South => [1.0 - bounds[1][0], 1.0 - bounds[0][0]],
        Direction::West => [1.0 - bounds[1][2], 1.0 - bounds[0][2]],
        Direction::East => [bounds[0][2], bounds[1][2]],
        Direction::Down | Direction::Up => [0.0, 1.0],
    }
}

fn fluid_side_normal_bounds(bounds: [[f32; 3]; 2], direction: Direction) -> [f32; 2] {
    match direction {
        Direction::North | Direction::South => [bounds[0][2], bounds[1][2]],
        Direction::West | Direction::East => [bounds[0][0], bounds[1][0]],
        Direction::Down | Direction::Up => [0.0, 1.0],
    }
}

fn selected_solid_boxes(models: &[&ModelApplication]) -> Vec<[[f32; 3]; 2]> {
    models
        .iter()
        .flat_map(|model| model.solid_boxes.iter().copied())
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn push_clipped_fluid_top(
    mesh: &mut ChunkMesh,
    chunks: &BTreeMap<ChunkCoordinate, Arc<Chunk>>,
    geometry: DimensionGeometry,
    resources: &BlockResources,
    biomes: &[RuntimeBiome],
    x: i32,
    y: i32,
    z: i32,
    heights: [f64; 4],
    region: crate::block_resources::AtlasRegion,
    top_uvs: [[f32; 2]; 4],
    tint_kind: TintKind,
    layer: RenderLayer,
    emissive: bool,
    models: &[&ModelApplication],
) -> Result<(), MeshError> {
    let boxes = selected_solid_boxes(models);
    let mut xs = vec![0.0_f32, 1.0];
    let mut zs = vec![0.0_f32, 1.0];
    for bounds in &boxes {
        xs.extend([bounds[0][0].clamp(0.0, 1.0), bounds[1][0].clamp(0.0, 1.0)]);
        zs.extend([bounds[0][2].clamp(0.0, 1.0), bounds[1][2].clamp(0.0, 1.0)]);
    }
    xs.sort_by(f32::total_cmp);
    zs.sort_by(f32::total_cmp);
    xs.dedup_by(|left, right| (*left - *right).abs() < 1.0e-6);
    zs.dedup_by(|left, right| (*left - *right).abs() < 1.0e-6);
    for x_pair in xs.windows(2) {
        for z_pair in zs.windows(2) {
            let (x0, x1) = (x_pair[0], x_pair[1]);
            let (z0, z1) = (z_pair[0], z_pair[1]);
            if x1 - x0 <= 1.0e-6 || z1 - z0 <= 1.0e-6 {
                continue;
            }
            let center_x = (x0 + x1) * 0.5;
            let center_z = (z0 + z1) * 0.5;
            let center_height = fluid_height_at(heights, center_x, center_z);
            if fluid_top_cell_is_enclosed(&boxes, center_x, center_height, center_z) {
                continue;
            }
            let corners = [
                [
                    x0,
                    fluid_height_at(heights, x0, z0) - FLUID_FACE_EPSILON,
                    z0,
                ],
                [
                    x0,
                    fluid_height_at(heights, x0, z1) - FLUID_FACE_EPSILON,
                    z1,
                ],
                [
                    x1,
                    fluid_height_at(heights, x1, z1) - FLUID_FACE_EPSILON,
                    z1,
                ],
                [
                    x1,
                    fluid_height_at(heights, x1, z0) - FLUID_FACE_EPSILON,
                    z0,
                ],
            ];
            let uvs = [
                interpolate_uv(top_uvs, x0, z0),
                interpolate_uv(top_uvs, x0, z1),
                interpolate_uv(top_uvs, x1, z1),
                interpolate_uv(top_uvs, x1, z0),
            ];
            push_fluid_quad(
                mesh,
                chunks,
                geometry,
                resources,
                biomes,
                x,
                y,
                z,
                Direction::Up,
                corners,
                region,
                uvs,
                tint_kind,
                layer,
                emissive,
                false,
                !boxes.is_empty(),
            )?;
        }
    }
    Ok(())
}

fn fluid_top_cell_is_enclosed(boxes: &[[[f32; 3]; 2]], x: f32, height: f32, z: f32) -> bool {
    boxes.iter().any(|bounds| {
        x >= bounds[0][0] - 1.0e-6
            && x <= bounds[1][0] + 1.0e-6
            && z >= bounds[0][2] - 1.0e-6
            && z <= bounds[1][2] + 1.0e-6
            && height >= bounds[0][1] - 1.0e-6
            && height <= bounds[1][1] + 1.0e-6
    })
}

fn fluid_height_at(heights: [f64; 4], x: f32, z: f32) -> f32 {
    let x = f64::from(x);
    let z = f64::from(z);
    (((heights[0] * (1.0 - x) + heights[3] * x) * (1.0 - z)
        + (heights[1] * (1.0 - x) + heights[2] * x) * z) as f32)
        .max(0.0)
}

fn interpolate_uv(uvs: [[f32; 2]; 4], x: f32, z: f32) -> [f32; 2] {
    let near = [
        uvs[0][0] * (1.0 - x) + uvs[3][0] * x,
        uvs[0][1] * (1.0 - x) + uvs[3][1] * x,
    ];
    let far = [
        uvs[1][0] * (1.0 - x) + uvs[2][0] * x,
        uvs[1][1] * (1.0 - x) + uvs[2][1] * x,
    ];
    [
        near[0] * (1.0 - z) + far[0] * z,
        near[1] * (1.0 - z) + far[1] * z,
    ]
}

const FLUID_FACE_EPSILON: f32 = 0.001;
const _: () = assert!(FLUID_FACE_EPSILON > 0.0 && FLUID_FACE_EPSILON < 1.0 / 16.0);
const FLUID_QUAD_UVS: [[f32; 2]; 4] = [[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]];

fn model_offset(kind: ModelOffset, x: i32, z: i32) -> [f32; 3] {
    if kind == ModelOffset::None {
        return [0.0; 3];
    }
    let seed = model_position_seed(x, 0, z);
    let offset_x = ((((seed & 15) as f32 / 15.0) - 0.5) * 0.5).clamp(-0.25, 0.25);
    let offset_z = (((((seed >> 8) & 15) as f32 / 15.0) - 0.5) * 0.5).clamp(-0.25, 0.25);
    let offset_y = if kind == ModelOffset::Xyz {
        ((((seed >> 4) & 15) as f32 / 15.0) - 1.0) * 0.2
    } else {
        0.0
    };
    [offset_x, offset_y, offset_z]
}

fn flowing_top_uvs(flow: [f64; 2]) -> [[f32; 2]; 4] {
    if flow[0].abs() <= f64::EPSILON && flow[1].abs() <= f64::EPSILON {
        return FLUID_QUAD_UVS;
    }
    // Java 26.1.2 FluidRenderer uses atan2(flow.z, flow.x) - PI/2 and a
    // quarter-sprite radius around the sprite centre.
    let angle = flow[1].atan2(flow[0]) - std::f64::consts::FRAC_PI_2;
    let sin = angle.sin() as f32 * 0.25;
    let cos = angle.cos() as f32 * 0.25;
    [
        [0.5 - cos - sin, 0.5 - cos + sin],
        [0.5 - cos + sin, 0.5 + cos + sin],
        [0.5 + cos + sin, 0.5 + cos - sin],
        [0.5 + cos - sin, 0.5 - cos - sin],
    ]
}

fn fluid_surface_flow(
    chunks: &BTreeMap<ChunkCoordinate, Arc<Chunk>>,
    geometry: DimensionGeometry,
    resources: &BlockResources,
    kind: FluidKind,
    x: i32,
    y: i32,
    z: i32,
) -> [f64; 2] {
    let Some(center) =
        fluid_at(chunks, geometry, resources, x, y, z).filter(|fluid| fluid.kind == kind)
    else {
        return [0.0; 2];
    };
    let center_height = center.own_height();
    let mut flow = [0.0_f64; 2];
    for (dx, dz) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
        let neighbor = fluid_at(chunks, geometry, resources, x + dx, y, z + dz)
            .filter(|fluid| fluid.kind == kind);
        let difference = if let Some(neighbor) = neighbor {
            center_height - neighbor.own_height()
        } else {
            let state = block_at(chunks, geometry, x + dx, y, z + dz);
            if state.is_some_and(|state| resources.state(state).full_opaque_cube) {
                continue;
            }
            let Some(below) = fluid_at(chunks, geometry, resources, x + dx, y - 1, z + dz)
                .filter(|fluid| fluid.kind == kind)
            else {
                continue;
            };
            center_height - (below.own_height() - 8.0 / 9.0)
        };
        flow[0] += f64::from(dx) * difference;
        flow[1] += f64::from(dz) * difference;
    }
    let length = flow[0].hypot(flow[1]);
    if length > f64::EPSILON {
        flow[0] /= length;
        flow[1] /= length;
    }
    flow
}

fn fluid_top_uses_flowing_texture(flow: [f64; 2]) -> bool {
    // FluidRenderer selects the top sprite from the local horizontal flow
    // vector. Geometry height differences alone do not make a source beside
    // a wall an animated surface.
    flow[0].abs() > f64::EPSILON || flow[1].abs() > f64::EPSILON
}

#[allow(clippy::too_many_arguments)]
fn push_fluid_quad(
    mesh: &mut ChunkMesh,
    chunks: &BTreeMap<ChunkCoordinate, Arc<Chunk>>,
    geometry: DimensionGeometry,
    resources: &BlockResources,
    biomes: &[RuntimeBiome],
    x: i32,
    y: i32,
    z: i32,
    direction: Direction,
    corners: [[f32; 3]; 4],
    region: crate::block_resources::AtlasRegion,
    uvs: [[f32; 2]; 4],
    tint_kind: TintKind,
    layer: RenderLayer,
    emissive: bool,
    two_sided: bool,
    debug_clipped: bool,
) -> Result<(), MeshError> {
    if (mesh.indices.len()
        + mesh.translucent_indices.len()
        + mesh.layered_translucent_indices.len())
        / 6
        >= MAX_CHUNK_MESH_FACES
    {
        return Err(MeshError::FaceLimit {
            max: MAX_CHUNK_MESH_FACES,
        });
    }
    let base = u32::try_from(mesh.vertices.len()).map_err(|_| MeshError::IndexOverflow)?;
    let tint = tint_at(resources, biomes, chunks, geometry, tint_kind, x, y, z);
    let light = if emissive {
        1.0
    } else {
        // FluidRenderer uses the maximum packed light at the fluid cell and
        // the cell above for every face; it does not sample the horizontal
        // neighbour selected by the face direction.
        sample_light(chunks, geometry, x, y, z).max(sample_light(chunks, geometry, x, y + 1, z))
    };
    for (corner, uv) in corners.into_iter().zip(uvs) {
        mesh.vertices.push(TerrainVertex {
            position: [
                x as f32 + corner[0],
                y as f32 + corner[1],
                z as f32 + corner[2],
            ],
            uv: [
                region.min[0] + (region.max[0] - region.min[0]) * uv[0],
                region.min[1] + (region.max[1] - region.min[1]) * uv[1],
            ],
            tint: tint.map(|component| component * direction_shade_for_fluid(direction) * light),
            layer: debug_fluid_layer(
                match layer {
                    RenderLayer::Opaque => 0,
                    RenderLayer::Cutout => 1,
                    RenderLayer::Translucent | RenderLayer::LayeredTranslucent => 2,
                },
                mesh.debug_face_colors,
                direction,
                debug_clipped,
            ),
        });
    }
    let target = match layer {
        RenderLayer::Opaque | RenderLayer::Cutout => &mut mesh.indices,
        RenderLayer::Translucent => &mut mesh.translucent_indices,
        RenderLayer::LayeredTranslucent => &mut mesh.layered_translucent_indices,
    };
    let forward = [base, base + 1, base + 2, base, base + 2, base + 3];
    target.extend_from_slice(&forward);
    let reverse = two_sided.then_some([base + 3, base + 2, base + 1, base + 3, base + 1, base]);
    if two_sided {
        // FluidRenderer emits ordinary horizontal fluid sides in both
        // windings, except when it selects the half-transparent/leaves
        // overlay material. Cubic's back-face-culling pipeline therefore
        // shades exactly one copy for any viewing direction; these indices do
        // not double-blend the surface. Depth-writing translucent terrain
        // prevents a farther reverse boundary showing through a nearer one.
        if let Some(reverse) = reverse {
            target.extend_from_slice(&reverse);
        }
    }
    if let Some(record) = &mut mesh.active_fluid_debug {
        let positions = corners.map(|corner| {
            [
                x as f32 + corner[0],
                y as f32 + corner[1],
                z as f32 + corner[2],
            ]
        });
        let final_uvs = uvs.map(|uv| {
            [
                region.min[0] + (region.max[0] - region.min[0]) * uv[0],
                region.min[1] + (region.max[1] - region.min[1]) * uv[1],
            ]
        });
        let batch = match layer {
            RenderLayer::Opaque | RenderLayer::Cutout => FluidDebugBatch::Opaque,
            RenderLayer::Translucent => FluidDebugBatch::Translucent,
            RenderLayer::LayeredTranslucent => FluidDebugBatch::LayeredTranslucent,
        };
        let diagnostic = FluidQuadDiagnostic {
            direction,
            clipped: debug_clipped,
            positions,
            uvs: final_uvs,
            base_vertex: base,
            forward_indices: forward,
            reverse_indices: reverse,
            batch,
            invariant: validate_fluid_quad(direction, positions, final_uvs, base, forward, reverse),
        };
        if record.quads.len() < MAX_FLUID_DEBUG_QUADS_PER_CELL {
            record.quads.push(diagnostic);
        } else {
            record.quads_truncated = true;
        }
    }
    mesh.statistics.quads_emitted = mesh.statistics.quads_emitted.saturating_add(1);
    Ok(())
}

fn debug_fluid_layer(
    material_layer: u32,
    enabled: bool,
    direction: Direction,
    clipped: bool,
) -> u32 {
    if !enabled {
        return material_layer;
    }
    let face = match direction {
        Direction::Up => 1,
        Direction::North => 2,
        Direction::South => 3,
        Direction::East => 4,
        Direction::West => 5,
        Direction::Down => 6,
    };
    material_layer | (face << 8) | (u32::from(clipped) << 16)
}

fn validate_fluid_quad(
    direction: Direction,
    positions: [[f32; 3]; 4],
    uvs: [[f32; 2]; 4],
    base: u32,
    forward: [u32; 6],
    reverse: Option<[u32; 6]>,
) -> Result<(), FluidQuadInvariantViolation> {
    if positions
        .iter()
        .flatten()
        .chain(uvs.iter().flatten())
        .any(|value| !value.is_finite())
    {
        return Err(FluidQuadInvariantViolation::NonFinite);
    }
    let plane_axis = match direction {
        Direction::North | Direction::South => Some(2),
        Direction::East | Direction::West => Some(0),
        Direction::Down | Direction::Up => None,
    };
    if plane_axis.is_some_and(|axis| {
        positions
            .iter()
            .skip(1)
            .any(|position| (position[axis] - positions[0][axis]).abs() > 1.0e-6)
    }) {
        return Err(FluidQuadInvariantViolation::NonPlanar);
    }
    if plane_axis.is_some()
        && (positions[0][1] + 1.0e-6 < positions[3][1]
            || positions[1][1] + 1.0e-6 < positions[2][1]
            || (positions[2][1] - positions[3][1]).abs() > 1.0e-6)
    {
        return Err(FluidQuadInvariantViolation::InvertedHeight);
    }
    let area_squared = |a: [f32; 3], b: [f32; 3], c: [f32; 3]| {
        let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let cross = [
            ab[1] * ac[2] - ab[2] * ac[1],
            ab[2] * ac[0] - ab[0] * ac[2],
            ab[0] * ac[1] - ab[1] * ac[0],
        ];
        cross.into_iter().map(|value| value * value).sum::<f32>()
    };
    if area_squared(positions[0], positions[1], positions[2])
        + area_squared(positions[0], positions[2], positions[3])
        <= 1.0e-12
    {
        return Err(FluidQuadInvariantViolation::ZeroArea);
    }
    if forward
        .into_iter()
        .chain(reverse.into_iter().flatten())
        .any(|index| index < base || index >= base.saturating_add(4))
    {
        return Err(FluidQuadInvariantViolation::ForeignIndex);
    }
    Ok(())
}

fn direction_shade_for_fluid(direction: Direction) -> f32 {
    match direction {
        Direction::Up => 1.0,
        Direction::Down => 0.5,
        Direction::North | Direction::South => 0.8,
        Direction::East | Direction::West => 0.6,
    }
}

fn fluid_corner_height(
    chunks: &BTreeMap<ChunkCoordinate, Arc<Chunk>>,
    geometry: DimensionGeometry,
    resources: &BlockResources,
    kind: FluidKind,
    block: (i32, i32, i32),
    corner_offset: (i32, i32),
) -> f64 {
    let (x, y, z) = block;
    let (x_offset, z_offset) = corner_offset;
    let sample = |sample_x: i32, sample_z: i32| {
        fluid_sample_height(chunks, geometry, resources, kind, sample_x, y, sample_z)
    };
    let center = sample(x, z);
    let along_x = sample(x + x_offset, z);
    let along_z = sample(x, z + z_offset);
    if along_x >= 1.0 || along_z >= 1.0 {
        return 1.0;
    }
    let diagonal = if along_x > 0.0 || along_z > 0.0 {
        Some(sample(x + x_offset, z + z_offset))
    } else {
        None
    };
    weighted_fluid_height([Some(center), Some(along_x), Some(along_z), diagonal])
}

fn fluid_surface_heights(
    chunks: &BTreeMap<ChunkCoordinate, Arc<Chunk>>,
    geometry: DimensionGeometry,
    resources: &BlockResources,
    kind: FluidKind,
    x: i32,
    y: i32,
    z: i32,
) -> [f64; 4] {
    // FluidRenderer short-circuits the complete local surface when matching
    // fluid exists above. Averaging that full center independently at each
    // corner produced the observed falling-column pyramids and wedges.
    if fluid_sample_height(chunks, geometry, resources, kind, x, y, z) >= 1.0 {
        return [1.0; 4];
    }
    [
        fluid_corner_height(chunks, geometry, resources, kind, (x, y, z), (-1, -1)),
        fluid_corner_height(chunks, geometry, resources, kind, (x, y, z), (-1, 1)),
        fluid_corner_height(chunks, geometry, resources, kind, (x, y, z), (1, 1)),
        fluid_corner_height(chunks, geometry, resources, kind, (x, y, z), (1, -1)),
    ]
}

fn fluid_sample_height(
    chunks: &BTreeMap<ChunkCoordinate, Arc<Chunk>>,
    geometry: DimensionGeometry,
    resources: &BlockResources,
    kind: FluidKind,
    x: i32,
    y: i32,
    z: i32,
) -> f64 {
    if let Some(fluid) =
        fluid_at(chunks, geometry, resources, x, y, z).filter(|fluid| fluid.kind == kind)
    {
        return if same_fluid(chunks, geometry, resources, kind, x, y + 1, z) {
            1.0
        } else {
            fluid.own_height()
        };
    }
    if block_at(chunks, geometry, x, y, z)
        .is_some_and(|state| resources.state(state).fluid_surface_solid)
    {
        -1.0
    } else {
        0.0
    }
}

fn weighted_fluid_height(samples: [Option<f64>; 4]) -> f64 {
    let mut total = 0.0;
    let mut weight = 0.0;
    for height in samples
        .into_iter()
        .flatten()
        .filter(|height| *height >= 0.0)
    {
        let sample_weight = if height >= 0.8 { 10.0 } else { 1.0 };
        total += height * sample_weight;
        weight += sample_weight;
    }
    if weight == 0.0 { 0.0 } else { total / weight }
}

fn fluid_at(
    chunks: &BTreeMap<ChunkCoordinate, Arc<Chunk>>,
    geometry: DimensionGeometry,
    resources: &BlockResources,
    x: i32,
    y: i32,
    z: i32,
) -> Option<cubic_world::FluidState> {
    resources.state(block_at(chunks, geometry, x, y, z)?).fluid
}

fn same_fluid(
    chunks: &BTreeMap<ChunkCoordinate, Arc<Chunk>>,
    geometry: DimensionGeometry,
    resources: &BlockResources,
    kind: FluidKind,
    x: i32,
    y: i32,
    z: i32,
) -> bool {
    fluid_at(chunks, geometry, resources, x, y, z).is_some_and(|fluid| fluid.kind == kind)
}

fn select_model<'model>(
    part: &'model crate::block_resources::WeightedApplications,
    random: &mut ModelVariantRandom,
) -> Option<&'model ModelApplication> {
    if part.total_weight == 0 {
        return None;
    }
    let mut choice = random.next_int(part.total_weight)?;
    for (weight, model) in &part.entries {
        if choice < *weight {
            return Some(model);
        }
        choice -= *weight;
    }
    part.entries.last().map(|(_, model)| model)
}

/// Minecraft's block-model variant RNG: the standard 48-bit Java LCG seeded
/// from the block position. One RNG is reset per block and consumed once by
/// each weighted model group; block state and face direction are not seed data.
#[derive(Clone, Copy, Debug)]
struct ModelVariantRandom {
    seed: u64,
}

impl ModelVariantRandom {
    const MULTIPLIER: u64 = 25_214_903_917;
    const INCREMENT: u64 = 11;
    const MASK: u64 = (1_u64 << 48) - 1;

    fn at_position(x: i32, y: i32, z: i32) -> Self {
        let position_seed = model_position_seed(x, y, z);
        Self {
            seed: ((position_seed as u64) ^ Self::MULTIPLIER) & Self::MASK,
        }
    }

    fn next(&mut self, bits: u32) -> u32 {
        self.seed = self
            .seed
            .wrapping_mul(Self::MULTIPLIER)
            .wrapping_add(Self::INCREMENT)
            & Self::MASK;
        (self.seed >> (48 - bits)) as u32
    }

    fn next_int(&mut self, bound: u32) -> Option<u32> {
        let signed_bound = i32::try_from(bound).ok()?;
        if signed_bound <= 0 {
            return None;
        }
        if bound.is_power_of_two() {
            return Some(((u64::from(bound) * u64::from(self.next(31))) >> 31) as u32);
        }
        loop {
            let bits = self.next(31) as i32;
            let value = bits % signed_bound;
            if bits.wrapping_sub(value).wrapping_add(signed_bound - 1) >= 0 {
                return Some(value as u32);
            }
        }
    }
}

fn model_position_seed(x: i32, y: i32, z: i32) -> i64 {
    let mixed = i64::from(x.wrapping_mul(3_129_871))
        ^ i64::from(z).wrapping_mul(116_129_781)
        ^ i64::from(y);
    mixed
        .wrapping_mul(mixed)
        .wrapping_mul(42_317_861)
        .wrapping_add(mixed.wrapping_mul(11))
        >> 16
}

#[cfg(test)]
mod tests {
    use super::*;
    use cubic_world::{ChunkLightSummary, ChunkSection, PalettedContainer};

    fn chunk(coordinate: ChunkCoordinate, states: Vec<RuntimeBlockStateId>) -> Arc<Chunk> {
        chunk_with_biome(coordinate, states, RuntimeBiomeId(0))
    }

    fn chunk_with_biome(
        coordinate: ChunkCoordinate,
        states: Vec<RuntimeBlockStateId>,
        biome: RuntimeBiomeId,
    ) -> Arc<Chunk> {
        Arc::new(Chunk {
            coordinate,
            sections: vec![ChunkSection {
                non_empty_block_count: states.iter().filter(|state| state.0 != 0).count() as u16,
                fluid_count: 0,
                blocks: PalettedContainer::Direct { values: states },
                biomes: PalettedContainer::Single {
                    value: biome,
                    entries: 64,
                },
            }],
            heightmaps: vec![],
            block_entities: vec![],
            light: ChunkLightSummary::default(),
        })
    }

    fn biome(raw_id: u32, name: &str, water_color: u32) -> RuntimeBiome {
        RuntimeBiome {
            raw_id,
            identifier: cubic_version::MinecraftIdentifier::new(name).unwrap(),
            temperature: 0.5,
            downfall: 0.5,
            water_color,
            foliage_color: None,
            dry_foliage_color: None,
            grass_color: None,
            grass_color_modifier: cubic_world::GrassColorModifier::None,
        }
    }

    #[test]
    fn isolated_block_emits_six_faces_and_air_emits_none() {
        let mut states = vec![RuntimeBlockStateId(0); 4096];
        states[0] = RuntimeBlockStateId(7);
        let coord = ChunkCoordinate::new(0, 0);
        let chunks = BTreeMap::from([(coord, chunk(coord, states))]);
        let mesh = mesh_chunk(
            coord,
            &chunks,
            DimensionGeometry {
                min_y: 0,
                height: 16,
            },
            &BlockResources::synthetic([RuntimeBlockStateId(0)]),
        )
        .unwrap();
        assert_eq!(mesh.indices.len(), 36);
        for axis in 0..3 {
            let minimum = mesh
                .vertices
                .iter()
                .map(|vertex| vertex.position[axis])
                .fold(f32::INFINITY, f32::min);
            let maximum = mesh
                .vertices
                .iter()
                .map(|vertex| vertex.position[axis])
                .fold(f32::NEG_INFINITY, f32::max);
            assert_eq!((minimum, maximum), (0.0, 1.0));
        }
        let empty = BTreeMap::from([(coord, chunk(coord, vec![RuntimeBlockStateId(0); 4096]))]);
        assert!(
            mesh_chunk(
                coord,
                &empty,
                DimensionGeometry {
                    min_y: 0,
                    height: 16
                },
                &BlockResources::synthetic([RuntimeBlockStateId(0)])
            )
            .unwrap()
            .indices
            .is_empty()
        );
    }

    #[test]
    fn biome_water_tint_blends_across_chunk_boundaries_and_negative_coordinates() {
        let states = vec![RuntimeBlockStateId(0); 4096];
        let left = ChunkCoordinate::new(-1, 0);
        let right = ChunkCoordinate::new(0, 0);
        let chunks = BTreeMap::from([
            (
                left,
                chunk_with_biome(left, states.clone(), RuntimeBiomeId(0)),
            ),
            (right, chunk_with_biome(right, states, RuntimeBiomeId(1))),
        ]);
        let biomes = [
            biome(0, "cubic:red", 0xff0000),
            biome(1, "cubic:blue", 0x0000ff),
        ];
        let tint = tint_at(
            &BlockResources::synthetic([RuntimeBlockStateId(0)]),
            &biomes,
            &chunks,
            DimensionGeometry {
                min_y: 0,
                height: 16,
            },
            TintKind::Water,
            -1,
            0,
            8,
        );
        assert!((tint[0] - (153.0 / 255.0)).abs() < 0.001);
        assert_eq!(tint[1], 0.0);
        assert!((tint[2] - (102.0 / 255.0)).abs() < 0.001);
        assert_eq!(
            tint_at(
                &BlockResources::synthetic([]),
                &[],
                &BTreeMap::new(),
                DimensionGeometry {
                    min_y: 0,
                    height: 16
                },
                TintKind::None,
                -17,
                0,
                -1,
            ),
            [1.0; 3]
        );
    }

    #[test]
    fn fluid_mesh_uses_surface_height_translucency_and_same_fluid_culling() {
        let state = RuntimeBlockStateId(7);
        let resources = BlockResources::synthetic_fluid(
            state,
            cubic_world::FluidState {
                kind: FluidKind::Water,
                level: 0,
                falling: false,
            },
        );
        let coordinate = ChunkCoordinate::new(0, 0);
        let mut states = vec![RuntimeBlockStateId(0); 4096];
        states[0] = state;
        let chunks = BTreeMap::from([(coordinate, chunk(coordinate, states.clone()))]);
        let mesh = mesh_chunk_with_biomes(
            coordinate,
            &chunks,
            DimensionGeometry {
                min_y: 0,
                height: 16,
            },
            &resources,
            &[],
        )
        .unwrap();
        assert!(mesh.indices.is_empty());
        assert_eq!(mesh.translucent_indices.len(), 60);
        // Vanilla shares a 1/1000-block lower edge between an exposed bottom
        // quad and all four side quads. The clipped-side subdivision must not
        // silently reintroduce Y=0 after the caller supplies that boundary.
        for side in 0..4 {
            let vertices = &mesh.vertices[4 + side * 4..8 + side * 4];
            assert!((vertices[2].position[1] - FLUID_FACE_EPSILON).abs() < 1.0e-7);
            assert!((vertices[3].position[1] - FLUID_FACE_EPSILON).abs() < 1.0e-7);
            assert_eq!(vertices[2].uv[1], vertices[3].uv[1]);
            let indices = &mesh.translucent_indices[6 + side * 12..18 + side * 12];
            let base = 4 + side as u32 * 4;
            assert_eq!(
                indices,
                &[
                    base,
                    base + 1,
                    base + 2,
                    base,
                    base + 2,
                    base + 3,
                    base + 3,
                    base + 2,
                    base + 1,
                    base + 3,
                    base + 1,
                    base,
                ]
            );
        }
        assert!(
            mesh.vertices[20..24]
                .iter()
                .all(|vertex| (vertex.position[1] - FLUID_FACE_EPSILON).abs() < 1.0e-7)
        );
        let top = mesh
            .vertices
            .iter()
            .map(|vertex| vertex.position[1])
            .fold(f32::NEG_INFINITY, f32::max);
        // The isolated source contributes with vanilla's high-fluid weight
        // while the two empty cardinal samples still contribute zero-height
        // weight: (8/9 * 10) / 12.
        assert!((top - (80.0 / 108.0)).abs() < 0.001);

        states[1] = state;
        let adjacent = BTreeMap::from([(coordinate, chunk(coordinate, states))]);
        let mesh = mesh_chunk_with_biomes(
            coordinate,
            &adjacent,
            DimensionGeometry {
                min_y: 0,
                height: 16,
            },
            &resources,
            &[],
        )
        .unwrap();
        assert_eq!(mesh.translucent_indices.len(), 96);
    }

    #[test]
    fn layered_translucent_models_use_the_separate_non_depth_writing_batch() {
        let air = RuntimeBlockStateId(0);
        let state = RuntimeBlockStateId(7);
        let resources = BlockResources::synthetic([air])
            .with_synthetic_render_layer(state, RenderLayer::LayeredTranslucent);
        let coordinate = ChunkCoordinate::new(0, 0);
        let mut states = vec![air; 4096];
        states[0] = state;
        let chunks = BTreeMap::from([(coordinate, chunk(coordinate, states))]);
        let mesh = mesh_chunk(
            coordinate,
            &chunks,
            DimensionGeometry {
                min_y: 0,
                height: 16,
            },
            &resources,
        )
        .unwrap();

        assert!(mesh.indices.is_empty());
        assert!(mesh.translucent_indices.is_empty());
        assert_eq!(mesh.layered_translucent_indices.len(), 36);
        assert!(mesh.vertices.iter().all(|vertex| vertex.layer == 2));
    }

    #[test]
    fn opaque_neighbor_suppresses_hidden_fluid_side_without_hiding_open_boundaries() {
        let fluid = RuntimeBlockStateId(7);
        let wall = RuntimeBlockStateId(9);
        let resources = BlockResources::synthetic_fluid(
            fluid,
            cubic_world::FluidState {
                kind: FluidKind::Water,
                level: 0,
                falling: false,
            },
        );
        let coordinate = ChunkCoordinate::new(0, 0);
        let index = |x: usize, y: usize, z: usize| y * 256 + z * 16 + x;
        let mut states = vec![RuntimeBlockStateId(0); 4096];
        states[index(8, 1, 8)] = fluid;
        states[index(9, 1, 8)] = wall;
        let chunks = BTreeMap::from([(coordinate, chunk(coordinate, states))]);
        let mesh = mesh_chunk_with_biomes(
            coordinate,
            &chunks,
            DimensionGeometry {
                min_y: 0,
                height: 16,
            },
            &resources,
            &[],
        )
        .unwrap();

        // Top + bottom contribute twelve indices. Three visible ordinary
        // sides contribute twelve each (front and reverse windings). The east
        // side is fully hidden by the opaque full-cube neighbour and is not
        // emitted on the nearly coplanar 0.999/1.0 boundary.
        assert_eq!(mesh.translucent_indices.len(), 48);
        assert!(!mesh.vertices.iter().any(|vertex| {
            (vertex.position[0] - (9.0 - FLUID_FACE_EPSILON)).abs() < 1.0e-7
                && vertex.position[2] >= 8.0
                && vertex.position[2] <= 9.0
        }));

        assert!(!neighbor_fully_occludes_fluid_side(
            &chunks,
            DimensionGeometry {
                min_y: 0,
                height: 16,
            },
            &resources,
            8,
            1,
            8,
            Direction::North,
            8.0 / 9.0,
        ));

        let partial_resources = BlockResources::synthetic_fluid(
            fluid,
            cubic_world::FluidState {
                kind: FluidKind::Water,
                level: 0,
                falling: false,
            },
        )
        .with_synthetic_non_full(wall);
        assert!(!neighbor_fully_occludes_fluid_side(
            &chunks,
            DimensionGeometry {
                min_y: 0,
                height: 16,
            },
            &partial_resources,
            8,
            1,
            8,
            Direction::East,
            8.0 / 9.0,
        ));

        let lower_slab_resources = BlockResources::synthetic_fluid(
            fluid,
            cubic_world::FluidState {
                kind: FluidKind::Water,
                level: 0,
                falling: false,
            },
        )
        .with_synthetic_opaque_boxes(wall, vec![[[0.0, 0.0, 0.0], [1.0, 0.5, 1.0]]]);
        assert!(neighbor_fully_occludes_fluid_side(
            &chunks,
            DimensionGeometry {
                min_y: 0,
                height: 16,
            },
            &lower_slab_resources,
            8,
            1,
            8,
            Direction::East,
            0.4,
        ));
        assert!(!neighbor_fully_occludes_fluid_side(
            &chunks,
            DimensionGeometry {
                min_y: 0,
                height: 16,
            },
            &lower_slab_resources,
            8,
            1,
            8,
            Direction::East,
            0.8,
        ));
    }

    #[test]
    fn fluid_side_visibility_matrix_covers_every_horizontal_direction() {
        let air = RuntimeBlockStateId(0);
        let fluid = RuntimeBlockStateId(7);
        let wall = RuntimeBlockStateId(9);
        let geometry = DimensionGeometry {
            min_y: 0,
            height: 16,
        };
        let coordinate = ChunkCoordinate::new(0, 0);
        let index = |x: usize, y: usize, z: usize| y * 256 + z * 16 + x;
        let base_resources = BlockResources::synthetic_fluid(
            fluid,
            cubic_world::FluidState {
                kind: FluidKind::Water,
                level: 0,
                falling: false,
            },
        );
        let transparent_resources = base_resources.clone().with_synthetic_non_full(wall);

        for (direction, [dx, _, dz]) in [
            (Direction::North, [0, 0, -1]),
            (Direction::South, [0, 0, 1]),
            (Direction::West, [-1, 0, 0]),
            (Direction::East, [1, 0, 0]),
        ] {
            let mut states = vec![air; 4096];
            states[index(8, 1, 8)] = fluid;
            let exposed = BTreeMap::from([(coordinate, chunk(coordinate, states.clone()))]);
            let mesh = mesh_chunk_with_biomes(coordinate, &exposed, geometry, &base_resources, &[])
                .unwrap();
            assert!(mesh_contains_fluid_side(&mesh, direction, 8, 1, 8));

            let neighbor_x = usize::try_from(8 + dx).unwrap();
            let neighbor_z = usize::try_from(8 + dz).unwrap();
            states[index(neighbor_x, 1, neighbor_z)] = fluid;
            let same_fluid_chunks =
                BTreeMap::from([(coordinate, chunk(coordinate, states.clone()))]);
            let mesh = mesh_chunk_with_biomes(
                coordinate,
                &same_fluid_chunks,
                geometry,
                &base_resources,
                &[],
            )
            .unwrap();
            assert!(!mesh_contains_fluid_side(&mesh, direction, 8, 1, 8));

            states[index(neighbor_x, 1, neighbor_z)] = wall;
            let wall_chunks = BTreeMap::from([(coordinate, chunk(coordinate, states))]);
            let mesh =
                mesh_chunk_with_biomes(coordinate, &wall_chunks, geometry, &base_resources, &[])
                    .unwrap();
            assert!(!mesh_contains_fluid_side(&mesh, direction, 8, 1, 8));

            let mesh = mesh_chunk_with_biomes(
                coordinate,
                &wall_chunks,
                geometry,
                &transparent_resources,
                &[],
            )
            .unwrap();
            assert!(mesh_contains_fluid_side(&mesh, direction, 8, 1, 8));
        }
    }

    #[test]
    fn fluid_debug_flags_default_off_and_preserve_normal_mesh_bytes() {
        use std::ffi::OsStr;

        assert!(!FluidDebugOptions::default().face_colors);
        assert!(!FluidDebugOptions::default().mesh_log);
        assert_eq!(
            FluidDebugOptions::from_values(None, None, None),
            FluidDebugOptions {
                face_colors: false,
                mesh_log: false,
                radius: 6,
            }
        );
        assert_eq!(
            FluidDebugOptions::from_values(
                Some(OsStr::new("1")),
                Some(OsStr::new("TRUE")),
                Some(OsStr::new("99")),
            ),
            FluidDebugOptions {
                face_colors: true,
                mesh_log: true,
                radius: 32,
            }
        );

        let fluid = RuntimeBlockStateId(7);
        let resources = BlockResources::synthetic_fluid(
            fluid,
            cubic_world::FluidState {
                kind: FluidKind::Water,
                level: 0,
                falling: false,
            },
        );
        let coordinate = ChunkCoordinate::new(0, 0);
        let mut states = vec![RuntimeBlockStateId(0); 4096];
        states[0] = fluid;
        let chunks = BTreeMap::from([(coordinate, chunk(coordinate, states))]);
        let geometry = DimensionGeometry {
            min_y: 0,
            height: 16,
        };
        let ordinary =
            mesh_chunk_with_biomes(coordinate, &chunks, geometry, &resources, &[]).unwrap();
        let explicit_disabled = mesh_chunk_with_debug(
            coordinate,
            &chunks,
            geometry,
            &resources,
            &[],
            FluidDebugOptions::default(),
            Some([0, 0, 0]),
        )
        .unwrap();
        assert_eq!(
            bytemuck::cast_slice::<TerrainVertex, u8>(&ordinary.vertices),
            bytemuck::cast_slice::<TerrainVertex, u8>(&explicit_disabled.vertices)
        );
        assert_eq!(ordinary.indices, explicit_disabled.indices);
        assert_eq!(
            ordinary.translucent_indices,
            explicit_disabled.translucent_indices
        );
        assert!(explicit_disabled.fluid_debug.is_empty());
    }

    #[test]
    fn fluid_debug_face_markers_and_final_mesh_records_are_bounded_and_valid() {
        for (direction, face) in [
            (Direction::Up, 1),
            (Direction::North, 2),
            (Direction::South, 3),
            (Direction::East, 4),
            (Direction::West, 5),
            (Direction::Down, 6),
        ] {
            assert_eq!(debug_fluid_layer(2, false, direction, true), 2);
            assert_eq!(debug_fluid_layer(2, true, direction, false), 2 | face << 8);
            assert_eq!(
                debug_fluid_layer(2, true, direction, true),
                2 | face << 8 | 1 << 16
            );
        }

        let air = RuntimeBlockStateId(0);
        let source = RuntimeBlockStateId(7);
        let lower = RuntimeBlockStateId(8);
        let resources = BlockResources::synthetic_fluids([
            (
                source,
                cubic_world::FluidState {
                    kind: FluidKind::Water,
                    level: 0,
                    falling: false,
                },
            ),
            (
                lower,
                cubic_world::FluidState {
                    kind: FluidKind::Water,
                    level: 5,
                    falling: false,
                },
            ),
        ]);
        let coordinate = ChunkCoordinate::new(0, 0);
        let index = |x: usize, y: usize, z: usize| y * 256 + z * 16 + x;
        let mut states = vec![air; 4096];
        states[index(8, 1, 8)] = source;
        states[index(9, 1, 8)] = lower;
        let chunks = BTreeMap::from([(coordinate, chunk(coordinate, states))]);
        let mesh = mesh_chunk_with_debug(
            coordinate,
            &chunks,
            DimensionGeometry {
                min_y: 0,
                height: 16,
            },
            &resources,
            &[],
            FluidDebugOptions {
                face_colors: true,
                mesh_log: true,
                radius: 8,
            },
            Some([8, 1, 8]),
        )
        .unwrap();
        assert!(!mesh.fluid_debug.is_empty());
        assert!(mesh.fluid_debug.len() <= MAX_FLUID_DEBUG_CELLS_PER_MESH);
        assert!(mesh.fluid_debug.iter().all(|record| {
            !record.quads_truncated
                && record.quads.len() <= MAX_FLUID_DEBUG_QUADS_PER_CELL
                && record.quads.iter().all(|quad| quad.invariant == Ok(()))
        }));
        assert!(mesh.vertices.iter().any(|vertex| vertex.layer >> 8 != 0));

        let remaining = AtomicUsize::new(2);
        log_fluid_mesh_diagnostics(&mesh, &remaining, coordinate, 3, 7);
        assert_eq!(remaining.load(std::sync::atomic::Ordering::Relaxed), 0);
        log_fluid_mesh_diagnostics(&mesh, &remaining, coordinate, 3, 7);
        assert_eq!(remaining.load(std::sync::atomic::Ordering::Relaxed), 0);
    }

    #[test]
    fn fluid_debug_cell_collection_has_a_hard_per_mesh_cap() {
        let fluid = RuntimeBlockStateId(7);
        let resources = BlockResources::synthetic_fluid(
            fluid,
            cubic_world::FluidState {
                kind: FluidKind::Water,
                level: 0,
                falling: false,
            },
        );
        let coordinate = ChunkCoordinate::new(0, 0);
        let mut states = vec![RuntimeBlockStateId(0); 4096];
        states.iter_mut().take(64).for_each(|state| *state = fluid);
        let chunks = BTreeMap::from([(coordinate, chunk(coordinate, states))]);
        let mesh = mesh_chunk_with_debug(
            coordinate,
            &chunks,
            DimensionGeometry {
                min_y: 0,
                height: 16,
            },
            &resources,
            &[],
            FluidDebugOptions {
                face_colors: false,
                mesh_log: true,
                radius: 32,
            },
            Some([8, 0, 8]),
        )
        .unwrap();
        assert_eq!(mesh.fluid_debug.len(), MAX_FLUID_DEBUG_CELLS_PER_MESH);
    }

    #[test]
    fn fluid_debug_invariants_cover_slopes_walls_lower_neighbors_and_waterfalls() {
        let air = RuntimeBlockStateId(0);
        let source = RuntimeBlockStateId(7);
        let lower = RuntimeBlockStateId(8);
        let falling = RuntimeBlockStateId(9);
        let wall = RuntimeBlockStateId(10);
        let resources = BlockResources::synthetic_fluids([
            (
                source,
                cubic_world::FluidState {
                    kind: FluidKind::Water,
                    level: 0,
                    falling: false,
                },
            ),
            (
                lower,
                cubic_world::FluidState {
                    kind: FluidKind::Water,
                    level: 6,
                    falling: false,
                },
            ),
            (
                falling,
                cubic_world::FluidState {
                    kind: FluidKind::Water,
                    level: 0,
                    falling: true,
                },
            ),
        ]);
        let coordinate = ChunkCoordinate::new(0, 0);
        let geometry = DimensionGeometry {
            min_y: 0,
            height: 16,
        };
        let index = |x: usize, y: usize, z: usize| y * 256 + z * 16 + x;

        for ([dx, dz], [wall_dx, wall_dz]) in [
            ([0, -1], [1, 0]),
            ([0, 1], [-1, 0]),
            ([-1, 0], [0, -1]),
            ([1, 0], [0, 1]),
        ] {
            let mut states = vec![air; 4096];
            states[index(8, 1, 8)] = source;
            states[index(
                usize::try_from(8 + dx).unwrap(),
                1,
                usize::try_from(8 + dz).unwrap(),
            )] = lower;
            states[index(
                usize::try_from(8 + wall_dx).unwrap(),
                1,
                usize::try_from(8 + wall_dz).unwrap(),
            )] = wall;
            states[index(8, 0, 8)] = wall;
            let chunks = BTreeMap::from([(coordinate, chunk(coordinate, states))]);
            let mesh = mesh_chunk_with_debug(
                coordinate,
                &chunks,
                geometry,
                &resources,
                &[],
                FluidDebugOptions {
                    face_colors: false,
                    mesh_log: true,
                    radius: 8,
                },
                Some([8, 1, 8]),
            )
            .unwrap();
            let center = mesh
                .fluid_debug
                .iter()
                .find(|record| record.coordinate == [8, 1, 8])
                .unwrap();
            assert!(center.heights.windows(2).any(|pair| pair[0] != pair[1]));
            assert!(center.quads.iter().all(|quad| quad.invariant == Ok(())));
            assert!(
                center
                    .sides
                    .iter()
                    .any(|decision| matches!(decision, Some(FluidSideDecision::Emitted { .. })))
            );
            assert!(center.sides.iter().any(|decision| {
                matches!(decision, Some(FluidSideDecision::NeighborOcclusion))
            }));
        }

        let mut waterfall = vec![air; 4096];
        waterfall[index(8, 3, 8)] = source;
        waterfall[index(8, 2, 8)] = falling;
        waterfall[index(8, 1, 8)] = falling;
        waterfall[index(8, 0, 8)] = wall;
        let chunks = BTreeMap::from([(coordinate, chunk(coordinate, waterfall))]);
        let mesh = mesh_chunk_with_debug(
            coordinate,
            &chunks,
            geometry,
            &resources,
            &[],
            FluidDebugOptions {
                face_colors: false,
                mesh_log: true,
                radius: 8,
            },
            Some([8, 2, 8]),
        )
        .unwrap();
        assert!(
            mesh.fluid_debug
                .iter()
                .all(|record| record.quads.iter().all(|quad| quad.invariant == Ok(())))
        );
    }

    fn mesh_contains_fluid_side(
        mesh: &ChunkMesh,
        direction: Direction,
        x: i32,
        y: i32,
        z: i32,
    ) -> bool {
        let plane = match direction {
            Direction::North => z as f32 + FLUID_FACE_EPSILON,
            Direction::South => z as f32 + 1.0 - FLUID_FACE_EPSILON,
            Direction::West => x as f32 + FLUID_FACE_EPSILON,
            Direction::East => x as f32 + 1.0 - FLUID_FACE_EPSILON,
            Direction::Down | Direction::Up => return false,
        };
        let axis = match direction {
            Direction::North | Direction::South => 2,
            Direction::West | Direction::East => 0,
            Direction::Down | Direction::Up => return false,
        };
        let (quads, _) = mesh.vertices.as_chunks::<4>();
        quads.iter().any(|quad| {
            quad.iter()
                .all(|vertex| vertex.layer == 2 && (vertex.position[axis] - plane).abs() < 1.0e-7)
                && quad
                    .iter()
                    .any(|vertex| vertex.position[1] > y as f32 + FLUID_FACE_EPSILON)
        })
    }

    #[test]
    fn fluid_top_selects_still_or_animated_flow_resource_without_remeshing() {
        assert!(!fluid_top_uses_flowing_texture([0.0, 0.0]));
        assert!(fluid_top_uses_flowing_texture([1.0, 0.0]));
        assert!(fluid_top_uses_flowing_texture([0.0, -1.0]));
    }

    #[test]
    fn source_beside_solid_ignores_the_wall_for_height_and_flow() {
        let source = RuntimeBlockStateId(7);
        let wall = RuntimeBlockStateId(9);
        let resources = BlockResources::synthetic_fluid(
            source,
            cubic_world::FluidState {
                kind: FluidKind::Water,
                level: 0,
                falling: false,
            },
        );
        let coordinate = ChunkCoordinate::new(0, 0);
        let mut states = vec![RuntimeBlockStateId(0); 4096];
        let index = |x: usize, y: usize, z: usize| y * 256 + z * 16 + x;
        states[index(8, 1, 8)] = source;
        states[index(7, 1, 8)] = wall;
        states[index(8, 1, 7)] = wall;
        states[index(7, 1, 7)] = wall;
        let chunks = BTreeMap::from([(coordinate, chunk(coordinate, states))]);
        let height = fluid_corner_height(
            &chunks,
            DimensionGeometry {
                min_y: 0,
                height: 16,
            },
            &resources,
            FluidKind::Water,
            (8, 1, 8),
            (-1, -1),
        );
        assert!((height - 8.0 / 9.0).abs() < 1.0e-9);
        let flow = fluid_surface_flow(
            &chunks,
            DimensionGeometry {
                min_y: 0,
                height: 16,
            },
            &resources,
            FluidKind::Water,
            8,
            1,
            8,
        );
        assert_eq!(flow, [0.0, 0.0]);
        assert!(!fluid_top_uses_flowing_texture(flow));
    }

    #[test]
    fn source_surface_corners_ignore_one_or_two_solid_neighbours_exactly() {
        let source = RuntimeBlockStateId(7);
        let wall = RuntimeBlockStateId(9);
        let resources = BlockResources::synthetic_fluid(
            source,
            cubic_world::FluidState {
                kind: FluidKind::Water,
                level: 0,
                falling: false,
            },
        );
        let geometry = DimensionGeometry {
            min_y: 0,
            height: 16,
        };
        let index = |x: usize, y: usize, z: usize| y * 256 + z * 16 + x;

        for walls in [[(7, 1, 8), (7, 1, 8)], [(7, 1, 8), (8, 1, 7)]] {
            let coordinate = ChunkCoordinate::new(0, 0);
            let mut states = vec![RuntimeBlockStateId(0); 4096];
            // Surround the source with matching fluid, then replace one or two
            // neighbours with solids. A solid is excluded from the weighted
            // samples; it must not lower either adjacent corner.
            for z in 7..=9 {
                for x in 7..=9 {
                    states[index(x, 1, z)] = source;
                }
            }
            for (x, y, z) in walls {
                states[index(x, y, z)] = wall;
            }
            let chunks = BTreeMap::from([(coordinate, chunk(coordinate, states))]);
            let heights =
                fluid_surface_heights(&chunks, geometry, &resources, FluidKind::Water, 8, 1, 8);
            assert!(
                heights
                    .iter()
                    .all(|height| (height - 8.0 / 9.0).abs() < 1.0e-12),
                "{heights:?}"
            );
        }
    }

    #[test]
    fn falling_columns_with_fluid_above_use_a_flat_full_surface() {
        let geometry = DimensionGeometry {
            min_y: 0,
            height: 16,
        };
        let index = |x: usize, y: usize, z: usize| y * 256 + z * 16 + x;
        for kind in [FluidKind::Water, FluidKind::Lava] {
            let falling = RuntimeBlockStateId(7);
            let source = RuntimeBlockStateId(8);
            let resources = BlockResources::synthetic_fluids([
                (
                    falling,
                    cubic_world::FluidState {
                        kind,
                        level: 0,
                        falling: true,
                    },
                ),
                (
                    source,
                    cubic_world::FluidState {
                        kind,
                        level: 0,
                        falling: false,
                    },
                ),
            ]);
            let coordinate = ChunkCoordinate::new(0, 0);
            let mut states = vec![RuntimeBlockStateId(0); 4096];
            states[index(8, 1, 8)] = falling;
            states[index(8, 2, 8)] = source;
            states[index(9, 1, 8)] = falling;
            states[index(9, 2, 8)] = falling;
            let chunks = BTreeMap::from([(coordinate, chunk(coordinate, states))]);
            for x in [8, 9] {
                assert_eq!(
                    fluid_surface_heights(&chunks, geometry, &resources, kind, x, 1, 8,),
                    [1.0; 4],
                    "{kind:?} column at x={x}"
                );
            }
        }
    }

    #[test]
    fn rendered_fluid_height_rules_match_source_flow_and_vertical_column_cases() {
        let source = cubic_world::FluidState {
            kind: FluidKind::Water,
            level: 0,
            falling: false,
        };
        assert_eq!(source.own_height(), 8.0 / 9.0);
        for level in 1..=7 {
            let flowing = cubic_world::FluidState {
                kind: FluidKind::Water,
                level,
                falling: false,
            };
            assert_eq!(flowing.own_height(), f64::from(8 - level) / 9.0);
        }
        let falling = cubic_world::FluidState {
            kind: FluidKind::Water,
            level: 0,
            falling: true,
        };
        assert_eq!(falling.own_height(), 8.0 / 9.0);
        // Matching fluid above is the separate renderer rule which raises a
        // connected vertical column to a complete block-height surface.
    }

    #[test]
    fn cardinal_surface_slopes_descend_in_the_same_direction_as_flow() {
        let source = RuntimeBlockStateId(7);
        let low = RuntimeBlockStateId(8);
        let resources = BlockResources::synthetic_fluids([
            (
                source,
                cubic_world::FluidState {
                    kind: FluidKind::Water,
                    level: 0,
                    falling: false,
                },
            ),
            (
                low,
                cubic_world::FluidState {
                    kind: FluidKind::Water,
                    level: 7,
                    falling: false,
                },
            ),
        ]);
        let geometry = DimensionGeometry {
            min_y: 0,
            height: 16,
        };
        let index = |x: usize, y: usize, z: usize| y * 256 + z * 16 + x;
        for (dx, dz) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
            let coordinate = ChunkCoordinate::new(0, 0);
            let mut states = vec![RuntimeBlockStateId(0); 4096];
            states[index(8, 1, 8)] = source;
            states[index((8 + dx) as usize, 1, (8 + dz) as usize)] = low;
            let chunks = BTreeMap::from([(coordinate, chunk(coordinate, states))]);
            let flow = fluid_surface_flow(&chunks, geometry, &resources, FluidKind::Water, 8, 1, 8);
            assert_eq!(flow, [f64::from(dx), f64::from(dz)]);
            let heights = [
                fluid_corner_height(
                    &chunks,
                    geometry,
                    &resources,
                    FluidKind::Water,
                    (8, 1, 8),
                    (-1, -1),
                ),
                fluid_corner_height(
                    &chunks,
                    geometry,
                    &resources,
                    FluidKind::Water,
                    (8, 1, 8),
                    (-1, 1),
                ),
                fluid_corner_height(
                    &chunks,
                    geometry,
                    &resources,
                    FluidKind::Water,
                    (8, 1, 8),
                    (1, 1),
                ),
                fluid_corner_height(
                    &chunks,
                    geometry,
                    &resources,
                    FluidKind::Water,
                    (8, 1, 8),
                    (1, -1),
                ),
            ];
            let high_side = if dx < 0 {
                [heights[2], heights[3]]
            } else if dx > 0 {
                [heights[0], heights[1]]
            } else if dz < 0 {
                [heights[1], heights[2]]
            } else {
                [heights[0], heights[3]]
            };
            let low_side = if dx < 0 {
                [heights[0], heights[1]]
            } else if dx > 0 {
                [heights[2], heights[3]]
            } else if dz < 0 {
                [heights[0], heights[3]]
            } else {
                [heights[1], heights[2]]
            };
            assert!(high_side.iter().sum::<f64>() > low_side.iter().sum::<f64>());
        }

        let coordinate = ChunkCoordinate::new(0, 0);
        let mut states = vec![RuntimeBlockStateId(0); 4096];
        states[index(8, 1, 8)] = source;
        states[index(9, 1, 8)] = low;
        states[index(8, 1, 9)] = low;
        let chunks = BTreeMap::from([(coordinate, chunk(coordinate, states))]);
        let diagonal = fluid_surface_flow(&chunks, geometry, &resources, FluidKind::Water, 8, 1, 8);
        assert!((diagonal[0] - f64::sqrt(0.5)).abs() < 1.0e-9);
        assert!((diagonal[1] - f64::sqrt(0.5)).abs() < 1.0e-9);
    }

    #[test]
    fn flowing_surface_uvs_follow_vanilla_cardinal_and_diagonal_flow() {
        let cases = [
            (
                [1.0, 0.0],
                [[0.75, 0.25], [0.25, 0.25], [0.25, 0.75], [0.75, 0.75]],
            ),
            (
                [-1.0, 0.0],
                [[0.25, 0.75], [0.75, 0.75], [0.75, 0.25], [0.25, 0.25]],
            ),
            (
                [0.0, 1.0],
                [[0.25, 0.25], [0.25, 0.75], [0.75, 0.75], [0.75, 0.25]],
            ),
            (
                [0.0, -1.0],
                [[0.75, 0.75], [0.75, 0.25], [0.25, 0.25], [0.25, 0.75]],
            ),
        ];
        for (flow, expected) in cases {
            let actual = flowing_top_uvs(flow);
            for (actual, expected) in actual.into_iter().zip(expected) {
                assert!((actual[0] - expected[0]).abs() < 1.0e-6, "flow={flow:?}");
                assert!((actual[1] - expected[1]).abs() < 1.0e-6, "flow={flow:?}");
            }
        }
        let diagonal = flowing_top_uvs([f64::sqrt(0.5), f64::sqrt(0.5)]);
        assert_eq!(diagonal, flowing_top_uvs([1.0, 1.0]));
        assert!(
            diagonal
                .into_iter()
                .flatten()
                .all(|value| (0.0..=1.0).contains(&value))
        );
        assert_eq!(flowing_top_uvs([0.0, 0.0]), FLUID_QUAD_UVS);
    }

    #[test]
    fn fluid_faces_are_inset_from_coplanar_block_boundaries() {
        for coordinate in [
            FLUID_FACE_EPSILON,
            1.0 - FLUID_FACE_EPSILON,
            8.0 / 9.0 - FLUID_FACE_EPSILON,
        ] {
            assert_ne!(coordinate, 0.0);
            assert_ne!(coordinate, 1.0);
        }
    }

    #[test]
    fn biome_tints_use_vanilla_normalized_vertex_colours_without_double_srgb_conversion() {
        assert_eq!(rgb(0xff0000), [1.0, 0.0, 0.0]);
        assert_eq!(rgb(0x00ff00), [0.0, 1.0, 0.0]);
        assert_eq!(rgb(0x0000ff), [0.0, 0.0, 1.0]);
        assert_eq!(rgb(0x808080), [128.0 / 255.0; 3]);
        assert_eq!(direction_shade_for_fluid(Direction::North), 0.8);
        assert_eq!(direction_shade_for_fluid(Direction::South), 0.8);
    }

    #[test]
    fn smooth_vertex_ambient_occlusion_darkens_bounded_corner_neighbours() {
        assert_eq!(ambient_occlusion_factor(false, false, false), 1.0);
        assert!((ambient_occlusion_factor(true, false, false) - 0.85).abs() < 0.001);
        assert!((ambient_occlusion_factor(true, false, true) - 0.70).abs() < 0.001);
        assert_eq!(ambient_occlusion_factor(true, true, false), 0.55);
        assert_eq!(ambient_occlusion_factor(true, true, true), 0.55);
    }

    #[test]
    fn adjacent_blocks_and_chunk_neighbors_cull_shared_faces() {
        let mut left = vec![RuntimeBlockStateId(0); 4096];
        left[15] = RuntimeBlockStateId(1);
        let mut right = vec![RuntimeBlockStateId(0); 4096];
        right[0] = RuntimeBlockStateId(1);
        let a = ChunkCoordinate::new(0, 0);
        let b = ChunkCoordinate::new(1, 0);
        let chunks = BTreeMap::from([(a, chunk(a, left)), (b, chunk(b, right))]);
        let mesh = mesh_chunk(
            a,
            &chunks,
            DimensionGeometry {
                min_y: -16,
                height: 16,
            },
            &BlockResources::synthetic([RuntimeBlockStateId(0)]),
        )
        .unwrap();
        assert_eq!(mesh.indices.len(), 30);
    }

    #[test]
    fn unknown_states_are_renderable_and_bad_geometry_is_rejected() {
        let coord = ChunkCoordinate::new(0, 0);
        let chunks = BTreeMap::from([(coord, chunk(coord, vec![RuntimeBlockStateId(99); 4096]))]);
        assert!(
            mesh_chunk(
                coord,
                &chunks,
                DimensionGeometry {
                    min_y: 0,
                    height: 32
                },
                &BlockResources::synthetic([])
            )
            .is_err()
        );
    }

    #[test]
    fn netherrack_style_variant_selection_matches_vanilla_position_vectors() {
        let application = |x_rotation, y_rotation| ModelApplication {
            faces: vec![],
            solid_boxes: vec![],
            x_rotation,
            y_rotation,
            uvlock: false,
            ambient_occlusion: true,
        };
        // The official 26.1.2 netherrack blockstate lists X rotations fastest,
        // nested inside Y rotations, yielding sixteen equal-weight variants.
        let variants = crate::block_resources::WeightedApplications {
            entries: (0_u16..16)
                .map(|index| (1, application((index % 4) * 90, (index / 4) * 90)))
                .collect(),
            total_weight: 16,
        };
        let vectors = [
            ((0, 0, 0), (270, 180)),
            ((1, 0, 0), (90, 0)),
            ((0, 1, 0), (270, 180)),
            ((0, 0, 1), (270, 90)),
            ((-17, 64, 31), (270, 270)),
            ((123, -64, -456), (180, 0)),
        ];
        for ((x, y, z), expected) in vectors {
            let mut first_random = ModelVariantRandom::at_position(x, y, z);
            let first = select_model(&variants, &mut first_random).unwrap();
            assert_eq!((first.x_rotation, first.y_rotation), expected);

            let mut repeated_random = ModelVariantRandom::at_position(x, y, z);
            let repeated = select_model(&variants, &mut repeated_random).unwrap();
            assert_eq!(
                (repeated.x_rotation, repeated.y_rotation),
                expected,
                "same state model and coordinate must choose identically"
            );
        }

        assert_eq!(model_position_seed(1, 64, 1), 50_782_340_185_060);
        assert_eq!(model_position_seed(-1, 64, -1), 50_782_340_185_060);
        assert_eq!(
            rotate_blockstate_corner([1.0, 0.0, 0.0], 0, 90),
            [1.0, 0.0, 1.0]
        );
        assert_eq!(
            rotate_blockstate_direction(Direction::North, 0, 90),
            Direction::East
        );
    }

    #[test]
    fn weighted_ranges_and_group_draw_sequence_match_vanilla() {
        let application = |rotation| ModelApplication {
            faces: vec![],
            solid_boxes: vec![],
            x_rotation: 0,
            y_rotation: rotation,
            uvlock: false,
            ambient_occlusion: true,
        };
        let weighted = crate::block_resources::WeightedApplications {
            entries: vec![
                (1, application(0)),
                (3, application(90)),
                (6, application(180)),
            ],
            total_weight: 10,
        };
        for ((x, y, z), expected) in [
            ((0, 0, 0), 0),
            ((1, 0, 0), 90),
            ((0, 1, 0), 180),
            ((-17, 64, 31), 180),
        ] {
            let mut random = ModelVariantRandom::at_position(x, y, z);
            assert_eq!(
                select_model(&weighted, &mut random).unwrap().y_rotation,
                expected
            );
        }

        let single = crate::block_resources::WeightedApplications {
            entries: vec![(1, application(0))],
            total_weight: 1,
        };
        let sixteen = crate::block_resources::WeightedApplications {
            entries: (0_u16..16).map(|index| (1, application(index))).collect(),
            total_weight: 16,
        };
        let mut random = ModelVariantRandom::at_position(0, 0, 0);
        assert_eq!(select_model(&single, &mut random).unwrap().y_rotation, 0);
        assert_eq!(select_model(&sixteen, &mut random).unwrap().y_rotation, 13);
    }

    #[test]
    fn blockstate_rotation_and_uvlock_preserve_the_top_left_texture_convention() {
        let face = crate::block_resources::ModelFace {
            direction: Direction::North,
            corners: [
                [0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [1.0, 1.0, 0.0],
                [1.0, 0.0, 0.0],
            ],
            uv: [[0.0, 1.0], [0.0, 0.0], [1.0, 0.0], [1.0, 1.0]],
            texture: "cubic:missing".to_owned(),
            atlas_region: crate::block_resources::AtlasRegion {
                min: [0.0, 0.0],
                max: [1.0, 1.0],
                layer: RenderLayer::Opaque,
            },
            cullface: None,
            tint_index: None,
            tint_kind: TintKind::None,
            render_layer: RenderLayer::Opaque,
            directional_shade: false,
            shade: 1.0,
        };
        let chunks = BTreeMap::new();
        let resources = BlockResources::synthetic([]);
        let geometry = DimensionGeometry {
            min_y: 0,
            height: 16,
        };
        let mut unlocked = ChunkMesh::default();
        push_model_face(
            &mut unlocked,
            &chunks,
            geometry,
            &resources,
            &[],
            0,
            0,
            0,
            &ModelApplication {
                faces: vec![face.clone()],
                solid_boxes: vec![],
                x_rotation: 0,
                y_rotation: 90,
                uvlock: false,
                ambient_occlusion: true,
            },
            &face,
        )
        .unwrap();
        assert_eq!(unlocked.vertices[0].uv, [0.0, 1.0]);
        assert_eq!(unlocked.vertices[1].uv, [0.0, 0.0]);

        let mut locked = ChunkMesh::default();
        push_model_face(
            &mut locked,
            &chunks,
            geometry,
            &resources,
            &[],
            0,
            0,
            0,
            &ModelApplication {
                faces: vec![face.clone()],
                solid_boxes: vec![],
                x_rotation: 0,
                y_rotation: 90,
                uvlock: true,
                ambient_occlusion: true,
            },
            &face,
        )
        .unwrap();
        assert_eq!(locked.vertices[0].uv, [0.0, 1.0]);
        assert_eq!(locked.vertices[3].uv, [1.0, 1.0]);
    }

    #[test]
    fn textured_fallback_vertices_preserve_uvs_at_negative_chunk_coordinates() {
        let mut states = vec![RuntimeBlockStateId(0); 4096];
        states[0] = RuntimeBlockStateId(7);
        let coordinate = ChunkCoordinate::new(-1, -1);
        let chunks = BTreeMap::from([(coordinate, chunk(coordinate, states))]);
        let mesh = mesh_chunk(
            coordinate,
            &chunks,
            DimensionGeometry {
                min_y: 0,
                height: 16,
            },
            &BlockResources::synthetic([RuntimeBlockStateId(0)]),
        )
        .unwrap();
        assert_eq!(mesh.indices.len(), 36);
        assert!(
            mesh.vertices
                .iter()
                .all(|vertex| vertex.position[0] <= -15.0 && vertex.position[2] <= -15.0)
        );
        assert!(
            mesh.vertices
                .iter()
                .all(|vertex| vertex.uv.iter().all(|value| (0.0..=1.0).contains(value)))
        );
    }

    #[test]
    fn dense_opaque_chunk_fast_rejects_buried_blocks_and_reports_bounded_statistics() {
        let coordinate = ChunkCoordinate::new(0, 0);
        let section = ChunkSection {
            non_empty_block_count: 4096,
            fluid_count: 0,
            blocks: PalettedContainer::Single {
                value: RuntimeBlockStateId(1),
                entries: 4096,
            },
            biomes: PalettedContainer::Single {
                value: cubic_world::RuntimeBiomeId(0),
                entries: 64,
            },
        };
        let chunk = Arc::new(Chunk {
            coordinate,
            sections: vec![section; 24],
            heightmaps: vec![],
            block_entities: vec![],
            light: ChunkLightSummary::default(),
        });
        let chunks = BTreeMap::from([(coordinate, chunk)]);
        let mesh = mesh_chunk(
            coordinate,
            &chunks,
            DimensionGeometry {
                min_y: -64,
                height: 384,
            },
            &BlockResources::synthetic([RuntimeBlockStateId(0)]),
        )
        .unwrap();
        eprintln!(
            "dense opaque 24-section chunk meshed in {:.3} ms",
            mesh.statistics.cpu_time.as_secs_f64() * 1000.0
        );
        assert_eq!(mesh.statistics.positions_visited, 98_304);
        assert_eq!(mesh.statistics.air_skipped, 0);
        assert_eq!(mesh.statistics.non_air_blocks, 98_304);
        assert_eq!(mesh.statistics.fully_occluded_fast_rejected, 74_872);
        assert_eq!(mesh.statistics.model_processed, 23_432);
        assert_eq!(mesh.statistics.geometry_emitting, 23_432);
        assert_eq!(mesh.statistics.quads_emitted, 25_088);
        assert_eq!(mesh.vertices.len(), 25_088 * 4);
        assert_eq!(mesh.indices.len(), 25_088 * 6);
    }

    #[test]
    fn non_full_models_never_take_the_opaque_full_cube_shortcut() {
        let mut states = vec![RuntimeBlockStateId(0); 4096];
        for y in 1_u8..=3 {
            for z in 1_u8..=3 {
                for x in 1_u8..=3 {
                    let index = usize::from(y) * 256 + usize::from(z) * 16 + usize::from(x);
                    states[index] = RuntimeBlockStateId(2);
                }
            }
        }
        let coordinate = ChunkCoordinate::new(0, 0);
        let chunks = BTreeMap::from([(coordinate, chunk(coordinate, states))]);
        let mesh = mesh_chunk(
            coordinate,
            &chunks,
            DimensionGeometry {
                min_y: 0,
                height: 16,
            },
            &BlockResources::synthetic_non_full([RuntimeBlockStateId(0)], RuntimeBlockStateId(2)),
        )
        .unwrap();
        assert_eq!(mesh.statistics.non_air_blocks, 27);
        assert_eq!(mesh.statistics.fully_occluded_fast_rejected, 0);
        assert_eq!(mesh.statistics.model_processed, 27);
    }

    #[test]
    fn mesh_statistics_accumulate_without_overflow() {
        let mut aggregate = MeshStatistics {
            positions_visited: u64::MAX,
            air_skipped: u64::MAX,
            cpu_time: Duration::MAX,
            ..MeshStatistics::default()
        };
        aggregate.accumulate(MeshStatistics {
            positions_visited: 1,
            air_skipped: 1,
            non_air_blocks: 2,
            cpu_time: Duration::from_secs(1),
            ..MeshStatistics::default()
        });
        assert_eq!(aggregate.positions_visited, u64::MAX);
        assert_eq!(aggregate.air_skipped, u64::MAX);
        assert_eq!(aggregate.non_air_blocks, 2);
        assert_eq!(aggregate.cpu_time, Duration::MAX);
    }

    #[test]
    fn fluid_side_top_edges_use_the_physical_corner_order_for_every_axis() {
        assert_eq!(fluid_side_height_indices(Direction::North), [0, 3]);
        assert_eq!(fluid_side_height_indices(Direction::South), [1, 2]);
        assert_eq!(fluid_side_height_indices(Direction::West), [0, 1]);
        assert_eq!(fluid_side_height_indices(Direction::East), [2, 3]);
        let heights = [0.1, 0.2, 0.3, 0.4];
        for (direction, expected) in [
            (Direction::North, [0.1, 0.4]),
            (Direction::South, [0.2, 0.3]),
            (Direction::West, [0.1, 0.2]),
            (Direction::East, [0.3, 0.4]),
        ] {
            let [left, right] = fluid_side_height_indices(direction);
            assert_eq!([heights[left], heights[right]], expected);
        }
    }

    #[test]
    fn asymmetric_fluid_sides_are_complete_trapezoids_for_every_axis() {
        let heights = [0.2, 0.35, 0.8, 0.65];
        let bottom = FLUID_FACE_EPSILON;
        for (direction, expected) in [
            (
                Direction::North,
                [
                    [0.0, 0.2, 0.001],
                    [1.0, 0.65, 0.001],
                    [1.0, 0.001, 0.001],
                    [0.0, 0.001, 0.001],
                ],
            ),
            (
                Direction::South,
                [
                    [1.0, 0.8, 0.999],
                    [0.0, 0.35, 0.999],
                    [0.0, 0.001, 0.999],
                    [1.0, 0.001, 0.999],
                ],
            ),
            (
                Direction::West,
                [
                    [0.001, 0.35, 1.0],
                    [0.001, 0.2, 0.0],
                    [0.001, 0.001, 0.0],
                    [0.001, 0.001, 1.0],
                ],
            ),
            (
                Direction::East,
                [
                    [0.999, 0.65, 0.0],
                    [0.999, 0.8, 1.0],
                    [0.999, 0.001, 1.0],
                    [0.999, 0.001, 0.0],
                ],
            ),
        ] {
            let [left, right] = fluid_side_height_indices(direction);
            let corners = fluid_side_corners(direction, heights, bottom);
            assert_eq!(corners, expected, "{direction:?}");
            assert_eq!(corners[2][1], bottom);
            assert_eq!(corners[3][1], bottom);

            // Vanilla's QUADS expansion selects vertex 0 -> 2: top-left to
            // bottom-right. The reverse quad selects the opposite physical
            // diagonal. Both cover exactly the complete trapezoid.
            let first = (corners[1][1] - bottom) * 0.5;
            let second = (corners[0][1] - bottom) * 0.5;
            let expected = ((heights[left] - bottom) + (heights[right] - bottom)) * 0.5;
            assert!((first + second - expected).abs() < 1.0e-6, "{direction:?}");

            let uvs = fluid_side_uvs(0.0, 1.0, bottom, corners[0][1], corners[1][1]);
            assert_eq!(uvs[2][1], uvs[3][1]);
            assert_eq!(uvs[0][1], (1.0 - corners[0][1]) * 0.5);
            assert_eq!(uvs[1][1], (1.0 - corners[1][1]) * 0.5);
        }
    }

    #[test]
    fn asymmetric_fluid_side_clipping_uses_the_emitted_basis_for_every_axis() {
        let state = RuntimeBlockStateId(7);
        let geometry = DimensionGeometry {
            min_y: 0,
            height: 16,
        };
        let heights = [0.82, 0.71, 0.64, 0.53];
        let cases = [
            (Direction::North, [[0.0, 0.0, 0.0], [0.25, 0.5, 0.5]]),
            (Direction::South, [[0.75, 0.0, 0.5], [1.0, 0.5, 1.0]]),
            (Direction::West, [[0.0, 0.0, 0.75], [0.5, 0.5, 1.0]]),
            (Direction::East, [[0.5, 0.0, 0.0], [1.0, 0.5, 0.25]]),
        ];

        for (kind, texture, layer, emissive) in [
            (
                FluidKind::Water,
                "minecraft:block/water_flow",
                RenderLayer::Translucent,
                false,
            ),
            (
                FluidKind::Lava,
                "minecraft:block/lava_flow",
                RenderLayer::Opaque,
                true,
            ),
        ] {
            let resources = BlockResources::synthetic_fluid(
                state,
                cubic_world::FluidState {
                    kind,
                    level: 0,
                    falling: false,
                },
            );
            let region = resources.atlas.region(texture);
            for (direction, solid_box) in cases {
                assert_eq!(
                    fluid_side_horizontal_bounds(solid_box, direction),
                    [0.0, 0.25],
                    "{kind:?} {direction:?}"
                );
                let solid_normal = if fluid_side_outward_is_positive(direction) {
                    0.75
                } else {
                    0.25
                };
                assert!(fluid_side_sample_is_solid(
                    &[solid_box],
                    direction,
                    0.125,
                    0.25,
                    solid_normal,
                ));
                assert!(!fluid_side_sample_is_solid(
                    &[solid_box],
                    direction,
                    0.625,
                    0.25,
                    solid_normal,
                ));

                let model = ModelApplication {
                    faces: Vec::new(),
                    solid_boxes: vec![solid_box],
                    x_rotation: 0,
                    y_rotation: 0,
                    uvlock: false,
                    ambient_occlusion: true,
                };
                let mut mesh = ChunkMesh::default();
                push_clipped_fluid_side(
                    &mut mesh,
                    &BTreeMap::new(),
                    geometry,
                    &resources,
                    &[],
                    0,
                    0,
                    0,
                    direction,
                    heights,
                    FLUID_FACE_EPSILON,
                    true,
                    region,
                    TintKind::None,
                    layer,
                    emissive,
                    &[&model],
                )
                .unwrap();

                let (quads, remainder) = mesh.vertices.as_chunks::<4>();
                assert!(remainder.is_empty());
                let indices = if layer == RenderLayer::Translucent {
                    &mesh.translucent_indices
                } else {
                    &mesh.indices
                };
                assert_eq!(indices.len(), quads.len() * 12);
                assert!(quads.iter().any(|quad| {
                    let center_t = (fluid_side_vertex_t(direction, quad[0].position)
                        + fluid_side_vertex_t(direction, quad[1].position))
                        * 0.5;
                    let center_y = (quad[2].position[1] + quad[0].position[1]) * 0.5;
                    center_t > 0.25 && center_y < 0.5
                }));
                assert!(!quads.iter().any(|quad| {
                    let center_t = (fluid_side_vertex_t(direction, quad[0].position)
                        + fluid_side_vertex_t(direction, quad[1].position))
                        * 0.5;
                    let center_y = (quad[2].position[1] + quad[0].position[1]) * 0.5;
                    let normal = fluid_side_vertex_normal(direction, quad[0].position);
                    center_t < 0.25
                        && center_y < 0.5
                        && (normal - fluid_side_outer_boundary(direction)).abs() < 0.01
                }));
                for (quad_index, _) in quads.iter().enumerate() {
                    let base = u32::try_from(quad_index * 4).unwrap();
                    assert_eq!(
                        &indices[quad_index * 12..quad_index * 12 + 12],
                        &[
                            base,
                            base + 1,
                            base + 2,
                            base,
                            base + 2,
                            base + 3,
                            base + 3,
                            base + 2,
                            base + 1,
                            base + 3,
                            base + 1,
                            base,
                        ],
                        "{kind:?} {direction:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn live_inner_stair_state_clipping_emits_retained_south_and_east_boundaries() {
        let state = RuntimeBlockStateId(3981);
        let resources = BlockResources::synthetic_fluid(
            state,
            cubic_world::FluidState {
                kind: FluidKind::Water,
                level: 0,
                falling: false,
            },
        );
        let region = resources.atlas.region("minecraft:block/water_flow");
        let geometry = DimensionGeometry {
            min_y: 0,
            height: 16,
        };
        let boxes = vec![
            [[0.0, 0.0, 0.0], [1.0, 0.5, 1.0]],
            [[0.5, 0.5, 0.0], [1.0, 1.0, 1.0]],
            [[0.0, 0.5, 0.5], [0.5, 1.0, 1.0]],
        ];
        let model = ModelApplication {
            faces: Vec::new(),
            solid_boxes: boxes.clone(),
            x_rotation: 0,
            y_rotation: 0,
            uvlock: false,
            ambient_occlusion: true,
        };
        let heights = [
            0.888_888_9_f32,
            0.846_560_84_f32,
            0.740_740_7_f32,
            0.808_080_8_f32,
        ];

        // This is the exact old outer-plane subdivision mask from the live
        // diagnostic. The lower slab covers both lower cells. The two upper
        // stair arms cover the upper cells, so testing only Z=1/X=1 removed
        // all four subdivisions for both directions.
        for direction in [Direction::South, Direction::East] {
            for t in [0.25, 0.75] {
                for y in [0.25, 0.75] {
                    assert!(fluid_side_sample_is_solid(&boxes, direction, t, y, 0.75,));
                }
            }
        }

        let mut mesh = ChunkMesh::default();
        for direction in [Direction::South, Direction::East] {
            push_clipped_fluid_side(
                &mut mesh,
                &BTreeMap::new(),
                geometry,
                &resources,
                &[],
                0,
                0,
                0,
                direction,
                heights,
                FLUID_FACE_EPSILON,
                true,
                region,
                TintKind::None,
                RenderLayer::Translucent,
                false,
                &[&model],
            )
            .unwrap();
        }

        let (quads, remainder) = mesh.vertices.as_chunks::<4>();
        assert!(remainder.is_empty());
        assert_eq!(quads.len(), 2);
        let south = quads
            .iter()
            .find(|quad| quad.iter().all(|vertex| vertex.position[2] == 0.499))
            .unwrap();
        assert_eq!(south[0].position, [0.5, 0.821_067_8, 0.499]);
        assert_eq!(south[1].position, [0.0, 0.867_724_9, 0.499]);
        assert_eq!(south[2].position, [0.0, 0.5, 0.499]);
        assert_eq!(south[3].position, [0.5, 0.5, 0.499]);

        let east = quads
            .iter()
            .find(|quad| quad.iter().all(|vertex| vertex.position[0] == 0.499))
            .unwrap();
        assert_eq!(east[0].position, [0.499, 0.848_484_9, 0.0]);
        assert_eq!(east[1].position, [0.499, 0.821_067_8, 0.5]);
        assert_eq!(east[2].position, [0.499, 0.5, 0.5]);
        assert_eq!(east[3].position, [0.499, 0.5, 0.0]);
    }

    fn fluid_side_vertex_t(direction: Direction, position: [f32; 3]) -> f32 {
        match direction {
            Direction::North => position[0],
            Direction::South => 1.0 - position[0],
            Direction::West => 1.0 - position[2],
            Direction::East => position[2],
            Direction::Down | Direction::Up => 0.0,
        }
    }

    fn fluid_side_vertex_normal(direction: Direction, position: [f32; 3]) -> f32 {
        match direction {
            Direction::North | Direction::South => position[2],
            Direction::West | Direction::East => position[0],
            Direction::Down | Direction::Up => 0.0,
        }
    }

    fn fluid_side_outer_boundary(direction: Direction) -> f32 {
        if fluid_side_outward_is_positive(direction) {
            1.0 - FLUID_FACE_EPSILON
        } else {
            FLUID_FACE_EPSILON
        }
    }

    #[test]
    fn clipped_sloped_side_preserves_vanilla_quad_order_diagonals_and_interpolation() {
        let state = RuntimeBlockStateId(7);
        let resources = BlockResources::synthetic_fluid(
            state,
            cubic_world::FluidState {
                kind: FluidKind::Water,
                level: 0,
                falling: false,
            },
        );
        let region = resources.atlas.region("minecraft:block/water_flow");
        let geometry = DimensionGeometry {
            min_y: 0,
            height: 16,
        };
        let heights = [0.9, 0.9, 0.6, 0.6];
        let clipping_model = ModelApplication {
            faces: Vec::new(),
            solid_boxes: vec![[[0.25, 0.0, 0.0], [0.75, 0.25, 1.0]]],
            x_rotation: 0,
            y_rotation: 0,
            uvlock: false,
            ambient_occlusion: true,
        };
        let chunks = BTreeMap::new();
        let mut mesh = ChunkMesh::default();
        push_clipped_fluid_side(
            &mut mesh,
            &chunks,
            geometry,
            &resources,
            &[],
            0,
            0,
            0,
            Direction::North,
            heights,
            FLUID_FACE_EPSILON,
            true,
            region,
            TintKind::None,
            RenderLayer::Translucent,
            false,
            &[&clipping_model],
        )
        .unwrap();

        assert_eq!(mesh.vertices.len(), 20);
        assert_eq!(mesh.translucent_indices.len(), 60);
        let (quads, remainder) = mesh.vertices.as_chunks::<4>();
        assert!(remainder.is_empty());
        for (quad_index, vertices) in quads.iter().enumerate() {
            let t0 = vertices[0].position[0];
            let t1 = vertices[1].position[0];
            assert!(t0 < t1, "quad {quad_index}");
            assert_eq!(vertices[1].position[0], vertices[2].position[0]);
            assert_eq!(vertices[0].position[0], vertices[3].position[0]);
            assert_eq!(vertices[2].position[1], vertices[3].position[1]);
            assert_eq!(vertices[0].position[2], FLUID_FACE_EPSILON);
            assert_eq!(vertices[1].position[2], FLUID_FACE_EPSILON);

            let atlas_uv = |uv: [f32; 2]| {
                [
                    region.min[0] + (region.max[0] - region.min[0]) * uv[0],
                    region.min[1] + (region.max[1] - region.min[1]) * uv[1],
                ]
            };
            let expected = fluid_side_uvs(
                t0,
                t1,
                vertices[3].position[1],
                vertices[0].position[1],
                vertices[1].position[1],
            )
            .map(atlas_uv);
            assert_eq!(
                [
                    vertices[0].uv,
                    vertices[1].uv,
                    vertices[2].uv,
                    vertices[3].uv
                ],
                expected
            );

            let indices = &mesh.translucent_indices[quad_index * 12..quad_index * 12 + 12];
            let base = u32::try_from(quad_index * 4).unwrap();
            assert_eq!(
                indices,
                &[
                    base,
                    base + 1,
                    base + 2,
                    base,
                    base + 2,
                    base + 3,
                    base + 3,
                    base + 2,
                    base + 1,
                    base + 3,
                    base + 1,
                    base,
                ]
            );
        }

        // Adjacent subdivision cells must share the exact boundary position;
        // no independent interpolation may create a crack or folded strip.
        for left in quads {
            for right in quads {
                if (left[1].position[0] - right[0].position[0]).abs() < 1.0e-7
                    && (left[2].position[1] - right[3].position[1]).abs() < 1.0e-7
                {
                    assert_eq!(left[1].position, right[0].position);
                    assert_eq!(left[2].position, right[3].position);
                }
            }
        }
    }

    #[test]
    fn flowing_side_uvs_use_half_sprite_and_track_physical_height() {
        let full = fluid_side_uvs(0.0, 1.0, 0.0, 1.0, 1.0);
        assert_eq!(full, [[0.0, 0.0], [0.5, 0.0], [0.5, 0.5], [0.0, 0.5]]);

        let partial = fluid_side_uvs(0.0, 1.0, 0.0, 0.75, 0.25);
        assert_eq!(partial[0], [0.0, 0.125]);
        assert_eq!(partial[1], [0.5, 0.375]);
        assert_eq!(partial[2][1], 0.5);
        assert_eq!(partial[3][1], 0.5);
        // TextureAtlasSprite.getU/getV use normalized sprite coordinates in
        // 26.1.2. A 32px animated flow frame sampled over 0..0.5 therefore
        // maps exactly 16 source texels across one full block side.
        assert_eq!((full[1][0] - full[0][0]) * 32.0, 16.0);
        assert_eq!((full[3][1] - full[0][1]) * 32.0, 16.0);
    }

    #[test]
    fn fluid_corner_weighting_matches_current_renderer_rules() {
        assert!(
            (weighted_fluid_height([Some(8.0 / 9.0), Some(0.0), Some(0.0), None]) - 80.0 / 108.0)
                .abs()
                < 1.0e-9
        );
        assert!((weighted_fluid_height([Some(7.0 / 9.0); 4]) - 7.0 / 9.0).abs() < 1.0e-9);
        assert_eq!(weighted_fluid_height([None; 4]), 0.0);
    }

    #[test]
    fn vanilla_model_offsets_are_deterministic_and_xyz_never_raises_vegetation() {
        for (x, z) in [(3, 7), (-3, -7), (-17, 31), (0, 0)] {
            let first = model_offset(ModelOffset::Xyz, x, z);
            assert_eq!(first, model_offset(ModelOffset::Xyz, x, z));
            assert!((-0.25..=0.25).contains(&first[0]));
            assert!((-0.2..=0.0).contains(&first[1]));
            assert!((-0.25..=0.25).contains(&first[2]));
            let xz = model_offset(ModelOffset::Xz, x, z);
            assert_eq!(xz[1], 0.0);
            assert_eq!([xz[0], xz[2]], [first[0], first[2]]);
        }
        assert_ne!(
            model_offset(ModelOffset::Xyz, 3, 7),
            model_offset(ModelOffset::Xyz, 4, 7)
        );
    }

    #[test]
    fn encoded_domain_terrain_composition_matches_vanilla_rgba8_multiplication() {
        fn srgb_to_linear(value: f32) -> f32 {
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        }
        fn linear_to_srgb(value: f32) -> f32 {
            if value <= 0.003_130_8 {
                value * 12.92
            } else {
                1.055 * value.powf(1.0 / 2.4) - 0.055
            }
        }
        for (texture_code, tint_code) in [(0.5, 0.5), (0.8, 0.7), (0.2, 1.0)] {
            let sampled_linear = srgb_to_linear(texture_code);
            let shader_linear = srgb_to_linear(linear_to_srgb(sampled_linear) * tint_code);
            let presented_code = linear_to_srgb(shader_linear);
            assert!((presented_code - texture_code * tint_code).abs() < 1.0e-5);
        }
    }

    #[test]
    fn waterlogged_partial_model_clipping_suppresses_only_solid_topology() {
        let lower_straight = [
            [[0.0, 0.0, 0.0], [1.0, 0.5, 1.0]],
            [[0.5, 0.5, 0.0], [1.0, 1.0, 1.0]],
        ];
        assert!(fluid_top_cell_is_enclosed(
            &lower_straight,
            0.75,
            8.0 / 9.0,
            0.5
        ));
        assert!(!fluid_top_cell_is_enclosed(
            &lower_straight,
            0.25,
            8.0 / 9.0,
            0.5
        ));

        let upper_straight = [
            [[0.0, 0.5, 0.0], [1.0, 1.0, 1.0]],
            [[0.0, 0.0, 0.0], [0.5, 0.5, 1.0]],
        ];
        assert!(fluid_top_cell_is_enclosed(
            &upper_straight,
            0.25,
            8.0 / 9.0,
            0.5
        ));
        assert!(fluid_top_cell_is_enclosed(
            &upper_straight,
            0.75,
            8.0 / 9.0,
            0.5
        ));

        let inner_corner = [
            [[0.0, 0.0, 0.0], [1.0, 0.5, 1.0]],
            [[0.5, 0.5, 0.0], [1.0, 1.0, 1.0]],
            [[0.0, 0.5, 0.5], [0.5, 1.0, 1.0]],
        ];
        assert!(fluid_top_cell_is_enclosed(
            &inner_corner,
            0.25,
            8.0 / 9.0,
            0.75
        ));
        assert!(!fluid_top_cell_is_enclosed(
            &inner_corner,
            0.25,
            8.0 / 9.0,
            0.25
        ));

        let lower_slab = [lower_straight[0]];
        let upper_slab = [upper_straight[0]];
        for direction in [
            Direction::North,
            Direction::South,
            Direction::West,
            Direction::East,
        ] {
            assert!(fluid_side_sample_is_solid(
                &lower_slab,
                direction,
                0.25,
                0.25,
                0.5,
            ));
            assert!(!fluid_side_sample_is_solid(
                &lower_slab,
                direction,
                0.25,
                0.75,
                0.5,
            ));
            assert!(!fluid_side_sample_is_solid(
                &upper_slab,
                direction,
                0.75,
                0.25,
                0.5,
            ));
            assert!(fluid_side_sample_is_solid(
                &upper_slab,
                direction,
                0.75,
                0.75,
                0.5,
            ));
        }
    }
}
