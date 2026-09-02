use std::{collections::BTreeMap, sync::Arc};

use cubic_version::GameData;
use thiserror::Error;

use crate::{
    Aabb, BlockCollisionProfile, BlockCoordinates, BlockVisualProfile, ChunkCoordinate,
    CollisionShape, DimensionGeometry, GameMode, LoadedChunks, RuntimeBlockStateId, Vec3d,
};

const MAX_RAYCAST_CELLS: usize = 64;
const DIRECTION_EPSILON: f64 = 1.0e-12;
const HIT_EPSILON: f64 = 1.0e-9;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockFace {
    Down,
    Up,
    North,
    South,
    West,
    East,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BlockTarget {
    pub position: BlockCoordinates,
    pub state: RuntimeBlockStateId,
    pub face: BlockFace,
    pub hit: Vec3d,
    pub distance: f64,
    /// Exact outline component intersected by the ray, in world coordinates.
    pub bounds: Aabb,
    /// Every component of the selected block's outline, in world coordinates.
    /// Rendering combines these into the external voxel-shape edge set while
    /// interaction continues to use `bounds` for the exact hit face.
    pub outline: Arc<[Aabb]>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlockReach(f64);

impl BlockReach {
    pub const SURVIVAL: Self = Self(4.5);
    pub const CREATIVE: Self = Self(5.0);

    #[must_use]
    pub const fn for_game_mode(mode: GameMode) -> Self {
        match mode {
            GameMode::Creative => Self::CREATIVE,
            GameMode::Survival | GameMode::Adventure | GameMode::Spectator | GameMode::Other(_) => {
                Self::SURVIVAL
            }
        }
    }

    #[must_use]
    pub const fn blocks(self) -> f64 {
        self.0
    }
}

/// Version-selected outline shapes. This is deliberately distinct from the
/// physical collision profile: non-colliding plants and scaffolding remain
/// targetable. Exact generated outline shapes can replace conservative entries
/// without changing ray traversal or movement collision.
#[derive(Clone, Debug)]
pub struct BlockOutlineProfile {
    states: BTreeMap<RuntimeBlockStateId, CollisionShape>,
    bare_hand_destroy_progress: BTreeMap<RuntimeBlockStateId, f32>,
    air_state: Option<RuntimeBlockStateId>,
}

impl BlockOutlineProfile {
    #[must_use]
    pub fn from_game_data(data: &GameData) -> Self {
        let rules = crate::collision_vanilla::CollisionRuleSet::for_version(
            &data.artifact().minecraft_version,
        );
        let collision = BlockCollisionProfile::from_game_data(data);
        let visual = BlockVisualProfile::from_game_data(data)
            .unwrap_or_else(|_| BlockVisualProfile::from_air_states([]));
        let mut states = BTreeMap::new();
        let mut bare_hand_destroy_progress = BTreeMap::new();
        let mut air_state = None;
        for block in &data.artifact().blocks {
            let path = block
                .identifier
                .as_str()
                .split_once(':')
                .map_or(block.identifier.as_str(), |(_, path)| path);
            for state in &block.states {
                let id = RuntimeBlockStateId(state.state_id);
                if block.identifier.as_str() == "minecraft:air"
                    && state.state_id == block.default_state_id
                {
                    air_state = Some(id);
                }
                let shape = if visual.is_air(id) {
                    CollisionShape::Empty
                } else {
                    match collision.shape(id) {
                        CollisionShape::Empty if collision.environment(id).fluid.is_some() => {
                            CollisionShape::Empty
                        }
                        _ => rules.outline_shape(path, &state.properties),
                    }
                };
                states.insert(id, shape);
                bare_hand_destroy_progress.insert(id, rules.bare_hand_destroy_progress(path));
            }
        }
        Self {
            states,
            bare_hand_destroy_progress,
            air_state,
        }
    }

    #[must_use]
    pub fn synthetic(
        states: impl IntoIterator<Item = (RuntimeBlockStateId, CollisionShape)>,
    ) -> Self {
        Self {
            states: states.into_iter().collect(),
            bare_hand_destroy_progress: BTreeMap::new(),
            air_state: None,
        }
    }

    #[must_use]
    pub fn synthetic_with_break_progress(
        states: impl IntoIterator<Item = (RuntimeBlockStateId, CollisionShape, f32)>,
    ) -> Self {
        let mut shapes = BTreeMap::new();
        let mut progress = BTreeMap::new();
        for (state, shape, per_tick) in states {
            shapes.insert(state, shape);
            progress.insert(state, per_tick.max(0.0));
        }
        Self {
            states: shapes,
            bare_hand_destroy_progress: progress,
            air_state: None,
        }
    }

    #[must_use]
    pub fn shape(&self, state: RuntimeBlockStateId) -> &CollisionShape {
        self.states.get(&state).unwrap_or(&CollisionShape::Empty)
    }

    #[must_use]
    pub fn bare_hand_destroy_progress(&self, state: RuntimeBlockStateId) -> f32 {
        self.bare_hand_destroy_progress
            .get(&state)
            .copied()
            .unwrap_or(0.0)
    }

    /// Exact-version default air state used for safe completed-break prediction.
    #[must_use]
    pub const fn air_state(&self) -> Option<RuntimeBlockStateId> {
        self.air_state
    }

    #[cfg(test)]
    pub fn set_synthetic_air_state(&mut self, state: RuntimeBlockStateId) {
        self.air_state = Some(state);
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq)]
pub enum RaycastError {
    #[error("block ray contains a non-finite value")]
    NonFinite,
    #[error("block ray direction has zero length")]
    ZeroDirection,
    #[error("block ray reach {value} is outside the supported range")]
    InvalidReach { value: f64 },
}

pub fn raycast_blocks(
    chunks: &LoadedChunks,
    geometry: DimensionGeometry,
    profile: &BlockOutlineProfile,
    origin: Vec3d,
    direction: Vec3d,
    reach: BlockReach,
) -> Result<Option<BlockTarget>, RaycastError> {
    if !origin.is_finite() || !direction.is_finite() {
        return Err(RaycastError::NonFinite);
    }
    let reach = reach.blocks();
    if !reach.is_finite() || !(0.0..=32.0).contains(&reach) {
        return Err(RaycastError::InvalidReach { value: reach });
    }
    let length =
        (direction.x * direction.x + direction.y * direction.y + direction.z * direction.z).sqrt();
    if length <= DIRECTION_EPSILON {
        return Err(RaycastError::ZeroDirection);
    }
    let direction = Vec3d::new(
        direction.x / length,
        direction.y / length,
        direction.z / length,
    );
    let mut cell = BlockCoordinates {
        x: floor_i32(origin.x),
        y: floor_i32(origin.y),
        z: floor_i32(origin.z),
    };
    let (step_x, mut t_x, delta_x) = axis_step(origin.x, direction.x, cell.x);
    let (step_y, mut t_y, delta_y) = axis_step(origin.y, direction.y, cell.y);
    let (step_z, mut t_z, delta_z) = axis_step(origin.z, direction.z, cell.z);

    for _ in 0..MAX_RAYCAST_CELLS {
        if let Some(state) = block_state_at(chunks, geometry, cell)
            && let Some((distance, face, bounds, outline)) =
                intersect_shape(origin, direction, reach, cell, profile.shape(state))
        {
            return Ok(Some(BlockTarget {
                position: cell,
                state,
                face,
                hit: Vec3d::new(
                    origin.x + direction.x * distance,
                    origin.y + direction.y * distance,
                    origin.z + direction.z * distance,
                ),
                distance,
                bounds,
                outline,
            }));
        }

        let next = t_x.min(t_y).min(t_z);
        if next > reach + HIT_EPSILON {
            break;
        }
        // Stable X/Y/Z tie order matches the deterministic traversal contract.
        if t_x <= t_y && t_x <= t_z {
            cell.x = cell.x.saturating_add(step_x);
            t_x += delta_x;
        } else if t_y <= t_z {
            cell.y = cell.y.saturating_add(step_y);
            t_y += delta_y;
        } else {
            cell.z = cell.z.saturating_add(step_z);
            t_z += delta_z;
        }
    }
    Ok(None)
}

fn block_state_at(
    chunks: &LoadedChunks,
    geometry: DimensionGeometry,
    position: BlockCoordinates,
) -> Option<RuntimeBlockStateId> {
    if position.y < geometry.min_y || position.y >= geometry.min_y + geometry.height as i32 {
        return None;
    }
    let chunk = chunks.get(ChunkCoordinate {
        x: position.x.div_euclid(16),
        z: position.z.div_euclid(16),
    })?;
    let section = usize::try_from((position.y - geometry.min_y).div_euclid(16)).ok()?;
    chunk.sections.get(section)?.block(
        u8::try_from(position.x.rem_euclid(16)).ok()?,
        u8::try_from(position.y.rem_euclid(16)).ok()?,
        u8::try_from(position.z.rem_euclid(16)).ok()?,
    )
}

fn intersect_shape(
    origin: Vec3d,
    direction: Vec3d,
    reach: f64,
    cell: BlockCoordinates,
    shape: &CollisionShape,
) -> Option<(f64, BlockFace, Aabb, Arc<[Aabb]>)> {
    let full = Aabb::new(Vec3d::new(0.0, 0.0, 0.0), Vec3d::new(1.0, 1.0, 1.0));
    let boxes: &[Aabb] = match shape {
        CollisionShape::Empty => return None,
        CollisionShape::FullCube => std::slice::from_ref(&full),
        CollisionShape::Boxes(boxes) => boxes,
    };
    let world_boxes: Arc<[Aabb]> = boxes
        .iter()
        .map(|bounds| bounds.translated(f64::from(cell.x), f64::from(cell.y), f64::from(cell.z)))
        .collect();
    world_boxes
        .iter()
        .filter_map(|bounds| {
            intersect_box(origin, direction, *bounds, reach)
                .map(|(distance, face)| (distance, face, *bounds, Arc::clone(&world_boxes)))
        })
        .min_by(|left, right| left.0.total_cmp(&right.0))
}

fn intersect_box(
    origin: Vec3d,
    direction: Vec3d,
    bounds: Aabb,
    reach: f64,
) -> Option<(f64, BlockFace)> {
    let mut near = 0.0_f64;
    let mut far = reach;
    let mut face = None;
    for (o, d, min, max, low_face, high_face) in [
        (
            origin.x,
            direction.x,
            bounds.min.x,
            bounds.max.x,
            BlockFace::West,
            BlockFace::East,
        ),
        (
            origin.y,
            direction.y,
            bounds.min.y,
            bounds.max.y,
            BlockFace::Down,
            BlockFace::Up,
        ),
        (
            origin.z,
            direction.z,
            bounds.min.z,
            bounds.max.z,
            BlockFace::North,
            BlockFace::South,
        ),
    ] {
        if d.abs() <= DIRECTION_EPSILON {
            if o < min - HIT_EPSILON || o > max + HIT_EPSILON {
                return None;
            }
            continue;
        }
        let first = (min - o) / d;
        let second = (max - o) / d;
        let (axis_near, axis_far, axis_face) = if first <= second {
            (first, second, low_face)
        } else {
            (second, first, high_face)
        };
        if axis_near > near {
            near = axis_near;
            face = Some(axis_face);
        }
        far = far.min(axis_far);
        if near > far + HIT_EPSILON {
            return None;
        }
    }
    if far < -HIT_EPSILON || near > reach + HIT_EPSILON {
        return None;
    }
    Some((
        near.max(0.0),
        face.unwrap_or_else(|| inside_face(direction)),
    ))
}

fn inside_face(direction: Vec3d) -> BlockFace {
    let (x, y, z) = (direction.x.abs(), direction.y.abs(), direction.z.abs());
    if x >= y && x >= z {
        if direction.x >= 0.0 {
            BlockFace::West
        } else {
            BlockFace::East
        }
    } else if y >= z {
        if direction.y >= 0.0 {
            BlockFace::Down
        } else {
            BlockFace::Up
        }
    } else if direction.z >= 0.0 {
        BlockFace::North
    } else {
        BlockFace::South
    }
}

fn axis_step(origin: f64, direction: f64, cell: i32) -> (i32, f64, f64) {
    if direction > DIRECTION_EPSILON {
        (
            1,
            (f64::from(cell) + 1.0 - origin) / direction,
            1.0 / direction,
        )
    } else if direction < -DIRECTION_EPSILON {
        (-1, (f64::from(cell) - origin) / direction, -1.0 / direction)
    } else {
        (0, f64::INFINITY, f64::INFINITY)
    }
}

fn floor_i32(value: f64) -> i32 {
    value
        .floor()
        .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Chunk, ChunkLightSummary, ChunkSection, PalettedContainer, RuntimeBiomeId,
        SECTION_BIOME_COUNT, SECTION_BLOCK_COUNT,
    };

    fn world_with(state: RuntimeBlockStateId, x: i32, y: i32, z: i32) -> LoadedChunks {
        let mut values = vec![RuntimeBlockStateId(0); SECTION_BLOCK_COUNT];
        let index = usize::try_from(y.rem_euclid(16)).unwrap() * 256
            + usize::try_from(z.rem_euclid(16)).unwrap() * 16
            + usize::try_from(x.rem_euclid(16)).unwrap();
        values[index] = state;
        let mut chunks = LoadedChunks::default();
        chunks
            .insert(Chunk {
                coordinate: ChunkCoordinate::new(x.div_euclid(16), z.div_euclid(16)),
                sections: vec![ChunkSection {
                    non_empty_block_count: 1,
                    fluid_count: 0,
                    blocks: PalettedContainer::Direct { values },
                    biomes: PalettedContainer::Single {
                        value: RuntimeBiomeId(0),
                        entries: SECTION_BIOME_COUNT,
                    },
                }],
                heightmaps: Vec::new(),
                block_entities: Vec::new(),
                light: ChunkLightSummary::default(),
            })
            .unwrap();
        chunks
    }

    fn geometry() -> DimensionGeometry {
        DimensionGeometry {
            min_y: 0,
            height: 16,
        }
    }

    #[test]
    fn full_cube_hits_all_axes_and_nearest_face() {
        let state = RuntimeBlockStateId(1);
        let chunks = world_with(state, 1, 1, 1);
        let profile = BlockOutlineProfile::synthetic([(state, CollisionShape::FullCube)]);
        let cases = [
            (
                Vec3d::new(0.0, 1.5, 1.5),
                Vec3d::new(1.0, 0.0, 0.0),
                BlockFace::West,
            ),
            (
                Vec3d::new(3.0, 1.5, 1.5),
                Vec3d::new(-1.0, 0.0, 0.0),
                BlockFace::East,
            ),
            (
                Vec3d::new(1.5, 0.0, 1.5),
                Vec3d::new(0.0, 1.0, 0.0),
                BlockFace::Down,
            ),
            (
                Vec3d::new(1.5, 3.0, 1.5),
                Vec3d::new(0.0, -1.0, 0.0),
                BlockFace::Up,
            ),
            (
                Vec3d::new(1.5, 1.5, 0.0),
                Vec3d::new(0.0, 0.0, 1.0),
                BlockFace::North,
            ),
            (
                Vec3d::new(1.5, 1.5, 3.0),
                Vec3d::new(0.0, 0.0, -1.0),
                BlockFace::South,
            ),
        ];
        for (origin, direction, face) in cases {
            let hit = raycast_blocks(
                &chunks,
                geometry(),
                &profile,
                origin,
                direction,
                BlockReach(5.0),
            )
            .unwrap()
            .unwrap();
            assert_eq!(hit.position, BlockCoordinates { x: 1, y: 1, z: 1 });
            assert_eq!(hit.face, face);
        }
    }

    #[test]
    fn partial_shape_miss_hit_inside_and_reach_boundary_are_bounded() {
        let state = RuntimeBlockStateId(2);
        let chunks = world_with(state, -1, 0, -1);
        let profile = BlockOutlineProfile::synthetic([(
            state,
            CollisionShape::Boxes(
                [Aabb::new(
                    Vec3d::new(0.0, 0.0, 0.0),
                    Vec3d::new(1.0, 0.5, 1.0),
                )]
                .into(),
            ),
        )]);
        assert!(
            raycast_blocks(
                &chunks,
                geometry(),
                &profile,
                Vec3d::new(-2.0, 0.75, -0.5),
                Vec3d::new(1.0, 0.0, 0.0),
                BlockReach(5.0),
            )
            .unwrap()
            .is_none()
        );
        let inside = raycast_blocks(
            &chunks,
            geometry(),
            &profile,
            Vec3d::new(-0.5, 0.25, -0.5),
            Vec3d::new(1.0, 0.0, 0.0),
            BlockReach(0.0),
        )
        .unwrap()
        .unwrap();
        assert_eq!(inside.distance, 0.0);
        assert_eq!(inside.position, BlockCoordinates { x: -1, y: 0, z: -1 });
    }

    #[test]
    fn diagonal_edges_are_deterministic_and_bad_rays_are_rejected() {
        let state = RuntimeBlockStateId(3);
        let chunks = world_with(state, 1, 1, 1);
        let profile = BlockOutlineProfile::synthetic([(state, CollisionShape::FullCube)]);
        let hit = raycast_blocks(
            &chunks,
            geometry(),
            &profile,
            Vec3d::new(0.0, 0.0, 0.0),
            Vec3d::new(1.0, 1.0, 1.0),
            BlockReach(5.0),
        )
        .unwrap()
        .unwrap();
        assert_eq!(hit.position, BlockCoordinates { x: 1, y: 1, z: 1 });
        assert!(matches!(
            raycast_blocks(
                &chunks,
                geometry(),
                &profile,
                Vec3d::new(0.0, 0.0, 0.0),
                Vec3d::default(),
                BlockReach(5.0),
            ),
            Err(RaycastError::ZeroDirection)
        ));
    }

    #[test]
    fn multipart_scaffolding_ray_and_render_target_share_one_identity() {
        let scaffold = RuntimeBlockStateId(21);
        let rear = RuntimeBlockStateId(22);
        let mut values = vec![RuntimeBlockStateId(0); SECTION_BLOCK_COUNT];
        values[0] = scaffold;
        values[16] = rear;
        let mut chunks = LoadedChunks::default();
        chunks
            .insert(Chunk {
                coordinate: ChunkCoordinate::new(0, 0),
                sections: vec![ChunkSection {
                    non_empty_block_count: 2,
                    fluid_count: 0,
                    blocks: PalettedContainer::Direct { values },
                    biomes: PalettedContainer::Single {
                        value: RuntimeBiomeId(0),
                        entries: SECTION_BIOME_COUNT,
                    },
                }],
                heightmaps: Vec::new(),
                block_entities: Vec::new(),
                light: ChunkLightSummary::default(),
            })
            .unwrap();
        let post = 2.0 / 16.0;
        let profile = BlockOutlineProfile::synthetic([
            (
                scaffold,
                CollisionShape::Boxes(
                    [
                        Aabb::new(Vec3d::new(0.0, 14.0 / 16.0, 0.0), Vec3d::new(1.0, 1.0, 1.0)),
                        Aabb::new(Vec3d::new(0.0, 0.0, 0.0), Vec3d::new(post, 1.0, post)),
                    ]
                    .into(),
                ),
            ),
            (rear, CollisionShape::FullCube),
        ]);
        let rear_target = raycast_blocks(
            &chunks,
            geometry(),
            &profile,
            Vec3d::new(0.5, 0.5, -1.0),
            Vec3d::new(0.0, 0.0, 1.0),
            BlockReach(5.0),
        )
        .unwrap()
        .unwrap();
        assert_eq!(rear_target.position, BlockCoordinates { x: 0, y: 0, z: 1 });
        assert_eq!(rear_target.state, rear);
        assert_eq!(rear_target.outline.as_ref(), &[rear_target.bounds]);

        let scaffold_target = raycast_blocks(
            &chunks,
            geometry(),
            &profile,
            Vec3d::new(0.05, 0.5, -1.0),
            Vec3d::new(0.0, 0.0, 1.0),
            BlockReach(5.0),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            scaffold_target.position,
            BlockCoordinates { x: 0, y: 0, z: 0 }
        );
        assert_eq!(scaffold_target.state, scaffold);
    }
}
