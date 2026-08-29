use std::{collections::BTreeMap, sync::Arc};

use bytemuck::{Pod, Zeroable};
use cubic_world::{
    BlockVisualProfile, Chunk, ChunkCoordinate, DimensionGeometry, RuntimeBlockStateId,
};
use thiserror::Error;

pub const MAX_CHUNK_MESH_FACES: usize = 1_000_000;

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub(crate) struct TerrainVertex {
    pub position: [f32; 3],
    pub color: [f32; 3],
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ChunkMesh {
    pub vertices: Vec<TerrainVertex>,
    pub indices: Vec<u32>,
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
    visual: &BlockVisualProfile,
) -> Result<ChunkMesh, MeshError> {
    let Some(chunk) = chunks.get(&coordinate) else {
        return Ok(ChunkMesh::default());
    };
    if chunk.sections.len() != geometry.section_count() {
        return Err(MeshError::SectionCount {
            actual: chunk.sections.len(),
            expected: geometry.section_count(),
        });
    }
    let mut mesh = ChunkMesh::default();
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
                    if visual.is_air(state) {
                        continue;
                    }
                    let world_x = coordinate.x.saturating_mul(16) + i32::from(x);
                    let world_y = geometry.min_y
                        + i32::try_from(section_index)
                            .unwrap_or(i32::MAX)
                            .saturating_mul(16)
                        + i32::from(y);
                    let world_z = coordinate.z.saturating_mul(16) + i32::from(z);
                    for face in FACES {
                        let neighbor = block_at(
                            chunks,
                            geometry,
                            world_x + face.normal[0],
                            world_y + face.normal[1],
                            world_z + face.normal[2],
                        );
                        if neighbor.is_none_or(|neighbor| visual.is_air(neighbor)) {
                            push_face(&mut mesh, world_x, world_y, world_z, state, face)?;
                        }
                    }
                }
            }
        }
    }
    Ok(mesh)
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

fn push_face(
    mesh: &mut ChunkMesh,
    x: i32,
    y: i32,
    z: i32,
    state: RuntimeBlockStateId,
    face: Face,
) -> Result<(), MeshError> {
    if mesh.indices.len() / 6 >= MAX_CHUNK_MESH_FACES {
        return Err(MeshError::FaceLimit {
            max: MAX_CHUNK_MESH_FACES,
        });
    }
    let base = u32::try_from(mesh.vertices.len()).map_err(|_| MeshError::IndexOverflow)?;
    let tint = diagnostic_color(state, face.shade);
    for corner in face.corners {
        mesh.vertices.push(TerrainVertex {
            position: [
                x as f32 + corner[0],
                y as f32 + corner[1],
                z as f32 + corner[2],
            ],
            color: tint,
        });
    }
    mesh.indices
        .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    Ok(())
}

fn diagnostic_color(state: RuntimeBlockStateId, shade: f32) -> [f32; 3] {
    let hash = state.0.wrapping_mul(0x9e37_79b9).rotate_left(13);
    [
        (0.28 + (hash & 0xff) as f32 / 510.0) * shade,
        (0.32 + ((hash >> 8) & 0xff) as f32 / 510.0) * shade,
        (0.30 + ((hash >> 16) & 0xff) as f32 / 510.0) * shade,
    ]
}

#[derive(Clone, Copy)]
struct Face {
    normal: [i32; 3],
    corners: [[f32; 3]; 4],
    shade: f32,
}

const FACES: [Face; 6] = [
    Face {
        normal: [1, 0, 0],
        corners: [[1., 0., 0.], [1., 1., 0.], [1., 1., 1.], [1., 0., 1.]],
        shade: 0.85,
    },
    Face {
        normal: [-1, 0, 0],
        corners: [[0., 0., 1.], [0., 1., 1.], [0., 1., 0.], [0., 0., 0.]],
        shade: 0.7,
    },
    Face {
        normal: [0, 1, 0],
        corners: [[0., 1., 0.], [0., 1., 1.], [1., 1., 1.], [1., 1., 0.]],
        shade: 1.0,
    },
    Face {
        normal: [0, -1, 0],
        corners: [[0., 0., 1.], [0., 0., 0.], [1., 0., 0.], [1., 0., 1.]],
        shade: 0.55,
    },
    Face {
        normal: [0, 0, 1],
        corners: [[1., 0., 1.], [1., 1., 1.], [0., 1., 1.], [0., 0., 1.]],
        shade: 0.8,
    },
    Face {
        normal: [0, 0, -1],
        corners: [[0., 0., 0.], [0., 1., 0.], [1., 1., 0.], [1., 0., 0.]],
        shade: 0.65,
    },
];

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
            &BlockVisualProfile::from_air_states([RuntimeBlockStateId(0)]),
        )
        .unwrap();
        assert_eq!(mesh.indices.len(), 36);
        let empty = BTreeMap::from([(coord, chunk(coord, vec![RuntimeBlockStateId(0); 4096]))]);
        assert!(
            mesh_chunk(
                coord,
                &empty,
                DimensionGeometry {
                    min_y: 0,
                    height: 16
                },
                &BlockVisualProfile::from_air_states([RuntimeBlockStateId(0)])
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
            &BlockVisualProfile::from_air_states([RuntimeBlockStateId(0)]),
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
                &BlockVisualProfile::from_air_states([])
            )
            .is_err()
        );
    }
}
