use std::{collections::BTreeMap, sync::Arc, time::Duration};

use bytemuck::{Pod, Zeroable};
use cubic_world::{Chunk, ChunkCoordinate, DimensionGeometry, RuntimeBlockStateId};
use thiserror::Error;

use crate::block_resources::{
    BlockResources, Direction, ModelApplication, RenderLayer, rotate_blockstate_corner,
    rotate_blockstate_direction, uvlock_quarter_turns,
};

pub const MAX_CHUNK_MESH_FACES: usize = 1_000_000;

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
    pub statistics: MeshStatistics,
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

pub(crate) fn mesh_chunk(
    coordinate: ChunkCoordinate,
    chunks: &BTreeMap<ChunkCoordinate, Arc<Chunk>>,
    geometry: DimensionGeometry,
    resources: &BlockResources,
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
        statistics: MeshStatistics::default(),
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
                    if models.parts.is_empty() {
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
                    for part in &models.parts {
                        mesh.statistics.model_selections =
                            mesh.statistics.model_selections.saturating_add(1);
                        let Some(model) = select_model(part, &mut variant_random) else {
                            continue;
                        };
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
                            push_model_face(&mut mesh, world_x, world_y, world_z, model, face)?;
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

fn push_model_face(
    mesh: &mut ChunkMesh,
    x: i32,
    y: i32,
    z: i32,
    model: &ModelApplication,
    face: &crate::block_resources::ModelFace,
) -> Result<(), MeshError> {
    if mesh.indices.len() / 6 >= MAX_CHUNK_MESH_FACES {
        return Err(MeshError::FaceLimit {
            max: MAX_CHUNK_MESH_FACES,
        });
    }
    let base = u32::try_from(mesh.vertices.len()).map_err(|_| MeshError::IndexOverflow)?;
    let region = face.atlas_region;
    let tint_base = if face.tint_index.is_some() {
        [0.58, 0.78, 0.42]
    } else {
        [1.0; 3]
    };
    let mut uvs = face.uv;
    if model.uvlock {
        uvs.rotate_left(uvlock_quarter_turns(
            face.direction,
            model.x_rotation,
            model.y_rotation,
        ));
    }
    for (corner, uv) in face.corners.into_iter().zip(uvs) {
        let corner = rotate_blockstate_corner(corner, model.x_rotation, model.y_rotation);
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
            tint: tint_base.map(|value| value * face.shade),
            layer: u32::from(region.layer == RenderLayer::Cutout),
        });
    }
    mesh.indices
        .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    mesh.statistics.quads_emitted = mesh.statistics.quads_emitted.saturating_add(1);
    Ok(())
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
        Arc::new(Chunk {
            coordinate,
            sections: vec![ChunkSection {
                non_empty_block_count: states.iter().filter(|state| state.0 != 0).count() as u16,
                fluid_count: 0,
                blocks: PalettedContainer::Direct { values: states },
                biomes: PalettedContainer::Single {
                    value: cubic_world::RuntimeBiomeId(0),
                    entries: 64,
                },
            }],
            heightmaps: vec![],
            block_entities: vec![],
            light: ChunkLightSummary::default(),
        })
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
            x_rotation,
            y_rotation,
            uvlock: false,
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
            x_rotation: 0,
            y_rotation: rotation,
            uvlock: false,
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
            shade: 1.0,
        };
        let mut unlocked = ChunkMesh::default();
        push_model_face(
            &mut unlocked,
            0,
            0,
            0,
            &ModelApplication {
                faces: vec![face.clone()],
                x_rotation: 0,
                y_rotation: 90,
                uvlock: false,
            },
            &face,
        )
        .unwrap();
        assert_eq!(unlocked.vertices[0].uv, [0.0, 1.0]);
        assert_eq!(unlocked.vertices[1].uv, [0.0, 0.0]);

        let mut locked = ChunkMesh::default();
        push_model_face(
            &mut locked,
            0,
            0,
            0,
            &ModelApplication {
                faces: vec![face.clone()],
                x_rotation: 0,
                y_rotation: 90,
                uvlock: true,
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
}
