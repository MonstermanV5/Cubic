use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use cubic_version::GameData;
use thiserror::Error;

use crate::{
    BlockEnvironment, BlockEnvironmentProfile, DimensionGeometry, FluidKind, LoadedChunks,
    RuntimeBlockStateId, SpecialSurface,
    collision_vanilla::{CollisionOffset, CollisionRuleSet},
};

pub const MAX_COLLISION_BOXES_PER_STATE: usize = 8;
const PLAYER_WIDTH: f64 = 0.6;
const STANDING_HEIGHT: f64 = 1.8;
const CROUCHING_HEIGHT: f64 = 1.5;
const STANDING_EYE_HEIGHT: f64 = 1.62;
const CROUCHING_EYE_HEIGHT: f64 = 1.27;
const SWIMMING_HEIGHT: f64 = 0.6;
const SWIMMING_EYE_HEIGHT: f64 = 0.4;
const STEP_HEIGHT: f64 = 0.6;
const COLLISION_EPSILON: f64 = 1.0e-7;
const GRAVITY: f64 = 0.08;
const VERTICAL_DRAG: f64 = 0.98;
const AIR_DRAG: f64 = 0.91;
const AIR_ACCELERATION: f64 = 0.02;
const GROUND_DRAG_SCALE: f64 = 0.91;
const GROUND_ACCELERATION_SCALE: f64 = 0.21600002;
const BASE_MOVEMENT_SPEED: f64 = 0.1;
const SPRINT_MULTIPLIER: f64 = 1.3;
const SPRINT_JUMP_IMPULSE: f64 = 0.2;
const SNEAK_MULTIPLIER: f64 = 0.3;
const JUMP_VELOCITY: f64 = 0.42;
const DEFAULT_FLYING_SPEED: f64 = 0.05;
const FLYING_SPRINT_MULTIPLIER: f64 = 2.0;
const FLYING_VERTICAL_INPUT_MULTIPLIER: f64 = 3.0;
const FLYING_VERTICAL_DRAG: f64 = 0.6;
const WATER_ACCELERATION: f64 = 0.02;
const WATER_DRAG: f64 = 0.8;
const WATER_SPRINT_DRAG: f64 = 0.9;
const WATER_GRAVITY: f64 = GRAVITY / 16.0;
const LAVA_DRAG: f64 = 0.5;
const FLUID_VERTICAL_INPUT: f64 = 0.04;
const CLIMB_HORIZONTAL_LIMIT: f64 = 0.15;
const CLIMB_FALL_LIMIT: f64 = -0.15;
const CLIMB_UP_SPEED: f64 = 0.2;
const HONEY_SLIDE_SPEED: f64 = -0.05;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec3d {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Vec3d {
    #[must_use]
    pub const fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }

    #[must_use]
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aabb {
    pub min: Vec3d,
    pub max: Vec3d,
}

impl Aabb {
    #[must_use]
    pub const fn new(min: Vec3d, max: Vec3d) -> Self {
        Self { min, max }
    }

    #[must_use]
    pub fn translated(self, x: f64, y: f64, z: f64) -> Self {
        Self::new(
            Vec3d::new(self.min.x + x, self.min.y + y, self.min.z + z),
            Vec3d::new(self.max.x + x, self.max.y + y, self.max.z + z),
        )
    }

    #[must_use]
    fn expanded_toward(self, movement: Vec3d) -> Self {
        Self::new(
            Vec3d::new(
                self.min.x + movement.x.min(0.0),
                self.min.y + movement.y.min(0.0),
                self.min.z + movement.z.min(0.0),
            ),
            Vec3d::new(
                self.max.x + movement.x.max(0.0),
                self.max.y + movement.y.max(0.0),
                self.max.z + movement.z.max(0.0),
            ),
        )
    }

    #[must_use]
    fn moved(self, movement: Vec3d) -> Self {
        self.translated(movement.x, movement.y, movement.z)
    }

    #[must_use]
    fn overlaps_yz(self, other: Self) -> bool {
        overlaps(self.min.y, self.max.y, other.min.y, other.max.y)
            && overlaps(self.min.z, self.max.z, other.min.z, other.max.z)
    }

    #[must_use]
    fn overlaps_xz(self, other: Self) -> bool {
        overlaps(self.min.x, self.max.x, other.min.x, other.max.x)
            && overlaps(self.min.z, self.max.z, other.min.z, other.max.z)
    }

    #[must_use]
    fn overlaps_xy(self, other: Self) -> bool {
        overlaps(self.min.x, self.max.x, other.min.x, other.max.x)
            && overlaps(self.min.y, self.max.y, other.min.y, other.max.y)
    }
}

fn overlaps(min: f64, max: f64, other_min: f64, other_max: f64) -> bool {
    max > other_min + COLLISION_EPSILON && min < other_max - COLLISION_EPSILON
}

#[derive(Clone, Debug, PartialEq)]
pub enum CollisionShape {
    Empty,
    FullCube,
    Boxes(Arc<[Aabb]>),
}

#[derive(Clone, Debug)]
pub struct BlockCollisionProfile {
    states: BTreeMap<RuntimeBlockStateId, CollisionShape>,
    offsets: BTreeMap<RuntimeBlockStateId, CollisionOffset>,
    slipperiness: BTreeMap<RuntimeBlockStateId, f64>,
    approximate_states: BTreeSet<RuntimeBlockStateId>,
    environment: BlockEnvironmentProfile,
    source_shape_bounds: Aabb,
}

impl BlockCollisionProfile {
    #[must_use]
    pub fn from_game_data(data: &GameData) -> Self {
        let rules = CollisionRuleSet::for_version(&data.artifact().minecraft_version);
        let environment = BlockEnvironmentProfile::from_game_data(data);
        let mut states = BTreeMap::new();
        let mut offsets = BTreeMap::new();
        let mut slipperiness = BTreeMap::new();
        let mut approximate_states = BTreeSet::new();
        for block in &data.artifact().blocks {
            let path = block
                .identifier
                .as_str()
                .split_once(':')
                .map_or(block.identifier.as_str(), |(_, path)| path);
            for state in &block.states {
                let id = RuntimeBlockStateId(state.state_id);
                let semantic = environment.state(id);
                states.insert(
                    id,
                    if semantic.scaffolding {
                        CollisionShape::Empty
                    } else {
                        rules.shape(path, &state.properties)
                    },
                );
                offsets.insert(id, rules.offset(path));
                slipperiness.insert(id, classify_slipperiness(environment.state(id).surface));
                if !rules.has_verified_shape(path) {
                    approximate_states.insert(id);
                }
            }
        }
        let source_shape_bounds = collision_shape_envelope(
            states.values(),
            offsets
                .values()
                .copied()
                .map(CollisionOffset::maximum_horizontal)
                .fold(0.0, f64::max),
        );
        Self {
            states,
            offsets,
            slipperiness,
            approximate_states,
            environment,
            source_shape_bounds,
        }
    }

    #[must_use]
    pub fn synthetic(
        states: impl IntoIterator<Item = (RuntimeBlockStateId, CollisionShape)>,
    ) -> Self {
        let mut result = Self {
            states: BTreeMap::new(),
            offsets: BTreeMap::new(),
            slipperiness: BTreeMap::new(),
            approximate_states: BTreeSet::new(),
            environment: BlockEnvironmentProfile::default(),
            source_shape_bounds: full_cube_bounds(),
        };
        for (id, shape) in states {
            result.states.insert(id, shape);
            result.offsets.insert(id, CollisionOffset::None);
            result.slipperiness.insert(id, 0.6);
        }
        result.source_shape_bounds = collision_shape_envelope(result.states.values(), 0.0);
        result
    }

    #[must_use]
    pub fn shape(&self, state: RuntimeBlockStateId) -> &CollisionShape {
        self.states.get(&state).unwrap_or(&CollisionShape::FullCube)
    }

    #[must_use]
    pub fn slipperiness(&self, state: RuntimeBlockStateId) -> f64 {
        self.slipperiness.get(&state).copied().unwrap_or(0.6)
    }

    #[must_use]
    pub fn environment(&self, state: RuntimeBlockStateId) -> BlockEnvironment {
        self.environment.state(state)
    }

    #[cfg(test)]
    fn set_environment(&mut self, environment: BlockEnvironmentProfile) {
        for (state, slipperiness) in &mut self.slipperiness {
            *slipperiness = classify_slipperiness(environment.state(*state).surface);
        }
        for (state, shape) in &mut self.states {
            if environment.state(*state).scaffolding {
                *shape = CollisionShape::Empty;
            }
        }
        self.environment = environment;
    }

    #[must_use]
    pub fn is_approximate(&self, state: RuntimeBlockStateId) -> bool {
        !self.states.contains_key(&state) || self.approximate_states.contains(&state)
    }

    fn offset(&self, state: RuntimeBlockStateId, x: i32, z: i32) -> Vec3d {
        self.offsets
            .get(&state)
            .copied()
            .unwrap_or_default()
            .at(x, z)
    }
}

fn full_cube_bounds() -> Aabb {
    Aabb::new(Vec3d::new(0.0, 0.0, 0.0), Vec3d::new(1.0, 1.0, 1.0))
}

fn collision_shape_envelope<'a>(
    shapes: impl IntoIterator<Item = &'a CollisionShape>,
    maximum_horizontal_offset: f64,
) -> Aabb {
    let mut envelope = full_cube_bounds();
    for shape in shapes {
        let CollisionShape::Boxes(boxes) = shape else {
            continue;
        };
        for bounds in boxes.iter().take(MAX_COLLISION_BOXES_PER_STATE) {
            envelope.min.x = envelope.min.x.min(bounds.min.x);
            envelope.min.y = envelope.min.y.min(bounds.min.y);
            envelope.min.z = envelope.min.z.min(bounds.min.z);
            envelope.max.x = envelope.max.x.max(bounds.max.x);
            envelope.max.y = envelope.max.y.max(bounds.max.y);
            envelope.max.z = envelope.max.z.max(bounds.max.z);
        }
    }
    envelope.min.x -= maximum_horizontal_offset;
    envelope.min.z -= maximum_horizontal_offset;
    envelope.max.x += maximum_horizontal_offset;
    envelope.max.z += maximum_horizontal_offset;
    envelope
}

fn classify_slipperiness(surface: SpecialSurface) -> f64 {
    match surface {
        SpecialSurface::Ice | SpecialSurface::PackedIce | SpecialSurface::FrostedIce => 0.98,
        SpecialSurface::BlueIce => 0.989,
        SpecialSurface::Slime => 0.8,
        SpecialSurface::Ordinary | SpecialSurface::Honey => 0.6,
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MovementInput {
    pub forward: bool,
    pub backward: bool,
    pub left: bool,
    pub right: bool,
    pub jump: bool,
    pub sneak: bool,
    pub sprint: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlayerPoseKind {
    Standing,
    Sneaking,
    Swimming,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerDimensions {
    pub width: f64,
    pub height: f64,
    pub eye_height: f64,
}

impl PlayerPoseKind {
    #[must_use]
    pub const fn dimensions(self) -> PlayerDimensions {
        match self {
            Self::Standing => PlayerDimensions {
                width: PLAYER_WIDTH,
                height: STANDING_HEIGHT,
                eye_height: STANDING_EYE_HEIGHT,
            },
            Self::Sneaking => PlayerDimensions {
                width: PLAYER_WIDTH,
                height: CROUCHING_HEIGHT,
                eye_height: CROUCHING_EYE_HEIGHT,
            },
            Self::Swimming => PlayerDimensions {
                width: PLAYER_WIDTH,
                height: SWIMMING_HEIGHT,
                eye_height: SWIMMING_EYE_HEIGHT,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LocalPlayerPose {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw: f32,
    pub pitch: f32,
    pub eye_height: f64,
}

impl LocalPlayerPose {
    #[must_use]
    pub const fn new(x: f64, y: f64, z: f64, yaw: f32, pitch: f32) -> Self {
        Self {
            x,
            y,
            z,
            yaw,
            pitch,
            eye_height: STANDING_EYE_HEIGHT,
        }
    }

    #[must_use]
    pub fn is_finite(self) -> bool {
        self.x.is_finite()
            && self.y.is_finite()
            && self.z.is_finite()
            && self.yaw.is_finite()
            && self.pitch.is_finite()
            && self.eye_height.is_finite()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlayerMovementState {
    pub pose: LocalPlayerPose,
    pub velocity: Vec3d,
    pub on_ground: bool,
    pub horizontal_collision: bool,
    pub sprinting: bool,
    pub pose_kind: PlayerPoseKind,
    pub may_fly: bool,
    pub flying: bool,
    pub flying_speed: f64,
    pub swimming: bool,
    pub in_water: bool,
    pub in_lava: bool,
    pub climbing: bool,
    // Vanilla keeps sneak-edge protection active during a short descent while
    // support remains within the player's step height. This is movement state,
    // not a block-specific property.
    fall_distance: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SimulationResult {
    pub requested: Vec3d,
    pub applied: Vec3d,
    pub stepped: bool,
    pub approximate_collision: bool,
    pub flight_changed: Option<bool>,
    /// True only when this tick applied the ordinary grounded jump impulse.
    pub jumped: bool,
    /// Start-of-tick grounded state used by vanilla's travel equations.
    pub grounded_at_start: bool,
    /// Horizontal acceleration factor applied to the normalized input vector.
    pub horizontal_acceleration: f64,
    /// Horizontal velocity multiplier applied after collision movement.
    pub horizontal_drag: f64,
    /// Horizontal sprint-jump impulse applied before travel, if any.
    pub sprint_jump_impulse: f64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CollisionShapeKind {
    FullCube,
    Boxes,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CollisionCandidate {
    pub block_x: i32,
    pub block_y: i32,
    pub block_z: i32,
    pub state: Option<RuntimeBlockStateId>,
    pub shape: CollisionShapeKind,
    pub approximate: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CollisionDiagnostics {
    pub player_bounds: Aabb,
    pub candidates: Vec<CollisionCandidate>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Error, PartialEq)]
pub enum SimulationError {
    #[error("local player simulation received a non-finite {field}")]
    NonFinite { field: &'static str },
    #[error("collision broadphase is too large")]
    BroadphaseTooLarge,
}

impl PlayerMovementState {
    pub fn from_authoritative(
        pose: LocalPlayerPose,
        velocity: Vec3d,
    ) -> Result<Self, SimulationError> {
        if !pose.is_finite() {
            return Err(SimulationError::NonFinite { field: "pose" });
        }
        if !velocity.is_finite() {
            return Err(SimulationError::NonFinite { field: "velocity" });
        }
        Ok(Self {
            pose,
            velocity,
            on_ground: false,
            horizontal_collision: false,
            sprinting: false,
            pose_kind: PlayerPoseKind::Standing,
            may_fly: false,
            flying: false,
            flying_speed: DEFAULT_FLYING_SPEED,
            swimming: false,
            in_water: false,
            in_lava: false,
            climbing: false,
            fall_distance: 0.0,
        })
    }

    pub fn reconcile(
        &mut self,
        pose: LocalPlayerPose,
        velocity: Vec3d,
    ) -> Result<(), SimulationError> {
        if !pose.is_finite() || !velocity.is_finite() {
            return Err(SimulationError::NonFinite {
                field: "authoritative correction",
            });
        }
        self.pose = pose;
        self.velocity = velocity;
        self.on_ground = false;
        self.horizontal_collision = false;
        self.fall_distance = 0.0;
        // A position correction is not a world/session reset. Preserve the
        // active pose state so the next environmental sample can apply the
        // continuation rule; clearing it here forced a surface swimmer back
        // through the stricter eye-submerged entry rule.
        self.in_water = false;
        self.in_lava = false;
        self.climbing = false;
        Ok(())
    }

    pub fn rotate(&mut self, yaw_delta: f32, pitch_delta: f32) -> Result<(), SimulationError> {
        if !yaw_delta.is_finite() || !pitch_delta.is_finite() {
            return Err(SimulationError::NonFinite {
                field: "look delta",
            });
        }
        self.pose.yaw = (self.pose.yaw + yaw_delta).rem_euclid(360.0);
        self.pose.pitch = (self.pose.pitch + pitch_delta).clamp(-90.0, 90.0);
        Ok(())
    }

    pub fn set_rotation(&mut self, yaw: f32, pitch: f32) -> Result<(), SimulationError> {
        if !yaw.is_finite() || !pitch.is_finite() {
            return Err(SimulationError::NonFinite {
                field: "look intent",
            });
        }
        self.pose.yaw = yaw.rem_euclid(360.0);
        self.pose.pitch = pitch.clamp(-90.0, 90.0);
        Ok(())
    }

    pub fn apply_flight_abilities(
        &mut self,
        may_fly: bool,
        flying: bool,
        flying_speed: f32,
    ) -> Result<(), SimulationError> {
        if !flying_speed.is_finite() || flying_speed < 0.0 {
            return Err(SimulationError::NonFinite {
                field: "flying speed",
            });
        }
        self.may_fly = may_fly;
        self.flying = may_fly && flying;
        self.flying_speed = f64::from(flying_speed);
        if self.flying {
            self.pose_kind = PlayerPoseKind::Standing;
            self.pose.eye_height = STANDING_EYE_HEIGHT;
            self.fall_distance = 0.0;
        }
        Ok(())
    }

    /// Applies a locally requested creative-flight toggle after the durable
    /// double-jump gesture has been resolved by the session controller.
    pub fn set_flying(&mut self, flying: bool) -> bool {
        let resolved = self.may_fly && flying;
        if self.flying == resolved {
            return false;
        }
        self.flying = resolved;
        if resolved {
            self.fall_distance = 0.0;
            if self.on_ground {
                self.velocity.y = JUMP_VELOCITY;
                self.on_ground = false;
            }
            self.pose_kind = PlayerPoseKind::Standing;
            self.pose.eye_height = STANDING_EYE_HEIGHT;
        }
        true
    }

    #[must_use]
    pub fn bounding_box(&self) -> Aabb {
        let dimensions = self.pose_kind.dimensions();
        let half = dimensions.width * 0.5;
        Aabb::new(
            Vec3d::new(self.pose.x - half, self.pose.y, self.pose.z - half),
            Vec3d::new(
                self.pose.x + half,
                self.pose.y + dimensions.height,
                self.pose.z + half,
            ),
        )
    }

    pub fn tick(
        &mut self,
        input: MovementInput,
        chunks: &LoadedChunks,
        geometry: DimensionGeometry,
        profile: &BlockCollisionProfile,
    ) -> Result<SimulationResult, SimulationError> {
        // Vanilla chooses the travel acceleration and post-move friction from
        // the grounded state at the beginning of the tick. jumpFromGround
        // changes velocity but does not clear onGround before travel/move.
        let grounded_at_start = self.on_ground;
        let environment = movement_environment(self.bounding_box(), chunks, geometry, profile);
        self.in_water = environment.water_immersion > 0.0;
        self.in_lava = environment.lava_immersion > 0.0;
        self.climbing = environment.climbable || environment.scaffolding;
        self.swimming = !self.flying
            && (self.sprinting || input.sprint)
            && if self.swimming {
                self.in_water
            } else {
                environment.water_at_eye && input.forward
            };
        self.pose_kind = if self.swimming {
            PlayerPoseKind::Swimming
        } else if input.sneak && !self.flying {
            PlayerPoseKind::Sneaking
        } else {
            PlayerPoseKind::Standing
        };
        self.pose.eye_height = self.pose_kind.dimensions().eye_height;
        let mut forward = f64::from(i8::from(input.forward) - i8::from(input.backward));
        let mut strafe = f64::from(i8::from(input.right) - i8::from(input.left));
        let length = forward.hypot(strafe);
        if length > 1.0 {
            forward /= length;
            strafe /= length;
        }
        let has_forward_impulse = forward > 1.0e-5;
        let shallow_water = self.in_water && !environment.water_at_eye;
        if self.sprinting {
            if !has_forward_impulse
                || (self.horizontal_collision && !self.swimming)
                || (shallow_water && !self.swimming)
            {
                self.sprinting = false;
            }
        } else if input.sprint
            && has_forward_impulse
            && !input.sneak
            && (!self.in_water || environment.water_at_eye)
        {
            self.sprinting = true;
        }
        if input.sneak && !self.flying && !self.in_water && !self.in_lava {
            forward *= SNEAK_MULTIPLIER;
            strafe *= SNEAK_MULTIPLIER;
        }
        if environment.support_surface == SpecialSurface::Honey && !self.flying {
            forward *= 0.4;
            strafe *= 0.4;
        }
        let speed = if self.flying {
            self.flying_speed
                * if self.sprinting {
                    FLYING_SPRINT_MULTIPLIER
                } else {
                    1.0
                }
        } else {
            BASE_MOVEMENT_SPEED
                * if self.sprinting {
                    SPRINT_MULTIPLIER
                } else {
                    1.0
                }
        };
        let below = block_state_at(
            chunks,
            geometry,
            self.pose.x.floor() as i32,
            (self.pose.y - 0.01).floor() as i32,
            self.pose.z.floor() as i32,
        );
        let slipperiness = below.map_or(0.6, |state| profile.slipperiness(state));
        let ground_drag = slipperiness * GROUND_DRAG_SCALE;
        let acceleration = if self.flying {
            speed
        } else if self.in_water || self.in_lava {
            WATER_ACCELERATION
        } else if grounded_at_start {
            // The 0.21600002 scale is divided by the raw block friction,
            // not by the already-scaled post-move drag factor.
            speed * (GROUND_ACCELERATION_SCALE / slipperiness.powi(3))
        } else {
            AIR_ACCELERATION
        };
        let yaw = f64::from(self.pose.yaw).to_radians();
        self.velocity.x += (-yaw.sin() * forward - yaw.cos() * strafe) * acceleration;
        self.velocity.z += (yaw.cos() * forward - yaw.sin() * strafe) * acceleration;
        if self.in_water {
            let current = player_fluid_current(environment.flow, self.velocity);
            self.velocity.x += current.x;
            self.velocity.z += current.z;
        }
        if self.swimming {
            // Player.travel in 26.1.2 steers vertical velocity toward the
            // look-vector Y component before ordinary fluid travel.
            let look_y = -f64::from(self.pose.pitch).to_radians().sin();
            let coefficient = if look_y < -0.2 { 0.085 } else { 0.06 };
            if look_y <= 0.0 || input.jump || environment.water_at_swim_ascent_probe {
                self.velocity.y += (look_y - self.velocity.y) * coefficient;
            }
        }
        let jumped = !self.flying
            && !self.in_water
            && !self.in_lava
            && !self.climbing
            && input.jump
            && grounded_at_start;
        let mut sprint_jump_impulse = 0.0;
        if self.flying {
            let vertical = f64::from(i8::from(input.jump) - i8::from(input.sneak));
            self.velocity.y += vertical * self.flying_speed * FLYING_VERTICAL_INPUT_MULTIPLIER;
        } else if self.in_water || self.in_lava {
            self.velocity.y +=
                f64::from(i8::from(input.jump) - i8::from(input.sneak)) * FLUID_VERTICAL_INPUT;
        } else if environment.scaffolding {
            // Scaffolding is a climbable-tagged volume with its own input
            // semantics: jump ascends immediately, sneak descends, and an
            // idle player retains the ordinary bounded downward slide.
            if input.jump && !input.sneak {
                self.velocity.y = self.velocity.y.max(CLIMB_UP_SPEED);
            } else if input.sneak {
                self.velocity.y = self.velocity.y.min(CLIMB_FALL_LIMIT);
            } else {
                self.velocity.y = self.velocity.y.max(CLIMB_FALL_LIMIT);
            }
            self.velocity.x = self
                .velocity
                .x
                .clamp(-CLIMB_HORIZONTAL_LIMIT, CLIMB_HORIZONTAL_LIMIT);
            self.velocity.z = self
                .velocity
                .z
                .clamp(-CLIMB_HORIZONTAL_LIMIT, CLIMB_HORIZONTAL_LIMIT);
        } else if self.climbing {
            self.velocity.x = self
                .velocity
                .x
                .clamp(-CLIMB_HORIZONTAL_LIMIT, CLIMB_HORIZONTAL_LIMIT);
            self.velocity.z = self
                .velocity
                .z
                .clamp(-CLIMB_HORIZONTAL_LIMIT, CLIMB_HORIZONTAL_LIMIT);
            self.velocity.y = self.velocity.y.max(CLIMB_FALL_LIMIT);
            if environment.scaffolding && input.sneak {
                self.velocity.y = self.velocity.y.min(CLIMB_FALL_LIMIT);
            } else if input.sneak {
                self.velocity.y = 0.0;
            }
        } else if jumped {
            let jump_factor = if environment.support_surface == SpecialSurface::Honey {
                0.5
            } else {
                1.0
            };
            self.velocity.y = self.velocity.y.max(JUMP_VELOCITY * jump_factor);
            if self.sprinting {
                self.velocity.x += -yaw.sin() * SPRINT_JUMP_IMPULSE;
                self.velocity.z += yaw.cos() * SPRINT_JUMP_IMPULSE;
                sprint_jump_impulse = SPRINT_JUMP_IMPULSE;
            }
        }
        let requested = Vec3d::new(
            self.velocity.x,
            self.velocity.y
                - if !self.flying && self.velocity.y <= 0.0 {
                    COLLISION_EPSILON
                } else {
                    0.0
                },
            self.velocity.z,
        );
        let bounds = self.bounding_box();
        let in_scaffolding_context =
            environment.scaffolding || environment.supported_on_scaffolding_top;
        let should_back_off =
            if input.sneak && !self.flying && !in_scaffolding_context && requested.y <= 0.0 {
                grounded_at_start
                    || (self.fall_distance < STEP_HEIGHT
                        && !can_fall_at_least(
                            bounds,
                            0.0,
                            0.0,
                            STEP_HEIGHT - self.fall_distance,
                            chunks,
                            geometry,
                            profile,
                        )?)
            } else {
                false
            };
        let requested_before_edge_backoff = requested;
        let requested = if should_back_off {
            back_off_from_edge(bounds, requested, chunks, geometry, profile)?
        } else {
            requested
        };
        let edge_backed_x =
            (requested_before_edge_backoff.x - requested.x).abs() > COLLISION_EPSILON;
        let edge_backed_z =
            (requested_before_edge_backoff.z - requested.z).abs() > COLLISION_EPSILON;
        let (mut applied, stepped) = collide(
            bounds,
            requested,
            chunks,
            geometry,
            profile,
            grounded_at_start,
        )?;
        if !input.sneak
            && requested.y <= 0.0
            && let Some(support_y) = scaffolding_support_y(
                bounds.translated(applied.x, 0.0, applied.z),
                applied.y,
                chunks,
                geometry,
                profile,
            )
        {
            applied.y = support_y - bounds.min.y;
        }
        self.pose.x += applied.x;
        self.pose.y += applied.y;
        self.pose.z += applied.z;
        self.horizontal_collision = (requested.x - applied.x).abs() > COLLISION_EPSILON
            || (requested.z - applied.z).abs() > COLLISION_EPSILON;
        if self.horizontal_collision && !self.swimming {
            self.sprinting = false;
        }
        let should_probe_fluid_exit = (self.in_water || self.in_lava) && self.horizontal_collision;
        // The stationary-ground probe is exactly COLLISION_EPSILON downward.
        // Treat clipping that full probe as contact; a strict greater-than
        // comparison incorrectly made a stationary player airborne every
        // other tick and selected air acceleration/friction.
        self.on_ground = requested.y < 0.0 && (requested.y - applied.y).abs() >= COLLISION_EPSILON;
        if self.flying {
            self.fall_distance = 0.0;
        } else if applied.y < 0.0 {
            self.fall_distance -= applied.y;
        }
        if self.on_ground {
            self.fall_distance = 0.0;
        }
        if edge_backed_x || (requested.x - applied.x).abs() > COLLISION_EPSILON {
            self.velocity.x = 0.0;
        }
        if edge_backed_z || (requested.z - applied.z).abs() > COLLISION_EPSILON {
            self.velocity.z = 0.0;
        }
        let (climbable_after_move, scaffolding_after_move) =
            touches_climbing_cell(self.bounding_box(), chunks, geometry, profile);
        self.climbing |= climbable_after_move || scaffolding_after_move;
        let climb_assist = self.climbing
            && !environment.scaffolding
            && !scaffolding_after_move
            && (input.jump || (self.horizontal_collision && input.forward));
        let support_after_move = support_surface_at(self.bounding_box(), chunks, geometry, profile);
        let mut slime_bounced = false;
        if (requested.y - applied.y).abs() > COLLISION_EPSILON {
            if requested.y < 0.0 && support_after_move == SpecialSurface::Slime && !input.sneak {
                self.velocity.y = -requested.y;
                slime_bounced = self.velocity.y >= 0.1;
                if self.velocity.y < 0.1 {
                    self.velocity.y = 0.0;
                }
            } else {
                self.velocity.y = 0.0;
            }
        }
        if climb_assist {
            // LivingEntity applies the post-collision climb assist after the
            // generic move response has cleared blocked vertical velocity.
            // Doing this earlier let ordinary ground contact overwrite 0.2,
            // so a player correctly touching a ladder never left the floor.
            self.velocity.y = CLIMB_UP_SPEED;
        }
        let horizontal_drag = if self.in_water {
            if self.sprinting {
                WATER_SPRINT_DRAG
            } else {
                WATER_DRAG
            }
        } else if self.in_lava {
            LAVA_DRAG
        } else if !self.flying && grounded_at_start {
            ground_drag
        } else {
            AIR_DRAG
        };
        self.velocity.x *= horizontal_drag;
        self.velocity.z *= horizontal_drag;
        let flight_changed = if self.flying && self.on_ground {
            self.flying = false;
            self.velocity.y = 0.0;
            Some(false)
        } else if self.flying {
            self.velocity.y *= FLYING_VERTICAL_DRAG;
            None
        } else if self.in_water {
            self.velocity.y *= WATER_DRAG;
            if !self.sprinting {
                self.velocity.y -= WATER_GRAVITY;
            }
            None
        } else if self.in_lava {
            let vertical_drag = if environment.lava_immersion <= 0.4 {
                WATER_DRAG
            } else {
                LAVA_DRAG
            };
            self.velocity.y = self.velocity.y * vertical_drag - WATER_GRAVITY;
            None
        } else if self.climbing {
            // Vanilla clamps climbable motion before the move, then still runs
            // ordinary air gravity/drag afterward. Keeping the pre-move value
            // unchanged made every assisted climb tick travel the full 0.2 m.
            self.velocity.y = ((self.velocity.y - GRAVITY) * VERTICAL_DRAG).max(CLIMB_FALL_LIMIT);
            None
        } else if environment.honey_side_contact && self.velocity.y < HONEY_SLIDE_SPEED {
            self.velocity.y = HONEY_SLIDE_SPEED;
            None
        } else if slime_bounced {
            self.fall_distance = 0.0;
            // SlimeBlock reflects a living entity's impact velocity exactly;
            // the enclosing air-travel step then applies normal gravity and
            // drag, which is the source of vanilla's bounce decay. Entity.move
            // has already established the landing state; bounceUp changes the
            // velocity without clearing that one-tick on-ground value, so
            // held Jump can select jumpFromGround on the following tick.
            self.velocity.y = (self.velocity.y - GRAVITY) * VERTICAL_DRAG;
            None
        } else if self.on_ground {
            self.velocity.y = 0.0;
            None
        } else {
            self.velocity.y = (self.velocity.y - GRAVITY) * VERTICAL_DRAG;
            None
        };
        if should_probe_fluid_exit
            && fluid_exit_clearance(
                self.bounding_box(),
                self.velocity,
                applied.y,
                chunks,
                geometry,
                profile,
            )?
        {
            self.velocity.y = 0.300_000_011_920_928_96;
        }
        if !self.pose.is_finite() || !self.velocity.is_finite() || !self.fall_distance.is_finite() {
            return Err(SimulationError::NonFinite {
                field: "simulation result",
            });
        }
        let had_collision = (requested.x - applied.x).abs() > COLLISION_EPSILON
            || (requested.y - applied.y).abs() > COLLISION_EPSILON
            || (requested.z - applied.z).abs() > COLLISION_EPSILON;
        let diagnostics = had_collision
            .then(|| collision_diagnostics(self.bounding_box(), chunks, geometry, profile));
        Ok(SimulationResult {
            requested,
            applied,
            stepped,
            approximate_collision: diagnostics.is_some_and(|value| {
                value
                    .candidates
                    .iter()
                    .any(|candidate| candidate.approximate)
            }),
            flight_changed,
            jumped,
            grounded_at_start,
            horizontal_acceleration: acceleration,
            horizontal_drag,
            sprint_jump_impulse,
        })
    }

    #[must_use]
    pub fn collision_diagnostics(
        &self,
        chunks: &LoadedChunks,
        geometry: DimensionGeometry,
        profile: &BlockCollisionProfile,
    ) -> CollisionDiagnostics {
        collision_diagnostics(self.bounding_box(), chunks, geometry, profile)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct MovementEnvironment {
    water_immersion: f64,
    water_at_eye: bool,
    water_at_swim_ascent_probe: bool,
    lava_immersion: f64,
    flow: Vec3d,
    climbable: bool,
    /// The player volume overlaps a scaffolding cell, as opposed to merely
    /// being supported by its one-way top face.
    scaffolding: bool,
    supported_on_scaffolding_top: bool,
    support_surface: SpecialSurface,
    honey_side_contact: bool,
}

fn movement_environment(
    bounds: Aabb,
    chunks: &LoadedChunks,
    geometry: DimensionGeometry,
    profile: &BlockCollisionProfile,
) -> MovementEnvironment {
    let mut result = MovementEnvironment::default();
    let mut water_currents = Vec::new();
    let min_x = (bounds.min.x + COLLISION_EPSILON).floor() as i32;
    let max_x = (bounds.max.x - COLLISION_EPSILON).floor() as i32;
    let min_y = (bounds.min.y + COLLISION_EPSILON).floor() as i32;
    let max_y = (bounds.max.y - COLLISION_EPSILON).floor() as i32;
    let min_z = (bounds.min.z + COLLISION_EPSILON).floor() as i32;
    let max_z = (bounds.max.z - COLLISION_EPSILON).floor() as i32;
    for y in min_y..=max_y {
        for z in min_z..=max_z {
            for x in min_x..=max_x {
                let Some(state) = block_state_at(chunks, geometry, x, y, z) else {
                    continue;
                };
                let semantic = profile.environment(state);
                if let Some(fluid) = semantic.fluid {
                    let surface = f64::from(y) + fluid.own_height();
                    if surface > bounds.min.y + COLLISION_EPSILON {
                        let immersion = ((surface - bounds.min.y)
                            / (bounds.max.y - bounds.min.y).max(COLLISION_EPSILON))
                        .clamp(0.0, 1.0);
                        match fluid.kind {
                            FluidKind::Water => {
                                result.water_immersion = result.water_immersion.max(immersion);
                                water_currents.push(fluid_flow(chunks, geometry, profile, x, y, z));
                            }
                            FluidKind::Lava => {
                                result.lava_immersion = result.lava_immersion.max(immersion)
                            }
                        }
                        if fluid.kind == FluidKind::Water
                            && surface > bounds.min.y + STANDING_EYE_HEIGHT
                        {
                            result.water_at_eye = true;
                        }
                    }
                }
                result.scaffolding |= semantic.scaffolding;
            }
        }
    }
    result.flow = averaged_player_fluid_flow(&water_currents, result.water_immersion);
    let center_x = ((bounds.min.x + bounds.max.x) * 0.5).floor() as i32;
    let center_z = ((bounds.min.z + bounds.max.z) * 0.5).floor() as i32;
    let swim_probe_y = (bounds.min.y + 0.9).floor() as i32;
    result.water_at_swim_ascent_probe =
        block_state_at(chunks, geometry, center_x, swim_probe_y, center_z)
            .and_then(|state| profile.environment(state).fluid)
            .is_some_and(|fluid| fluid.kind == FluidKind::Water);
    // Climbing is semantic occupancy/contact with a tagged block cell. It is
    // independent of the thin physical ladder shape used by Cubic's solver.
    let (semantic_climbable, semantic_scaffolding) =
        touches_climbing_cell(bounds, chunks, geometry, profile);
    result.climbable |= semantic_climbable;
    result.scaffolding |= semantic_scaffolding;
    result.supported_on_scaffolding_top =
        scaffolding_support_y(bounds, -COLLISION_EPSILON, chunks, geometry, profile).is_some();
    let below_y = (bounds.min.y - 0.01).floor() as i32;
    if let Some(state) = block_state_at(chunks, geometry, center_x, below_y, center_z) {
        let below = profile.environment(state);
        result.support_surface = below.surface;
    }
    result.support_surface = support_surface_at(bounds, chunks, geometry, profile);
    result.honey_side_contact = honey_side_contact(bounds, chunks, geometry, profile);
    result
}

fn touches_climbing_cell(
    bounds: Aabb,
    chunks: &LoadedChunks,
    geometry: DimensionGeometry,
    profile: &BlockCollisionProfile,
) -> (bool, bool) {
    // LivingEntity.onClimbable reads the tagged block at the entity's own
    // BlockPos (horizontal centre and feet Y). Using every cell touched by the
    // 0.6-wide AABB made lateral contact with the edge of a ladder cell count
    // as climbing even while the player's centre remained in the adjacent
    // cell. Ladder facing still determines where its thin physical plane is;
    // this semantic query deliberately follows the entity position instead.
    let center_x = ((bounds.min.x + bounds.max.x) * 0.5).floor() as i32;
    let feet_y = bounds.min.y.floor() as i32;
    let center_z = ((bounds.min.z + bounds.max.z) * 0.5).floor() as i32;
    let climbable = block_state_at(chunks, geometry, center_x, feet_y, center_z)
        .is_some_and(|state| profile.environment(state).climbable);

    // Scaffolding's already-accepted volume semantics remain AABB-based and
    // independent from the ladder-centre correction above.
    let min_x = (bounds.min.x - COLLISION_EPSILON).floor() as i32;
    let max_x = (bounds.max.x + COLLISION_EPSILON).floor() as i32;
    let min_y = bounds.min.y.floor() as i32;
    let max_y = (bounds.max.y - COLLISION_EPSILON).floor() as i32;
    let min_z = (bounds.min.z - COLLISION_EPSILON).floor() as i32;
    let max_z = (bounds.max.z + COLLISION_EPSILON).floor() as i32;
    let mut scaffolding = false;
    for y in min_y..=max_y {
        for z in min_z..=max_z {
            for x in min_x..=max_x {
                let Some(state) = block_state_at(chunks, geometry, x, y, z) else {
                    continue;
                };
                let semantic = profile.environment(state);
                if !semantic.scaffolding {
                    continue;
                }
                let cell = Aabb::new(
                    Vec3d::new(f64::from(x), f64::from(y), f64::from(z)),
                    Vec3d::new(f64::from(x) + 1.0, f64::from(y) + 1.0, f64::from(z) + 1.0),
                );
                if aabbs_overlap(bounds, cell) {
                    scaffolding |= semantic.scaffolding;
                }
            }
        }
    }
    (climbable, scaffolding)
}

fn scaffolding_support_y(
    horizontally_moved_bounds: Aabb,
    vertical_movement: f64,
    chunks: &LoadedChunks,
    geometry: DimensionGeometry,
    profile: &BlockCollisionProfile,
) -> Option<f64> {
    let final_feet = horizontally_moved_bounds.min.y + vertical_movement;
    let min_x = (horizontally_moved_bounds.min.x + COLLISION_EPSILON).floor() as i32;
    let max_x = (horizontally_moved_bounds.max.x - COLLISION_EPSILON).floor() as i32;
    let min_z = (horizontally_moved_bounds.min.z + COLLISION_EPSILON).floor() as i32;
    let max_z = (horizontally_moved_bounds.max.z - COLLISION_EPSILON).floor() as i32;
    let min_y = final_feet.floor() as i32 - 1;
    let max_y = horizontally_moved_bounds.min.y.floor() as i32;
    let mut support: Option<f64> = None;
    for y in min_y..=max_y {
        let top = f64::from(y + 1);
        if horizontally_moved_bounds.min.y < top - COLLISION_EPSILON
            || final_feet > top - COLLISION_EPSILON
        {
            continue;
        }
        for z in min_z..=max_z {
            for x in min_x..=max_x {
                let is_scaffolding = block_state_at(chunks, geometry, x, y, z)
                    .is_some_and(|state| profile.environment(state).scaffolding);
                if is_scaffolding {
                    support = Some(support.map_or(top, |current| current.max(top)));
                }
            }
        }
    }
    support
}

fn aabbs_overlap(left: Aabb, right: Aabb) -> bool {
    overlaps(left.min.x, left.max.x, right.min.x, right.max.x)
        && overlaps(left.min.y, left.max.y, right.min.y, right.max.y)
        && overlaps(left.min.z, left.max.z, right.min.z, right.max.z)
}

fn averaged_player_fluid_flow(currents: &[Vec3d], immersion: f64) -> Vec3d {
    if currents.is_empty() {
        return Vec3d::default();
    }
    let mut total = currents
        .iter()
        .copied()
        .fold(Vec3d::default(), |sum, value| {
            Vec3d::new(sum.x + value.x, sum.y + value.y, sum.z + value.z)
        });
    // EntityFluidInteraction ignores a cancelling/near-zero aggregate before
    // applying its player-specific average. This avoids normalizing floating
    // residue from opposing currents into a full-strength direction.
    if total.x.mul_add(total.x, total.z * total.z) < 1.0e-5 {
        return Vec3d::default();
    }
    let scale = immersion.min(0.4) / 0.4;
    total.x = total.x / currents.len() as f64 * scale;
    total.z = total.z / currents.len() as f64 * scale;
    total
}

fn player_fluid_current(flow: Vec3d, velocity: Vec3d) -> Vec3d {
    let mut current = Vec3d::new(flow.x * 0.014, 0.0, flow.z * 0.014);
    let length = current.x.hypot(current.z);
    if velocity.x.abs() < 0.003
        && velocity.z.abs() < 0.003
        && length > f64::EPSILON
        && length < 0.0045
    {
        current.x = current.x / length * 0.0045;
        current.z = current.z / length * 0.0045;
    }
    current
}

fn support_surface_at(
    bounds: Aabb,
    chunks: &LoadedChunks,
    geometry: DimensionGeometry,
    profile: &BlockCollisionProfile,
) -> SpecialSurface {
    let y = (bounds.min.y - 0.01).floor() as i32;
    let min_x = (bounds.min.x + COLLISION_EPSILON).floor() as i32;
    let max_x = (bounds.max.x - COLLISION_EPSILON).floor() as i32;
    let min_z = (bounds.min.z + COLLISION_EPSILON).floor() as i32;
    let max_z = (bounds.max.z - COLLISION_EPSILON).floor() as i32;
    let mut surface = SpecialSurface::Ordinary;
    for z in min_z..=max_z {
        for x in min_x..=max_x {
            let Some(state) = block_state_at(chunks, geometry, x, y, z) else {
                continue;
            };
            let candidate = profile.environment(state).surface;
            if candidate == SpecialSurface::Honey {
                return candidate;
            }
            if candidate != SpecialSurface::Ordinary {
                surface = candidate;
            }
        }
    }
    surface
}

fn honey_side_contact(
    bounds: Aabb,
    chunks: &LoadedChunks,
    geometry: DimensionGeometry,
    profile: &BlockCollisionProfile,
) -> bool {
    let probe = Aabb::new(
        Vec3d::new(
            bounds.min.x - 1.0 / 16.0,
            bounds.min.y,
            bounds.min.z - 1.0 / 16.0,
        ),
        Vec3d::new(
            bounds.max.x + 1.0 / 16.0,
            bounds.max.y,
            bounds.max.z + 1.0 / 16.0,
        ),
    );
    let center_x = (bounds.min.x + bounds.max.x) * 0.5;
    let center_z = (bounds.min.z + bounds.max.z) * 0.5;
    let half_width = (bounds.max.x - bounds.min.x) * 0.5;
    environment_cells(probe).any(|(x, y, z)| {
        let is_honey = block_state_at(chunks, geometry, x, y, z)
            .is_some_and(|state| profile.environment(state).surface == SpecialSurface::Honey);
        is_honey
            && bounds.min.y <= f64::from(y) + 15.0 / 16.0 - COLLISION_EPSILON
            && ((center_x - (f64::from(x) + 0.5)).abs() + COLLISION_EPSILON
                > 7.0 / 16.0 + half_width
                || (center_z - (f64::from(z) + 0.5)).abs() + COLLISION_EPSILON
                    > 7.0 / 16.0 + half_width)
    })
}

fn environment_cells(bounds: Aabb) -> impl Iterator<Item = (i32, i32, i32)> {
    let min_x = bounds.min.x.floor() as i32;
    let max_x = bounds.max.x.floor() as i32;
    let min_y = bounds.min.y.floor() as i32;
    let max_y = bounds.max.y.floor() as i32;
    let min_z = bounds.min.z.floor() as i32;
    let max_z = bounds.max.z.floor() as i32;
    (min_y..=max_y).flat_map(move |y| {
        (min_z..=max_z).flat_map(move |z| (min_x..=max_x).map(move |x| (x, y, z)))
    })
}

fn fluid_flow(
    chunks: &LoadedChunks,
    geometry: DimensionGeometry,
    profile: &BlockCollisionProfile,
    x: i32,
    y: i32,
    z: i32,
) -> Vec3d {
    let Some(center) = fluid_semantic_at(chunks, geometry, profile, x, y, z) else {
        return Vec3d::default();
    };
    let center_height = center.own_height();
    let mut flow = Vec3d::default();
    for (dx, dz) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
        let neighbor = fluid_semantic_at(chunks, geometry, profile, x + dx, y, z + dz);
        let difference = if let Some(neighbor) = neighbor.filter(|fluid| fluid.kind == center.kind)
        {
            center_height - neighbor.own_height()
        } else {
            let neighbor_state = block_state_at(chunks, geometry, x + dx, y, z + dz);
            let open = neighbor_state
                .is_some_and(|state| matches!(profile.shape(state), CollisionShape::Empty));
            if !open {
                continue;
            }
            let Some(below) = fluid_semantic_at(chunks, geometry, profile, x + dx, y - 1, z + dz)
                .filter(|fluid| fluid.kind == center.kind)
            else {
                continue;
            };
            center_height - (below.own_height() - 8.0 / 9.0)
        };
        flow.x += f64::from(dx) * difference;
        flow.z += f64::from(dz) * difference;
    }
    let length = flow.x.hypot(flow.z);
    if length > COLLISION_EPSILON {
        flow.x /= length;
        flow.z /= length;
    }
    flow
}

fn fluid_exit_clearance(
    bounds: Aabb,
    adjusted_velocity: Vec3d,
    applied_y: f64,
    chunks: &LoadedChunks,
    geometry: DimensionGeometry,
    profile: &BlockCollisionProfile,
) -> Result<bool, SimulationError> {
    // LivingEntity.jumpOutOfFluid runs after water drag/gravity and checks the
    // current AABB at adjusted velocity Y + 0.6 - the Y already applied by
    // move(). This preserves actual partial-shape clearance (slabs/stairs)
    // instead of probing with the pre-drag requested displacement.
    let probe = Vec3d::new(
        adjusted_velocity.x,
        adjusted_velocity.y + 0.600_000_023_841_857_9 - applied_y,
        adjusted_velocity.z,
    );
    let (applied, _) = collide(bounds, probe, chunks, geometry, profile, false)?;
    let translated = bounds.translated(probe.x, probe.y, probe.z);
    Ok(!contains_any_fluid(translated, chunks, geometry, profile)
        && (applied.x - probe.x).abs() <= COLLISION_EPSILON
        && (applied.y - probe.y).abs() <= COLLISION_EPSILON
        && (applied.z - probe.z).abs() <= COLLISION_EPSILON)
}

fn contains_any_fluid(
    bounds: Aabb,
    chunks: &LoadedChunks,
    geometry: DimensionGeometry,
    profile: &BlockCollisionProfile,
) -> bool {
    // LevelReader.containsAnyLiquid scans every block cell touched by the
    // translated AABB and rejects any non-empty FluidState in that cell. It
    // deliberately does not clip the test to the rendered fluid surface.
    let min_x = bounds.min.x.floor() as i32;
    let max_x = (bounds.max.x.ceil() as i32).saturating_sub(1);
    let min_y = bounds.min.y.floor() as i32;
    let max_y = (bounds.max.y.ceil() as i32).saturating_sub(1);
    let min_z = bounds.min.z.floor() as i32;
    let max_z = (bounds.max.z.ceil() as i32).saturating_sub(1);
    for y in min_y..=max_y {
        for z in min_z..=max_z {
            for x in min_x..=max_x {
                let has_fluid = block_state_at(chunks, geometry, x, y, z)
                    .and_then(|state| profile.environment(state).fluid)
                    .is_some();
                if has_fluid {
                    return true;
                }
            }
        }
    }
    false
}

fn fluid_semantic_at(
    chunks: &LoadedChunks,
    geometry: DimensionGeometry,
    profile: &BlockCollisionProfile,
    x: i32,
    y: i32,
    z: i32,
) -> Option<crate::FluidState> {
    block_state_at(chunks, geometry, x, y, z).and_then(|state| profile.environment(state).fluid)
}

const MAX_DIAGNOSTIC_CANDIDATES: usize = 24;

fn collision_diagnostics(
    bounds: Aabb,
    chunks: &LoadedChunks,
    geometry: DimensionGeometry,
    profile: &BlockCollisionProfile,
) -> CollisionDiagnostics {
    let min_x = (bounds.min.x - COLLISION_EPSILON).floor() as i32;
    let max_x = (bounds.max.x + COLLISION_EPSILON).floor() as i32;
    let min_y = (bounds.min.y - COLLISION_EPSILON).floor() as i32;
    let max_y = (bounds.max.y + COLLISION_EPSILON).floor() as i32;
    let min_z = (bounds.min.z - COLLISION_EPSILON).floor() as i32;
    let max_z = (bounds.max.z + COLLISION_EPSILON).floor() as i32;
    let mut candidates = Vec::new();
    let mut truncated = false;
    'cells: for y in min_y..=max_y {
        for z in min_z..=max_z {
            for x in min_x..=max_x {
                let state = block_state_at(chunks, geometry, x, y, z);
                let (shape, approximate) = if y < geometry.min_y {
                    (&CollisionShape::FullCube, true)
                } else if y >= geometry.min_y + i32::try_from(geometry.height).unwrap_or(i32::MAX) {
                    (&CollisionShape::Empty, false)
                } else if let Some(state) = state {
                    (profile.shape(state), profile.is_approximate(state))
                } else {
                    (&CollisionShape::FullCube, true)
                };
                let kind = match shape {
                    CollisionShape::Empty => continue,
                    CollisionShape::FullCube => CollisionShapeKind::FullCube,
                    CollisionShape::Boxes(_) => CollisionShapeKind::Boxes,
                };
                if candidates.len() == MAX_DIAGNOSTIC_CANDIDATES {
                    truncated = true;
                    break 'cells;
                }
                candidates.push(CollisionCandidate {
                    block_x: x,
                    block_y: y,
                    block_z: z,
                    state,
                    shape: kind,
                    approximate,
                });
            }
        }
    }
    CollisionDiagnostics {
        player_bounds: bounds,
        candidates,
        truncated,
    }
}

fn block_state_at(
    chunks: &LoadedChunks,
    geometry: DimensionGeometry,
    x: i32,
    y: i32,
    z: i32,
) -> Option<RuntimeBlockStateId> {
    let section = y.div_euclid(16) - geometry.min_section_y();
    let section = usize::try_from(section).ok()?;
    let chunk = chunks.get(crate::ChunkCoordinate::new(
        x.div_euclid(16),
        z.div_euclid(16),
    ))?;
    chunk.sections.get(section)?.block(
        u8::try_from(x.rem_euclid(16)).ok()?,
        u8::try_from(y.rem_euclid(16)).ok()?,
        u8::try_from(z.rem_euclid(16)).ok()?,
    )
}

fn collide(
    bounds: Aabb,
    requested: Vec3d,
    chunks: &LoadedChunks,
    geometry: DimensionGeometry,
    profile: &BlockCollisionProfile,
    was_on_ground: bool,
) -> Result<(Vec3d, bool), SimulationError> {
    let mut broadphase = bounds.expanded_toward(requested);
    broadphase.max.y += STEP_HEIGHT;
    let obstacles = collect_obstacles(broadphase, chunks, geometry, profile)?;
    let direct = resolve_axes(bounds, requested, &obstacles);
    let horizontal_blocked = direct.x != requested.x || direct.z != requested.z;
    if !horizontal_blocked || !(was_on_ground || (requested.y < 0.0 && direct.y != requested.y)) {
        return Ok((direct, false));
    }
    let up = resolve_y(bounds, STEP_HEIGHT, &obstacles);
    if up <= COLLISION_EPSILON {
        return Ok((direct, false));
    }
    let raised = bounds.moved(Vec3d::new(0.0, up, 0.0));
    let horizontal = resolve_horizontal(raised, requested.x, requested.z, &obstacles);
    let after_horizontal = raised.moved(Vec3d::new(horizontal.0, 0.0, horizontal.1));
    let down = resolve_y(after_horizontal, requested.y - up, &obstacles);
    let stepped = Vec3d::new(horizontal.0, up + down, horizontal.1);
    if stepped.x.mul_add(stepped.x, stepped.z * stepped.z)
        > direct.x.mul_add(direct.x, direct.z * direct.z)
    {
        Ok((stepped, true))
    } else {
        Ok((direct, false))
    }
}

fn resolve_axes(bounds: Aabb, requested: Vec3d, obstacles: &[Aabb]) -> Vec3d {
    let y = resolve_y(bounds, requested.y, obstacles);
    let bounds = bounds.moved(Vec3d::new(0.0, y, 0.0));
    let (x, z) = resolve_horizontal(bounds, requested.x, requested.z, obstacles);
    Vec3d::new(x, y, z)
}

fn resolve_horizontal(mut bounds: Aabb, mut x: f64, mut z: f64, obstacles: &[Aabb]) -> (f64, f64) {
    if x.abs() >= z.abs() {
        x = resolve_x(bounds, x, obstacles);
        bounds = bounds.moved(Vec3d::new(x, 0.0, 0.0));
        z = resolve_z(bounds, z, obstacles);
    } else {
        z = resolve_z(bounds, z, obstacles);
        bounds = bounds.moved(Vec3d::new(0.0, 0.0, z));
        x = resolve_x(bounds, x, obstacles);
    }
    (x, z)
}

fn resolve_x(bounds: Aabb, mut delta: f64, obstacles: &[Aabb]) -> f64 {
    for obstacle in obstacles {
        if !bounds.overlaps_yz(*obstacle) {
            continue;
        }
        if delta > 0.0
            && bounds.min.x < obstacle.min.x
            && bounds.max.x <= obstacle.min.x + COLLISION_EPSILON
        {
            delta = delta.min(obstacle.min.x - bounds.max.x);
        } else if delta < 0.0
            && bounds.max.x > obstacle.max.x
            && bounds.min.x >= obstacle.max.x - COLLISION_EPSILON
        {
            delta = delta.max(obstacle.max.x - bounds.min.x);
        }
    }
    delta
}

fn resolve_y(bounds: Aabb, mut delta: f64, obstacles: &[Aabb]) -> f64 {
    for obstacle in obstacles {
        if !bounds.overlaps_xz(*obstacle) {
            continue;
        }
        if delta > 0.0
            && bounds.min.y < obstacle.min.y
            && bounds.max.y <= obstacle.min.y + COLLISION_EPSILON
        {
            delta = delta.min(obstacle.min.y - bounds.max.y);
        } else if delta < 0.0
            && bounds.max.y > obstacle.max.y
            && bounds.min.y >= obstacle.max.y - COLLISION_EPSILON
        {
            delta = delta.max(obstacle.max.y - bounds.min.y);
        }
    }
    delta
}

fn resolve_z(bounds: Aabb, mut delta: f64, obstacles: &[Aabb]) -> f64 {
    for obstacle in obstacles {
        if !bounds.overlaps_xy(*obstacle) {
            continue;
        }
        if delta > 0.0
            && bounds.min.z < obstacle.min.z
            && bounds.max.z <= obstacle.min.z + COLLISION_EPSILON
        {
            delta = delta.min(obstacle.min.z - bounds.max.z);
        } else if delta < 0.0
            && bounds.max.z > obstacle.max.z
            && bounds.min.z >= obstacle.max.z - COLLISION_EPSILON
        {
            delta = delta.max(obstacle.max.z - bounds.min.z);
        }
    }
    delta
}

fn collect_obstacles(
    broadphase: Aabb,
    chunks: &LoadedChunks,
    geometry: DimensionGeometry,
    profile: &BlockCollisionProfile,
) -> Result<Vec<Aabb>, SimulationError> {
    let (min_x, max_x) = source_cell_range(
        broadphase.min.x,
        broadphase.max.x,
        profile.source_shape_bounds.min.x,
        profile.source_shape_bounds.max.x,
    );
    let (min_y, max_y) = source_cell_range(
        broadphase.min.y,
        broadphase.max.y,
        profile.source_shape_bounds.min.y,
        profile.source_shape_bounds.max.y,
    );
    let (min_z, max_z) = source_cell_range(
        broadphase.min.z,
        broadphase.max.z,
        profile.source_shape_bounds.min.z,
        profile.source_shape_bounds.max.z,
    );
    let cells = i64::from(max_x - min_x + 1)
        .saturating_mul(i64::from(max_y - min_y + 1))
        .saturating_mul(i64::from(max_z - min_z + 1));
    if cells > 4_096 {
        return Err(SimulationError::BroadphaseTooLarge);
    }
    let mut obstacles = Vec::new();
    for y in min_y..=max_y {
        for z in min_z..=max_z {
            for x in min_x..=max_x {
                let (shape, offset) = if y < geometry.min_y {
                    (&CollisionShape::FullCube, Vec3d::default())
                } else if y >= geometry.min_y + i32::try_from(geometry.height).unwrap_or(i32::MAX) {
                    (&CollisionShape::Empty, Vec3d::default())
                } else if let Some(state) = block_state_at(chunks, geometry, x, y, z) {
                    (profile.shape(state), profile.offset(state, x, z))
                } else {
                    // Missing chunks are conservative solid boundaries rather than accidental noclip.
                    (&CollisionShape::FullCube, Vec3d::default())
                };
                match shape {
                    CollisionShape::Empty => {}
                    CollisionShape::FullCube => obstacles.push(Aabb::new(
                        Vec3d::new(f64::from(x), f64::from(y), f64::from(z)),
                        Vec3d::new(f64::from(x) + 1.0, f64::from(y) + 1.0, f64::from(z) + 1.0),
                    )),
                    CollisionShape::Boxes(boxes) => {
                        obstacles.extend(boxes.iter().take(MAX_COLLISION_BOXES_PER_STATE).map(
                            |bounds| {
                                bounds.translated(
                                    f64::from(x) + offset.x,
                                    f64::from(y) + offset.y,
                                    f64::from(z) + offset.z,
                                )
                            },
                        ))
                    }
                }
            }
        }
    }
    Ok(obstacles)
}

fn source_cell_range(
    broadphase_min: f64,
    broadphase_max: f64,
    local_min: f64,
    local_max: f64,
) -> (i32, i32) {
    // The normal source cell contributes [0, 1]. Shapes may legitimately
    // extend beyond it (fences reach 1.5 blocks high), so include owning cells
    // displaced by the profile's bounded local envelope. This changes only
    // which source states are inspected; their exact AABBs remain unclamped.
    let negative_extension = (-local_min).max(0.0);
    let positive_extension = (local_max - 1.0).max(0.0);
    (
        (broadphase_min - positive_extension - COLLISION_EPSILON).floor() as i32,
        (broadphase_max + negative_extension + COLLISION_EPSILON).floor() as i32,
    )
}

/// Reproduces the bounded 26.1.2 `Player.maybeBackOffFromEdge` movement
/// adjustment. Each horizontal component is reduced toward zero in 0.05-block
/// increments until the shrunken player footprint has support within the
/// ordinary 0.6-block step distance. This changes the requested displacement;
/// it does not clamp the player's coordinates to a block grid.
fn back_off_from_edge(
    bounds: Aabb,
    requested: Vec3d,
    chunks: &LoadedChunks,
    geometry: DimensionGeometry,
    profile: &BlockCollisionProfile,
) -> Result<Vec3d, SimulationError> {
    const EDGE_INCREMENT: f64 = 0.05;
    let mut x = requested.x;
    let mut z = requested.z;
    let x_increment = x.signum() * EDGE_INCREMENT;
    let z_increment = z.signum() * EDGE_INCREMENT;
    while x != 0.0 && can_fall_at_least(bounds, x, 0.0, STEP_HEIGHT, chunks, geometry, profile)? {
        x = reduce_toward_zero(x, x_increment, EDGE_INCREMENT);
    }
    while z != 0.0 && can_fall_at_least(bounds, 0.0, z, STEP_HEIGHT, chunks, geometry, profile)? {
        z = reduce_toward_zero(z, z_increment, EDGE_INCREMENT);
    }
    while x != 0.0
        && z != 0.0
        && can_fall_at_least(bounds, x, z, STEP_HEIGHT, chunks, geometry, profile)?
    {
        x = reduce_toward_zero(x, x_increment, EDGE_INCREMENT);
        z = reduce_toward_zero(z, z_increment, EDGE_INCREMENT);
    }
    Ok(Vec3d::new(x, requested.y, z))
}

fn reduce_toward_zero(value: f64, increment: f64, threshold: f64) -> f64 {
    if value.abs() <= threshold {
        0.0
    } else {
        value - increment
    }
}

fn can_fall_at_least(
    bounds: Aabb,
    x: f64,
    z: f64,
    distance: f64,
    chunks: &LoadedChunks,
    geometry: DimensionGeometry,
    profile: &BlockCollisionProfile,
) -> Result<bool, SimulationError> {
    let probe = Aabb::new(
        Vec3d::new(
            bounds.min.x + COLLISION_EPSILON + x,
            bounds.min.y - distance - COLLISION_EPSILON,
            bounds.min.z + COLLISION_EPSILON + z,
        ),
        Vec3d::new(
            bounds.max.x - COLLISION_EPSILON + x,
            bounds.min.y,
            bounds.max.z - COLLISION_EPSILON + z,
        ),
    );
    let obstacles = collect_obstacles(probe, chunks, geometry, profile)?;
    Ok(!obstacles.iter().any(|obstacle| {
        strict_overlaps(probe.min.x, probe.max.x, obstacle.min.x, obstacle.max.x)
            && strict_overlaps(probe.min.y, probe.max.y, obstacle.min.y, obstacle.max.y)
            && strict_overlaps(probe.min.z, probe.max.z, obstacle.min.z, obstacle.max.z)
    }))
}

fn strict_overlaps(min: f64, max: f64, other_min: f64, other_max: f64) -> bool {
    max > other_min && min < other_max
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Chunk, ChunkCoordinate, ChunkLightSummary, ChunkSection, PalettedContainer, RuntimeBiomeId,
        collision_vanilla::{classify_shape, has_verified_shape},
    };

    fn geometry() -> DimensionGeometry {
        DimensionGeometry {
            min_y: 0,
            height: 32,
        }
    }

    fn boxes(values: &[Aabb]) -> CollisionShape {
        CollisionShape::Boxes(Arc::from(values))
    }
    fn section(value: RuntimeBlockStateId) -> ChunkSection {
        ChunkSection {
            non_empty_block_count: if value.0 == 0 { 0 } else { 4096 },
            fluid_count: 0,
            blocks: PalettedContainer::Single {
                value,
                entries: 4096,
            },
            biomes: PalettedContainer::Single {
                value: RuntimeBiomeId(0),
                entries: 64,
            },
        }
    }
    fn flat_world() -> (LoadedChunks, BlockCollisionProfile) {
        let mut chunks = LoadedChunks::default();
        let mut blocks = vec![RuntimeBlockStateId(0); 4096];
        for z in 0..16 {
            for x in 0..16 {
                blocks[z * 16 + x] = RuntimeBlockStateId(1);
            }
        }
        chunks
            .insert(Chunk {
                coordinate: ChunkCoordinate::new(0, 0),
                sections: vec![
                    ChunkSection {
                        non_empty_block_count: 256,
                        fluid_count: 0,
                        blocks: PalettedContainer::Direct { values: blocks },
                        biomes: PalettedContainer::Single {
                            value: RuntimeBiomeId(0),
                            entries: 64,
                        },
                    },
                    section(RuntimeBlockStateId(0)),
                ],
                heightmaps: vec![],
                block_entities: vec![],
                light: ChunkLightSummary::default(),
            })
            .unwrap();
        (
            chunks,
            BlockCollisionProfile::synthetic([
                (RuntimeBlockStateId(0), CollisionShape::Empty),
                (RuntimeBlockStateId(1), CollisionShape::FullCube),
            ]),
        )
    }

    fn special_world(
        floor: BlockEnvironment,
        occupant: Option<BlockEnvironment>,
    ) -> (LoadedChunks, BlockCollisionProfile) {
        let mut values = vec![RuntimeBlockStateId(0); 4096];
        for z in 0..16 {
            for x in 0..16 {
                values[z * 16 + x] = RuntimeBlockStateId(1);
            }
        }
        if occupant.is_some() {
            values[256 + 8 * 16 + 8] = RuntimeBlockStateId(2);
        }
        let mut chunks = LoadedChunks::default();
        chunks
            .insert(Chunk {
                coordinate: ChunkCoordinate::new(0, 0),
                sections: vec![
                    ChunkSection {
                        non_empty_block_count: 257,
                        fluid_count: u16::from(occupant.is_some()),
                        blocks: PalettedContainer::Direct { values },
                        biomes: PalettedContainer::Single {
                            value: RuntimeBiomeId(0),
                            entries: 64,
                        },
                    },
                    section(RuntimeBlockStateId(0)),
                ],
                heightmaps: vec![],
                block_entities: vec![],
                light: ChunkLightSummary::default(),
            })
            .unwrap();
        let mut profile = BlockCollisionProfile::synthetic([
            (RuntimeBlockStateId(0), CollisionShape::Empty),
            (RuntimeBlockStateId(1), CollisionShape::FullCube),
            (RuntimeBlockStateId(2), CollisionShape::Empty),
        ]);
        let mut environments = vec![(RuntimeBlockStateId(1), floor)];
        if let Some(occupant) = occupant {
            environments.push((RuntimeBlockStateId(2), occupant));
        }
        profile.set_environment(BlockEnvironmentProfile::synthetic(environments));
        (chunks, profile)
    }

    fn obstacle_world(
        obstacle: RuntimeBlockStateId,
        ceiling: bool,
    ) -> (LoadedChunks, BlockCollisionProfile) {
        let mut values = vec![RuntimeBlockStateId(0); 4096];
        for z in 0..16 {
            for x in 0..16 {
                values[z * 16 + x] = RuntimeBlockStateId(1);
            }
        }
        values[256 + 8 * 16 + 9] = obstacle;
        if ceiling {
            values[2 * 256 + 8 * 16 + 9] = RuntimeBlockStateId(1);
        }
        let mut chunks = LoadedChunks::default();
        chunks
            .insert(Chunk {
                coordinate: ChunkCoordinate::new(0, 0),
                sections: vec![
                    ChunkSection {
                        non_empty_block_count: 258,
                        fluid_count: 0,
                        blocks: PalettedContainer::Direct { values },
                        biomes: PalettedContainer::Single {
                            value: RuntimeBiomeId(0),
                            entries: 64,
                        },
                    },
                    section(RuntimeBlockStateId(0)),
                ],
                heightmaps: vec![],
                block_entities: vec![],
                light: ChunkLightSummary::default(),
            })
            .unwrap();
        let profile = BlockCollisionProfile::synthetic([
            (RuntimeBlockStateId(0), CollisionShape::Empty),
            (RuntimeBlockStateId(1), CollisionShape::FullCube),
            (
                RuntimeBlockStateId(2),
                boxes(&[Aabb::new(
                    Vec3d::new(0.0, 0.0, 0.0),
                    Vec3d::new(1.0, 0.5, 1.0),
                )]),
            ),
            (
                RuntimeBlockStateId(3),
                boxes(&[Aabb::new(
                    Vec3d::new(0.0, 0.0, 0.0),
                    Vec3d::new(1.0, 0.75, 1.0),
                )]),
            ),
        ]);
        (chunks, profile)
    }

    fn single_shape_world(
        x: i32,
        y: u8,
        z: i32,
        state: RuntimeBlockStateId,
        shape: CollisionShape,
    ) -> (LoadedChunks, BlockCollisionProfile) {
        let chunk_x = x.div_euclid(16);
        let chunk_z = z.div_euclid(16);
        let local_x = usize::try_from(x.rem_euclid(16)).unwrap();
        let local_z = usize::try_from(z.rem_euclid(16)).unwrap();
        let mut values = vec![RuntimeBlockStateId(0); 4096];
        values[usize::from(y) * 256 + local_z * 16 + local_x] = state;
        let mut chunks = LoadedChunks::default();
        chunks
            .insert(Chunk {
                coordinate: ChunkCoordinate::new(chunk_x, chunk_z),
                sections: vec![
                    ChunkSection {
                        non_empty_block_count: 1,
                        fluid_count: 0,
                        blocks: PalettedContainer::Direct { values },
                        biomes: PalettedContainer::Single {
                            value: RuntimeBiomeId(0),
                            entries: 64,
                        },
                    },
                    section(RuntimeBlockStateId(0)),
                ],
                heightmaps: vec![],
                block_entities: vec![],
                light: ChunkLightSummary::default(),
            })
            .unwrap();
        (
            chunks,
            BlockCollisionProfile::synthetic([
                (RuntimeBlockStateId(0), CollisionShape::Empty),
                (state, shape),
            ]),
        )
    }

    fn fluid_ledge_world(shape: CollisionShape) -> (LoadedChunks, BlockCollisionProfile) {
        let water = BlockEnvironment {
            fluid: Some(crate::FluidState {
                kind: FluidKind::Water,
                level: 0,
                falling: false,
            }),
            ..BlockEnvironment::default()
        };
        let (mut chunks, mut profile) = special_world(BlockEnvironment::default(), Some(water));
        let ledge = RuntimeBlockStateId(3);
        assert!(chunks.update_block(ChunkCoordinate::new(0, 0), 0, 8, 1, 9, ledge,));
        profile.states.insert(ledge, shape);
        profile.offsets.insert(ledge, CollisionOffset::None);
        profile.slipperiness.insert(ledge, 0.6);
        profile.source_shape_bounds = collision_shape_envelope(profile.states.values(), 0.0);
        (chunks, profile)
    }

    fn upright_water_player_before_ledge() -> PlayerMovementState {
        let mut state = PlayerMovementState::from_authoritative(
            LocalPlayerPose::new(8.5, 1.0, 8.69, 0.0, 0.0),
            Vec3d::default(),
        )
        .unwrap();
        state.on_ground = true;
        state
    }

    fn partial_height_edge_world(shape: CollisionShape) -> (LoadedChunks, BlockCollisionProfile) {
        let mut values = vec![RuntimeBlockStateId(0); 4096];
        for z in 7..=8 {
            values[256 + z * 16 + 7] = RuntimeBlockStateId(1);
            values[256 + z * 16 + 8] = RuntimeBlockStateId(2);
        }
        let mut chunks = LoadedChunks::default();
        chunks
            .insert(Chunk {
                coordinate: ChunkCoordinate::new(0, 0),
                sections: vec![
                    ChunkSection {
                        non_empty_block_count: 4,
                        fluid_count: 0,
                        blocks: PalettedContainer::Direct { values },
                        biomes: PalettedContainer::Single {
                            value: RuntimeBiomeId(0),
                            entries: 64,
                        },
                    },
                    section(RuntimeBlockStateId(0)),
                ],
                heightmaps: vec![],
                block_entities: vec![],
                light: ChunkLightSummary::default(),
            })
            .unwrap();
        (
            chunks,
            BlockCollisionProfile::synthetic([
                (RuntimeBlockStateId(0), CollisionShape::Empty),
                (RuntimeBlockStateId(1), CollisionShape::FullCube),
                (RuntimeBlockStateId(2), shape),
            ]),
        )
    }

    fn descending_edge_player(
        x: f64,
        y: f64,
        z: f64,
        fall_distance: f64,
        velocity: Vec3d,
    ) -> PlayerMovementState {
        let mut state = PlayerMovementState::from_authoritative(
            LocalPlayerPose::new(x, y, z, 0.0, 0.0),
            velocity,
        )
        .unwrap();
        state.on_ground = false;
        state.fall_distance = fall_distance;
        state
    }

    fn player() -> PlayerMovementState {
        let mut player = PlayerMovementState::from_authoritative(
            LocalPlayerPose::new(8.0, 1.0, 8.0, 0.0, 0.0),
            Vec3d::default(),
        )
        .unwrap();
        player.on_ground = true;
        player
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1.0e-9,
            "expected {expected:.12}, got {actual:.12}"
        );
    }

    fn add_low_ceiling(chunks: &mut LoadedChunks) {
        for z in 0..16 {
            for x in 0..16 {
                assert!(chunks.update_block(
                    ChunkCoordinate::new(0, 0),
                    0,
                    x,
                    3,
                    z,
                    RuntimeBlockStateId(1),
                ));
            }
        }
    }

    #[test]
    fn vanilla_ground_acceleration_and_release_curve_use_raw_slipperiness() {
        let (chunks, profile) = flat_world();
        let mut state = player();
        let expected_moves = [
            0.100000009259259,
            0.154600014314815,
            0.184411617075148,
            0.200688752182290,
            0.209576067950790,
        ];
        let expected_post_drag = [
            0.054600005055556,
            0.084411607815889,
            0.100688742923031,
            0.109576058691530,
            0.114428533101131,
        ];
        for (expected_move, expected_velocity) in expected_moves.into_iter().zip(expected_post_drag)
        {
            let result = state
                .tick(
                    MovementInput {
                        forward: true,
                        ..Default::default()
                    },
                    &chunks,
                    geometry(),
                    &profile,
                )
                .unwrap();
            assert_close(result.applied.z, expected_move);
            assert_close(state.velocity.z, expected_velocity);
            assert_close(result.horizontal_acceleration, 0.100000009259259);
            assert_close(result.horizontal_drag, 0.546);
        }

        for (expected_move, expected_velocity) in [
            (0.114428533101131, 0.062477979073218),
            (0.062477979073218, 0.034112976573977),
            (0.034112976573977, 0.018625685209391),
        ] {
            let result = state
                .tick(MovementInput::default(), &chunks, geometry(), &profile)
                .unwrap();
            assert_close(result.applied.z, expected_move);
            assert_close(state.velocity.z, expected_velocity);
        }
    }

    #[test]
    fn vanilla_sprint_curve_builds_progressively_from_rest() {
        let (chunks, profile) = flat_world();
        let mut state = player();
        for (expected_move, expected_velocity) in [
            (0.130000012037037, 0.070980006572222),
            (0.200980018609259, 0.109735090160656),
            (0.239735102197693, 0.130895365799940),
            (0.260895377836977, 0.142448876298990),
            (0.272448888336027, 0.148757093031471),
        ] {
            let result = state
                .tick(
                    MovementInput {
                        forward: true,
                        sprint: true,
                        ..Default::default()
                    },
                    &chunks,
                    geometry(),
                    &profile,
                )
                .unwrap();
            assert_close(result.applied.z, expected_move);
            assert_close(state.velocity.z, expected_velocity);
        }
    }

    #[test]
    fn block_slipperiness_changes_acceleration_and_retained_momentum_separately() {
        let (chunks, mut normal) = flat_world();
        let mut ice = normal.clone();
        normal.slipperiness.insert(RuntimeBlockStateId(1), 0.6);
        ice.slipperiness.insert(RuntimeBlockStateId(1), 0.98);
        let sample = |profile: &BlockCollisionProfile| {
            let mut state = player();
            let result = state
                .tick(
                    MovementInput {
                        forward: true,
                        ..Default::default()
                    },
                    &chunks,
                    geometry(),
                    profile,
                )
                .unwrap();
            (result.horizontal_acceleration, result.horizontal_drag)
        };
        let (normal_acceleration, normal_drag) = sample(&normal);
        let (ice_acceleration, ice_drag) = sample(&ice);
        assert!(ice_acceleration < normal_acceleration);
        assert!(ice_drag > normal_drag);
        assert_close(ice_acceleration, 0.1 * 0.21600002 / 0.98_f64.powi(3));
        assert_close(ice_drag, 0.98 * 0.91);
    }

    #[test]
    fn normal_and_sprint_jump_follow_distinct_takeoff_equations() {
        let (chunks, profile) = flat_world();
        let mut walking = player();
        let walking_launch = walking
            .tick(
                MovementInput {
                    forward: true,
                    jump: true,
                    ..Default::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert!(walking_launch.jumped && walking_launch.grounded_at_start);
        assert_close(walking_launch.sprint_jump_impulse, 0.0);
        assert_close(walking_launch.applied.z, 0.100000009259259);
        assert_close(walking.velocity.z, 0.054600005055556);
        assert_close(walking.velocity.y, 0.3332);

        let mut sprinting = player();
        let sprint_launch = sprinting
            .tick(
                MovementInput {
                    forward: true,
                    sprint: true,
                    jump: true,
                    ..Default::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert!(sprint_launch.jumped && sprint_launch.grounded_at_start);
        assert_close(sprint_launch.sprint_jump_impulse, 0.2);
        assert_close(sprint_launch.applied.z, 0.330000012037037);
        assert_close(sprinting.velocity.z, 0.180180006572222);

        let airborne = sprinting
            .tick(
                MovementInput {
                    forward: true,
                    sprint: true,
                    ..Default::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert!(!airborne.grounded_at_start);
        assert_close(airborne.horizontal_acceleration, 0.02);
        assert_close(airborne.horizontal_drag, 0.91);
        assert_close(airborne.applied.z, 0.200180006572222);
        assert_close(sprinting.velocity.z, 0.182163805980722);
    }

    #[test]
    fn airborne_turning_preserves_momentum_and_adds_only_air_control() {
        let (chunks, profile) = flat_world();
        let mut state = player();
        state.pose.y = 5.0;
        state.pose.yaw = 90.0;
        state.on_ground = false;
        state.velocity.z = 0.1;
        let result = state
            .tick(
                MovementInput {
                    forward: true,
                    ..Default::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert_close(result.applied.x, -0.02);
        assert_close(result.applied.z, 0.1);
        assert_close(state.velocity.x, -0.0182);
        assert_close(state.velocity.z, 0.091);
    }

    #[test]
    fn reversing_ground_input_changes_velocity_progressively_not_instantly() {
        let (chunks, profile) = flat_world();
        let mut state = player();
        for _ in 0..5 {
            state
                .tick(
                    MovementInput {
                        forward: true,
                        ..Default::default()
                    },
                    &chunks,
                    geometry(),
                    &profile,
                )
                .unwrap();
        }
        let before = state.velocity.z;
        let reversal = state
            .tick(
                MovementInput {
                    backward: true,
                    ..Default::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert!(before > 0.1);
        assert!(
            reversal.applied.z > 0.0,
            "retained forward momentum should survive the first reverse-input tick"
        );
        assert!(state.velocity.z < before);
        for _ in 0..2 {
            state
                .tick(
                    MovementInput {
                        backward: true,
                        ..Default::default()
                    },
                    &chunks,
                    geometry(),
                    &profile,
                )
                .unwrap();
        }
        assert!(state.velocity.z < 0.0);
    }

    #[test]
    fn chained_sprint_jumps_retain_more_momentum_than_first_takeoff() {
        let (chunks, profile) = flat_world();
        let input = MovementInput {
            forward: true,
            sprint: true,
            jump: true,
            ..Default::default()
        };
        let mut state = player();
        let first = state.tick(input, &chunks, geometry(), &profile).unwrap();
        let first_takeoff_speed = first.requested.x.hypot(first.requested.z);
        let mut second_takeoff_speed = None;
        for _ in 0..40 {
            let result = state.tick(input, &chunks, geometry(), &profile).unwrap();
            if result.jumped {
                second_takeoff_speed = Some(result.requested.x.hypot(result.requested.z));
                break;
            }
        }
        let second_takeoff_speed = second_takeoff_speed.expect("player should land and jump again");
        assert!(second_takeoff_speed > first_takeoff_speed + 0.05);
    }

    #[test]
    fn low_ceiling_shortens_jump_cycle_without_destroying_horizontal_momentum() {
        let (open_chunks, profile) = flat_world();
        let mut ceiling_chunks = open_chunks.clone();
        add_low_ceiling(&mut ceiling_chunks);
        let input = MovementInput {
            forward: true,
            sprint: true,
            jump: true,
            ..Default::default()
        };
        let cycle = |chunks: &LoadedChunks| {
            let mut state = player();
            state.tick(input, chunks, geometry(), &profile).unwrap();
            for tick in 2..=40 {
                let result = state.tick(input, chunks, geometry(), &profile).unwrap();
                if result.jumped {
                    return (tick, result.requested.z);
                }
            }
            panic!("player did not complete a jump cycle")
        };
        let (open_ticks, open_speed) = cycle(&open_chunks);
        let (ceiling_ticks, ceiling_speed) = cycle(&ceiling_chunks);
        assert!(ceiling_ticks < open_ticks);
        assert!(
            ceiling_speed > 0.2,
            "head hit must not zero horizontal momentum"
        );
        assert_ne!(ceiling_speed, open_speed);
    }

    #[test]
    fn ordinary_jump_airtime_cannot_cover_three_blocks_from_rest() {
        let (chunks, profile) = flat_world();
        let mut state = player();
        let start_z = state.pose.z;
        let input = MovementInput {
            forward: true,
            jump: true,
            ..Default::default()
        };
        let mut launched = false;
        for _ in 0..40 {
            let result = state.tick(input, &chunks, geometry(), &profile).unwrap();
            launched |= result.jumped;
            if launched && state.on_ground {
                break;
            }
        }
        assert!(launched && state.on_ground);
        assert!(
            state.pose.z - start_z < 3.0,
            "ordinary jump travelled {} blocks",
            state.pose.z - start_z
        );
    }

    #[test]
    fn minecraft_input_basis_and_diagonal_normalization_are_correct() {
        let (chunks, profile) = flat_world();
        for (yaw, expected_x, expected_z) in [
            (0.0, 0.0, 1.0),
            (90.0, -1.0, 0.0),
            (-90.0, 1.0, 0.0),
            (180.0, 0.0, -1.0),
        ] {
            let mut state = player();
            state.pose.yaw = yaw;
            state
                .tick(
                    MovementInput {
                        forward: true,
                        ..Default::default()
                    },
                    &chunks,
                    geometry(),
                    &profile,
                )
                .unwrap();
            assert!(
                (state.velocity.x.signum() - expected_x).abs() < 0.01
                    || expected_x == 0.0 && state.velocity.x.abs() < 1e-9
            );
            assert!(
                (state.velocity.z.signum() - expected_z).abs() < 0.01
                    || expected_z == 0.0 && state.velocity.z.abs() < 1e-9
            );
        }
        let mut straight = player();
        straight
            .tick(
                MovementInput {
                    forward: true,
                    ..Default::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        let mut diagonal = player();
        diagonal
            .tick(
                MovementInput {
                    forward: true,
                    right: true,
                    ..Default::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert!(
            (straight.velocity.x.hypot(straight.velocity.z)
                - diagonal.velocity.x.hypot(diagonal.velocity.z))
            .abs()
                < 1e-9
        );
    }

    #[test]
    fn gravity_jump_landing_and_repeated_jump_are_bounded() {
        let (chunks, profile) = flat_world();
        let mut state = player();
        let launch = state
            .tick(
                MovementInput {
                    jump: true,
                    ..Default::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert!(launch.jumped);
        assert!(state.velocity.y > 0.0);
        let first = state.velocity.y;
        let airborne = state
            .tick(MovementInput::default(), &chunks, geometry(), &profile)
            .unwrap();
        assert!(!airborne.jumped);
        assert!(state.velocity.y < first);
        for _ in 0..80 {
            state
                .tick(MovementInput::default(), &chunks, geometry(), &profile)
                .unwrap();
        }
        assert!(state.on_ground);
        assert!(
            (state.pose.y - 1.0).abs() < 1e-6,
            "unexpected landing state: {state:?}"
        );
        assert!(state.velocity.is_finite());
    }

    #[test]
    fn held_jump_repeats_only_after_each_landing() {
        let (chunks, profile) = flat_world();
        let mut state = player();
        let input = MovementInput {
            jump: true,
            ..Default::default()
        };
        let mut launches = 0;
        for _ in 0..120 {
            let was_grounded = state.on_ground;
            state.tick(input, &chunks, geometry(), &profile).unwrap();
            if was_grounded && state.velocity.y > 0.0 {
                launches += 1;
            }
        }
        assert!(launches >= 2, "held jump launched only {launches} time(s)");
        assert!(state.pose.is_finite() && state.velocity.is_finite());
    }

    #[test]
    fn corrected_decimal_boundary_cannot_reenter_the_adjacent_full_cube() {
        let center_x = -2.3_f64;
        let bounds = Aabb::new(
            Vec3d::new(center_x - 0.3, 69.0, -4.8),
            Vec3d::new(center_x + 0.3, 70.8, -4.2),
        );
        let obstacle = Aabb::new(Vec3d::new(-2.0, 69.0, -5.0), Vec3d::new(-1.0, 70.0, -4.0));

        assert!(bounds.max.x > obstacle.min.x);
        let resolved = resolve_x(bounds, 0.05, &[obstacle]);
        assert!(resolved <= 0.0, "boundary allowed re-entry by {resolved}");
        assert!(resolved.abs() <= COLLISION_EPSILON);
    }

    #[test]
    fn decimal_boundary_regression_eliminates_a_synthetic_correction_storm() {
        fn old_exact_resolve_x(bounds: Aabb, delta: f64, obstacle: Aabb) -> f64 {
            if delta > 0.0 && bounds.max.x <= obstacle.min.x {
                delta.min(obstacle.min.x - bounds.max.x)
            } else {
                delta
            }
        }

        let obstacle = Aabb::new(Vec3d::new(-2.0, 69.0, -5.0), Vec3d::new(-1.0, 70.0, -4.0));
        let mut old_corrections = 0;
        let mut fixed_corrections = 0;
        for _ in 0..20 {
            let bounds = Aabb::new(
                Vec3d::new(-2.6, 69.0, -4.8),
                Vec3d::new(-2.3 + 0.3, 70.8, -4.2),
            );
            let old = old_exact_resolve_x(bounds, 0.05, obstacle);
            if bounds.max.x + old > obstacle.min.x + COLLISION_EPSILON {
                old_corrections += 1;
            }
            let fixed = resolve_x(bounds, 0.05, &[obstacle]);
            if bounds.max.x + fixed > obstacle.min.x + COLLISION_EPSILON {
                fixed_corrections += 1;
            }
        }
        assert_eq!(old_corrections, 20);
        assert_eq!(fixed_corrections, 0);
    }

    #[test]
    fn walk_sprint_sneak_and_opposites_have_expected_relative_speeds() {
        let (chunks, profile) = flat_world();
        let speed = |input| {
            let mut state = player();
            state.tick(input, &chunks, geometry(), &profile).unwrap();
            state.velocity.x.hypot(state.velocity.z)
        };
        let walk = speed(MovementInput {
            forward: true,
            ..Default::default()
        });
        let sprint = speed(MovementInput {
            forward: true,
            sprint: true,
            ..Default::default()
        });
        let sneak = speed(MovementInput {
            forward: true,
            sneak: true,
            ..Default::default()
        });
        let cancel = speed(MovementInput {
            forward: true,
            backward: true,
            ..Default::default()
        });
        assert!(sprint > walk);
        assert!(walk > sneak);
        assert_eq!(cancel, 0.0);
    }

    #[test]
    fn sprint_requires_forward_impulse_and_uses_stateful_vanilla_transitions() {
        let (chunks, profile) = flat_world();
        let speed = |input| {
            let mut state = player();
            state.tick(input, &chunks, geometry(), &profile).unwrap();
            (state.velocity.x.hypot(state.velocity.z), state.sprinting)
        };
        let (walk, _) = speed(MovementInput {
            forward: true,
            ..Default::default()
        });
        let (forward, forward_sprinting) = speed(MovementInput {
            forward: true,
            sprint: true,
            ..Default::default()
        });
        let (backward, backward_sprinting) = speed(MovementInput {
            backward: true,
            sprint: true,
            ..Default::default()
        });
        let (lateral, lateral_sprinting) = speed(MovementInput {
            right: true,
            sprint: true,
            ..Default::default()
        });
        let (diagonal, diagonal_sprinting) = speed(MovementInput {
            forward: true,
            right: true,
            sprint: true,
            ..Default::default()
        });
        assert!(forward_sprinting && diagonal_sprinting);
        assert!(!backward_sprinting && !lateral_sprinting);
        assert!(forward > walk && diagonal > walk);
        assert!((backward - walk).abs() < 1.0e-9);
        assert!((lateral - walk).abs() < 1.0e-9);

        let mut state = player();
        state
            .tick(
                MovementInput {
                    forward: true,
                    sprint: true,
                    ..Default::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert!(state.sprinting);
        state
            .tick(
                MovementInput {
                    forward: true,
                    ..Default::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert!(
            state.sprinting,
            "releasing Ctrl does not stop a forward sprint"
        );
        state
            .tick(MovementInput::default(), &chunks, geometry(), &profile)
            .unwrap();
        assert!(!state.sprinting, "losing forward impulse stops sprinting");

        for changed_direction in [
            MovementInput {
                right: true,
                sprint: true,
                ..Default::default()
            },
            MovementInput {
                backward: true,
                sprint: true,
                ..Default::default()
            },
        ] {
            let mut changing = player();
            changing
                .tick(
                    MovementInput {
                        forward: true,
                        sprint: true,
                        ..Default::default()
                    },
                    &chunks,
                    geometry(),
                    &profile,
                )
                .unwrap();
            assert!(changing.sprinting);
            changing
                .tick(changed_direction, &chunks, geometry(), &profile)
                .unwrap();
            assert!(!changing.sprinting);
        }
    }

    #[test]
    fn crouch_jump_and_collision_have_verified_sprint_interactions() {
        let (chunks, profile) = flat_world();
        let mut crouched_start = player();
        crouched_start
            .tick(
                MovementInput {
                    forward: true,
                    sneak: true,
                    sprint: true,
                    ..Default::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert!(!crouched_start.sprinting);

        let mut active = player();
        active
            .tick(
                MovementInput {
                    forward: true,
                    sprint: true,
                    ..Default::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        active.on_ground = true;
        active
            .tick(
                MovementInput {
                    forward: true,
                    jump: true,
                    ..Default::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert!(active.sprinting);
        assert!(active.velocity.y > 0.0);

        active.on_ground = true;
        active
            .tick(
                MovementInput {
                    forward: true,
                    sneak: true,
                    ..Default::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert!(
            active.sprinting,
            "26.1.2 preserves an existing ground sprint while crouch input slows movement"
        );

        active.horizontal_collision = true;
        active
            .tick(
                MovementInput {
                    forward: true,
                    ..Default::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert!(!active.sprinting);
    }

    #[test]
    fn pose_dimensions_eye_height_and_look_limits_are_separate() {
        assert_eq!(
            PlayerPoseKind::Standing.dimensions(),
            PlayerDimensions {
                width: 0.6,
                height: 1.8,
                eye_height: 1.62
            }
        );
        assert_eq!(
            PlayerPoseKind::Sneaking.dimensions(),
            PlayerDimensions {
                width: 0.6,
                height: 1.5,
                eye_height: 1.27
            }
        );
        let mut state = player();
        state.rotate(450.0, 200.0).unwrap();
        assert_eq!(state.pose.yaw, 90.0);
        assert_eq!(state.pose.pitch, 90.0);
    }

    #[test]
    fn wall_ceiling_corner_and_missing_chunk_policy_prevent_penetration() {
        let (chunks, profile) = flat_world();
        let bounds = Aabb::new(Vec3d::new(15.2, 1.0, 7.7), Vec3d::new(15.8, 2.8, 8.3));
        let (movement, _) = collide(
            bounds,
            Vec3d::new(1.0, 0.0, 0.0),
            &chunks,
            geometry(),
            &profile,
            true,
        )
        .unwrap();
        assert!(movement.x <= 0.2 + 1e-9);
        let mut chunks_with_ceiling = chunks.clone();
        let mut values = vec![RuntimeBlockStateId(0); 4096];
        values[2 * 256 + 8 * 16 + 8] = RuntimeBlockStateId(1);
        chunks_with_ceiling
            .insert(Chunk {
                coordinate: ChunkCoordinate::new(0, 0),
                sections: vec![
                    ChunkSection {
                        non_empty_block_count: 1,
                        fluid_count: 0,
                        blocks: PalettedContainer::Direct { values },
                        biomes: PalettedContainer::Single {
                            value: RuntimeBiomeId(0),
                            entries: 64,
                        },
                    },
                    section(RuntimeBlockStateId(0)),
                ],
                heightmaps: vec![],
                block_entities: vec![],
                light: ChunkLightSummary::default(),
            })
            .unwrap();
        let ceiling = Aabb::new(Vec3d::new(7.7, 0.2, 7.7), Vec3d::new(8.3, 1.8, 8.3));
        let (movement, _) = collide(
            ceiling,
            Vec3d::new(0.0, 1.0, 0.0),
            &chunks_with_ceiling,
            geometry(),
            &profile,
            false,
        )
        .unwrap();
        assert!(movement.y <= 0.4 + 1e-9);
    }

    #[test]
    fn partial_and_multiple_boxes_support_step_candidates() {
        let profile = BlockCollisionProfile::synthetic([(
            RuntimeBlockStateId(2),
            boxes(&[
                Aabb::new(Vec3d::new(0.0, 0.0, 0.0), Vec3d::new(1.0, 0.5, 1.0)),
                Aabb::new(Vec3d::new(0.0, 0.5, 0.5), Vec3d::new(1.0, 1.0, 1.0)),
            ]),
        )]);
        assert!(
            matches!(profile.shape(RuntimeBlockStateId(2)),CollisionShape::Boxes(values) if values.len()==2)
        );
    }

    #[test]
    fn verified_empty_stems_neither_block_movement_nor_create_support() {
        for path in [
            "pumpkin_stem",
            "melon_stem",
            "attached_pumpkin_stem",
            "attached_melon_stem",
        ] {
            let shape = classify_shape(path, &BTreeMap::new());
            assert_eq!(shape, CollisionShape::Empty, "{path}");
            let (chunks, profile) = single_shape_world(8, 1, 8, RuntimeBlockStateId(20), shape);

            let beside = Aabb::new(Vec3d::new(7.2, 1.0, 7.7), Vec3d::new(7.8, 2.8, 8.3));
            let horizontal = collide(
                beside,
                Vec3d::new(1.5, 0.0, 0.0),
                &chunks,
                geometry(),
                &profile,
                false,
            )
            .unwrap()
            .0;
            assert_close(horizontal.x, 1.5);

            let above = Aabb::new(Vec3d::new(8.2, 2.0, 8.2), Vec3d::new(8.8, 3.8, 8.8));
            let downward = collide(
                above,
                Vec3d::new(0.0, -2.0, 0.0),
                &chunks,
                geometry(),
                &profile,
                false,
            )
            .unwrap()
            .0;
            assert_close(downward.y, -2.0);
        }
    }

    #[test]
    fn step_candidate_clears_half_block_but_not_tall_or_ceiling_blocked_obstacles() {
        let bounds = Aabb::new(Vec3d::new(8.0, 1.0, 7.7), Vec3d::new(8.6, 2.8, 8.3));
        let (slab_world, profile) = obstacle_world(RuntimeBlockStateId(2), false);
        let (slab, stepped) = collide(
            bounds,
            Vec3d::new(0.8, 0.0, 0.0),
            &slab_world,
            geometry(),
            &profile,
            true,
        )
        .unwrap();
        assert!(stepped);
        assert!((slab.x - 0.8).abs() < 1.0e-9);
        assert!((slab.y - 0.5).abs() < 1.0e-9);

        let (tall_world, profile) = obstacle_world(RuntimeBlockStateId(3), false);
        let (tall, stepped) = collide(
            bounds,
            Vec3d::new(0.8, 0.0, 0.0),
            &tall_world,
            geometry(),
            &profile,
            true,
        )
        .unwrap();
        assert!(!stepped);
        assert!(tall.x <= 0.4 + 1.0e-9);

        let (blocked_world, profile) = obstacle_world(RuntimeBlockStateId(2), true);
        let (blocked, stepped) = collide(
            bounds,
            Vec3d::new(0.8, 0.0, 0.0),
            &blocked_world,
            geometry(),
            &profile,
            true,
        )
        .unwrap();
        assert!(!stepped);
        assert!(blocked.x <= 0.4 + 1.0e-9);
    }

    #[test]
    fn non_colliding_shape_allows_motion_and_unknown_state_is_conservative() {
        let bounds = Aabb::new(Vec3d::new(8.0, 1.0, 7.7), Vec3d::new(8.6, 2.8, 8.3));
        let (empty_world, profile) = obstacle_world(RuntimeBlockStateId(0), false);
        let (movement, _) = collide(
            bounds,
            Vec3d::new(0.8, 0.0, 0.0),
            &empty_world,
            geometry(),
            &profile,
            true,
        )
        .unwrap();
        assert!((movement.x - 0.8).abs() < 1.0e-9);
        assert!(matches!(
            profile.shape(RuntimeBlockStateId(u32::MAX)),
            CollisionShape::FullCube
        ));
        assert!(!profile.is_approximate(RuntimeBlockStateId(0)));
        assert!(profile.is_approximate(RuntimeBlockStateId(u32::MAX)));
    }

    #[test]
    fn door_collision_planes_follow_facing_open_hinge_and_half() {
        const T: f64 = 3.0 / 16.0;
        let expected = |facing: &str, open: bool, hinge: &str| match (facing, open, hinge) {
            ("east", false, _) => Aabb::new(Vec3d::new(0.0, 0.0, 0.0), Vec3d::new(T, 1.0, 1.0)),
            ("south", false, _) => Aabb::new(Vec3d::new(0.0, 0.0, 0.0), Vec3d::new(1.0, 1.0, T)),
            ("west", false, _) => {
                Aabb::new(Vec3d::new(1.0 - T, 0.0, 0.0), Vec3d::new(1.0, 1.0, 1.0))
            }
            ("north", false, _) => {
                Aabb::new(Vec3d::new(0.0, 0.0, 1.0 - T), Vec3d::new(1.0, 1.0, 1.0))
            }
            ("east", true, "left") | ("west", true, "right") => {
                Aabb::new(Vec3d::new(0.0, 0.0, 0.0), Vec3d::new(1.0, 1.0, T))
            }
            ("east", true, "right") | ("west", true, "left") => {
                Aabb::new(Vec3d::new(0.0, 0.0, 1.0 - T), Vec3d::new(1.0, 1.0, 1.0))
            }
            ("north", true, "left") | ("south", true, "right") => {
                Aabb::new(Vec3d::new(0.0, 0.0, 0.0), Vec3d::new(T, 1.0, 1.0))
            }
            ("north", true, "right") | ("south", true, "left") => {
                Aabb::new(Vec3d::new(1.0 - T, 0.0, 0.0), Vec3d::new(1.0, 1.0, 1.0))
            }
            _ => unreachable!(),
        };
        for facing in ["north", "east", "south", "west"] {
            for open in [false, true] {
                for hinge in ["left", "right"] {
                    let mut properties = BTreeMap::from([
                        ("facing".to_owned(), facing.to_owned()),
                        ("open".to_owned(), open.to_string()),
                        ("hinge".to_owned(), hinge.to_owned()),
                        ("half".to_owned(), "lower".to_owned()),
                    ]);
                    let lower = classify_shape("oak_door", &properties);
                    properties.insert("half".to_owned(), "upper".to_owned());
                    let upper = classify_shape("oak_door", &properties);
                    let expected = expected(facing, open, hinge);
                    assert!(
                        matches!(&lower, CollisionShape::Boxes(values) if values.as_ref() == [expected])
                    );
                    assert_eq!(lower, upper);
                }
            }
        }
        assert!(has_verified_shape("oak_door"));
        assert!(has_verified_shape("iron_door"));
    }

    #[test]
    fn water_and_lava_use_bounded_medium_specific_travel() {
        for (kind, expected_drag) in [(FluidKind::Water, WATER_DRAG), (FluidKind::Lava, LAVA_DRAG)]
        {
            let (chunks, profile) = special_world(
                BlockEnvironment::default(),
                Some(BlockEnvironment {
                    fluid: Some(crate::FluidState {
                        kind,
                        level: 0,
                        falling: true,
                    }),
                    ..BlockEnvironment::default()
                }),
            );
            let mut state = PlayerMovementState::from_authoritative(
                LocalPlayerPose::new(8.5, 1.0, 8.5, 0.0, 0.0),
                Vec3d::default(),
            )
            .unwrap();
            let result = state
                .tick(
                    MovementInput {
                        forward: true,
                        jump: true,
                        ..MovementInput::default()
                    },
                    &chunks,
                    geometry(),
                    &profile,
                )
                .unwrap();
            assert_eq!(state.in_water, kind == FluidKind::Water);
            assert_eq!(state.in_lava, kind == FluidKind::Lava);
            assert_close(result.horizontal_acceleration, WATER_ACCELERATION);
            assert_close(result.horizontal_drag, expected_drag);
            if kind == FluidKind::Water {
                assert!(state.velocity.y > 0.0);
            } else {
                assert!(state.velocity.y >= 0.0);
            }
        }
    }

    #[test]
    fn sprint_swimming_uses_current_client_horizontal_drag_and_pose() {
        let (mut chunks, profile) = special_world(
            BlockEnvironment::default(),
            Some(BlockEnvironment {
                fluid: Some(crate::FluidState {
                    kind: FluidKind::Water,
                    level: 0,
                    falling: true,
                }),
                ..BlockEnvironment::default()
            }),
        );
        assert!(chunks.update_block(
            ChunkCoordinate::new(0, 0),
            0,
            8,
            2,
            8,
            RuntimeBlockStateId(2),
        ));
        let mut state = PlayerMovementState::from_authoritative(
            LocalPlayerPose::new(8.5, 1.0, 8.5, 0.0, 0.0),
            Vec3d::default(),
        )
        .unwrap();
        let result = state
            .tick(
                MovementInput {
                    forward: true,
                    sprint: true,
                    ..MovementInput::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert!(state.sprinting && state.swimming);
        assert_eq!(state.pose_kind, PlayerPoseKind::Swimming);
        assert_close(result.horizontal_drag, WATER_SPRINT_DRAG);
    }

    #[test]
    fn shallow_water_does_not_enter_swimming_pose() {
        let (chunks, profile) = special_world(
            BlockEnvironment::default(),
            Some(BlockEnvironment {
                fluid: Some(crate::FluidState {
                    kind: FluidKind::Water,
                    level: 0,
                    falling: false,
                }),
                ..BlockEnvironment::default()
            }),
        );
        let mut state = PlayerMovementState::from_authoritative(
            LocalPlayerPose::new(8.5, 1.0, 8.5, 0.0, 0.0),
            Vec3d::default(),
        )
        .unwrap();
        state
            .tick(
                MovementInput {
                    forward: true,
                    sprint: true,
                    ..MovementInput::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert!(state.in_water);
        assert!(!state.swimming);
        assert_eq!(state.pose_kind, PlayerPoseKind::Standing);
        assert_eq!(
            fluid_flow(&chunks, geometry(), &profile, 8, 1, 8),
            Vec3d::default(),
            "an isolated source bounded by solid floor must not invent horizontal current"
        );
    }

    #[test]
    fn active_swimming_continues_through_shallow_water_and_at_the_surface() {
        let (mut chunks, profile) = special_world(
            BlockEnvironment::default(),
            Some(BlockEnvironment {
                fluid: Some(crate::FluidState {
                    kind: FluidKind::Water,
                    level: 0,
                    falling: true,
                }),
                ..BlockEnvironment::default()
            }),
        );
        assert!(chunks.update_block(
            ChunkCoordinate::new(0, 0),
            0,
            8,
            2,
            8,
            RuntimeBlockStateId(2),
        ));
        let mut state = PlayerMovementState::from_authoritative(
            LocalPlayerPose::new(8.5, 1.0, 8.5, 0.0, 0.0),
            Vec3d::default(),
        )
        .unwrap();
        state
            .tick(
                MovementInput {
                    forward: true,
                    sprint: true,
                    ..MovementInput::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert!(state.swimming);

        assert!(chunks.update_block(
            ChunkCoordinate::new(0, 0),
            0,
            8,
            2,
            8,
            RuntimeBlockStateId(0),
        ));
        state
            .tick(
                MovementInput {
                    forward: true,
                    sprint: true,
                    ..MovementInput::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert!(state.in_water);
        assert!(
            state.swimming,
            "eye emergence is not a stop-swimming condition"
        );

        assert!(chunks.update_block(
            ChunkCoordinate::new(0, 0),
            0,
            8,
            2,
            8,
            RuntimeBlockStateId(2),
        ));
        state
            .tick(
                MovementInput {
                    forward: true,
                    sprint: true,
                    ..MovementInput::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert!(
            state.swimming,
            "deep -> shallow -> deep keeps the active pose"
        );

        assert!(chunks.update_block(
            ChunkCoordinate::new(0, 0),
            0,
            8,
            2,
            8,
            RuntimeBlockStateId(0),
        ));

        assert!(chunks.update_block(
            ChunkCoordinate::new(0, 0),
            0,
            8,
            1,
            8,
            RuntimeBlockStateId(0),
        ));
        state
            .tick(
                MovementInput {
                    forward: true,
                    sprint: true,
                    ..MovementInput::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert!(!state.in_water);
        assert!(!state.swimming);
    }

    #[test]
    fn upward_look_at_surface_does_not_force_swim_exit_without_jump() {
        let (mut chunks, profile) = special_world(
            BlockEnvironment::default(),
            Some(BlockEnvironment {
                fluid: Some(crate::FluidState {
                    kind: FluidKind::Water,
                    level: 0,
                    falling: false,
                }),
                ..BlockEnvironment::default()
            }),
        );
        for z in 9..16 {
            assert!(chunks.update_block(
                ChunkCoordinate::new(0, 0),
                0,
                8,
                1,
                z,
                RuntimeBlockStateId(2),
            ));
        }
        let mut state = PlayerMovementState::from_authoritative(
            LocalPlayerPose::new(8.5, 1.0, 8.5, 0.0, -90.0),
            Vec3d::default(),
        )
        .unwrap();
        state.swimming = true;
        state.sprinting = true;
        for _ in 0..20 {
            state
                .tick(
                    MovementInput {
                        forward: true,
                        sprint: true,
                        ..MovementInput::default()
                    },
                    &chunks,
                    geometry(),
                    &profile,
                )
                .unwrap();
            assert!(state.in_water);
            assert!(state.swimming);
        }

        let mut intentional_exit = PlayerMovementState::from_authoritative(
            LocalPlayerPose::new(8.5, 1.0, 8.5, 0.0, -90.0),
            Vec3d::default(),
        )
        .unwrap();
        intentional_exit.swimming = true;
        intentional_exit.sprinting = true;
        for _ in 0..20 {
            intentional_exit
                .tick(
                    MovementInput {
                        forward: true,
                        sprint: true,
                        jump: true,
                        ..MovementInput::default()
                    },
                    &chunks,
                    geometry(),
                    &profile,
                )
                .unwrap();
            if !intentional_exit.in_water {
                break;
            }
        }
        assert!(!intentional_exit.in_water);
        assert!(!intentional_exit.swimming);
    }

    #[test]
    fn authoritative_position_correction_preserves_surface_swim_continuation() {
        let (chunks, profile) = special_world(
            BlockEnvironment::default(),
            Some(BlockEnvironment {
                fluid: Some(crate::FluidState {
                    kind: FluidKind::Water,
                    level: 0,
                    falling: false,
                }),
                ..BlockEnvironment::default()
            }),
        );
        let mut state = PlayerMovementState::from_authoritative(
            LocalPlayerPose::new(8.5, 1.0, 8.5, 0.0, 0.0),
            Vec3d::default(),
        )
        .unwrap();
        state.swimming = true;
        state.sprinting = true;
        state.pose_kind = PlayerPoseKind::Swimming;
        state
            .reconcile(
                LocalPlayerPose::new(8.5, 1.1, 8.5, 0.0, 0.0),
                Vec3d::default(),
            )
            .unwrap();
        state
            .tick(
                MovementInput {
                    forward: true,
                    sprint: true,
                    ..MovementInput::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert!(state.in_water);
        assert!(state.swimming);
    }

    #[test]
    fn level_sprint_swimming_has_no_artificial_downward_drift() {
        let (chunks, profile) = special_world(
            BlockEnvironment::default(),
            Some(BlockEnvironment {
                fluid: Some(crate::FluidState {
                    kind: FluidKind::Water,
                    level: 0,
                    falling: true,
                }),
                ..BlockEnvironment::default()
            }),
        );
        let mut state = PlayerMovementState::from_authoritative(
            LocalPlayerPose::new(8.5, 1.1, 8.5, 0.0, 0.0),
            Vec3d::default(),
        )
        .unwrap();
        state.swimming = true;
        state.sprinting = true;
        let result = state
            .tick(
                MovementInput {
                    forward: true,
                    sprint: true,
                    ..MovementInput::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert!(result.applied.y.abs() <= COLLISION_EPSILON);
        assert_close(state.velocity.y, 0.0);
    }

    #[test]
    fn upright_water_sprint_uses_fluid_acceleration_not_land_sprint_acceleration() {
        let (chunks, profile) = special_world(
            BlockEnvironment::default(),
            Some(BlockEnvironment {
                fluid: Some(crate::FluidState {
                    kind: FluidKind::Water,
                    level: 0,
                    falling: false,
                }),
                ..BlockEnvironment::default()
            }),
        );
        let sample = |sprint| {
            let mut state = PlayerMovementState::from_authoritative(
                LocalPlayerPose::new(8.5, 1.1, 8.5, 0.0, 0.0),
                Vec3d::default(),
            )
            .unwrap();
            state
                .tick(
                    MovementInput {
                        forward: true,
                        sprint,
                        ..MovementInput::default()
                    },
                    &chunks,
                    geometry(),
                    &profile,
                )
                .unwrap()
        };
        let walking = sample(false);
        let sprinting = sample(true);
        assert_close(walking.horizontal_acceleration, WATER_ACCELERATION);
        assert_close(sprinting.horizontal_acceleration, WATER_ACCELERATION);
        assert_close(walking.applied.z, sprinting.applied.z);
        assert_eq!(sprinting.horizontal_drag, WATER_DRAG);
    }

    #[test]
    fn player_currents_average_and_opposing_flows_cancel_independently_of_order() {
        let east = Vec3d::new(1.0, 0.0, 0.0);
        let west = Vec3d::new(-1.0, 0.0, 0.0);
        let north = Vec3d::new(0.0, 0.0, -1.0);
        assert_eq!(
            averaged_player_fluid_flow(&[east, west], 1.0),
            Vec3d::default()
        );
        assert_eq!(
            averaged_player_fluid_flow(&[west, east], 1.0),
            Vec3d::default()
        );
        assert_eq!(
            averaged_player_fluid_flow(&[east, west, north, Vec3d::new(0.0, 0.0, 1.0)], 1.0),
            Vec3d::default()
        );
        let biased = averaged_player_fluid_flow(&[east, east, west], 1.0);
        assert_close(biased.x, 1.0 / 3.0);
        assert_close(biased.z, 0.0);
        let shallow = averaged_player_fluid_flow(&[east], 0.2);
        assert_close(shallow.x, 0.5);
        let delta = player_fluid_current(shallow, Vec3d::default());
        assert_close(delta.x, 0.007);
    }

    #[test]
    fn slime_reflection_receives_same_tick_air_gravity_and_drag() {
        let reflected = 0.42_f64;
        let after_travel = (reflected - GRAVITY) * VERTICAL_DRAG;
        assert_close(after_travel, 0.3332);
        assert!(after_travel < reflected);
        let second = (after_travel - GRAVITY) * VERTICAL_DRAG;
        assert!(second < after_travel);
    }

    #[test]
    fn climb_assist_receives_air_gravity_and_drag_after_motion() {
        let post_tick = (CLIMB_UP_SPEED - GRAVITY) * VERTICAL_DRAG;
        assert_close(post_tick, 0.1176);
        assert!(post_tick < CLIMB_UP_SPEED);
        assert!(post_tick > 0.0);
    }

    #[test]
    fn climbable_contact_clamps_fall_and_allows_ascent() {
        let (mut chunks, profile) = special_world(
            BlockEnvironment::default(),
            Some(BlockEnvironment {
                climbable: true,
                ..BlockEnvironment::default()
            }),
        );
        let mut state = PlayerMovementState::from_authoritative(
            LocalPlayerPose::new(8.5, 1.0, 8.5, 0.0, 0.0),
            Vec3d::new(0.4, -0.8, 0.4),
        )
        .unwrap();
        state
            .tick(MovementInput::default(), &chunks, geometry(), &profile)
            .unwrap();
        assert!(state.climbing);
        assert!(state.velocity.y >= CLIMB_FALL_LIMIT);
        assert!(chunks.update_block(
            ChunkCoordinate::new(0, 0),
            0,
            8,
            1,
            9,
            RuntimeBlockStateId(1),
        ));
        state.pose.z = 8.69;
        state.velocity = Vec3d::default();
        state
            .tick(
                MovementInput {
                    forward: true,
                    ..MovementInput::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert!(state.velocity.y > 0.0);
    }

    #[test]
    fn touching_a_thin_ladder_plane_restores_collision_assisted_climb() {
        let (chunks, mut profile) = special_world(
            BlockEnvironment::default(),
            Some(BlockEnvironment {
                climbable: true,
                ..BlockEnvironment::default()
            }),
        );
        profile.states.insert(
            RuntimeBlockStateId(2),
            boxes(&[Aabb::new(
                Vec3d::new(0.0, 0.0, 13.0 / 16.0),
                Vec3d::new(1.0, 1.0, 1.0),
            )]),
        );
        profile.source_shape_bounds = collision_shape_envelope(profile.states.values(), 0.0);
        let mut state = PlayerMovementState::from_authoritative(
            LocalPlayerPose::new(8.5, 1.0, 7.7, 0.0, 0.0),
            Vec3d::default(),
        )
        .unwrap();
        state.on_ground = true;
        for _ in 0..20 {
            state
                .tick(
                    MovementInput {
                        forward: true,
                        ..MovementInput::default()
                    },
                    &chunks,
                    geometry(),
                    &profile,
                )
                .unwrap();
            if state.climbing && state.horizontal_collision {
                break;
            }
        }
        assert!(state.climbing);
        assert!(state.horizontal_collision);
        assert_close(state.velocity.y, (CLIMB_UP_SPEED - GRAVITY) * VERTICAL_DRAG);
    }

    #[test]
    fn ladder_semantic_contact_climbs_multiple_blocks_for_every_facing() {
        let cases = [
            (
                LocalPlayerPose::new(8.5, 1.0, 7.3, 0.0, 0.0),
                Aabb::new(Vec3d::new(0.0, 0.0, 13.0 / 16.0), Vec3d::new(1.0, 1.0, 1.0)),
                (8, 9),
            ),
            (
                LocalPlayerPose::new(8.5, 1.0, 9.7, 180.0, 0.0),
                Aabb::new(Vec3d::new(0.0, 0.0, 0.0), Vec3d::new(1.0, 1.0, 3.0 / 16.0)),
                (8, 7),
            ),
            (
                LocalPlayerPose::new(7.3, 1.0, 8.5, -90.0, 0.0),
                Aabb::new(Vec3d::new(13.0 / 16.0, 0.0, 0.0), Vec3d::new(1.0, 1.0, 1.0)),
                (9, 8),
            ),
            (
                LocalPlayerPose::new(9.7, 1.0, 8.5, 90.0, 0.0),
                Aabb::new(Vec3d::new(0.0, 0.0, 0.0), Vec3d::new(3.0 / 16.0, 1.0, 1.0)),
                (7, 8),
            ),
        ];
        for (pose, shape, wall) in cases {
            let (mut chunks, mut profile) = special_world(
                BlockEnvironment::default(),
                Some(BlockEnvironment {
                    climbable: true,
                    ..BlockEnvironment::default()
                }),
            );
            for y in 2..6 {
                assert!(chunks.update_block(
                    ChunkCoordinate::new(0, 0),
                    0,
                    8,
                    y,
                    8,
                    RuntimeBlockStateId(2),
                ));
            }
            for y in 1..6 {
                assert!(chunks.update_block(
                    ChunkCoordinate::new(0, 0),
                    0,
                    wall.0,
                    y,
                    wall.1,
                    RuntimeBlockStateId(1),
                ));
            }
            profile
                .states
                .insert(RuntimeBlockStateId(2), boxes(&[shape]));
            profile.source_shape_bounds = collision_shape_envelope(profile.states.values(), 0.0);
            for jump in [false, true] {
                let mut state =
                    PlayerMovementState::from_authoritative(pose, Vec3d::default()).unwrap();
                let start_y = state.pose.y;
                assert!(
                    !touches_climbing_cell(state.bounding_box(), &chunks, geometry(), &profile,).0
                );
                for _ in 0..30 {
                    state
                        .tick(
                            MovementInput {
                                forward: true,
                                jump,
                                ..MovementInput::default()
                            },
                            &chunks,
                            geometry(),
                            &profile,
                        )
                        .unwrap();
                }
                assert!(
                    state.pose.y > start_y + 1.0,
                    "start={pose:?}, final={:?}, velocity={:?}, horizontal_collision={}, climbing={}, jump={jump}",
                    state.pose,
                    state.velocity,
                    state.horizontal_collision,
                    state.climbing,
                );
                assert!(state.climbing);
            }
        }
    }

    #[test]
    fn ladder_side_contact_does_not_activate_semantic_climbing() {
        let cases = [
            (
                Aabb::new(Vec3d::new(0.0, 0.0, 13.0 / 16.0), Vec3d::new(1.0, 1.0, 1.0)),
                LocalPlayerPose::new(7.7, 1.0, 8.9, 0.0, 0.0),
                (8, 9),
            ),
            (
                Aabb::new(Vec3d::new(0.0, 0.0, 0.0), Vec3d::new(1.0, 1.0, 3.0 / 16.0)),
                LocalPlayerPose::new(7.7, 1.0, 8.1, 180.0, 0.0),
                (8, 7),
            ),
            (
                Aabb::new(Vec3d::new(13.0 / 16.0, 0.0, 0.0), Vec3d::new(1.0, 1.0, 1.0)),
                LocalPlayerPose::new(8.9, 1.0, 7.7, -90.0, 0.0),
                (9, 8),
            ),
            (
                Aabb::new(Vec3d::new(0.0, 0.0, 0.0), Vec3d::new(3.0 / 16.0, 1.0, 1.0)),
                LocalPlayerPose::new(8.1, 1.0, 7.7, 90.0, 0.0),
                (7, 8),
            ),
        ];
        for (shape, pose, wall) in cases {
            let (mut chunks, mut profile) = special_world(
                BlockEnvironment::default(),
                Some(BlockEnvironment {
                    climbable: true,
                    ..BlockEnvironment::default()
                }),
            );
            for y in 1..4 {
                let _ = chunks.update_block(
                    ChunkCoordinate::new(0, 0),
                    0,
                    8,
                    y,
                    8,
                    RuntimeBlockStateId(2),
                );
                let _ = chunks.update_block(
                    ChunkCoordinate::new(0, 0),
                    0,
                    wall.0,
                    y,
                    wall.1,
                    RuntimeBlockStateId(1),
                );
            }
            profile
                .states
                .insert(RuntimeBlockStateId(2), boxes(&[shape]));
            profile.source_shape_bounds = collision_shape_envelope(profile.states.values(), 0.0);
            let mut state =
                PlayerMovementState::from_authoritative(pose, Vec3d::default()).unwrap();
            state.on_ground = true;
            assert!(!touches_climbing_cell(state.bounding_box(), &chunks, geometry(), &profile).0);
            for _ in 0..5 {
                state
                    .tick(
                        MovementInput {
                            forward: true,
                            ..Default::default()
                        },
                        &chunks,
                        geometry(),
                        &profile,
                    )
                    .unwrap();
                assert!(!state.climbing);
                assert!(state.velocity.y <= 0.0);
            }
        }
    }

    #[test]
    fn scaffolding_top_catches_falls_and_is_bypassed_only_while_descending() {
        let (mut chunks, profile) = special_world(
            BlockEnvironment::default(),
            Some(BlockEnvironment {
                scaffolding: true,
                ..BlockEnvironment::default()
            }),
        );
        let bounds = Aabb::new(Vec3d::new(8.2, 2.4, 8.2), Vec3d::new(8.8, 4.2, 8.8));
        assert_eq!(
            scaffolding_support_y(bounds, -0.8, &chunks, geometry(), &profile),
            Some(2.0)
        );

        let mut falling = PlayerMovementState::from_authoritative(
            LocalPlayerPose::new(8.5, 2.4, 8.5, 0.0, 0.0),
            Vec3d::new(0.0, -0.8, 0.0),
        )
        .unwrap();
        falling.on_ground = false;
        falling
            .tick(MovementInput::default(), &chunks, geometry(), &profile)
            .unwrap();
        assert!((falling.pose.y - 2.0).abs() <= COLLISION_EPSILON);
        assert!(falling.on_ground);

        let mut descending = PlayerMovementState::from_authoritative(
            LocalPlayerPose::new(8.5, 2.0, 8.5, 0.0, 0.0),
            Vec3d::new(0.0, -0.2, 0.0),
        )
        .unwrap();
        let result = descending
            .tick(
                MovementInput {
                    sneak: true,
                    ..MovementInput::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert!(result.applied.y < 0.0);

        // A neighbouring ordinary support block lets the player approach the
        // scaffolding at the same top height. Horizontal motion must discover
        // support after X/Z collision resolution rather than only at the
        // tick's original footprint.
        assert!(chunks.update_block(
            ChunkCoordinate::new(0, 0),
            0,
            7,
            1,
            8,
            RuntimeBlockStateId(1),
        ));
        let mut walking = PlayerMovementState::from_authoritative(
            LocalPlayerPose::new(7.5, 2.0, 8.5, -90.0, 0.0),
            Vec3d::default(),
        )
        .unwrap();
        walking.on_ground = true;
        for _ in 0..6 {
            walking
                .tick(
                    MovementInput {
                        forward: true,
                        ..MovementInput::default()
                    },
                    &chunks,
                    geometry(),
                    &profile,
                )
                .unwrap();
        }
        assert!(walking.pose.x > 8.0, "pose={:?}", walking.pose);
        assert!(
            (walking.pose.y - 2.0).abs() <= COLLISION_EPSILON * 2.0,
            "pose={:?}",
            walking.pose
        );
        assert!(walking.on_ground);
    }

    #[test]
    fn scaffolding_support_is_context_sensitive_for_stand_ascend_and_descend() {
        let (chunks, profile) = special_world(
            BlockEnvironment {
                scaffolding: true,
                ..BlockEnvironment::default()
            },
            None,
        );
        let mut standing = PlayerMovementState::from_authoritative(
            LocalPlayerPose::new(8.5, 1.0, 8.5, 0.0, 0.0),
            Vec3d::new(0.0, -0.2, 0.0),
        )
        .unwrap();
        let stopped = standing
            .tick(MovementInput::default(), &chunks, geometry(), &profile)
            .unwrap();
        assert_eq!(stopped.applied.y, 0.0);
        assert!(standing.on_ground);
        let idle = standing
            .tick(MovementInput::default(), &chunks, geometry(), &profile)
            .unwrap();
        assert_eq!(idle.applied.y, 0.0);
        assert!(idle.grounded_at_start);
        assert!(standing.on_ground);

        let mut descending = PlayerMovementState::from_authoritative(
            LocalPlayerPose::new(8.5, 1.0, 8.5, 0.0, 0.0),
            Vec3d::new(0.0, -0.2, 0.0),
        )
        .unwrap();
        let passed = descending
            .tick(
                MovementInput {
                    jump: true,
                    sneak: true,
                    ..MovementInput::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert!(passed.applied.y < 0.0);

        assert!(standing.on_ground);
        let top_environment =
            movement_environment(standing.bounding_box(), &chunks, geometry(), &profile);
        assert!(top_environment.supported_on_scaffolding_top);
        assert!(!top_environment.scaffolding);
        let top_jump = standing
            .tick(
                MovementInput {
                    jump: true,
                    ..MovementInput::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert!(top_jump.jumped);
        assert_close(top_jump.applied.y, JUMP_VELOCITY);
        assert_close(
            standing.velocity.y,
            (JUMP_VELOCITY - GRAVITY) * VERTICAL_DRAG,
        );
        assert!(!standing.climbing);

        let mut landed = false;
        for _ in 0..80 {
            standing
                .tick(MovementInput::default(), &chunks, geometry(), &profile)
                .unwrap();
            if standing.on_ground {
                landed = true;
                break;
            }
        }
        assert!(landed, "ordinary top jump must land back on scaffolding");
        assert_close(standing.pose.y, 1.0);
    }

    #[test]
    fn scaffolding_stack_has_independent_multitick_ascent_and_descent() {
        let (mut chunks, profile) = special_world(
            BlockEnvironment::default(),
            Some(BlockEnvironment {
                scaffolding: true,
                ..BlockEnvironment::default()
            }),
        );
        for y in 2..6 {
            assert!(chunks.update_block(
                ChunkCoordinate::new(0, 0),
                0,
                8,
                y,
                8,
                RuntimeBlockStateId(2),
            ));
        }
        let mut state = PlayerMovementState::from_authoritative(
            LocalPlayerPose::new(8.5, 1.0, 8.5, 0.0, 0.0),
            Vec3d::default(),
        )
        .unwrap();
        for _ in 0..10 {
            state
                .tick(
                    MovementInput {
                        jump: true,
                        ..MovementInput::default()
                    },
                    &chunks,
                    geometry(),
                    &profile,
                )
                .unwrap();
        }
        assert!(state.pose.y > 2.0);
        let high = state.pose.y;
        for _ in 0..5 {
            state
                .tick(
                    MovementInput {
                        sneak: true,
                        ..MovementInput::default()
                    },
                    &chunks,
                    geometry(),
                    &profile,
                )
                .unwrap();
        }
        assert!(state.pose.y < high);
        assert!(state.climbing);
    }

    #[test]
    fn scaffolding_stack_transitions_to_top_support_and_ordinary_jump() {
        let (mut chunks, profile) = special_world(
            BlockEnvironment {
                scaffolding: true,
                ..BlockEnvironment::default()
            },
            Some(BlockEnvironment {
                scaffolding: true,
                ..BlockEnvironment::default()
            }),
        );
        assert!(chunks.update_block(
            ChunkCoordinate::new(0, 0),
            0,
            8,
            2,
            8,
            RuntimeBlockStateId(2),
        ));
        let mut state = PlayerMovementState::from_authoritative(
            LocalPlayerPose::new(8.5, 1.0, 8.5, 0.0, 0.0),
            Vec3d::default(),
        )
        .unwrap();

        let internal_ascent = state
            .tick(
                MovementInput {
                    jump: true,
                    ..MovementInput::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert!(!internal_ascent.jumped);
        assert_close(internal_ascent.applied.y, CLIMB_UP_SPEED);
        for _ in 0..11 {
            state
                .tick(
                    MovementInput {
                        jump: true,
                        ..MovementInput::default()
                    },
                    &chunks,
                    geometry(),
                    &profile,
                )
                .unwrap();
        }

        let mut landed_on_top = false;
        for _ in 0..80 {
            state
                .tick(MovementInput::default(), &chunks, geometry(), &profile)
                .unwrap();
            if state.on_ground {
                landed_on_top = true;
                break;
            }
        }
        assert!(landed_on_top);
        assert_close(state.pose.y, 3.0);
        let environment = movement_environment(state.bounding_box(), &chunks, geometry(), &profile);
        assert!(environment.supported_on_scaffolding_top);
        assert!(!environment.scaffolding);

        let top_jump = state
            .tick(
                MovementInput {
                    jump: true,
                    ..MovementInput::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert!(top_jump.jumped);
        assert_close(top_jump.applied.y, JUMP_VELOCITY);
    }

    #[test]
    fn special_support_surfaces_preserve_ice_and_slime_semantics() {
        let (chunks, profile) = special_world(
            BlockEnvironment {
                surface: SpecialSurface::BlueIce,
                ..BlockEnvironment::default()
            },
            None,
        );
        assert_close(profile.slipperiness(RuntimeBlockStateId(1)), 0.989);
        let mut state = PlayerMovementState::from_authoritative(
            LocalPlayerPose::new(8.5, 1.0, 8.5, 0.0, 0.0),
            Vec3d::new(0.0, -0.5, 0.0),
        )
        .unwrap();
        state.on_ground = false;
        let mut slime_profile = profile.clone();
        slime_profile.set_environment(BlockEnvironmentProfile::synthetic([(
            RuntimeBlockStateId(1),
            BlockEnvironment {
                surface: SpecialSurface::Slime,
                ..BlockEnvironment::default()
            },
        )]));
        state
            .tick(
                MovementInput::default(),
                &chunks,
                geometry(),
                &slime_profile,
            )
            .unwrap();
        assert!(state.velocity.y > 0.0);
        assert!(state.on_ground);

        let mut sneaking = PlayerMovementState::from_authoritative(
            LocalPlayerPose::new(8.5, 1.0, 8.5, 0.0, 0.0),
            Vec3d::new(0.0, -0.5, 0.0),
        )
        .unwrap();
        sneaking.on_ground = false;
        sneaking
            .tick(
                MovementInput {
                    jump: true,
                    sneak: true,
                    ..MovementInput::default()
                },
                &chunks,
                geometry(),
                &slime_profile,
            )
            .unwrap();
        assert_eq!(sneaking.velocity.y, 0.0);
        assert!(sneaking.on_ground);
    }

    #[test]
    fn slime_bounce_sequence_decays_and_eventually_rests() {
        let (chunks, mut profile) = special_world(BlockEnvironment::default(), None);
        profile.set_environment(BlockEnvironmentProfile::synthetic([(
            RuntimeBlockStateId(1),
            BlockEnvironment {
                surface: SpecialSurface::Slime,
                ..BlockEnvironment::default()
            },
        )]));
        let mut state = PlayerMovementState::from_authoritative(
            LocalPlayerPose::new(8.5, 5.0, 8.5, 0.0, 0.0),
            Vec3d::default(),
        )
        .unwrap();
        state.on_ground = false;
        let mut launches = Vec::new();
        for _ in 0..500 {
            let before = state.velocity.y;
            state
                .tick(MovementInput::default(), &chunks, geometry(), &profile)
                .unwrap();
            if before < 0.0 && state.velocity.y > 0.0 {
                launches.push(state.velocity.y);
            }
            if state.on_ground && state.velocity.y == 0.0 && launches.len() >= 3 {
                break;
            }
        }
        assert!(launches.len() >= 3, "expected several observable bounces");
        assert!(launches.windows(2).all(|pair| pair[1] < pair[0]));
        assert!(state.on_ground);
        assert_eq!(state.velocity.y, 0.0);
    }

    #[test]
    fn held_jump_is_sampled_after_slime_contact_without_replacing_the_bounce() {
        let (chunks, mut profile) = special_world(BlockEnvironment::default(), None);
        profile.set_environment(BlockEnvironmentProfile::synthetic([(
            RuntimeBlockStateId(1),
            BlockEnvironment {
                surface: SpecialSurface::Slime,
                ..BlockEnvironment::default()
            },
        )]));
        let mut state = PlayerMovementState::from_authoritative(
            LocalPlayerPose::new(8.5, 5.0, 8.5, 0.0, 0.0),
            Vec3d::default(),
        )
        .unwrap();
        state.on_ground = false;

        let mut contact = None;
        for _ in 0..100 {
            let before = state.velocity.y;
            let result = state
                .tick(
                    MovementInput {
                        jump: true,
                        ..MovementInput::default()
                    },
                    &chunks,
                    geometry(),
                    &profile,
                )
                .unwrap();
            if before < 0.0 && state.velocity.y > 0.0 {
                contact = Some((result, state.velocity.y));
                break;
            }
        }
        let (contact, reflected_after_travel) = contact.expect("player must contact slime");
        assert!(!contact.grounded_at_start);
        assert!(
            !contact.jumped,
            "airborne input must not jump before landing"
        );
        assert!(
            state.on_ground,
            "slime callback preserves the landing state"
        );

        let manual = state
            .tick(
                MovementInput {
                    jump: true,
                    ..MovementInput::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert!(manual.grounded_at_start);
        assert!(manual.jumped);
        assert_close(manual.applied.y, reflected_after_travel.max(JUMP_VELOCITY));
        assert!(!state.on_ground);

        let airborne = state
            .tick(
                MovementInput {
                    jump: true,
                    ..MovementInput::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert!(!airborne.grounded_at_start);
        assert!(
            !airborne.jumped,
            "held input must not reapply a jump in mid-air"
        );
    }

    #[test]
    fn late_slime_bounce_can_be_raised_to_an_ordinary_jump() {
        let (chunks, mut profile) = special_world(BlockEnvironment::default(), None);
        profile.set_environment(BlockEnvironmentProfile::synthetic([(
            RuntimeBlockStateId(1),
            BlockEnvironment {
                surface: SpecialSurface::Slime,
                ..BlockEnvironment::default()
            },
        )]));
        let mut state = PlayerMovementState::from_authoritative(
            LocalPlayerPose::new(8.5, 5.0, 8.5, 0.0, 0.0),
            Vec3d::default(),
        )
        .unwrap();
        state.on_ground = false;

        let mut late_contact = false;
        for _ in 0..500 {
            let before = state.velocity.y;
            state
                .tick(MovementInput::default(), &chunks, geometry(), &profile)
                .unwrap();
            if before < 0.0 && state.velocity.y > 0.0 && state.velocity.y < JUMP_VELOCITY {
                late_contact = true;
                break;
            }
        }
        assert!(
            late_contact,
            "automatic bounce must decay below jump velocity"
        );
        assert!(state.on_ground);

        let result = state
            .tick(
                MovementInput {
                    jump: true,
                    ..MovementInput::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert!(result.jumped);
        assert_close(result.applied.y, JUMP_VELOCITY);
        assert_close(state.velocity.y, (JUMP_VELOCITY - GRAVITY) * VERTICAL_DRAG);
    }

    #[test]
    fn jump_pressed_only_on_slime_impact_is_not_buffered_after_release() {
        let (chunks, mut profile) = special_world(BlockEnvironment::default(), None);
        profile.set_environment(BlockEnvironmentProfile::synthetic([(
            RuntimeBlockStateId(1),
            BlockEnvironment {
                surface: SpecialSurface::Slime,
                ..BlockEnvironment::default()
            },
        )]));
        let mut state = PlayerMovementState::from_authoritative(
            LocalPlayerPose::new(8.5, 1.2, 8.5, 0.0, 0.0),
            Vec3d::new(0.0, -0.5, 0.0),
        )
        .unwrap();
        state.on_ground = false;

        let contact = state
            .tick(
                MovementInput {
                    jump: true,
                    ..MovementInput::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert!(!contact.grounded_at_start);
        assert!(!contact.jumped);
        assert!(state.on_ground);
        assert!(state.velocity.y > 0.0);
        let reflected = state.velocity.y;
        let released = state
            .tick(MovementInput::default(), &chunks, geometry(), &profile)
            .unwrap();
        assert!(released.grounded_at_start);
        assert!(!released.jumped);
        assert_close(released.applied.y, reflected);
    }

    #[test]
    fn standing_on_slime_uses_the_ordinary_grounded_jump() {
        let (chunks, mut profile) = special_world(BlockEnvironment::default(), None);
        profile.set_environment(BlockEnvironmentProfile::synthetic([(
            RuntimeBlockStateId(1),
            BlockEnvironment {
                surface: SpecialSurface::Slime,
                ..BlockEnvironment::default()
            },
        )]));
        let mut state = PlayerMovementState::from_authoritative(
            LocalPlayerPose::new(8.5, 1.0, 8.5, 0.0, 0.0),
            Vec3d::default(),
        )
        .unwrap();
        state.on_ground = true;
        let result = state
            .tick(
                MovementInput {
                    jump: true,
                    ..MovementInput::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert!(result.jumped);
        assert_close(result.applied.y, JUMP_VELOCITY);
    }

    #[test]
    fn honey_support_reduces_input_and_reduces_jump() {
        let (chunks, profile) = special_world(
            BlockEnvironment {
                surface: SpecialSurface::Honey,
                ..BlockEnvironment::default()
            },
            None,
        );
        let mut state = PlayerMovementState::from_authoritative(
            LocalPlayerPose::new(8.5, 1.0, 8.5, 0.0, 0.0),
            Vec3d::default(),
        )
        .unwrap();
        state.on_ground = true;
        let result = state
            .tick(
                MovementInput {
                    forward: true,
                    jump: true,
                    ..MovementInput::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert!(result.jumped);
        assert_close(result.applied.y, JUMP_VELOCITY * 0.5);
        assert!(state.velocity.z > 0.0);
        assert!(state.velocity.z < BASE_MOVEMENT_SPEED);
    }

    #[test]
    fn creative_flight_ascends_descends_and_normalizes_horizontal_input() {
        let (chunks, profile) = flat_world();
        let mut ascending = player();
        ascending.pose.y = 5.0;
        ascending.apply_flight_abilities(true, true, 0.05).unwrap();
        ascending
            .tick(
                MovementInput {
                    forward: true,
                    right: true,
                    jump: true,
                    ..Default::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert!(ascending.flying);
        assert!((ascending.velocity.y - 0.09).abs() < 1.0e-7);
        let diagonal_speed = ascending.velocity.x.hypot(ascending.velocity.z);

        let mut straight = player();
        straight.pose.y = 5.0;
        straight.apply_flight_abilities(true, true, 0.05).unwrap();
        straight
            .tick(
                MovementInput {
                    forward: true,
                    ..Default::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert!((diagonal_speed - straight.velocity.x.hypot(straight.velocity.z)).abs() < 1.0e-9);

        let mut descending = player();
        descending.pose.y = 5.0;
        descending.apply_flight_abilities(true, true, 0.05).unwrap();
        descending
            .tick(
                MovementInput {
                    sneak: true,
                    ..Default::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert!((descending.velocity.y + 0.09).abs() < 1.0e-7);
        assert_eq!(descending.pose_kind, PlayerPoseKind::Standing);

        let mut cancelled = player();
        cancelled.pose.y = 5.0;
        cancelled.apply_flight_abilities(true, true, 0.05).unwrap();
        cancelled
            .tick(
                MovementInput {
                    jump: true,
                    sneak: true,
                    ..Default::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert_eq!(cancelled.velocity.y, 0.0);
    }

    #[test]
    fn sprint_flight_is_twice_as_fast_and_server_revocation_wins() {
        let (chunks, profile) = flat_world();
        let fly = |sprint| {
            let mut state = player();
            state.pose.y = 5.0;
            state.apply_flight_abilities(true, true, 0.05).unwrap();
            state
                .tick(
                    MovementInput {
                        forward: true,
                        sprint,
                        ..Default::default()
                    },
                    &chunks,
                    geometry(),
                    &profile,
                )
                .unwrap();
            state.velocity.x.hypot(state.velocity.z)
        };
        assert!((fly(true) / fly(false) - 2.0).abs() < 1.0e-9);

        let mut state = player();
        state.apply_flight_abilities(true, true, 0.05).unwrap();
        state.apply_flight_abilities(false, true, 0.05).unwrap();
        assert!(!state.may_fly && !state.flying);
        assert!(!state.set_flying(true));
    }

    #[test]
    fn creative_flight_collides_lands_and_returns_to_gravity() {
        let (chunks, profile) = flat_world();
        let mut state = player();
        state.pose.y = 1.2;
        state.velocity.y = -0.4;
        state.apply_flight_abilities(true, true, 0.05).unwrap();
        let result = state
            .tick(MovementInput::default(), &chunks, geometry(), &profile)
            .unwrap();
        assert_eq!(state.pose.y, 1.0);
        assert!(state.on_ground);
        assert!(!state.flying);
        assert_eq!(result.flight_changed, Some(false));

        state.pose.y = 4.0;
        state.on_ground = false;
        state.apply_flight_abilities(true, true, 0.05).unwrap();
        assert!(state.set_flying(false));
        state
            .tick(MovementInput::default(), &chunks, geometry(), &profile)
            .unwrap();
        assert!(
            state.velocity.y < 0.0,
            "gravity must resume after flight is disabled"
        );

        let (obstacles, collision_profile) = obstacle_world(RuntimeBlockStateId(1), false);
        let mut flying = PlayerMovementState::from_authoritative(
            LocalPlayerPose::new(8.5, 1.0, 8.5, -90.0, 0.0),
            Vec3d::default(),
        )
        .unwrap();
        flying.apply_flight_abilities(true, true, 0.05).unwrap();
        for _ in 0..20 {
            flying
                .tick(
                    MovementInput {
                        forward: true,
                        ..Default::default()
                    },
                    &obstacles,
                    geometry(),
                    &collision_profile,
                )
                .unwrap();
        }
        assert!(flying.horizontal_collision);
        assert!(flying.pose.x <= 8.7 + 1.0e-7);

        flying
            .reconcile(
                LocalPlayerPose::new(7.0, 4.0, 7.0, 0.0, 0.0),
                Vec3d::new(0.0, 0.1, 0.0),
            )
            .unwrap();
        assert!(flying.may_fly && flying.flying);
    }

    #[test]
    fn sneak_edge_prevention_reduces_motion_but_keeps_supported_motion() {
        let (mut chunks, profile) = flat_world();
        for z in 0..16 {
            for x in 9..16 {
                let _ = chunks.update_block(
                    ChunkCoordinate::new(0, 0),
                    0,
                    x,
                    0,
                    z,
                    RuntimeBlockStateId(0),
                );
            }
        }

        let edge = Aabb::new(Vec3d::new(8.9, 1.0, 7.7), Vec3d::new(9.5, 2.8, 8.3));
        let adjusted = back_off_from_edge(
            edge,
            Vec3d::new(0.2, -COLLISION_EPSILON, 0.0),
            &chunks,
            geometry(),
            &profile,
        )
        .unwrap();
        assert!(adjusted.x <= 0.05 + COLLISION_EPSILON);

        let supported = Aabb::new(Vec3d::new(8.1, 1.0, 7.7), Vec3d::new(8.7, 2.8, 8.3));
        let unchanged = back_off_from_edge(
            supported,
            Vec3d::new(0.2, -COLLISION_EPSILON, 0.0),
            &chunks,
            geometry(),
            &profile,
        )
        .unwrap();
        assert_close(unchanged.x, 0.2);

        let ordinary = collide(
            edge,
            Vec3d::new(0.2, -COLLISION_EPSILON, 0.0),
            &chunks,
            geometry(),
            &profile,
            true,
        )
        .unwrap()
        .0;
        assert_close(ordinary.x, 0.2);
    }

    #[test]
    fn sneak_edge_prevention_handles_diagonal_and_negative_boundaries() {
        let (mut chunks, profile) = flat_world();
        for z in 9..16 {
            for x in 0..16 {
                let _ = chunks.update_block(
                    ChunkCoordinate::new(0, 0),
                    0,
                    x,
                    0,
                    z,
                    RuntimeBlockStateId(0),
                );
            }
        }
        for z in 0..16 {
            for x in 9..16 {
                let _ = chunks.update_block(
                    ChunkCoordinate::new(0, 0),
                    0,
                    x,
                    0,
                    z,
                    RuntimeBlockStateId(0),
                );
            }
        }
        let corner = Aabb::new(Vec3d::new(8.9, 1.0, 8.9), Vec3d::new(9.5, 2.8, 9.5));
        let adjusted = back_off_from_edge(
            corner,
            Vec3d::new(0.2, -COLLISION_EPSILON, 0.2),
            &chunks,
            geometry(),
            &profile,
        )
        .unwrap();
        assert!(adjusted.x <= 0.05 + COLLISION_EPSILON);
        assert!(adjusted.z <= 0.05 + COLLISION_EPSILON);

        let negative_bounds = Aabb::new(Vec3d::new(0.3, 1.0, 0.2), Vec3d::new(0.9, 2.8, 0.8));
        let obstacle = Aabb::new(Vec3d::new(-1.0, 0.0, 0.0), Vec3d::new(0.0, 3.0, 1.0));
        assert_close(resolve_x(negative_bounds, -0.8, &[obstacle]), -0.3);
    }

    #[test]
    fn sneak_edge_remains_active_during_a_partial_height_descent() {
        let height = 7.0 / 8.0;
        let partial = boxes(&[Aabb::new(
            Vec3d::new(0.0, 0.0, 0.0),
            Vec3d::new(1.0, height, 1.0),
        )]);
        let (chunks, profile) = partial_height_edge_world(partial);

        // This is the exact state after leaving the full block for the lower
        // support: no longer onGround, but only 1/8 block into a fall whose
        // destination remains within the player's 0.6-block step height.
        let make_player = || {
            descending_edge_player(
                9.2,
                1.0 + height,
                8.0,
                1.0 - height,
                Vec3d::new(0.2, 0.0, 0.0),
            )
        };
        let mut sneaking = make_player();
        let protected = sneaking
            .tick(
                MovementInput {
                    sneak: true,
                    ..Default::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert!(protected.applied.x <= 0.05 + COLLISION_EPSILON);
        assert!(sneaking.pose.x < 9.3);

        let mut released = make_player();
        let ordinary = released
            .tick(MovementInput::default(), &chunks, geometry(), &profile)
            .unwrap();
        assert_close(ordinary.applied.x, 0.2);
        assert!(released.pose.x > sneaking.pose.x + 0.1);

        // Reversing the transition still uses the ordinary generic step-up
        // candidate; edge protection does not turn the lower surface into a
        // one-way ledge.
        let reverse = Aabb::new(
            Vec3d::new(8.0, 1.0 + height, 7.7),
            Vec3d::new(8.6, 1.0 + height + 1.8, 8.3),
        );
        let (applied, stepped) = collide(
            reverse,
            Vec3d::new(-0.5, -COLLISION_EPSILON, 0.0),
            &chunks,
            geometry(),
            &profile,
            true,
        )
        .unwrap();
        assert!(stepped);
        assert_close(applied.x, -0.5);
        assert_close(applied.y, 1.0 - height);
    }

    #[test]
    fn snow_layer_descents_use_actual_heights_and_remaining_step_distance() {
        for layers in [7_u8, 8] {
            let shape = classify_shape(
                "snow",
                &BTreeMap::from([("layers".to_owned(), layers.to_string())]),
            );
            let height = f64::from(layers - 1) / 8.0;
            let (chunks, profile) = partial_height_edge_world(shape);
            let mut state = descending_edge_player(
                9.2,
                1.0 + height,
                8.0,
                1.0 - height,
                Vec3d::new(0.2, 0.0, 0.0),
            );
            let result = state
                .tick(
                    MovementInput {
                        sneak: true,
                        ..Default::default()
                    },
                    &chunks,
                    geometry(),
                    &profile,
                )
                .unwrap();
            assert!(
                result.applied.x <= 0.05 + COLLISION_EPSILON,
                "snow layers={layers}"
            );
        }

        // Three collision layers are 5/8 below a full block, just beyond the
        // 0.6-block step height. Vanilla no longer treats that as staying on
        // the ground surface, so it must not invent edge protection there.
        let layers = 4_u8;
        let shape = classify_shape(
            "snow",
            &BTreeMap::from([("layers".to_owned(), layers.to_string())]),
        );
        let height = f64::from(layers - 1) / 8.0;
        let (chunks, profile) = partial_height_edge_world(shape);
        let mut state = descending_edge_player(
            9.2,
            1.0 + height,
            8.0,
            1.0 - height,
            Vec3d::new(0.2, 0.0, 0.0),
        );
        let result = state
            .tick(
                MovementInput {
                    sneak: true,
                    ..Default::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert_close(result.applied.x, 0.2);
    }

    #[test]
    fn descending_sneak_edge_handles_diagonal_corner_and_negative_coordinates() {
        let height = 0.75;
        let shape = boxes(&[Aabb::new(
            Vec3d::new(0.0, 0.0, 0.0),
            Vec3d::new(1.0, height, 1.0),
        )]);
        let (chunks, profile) =
            single_shape_world(-8, 0, -8, RuntimeBlockStateId(9), shape.clone());
        let mut negative =
            descending_edge_player(-6.8, height, -7.5, 1.0 - height, Vec3d::new(0.2, 0.0, 0.0));
        let result = negative
            .tick(
                MovementInput {
                    sneak: true,
                    ..Default::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert!(result.applied.x <= 0.05 + COLLISION_EPSILON);

        let (chunks, profile) = single_shape_world(8, 0, 8, RuntimeBlockStateId(9), shape);
        let mut diagonal =
            descending_edge_player(9.2, height, 9.2, 1.0 - height, Vec3d::new(0.2, 0.0, 0.2));
        let result = diagonal
            .tick(
                MovementInput {
                    sneak: true,
                    ..Default::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        assert!(result.applied.x <= 0.05 + COLLISION_EPSILON);
        assert!(result.applied.z <= 0.05 + COLLISION_EPSILON);
    }

    #[test]
    fn multiple_small_descents_preserve_edge_protection_until_step_height_is_exceeded() {
        let cases = [
            (
                "path",
                15.0 / 16.0,
                classify_shape("dirt_path", &BTreeMap::new()),
            ),
            (
                "synthetic",
                0.75,
                boxes(&[Aabb::new(
                    Vec3d::new(0.0, 0.0, 0.0),
                    Vec3d::new(1.0, 0.75, 1.0),
                )]),
            ),
            (
                "lower slab",
                0.5,
                classify_shape(
                    "stone_slab",
                    &BTreeMap::from([("type".to_owned(), "bottom".to_owned())]),
                ),
            ),
        ];
        for (name, height, shape) in cases {
            let fall_distance = 1.0 - height;
            let (chunks, profile) = partial_height_edge_world(shape);
            let mut state = descending_edge_player(
                9.2,
                1.0 + height,
                8.0,
                fall_distance,
                Vec3d::new(0.2, 0.0, 0.0),
            );
            assert!(
                !can_fall_at_least(
                    state.bounding_box(),
                    0.0,
                    0.0,
                    STEP_HEIGHT - fall_distance,
                    &chunks,
                    geometry(),
                    &profile,
                )
                .unwrap(),
                "current {name} support missing at height={height} fall_distance={fall_distance}"
            );
            let result = state
                .tick(
                    MovementInput {
                        sneak: true,
                        ..Default::default()
                    },
                    &chunks,
                    geometry(),
                    &profile,
                )
                .unwrap();
            assert!(
                result.applied.x <= 0.05 + COLLISION_EPSILON,
                "{name} height={height} fall_distance={fall_distance} applied={:?}",
                result.applied
            );
        }
    }

    #[test]
    fn partial_overhead_and_support_shapes_clip_only_their_real_boxes() {
        let player_bounds = Aabb::new(Vec3d::new(0.2, 0.0, 0.2), Vec3d::new(0.8, 1.8, 0.8));
        let high_ceiling = Aabb::new(Vec3d::new(0.0, 2.5, 0.0), Vec3d::new(1.0, 3.0, 1.0));
        assert_close(resolve_y(player_bounds, 1.0, &[high_ceiling]), 0.7);

        let falling = Aabb::new(Vec3d::new(0.2, 2.0, 0.2), Vec3d::new(0.8, 3.8, 0.8));
        let lower_slab = Aabb::new(Vec3d::new(0.0, 0.0, 0.0), Vec3d::new(1.0, 0.5, 1.0));
        assert_close(resolve_y(falling, -3.0, &[lower_slab]), -1.5);
        assert_close(resolve_y(falling, -3.0, &[]), -3.0);
    }

    #[test]
    fn fluid_exit_probe_uses_partial_shape_clearance_after_adjusted_travel() {
        let lower = classify_shape(
            "stone_slab",
            &BTreeMap::from([("type".to_owned(), "bottom".to_owned())]),
        );
        let upper = classify_shape(
            "stone_slab",
            &BTreeMap::from([("type".to_owned(), "top".to_owned())]),
        );
        let bounds = Aabb::new(Vec3d::new(8.4, 0.0, 8.2), Vec3d::new(9.0, 1.8, 8.8));
        let velocity = Vec3d::new(0.1, -0.005, 0.0);
        for (name, shape, expected) in [
            ("lower slab", lower, true),
            ("upper slab", upper, false),
            ("full cube", CollisionShape::FullCube, false),
        ] {
            let (chunks, profile) = single_shape_world(9, 0, 8, RuntimeBlockStateId(9), shape);
            assert_eq!(
                fluid_exit_clearance(bounds, velocity, 0.0, &chunks, geometry(), &profile,)
                    .unwrap(),
                expected,
                "{name}"
            );
        }
        let stair = classify_shape(
            "oak_stairs",
            &BTreeMap::from([
                ("facing".to_owned(), "east".to_owned()),
                ("half".to_owned(), "bottom".to_owned()),
                ("shape".to_owned(), "straight".to_owned()),
            ]),
        );
        let (chunks, profile) = single_shape_world(9, 0, 8, RuntimeBlockStateId(9), stair);
        assert!(
            fluid_exit_clearance(bounds, velocity, 0.0, &chunks, geometry(), &profile,).unwrap(),
            "the raised probe clears the lower half at the approached stair edge"
        );
    }

    #[test]
    fn fluid_exit_probe_requires_the_translated_player_volume_to_leave_liquid_cells() {
        let (chunks, profile) = special_world(
            BlockEnvironment::default(),
            Some(BlockEnvironment {
                fluid: Some(crate::FluidState {
                    kind: FluidKind::Water,
                    level: 0,
                    falling: false,
                }),
                ..BlockEnvironment::default()
            }),
        );
        let bounds = Aabb::new(Vec3d::new(8.2, 1.0, 8.2), Vec3d::new(8.8, 2.8, 8.8));
        assert!(
            !fluid_exit_clearance(
                bounds,
                Vec3d::new(0.0, -WATER_GRAVITY, 0.0),
                0.0,
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap(),
            "block-clear vertical space is not free while its translated AABB still covers water"
        );
        assert!(
            fluid_exit_clearance(
                bounds,
                Vec3d::new(0.0, 0.41, 0.0),
                0.0,
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap(),
            "deliberate upward motion can move the translated AABB beyond the water cell"
        );
    }

    #[test]
    fn fluid_exit_upright_water_ledge_preserves_forward_intent_without_inventing_jump() {
        let lower_slab = classify_shape(
            "stone_slab",
            &BTreeMap::from([("type".to_owned(), "bottom".to_owned())]),
        );
        let stair = classify_shape(
            "oak_stairs",
            &BTreeMap::from([
                ("facing".to_owned(), "east".to_owned()),
                ("half".to_owned(), "bottom".to_owned()),
                ("shape".to_owned(), "straight".to_owned()),
            ]),
        );

        for (name, shape, expected_impulse_tick, expected_left_water_tick) in [
            ("full block", CollisionShape::FullCube, Some(5), Some(7)),
            ("lower slab", lower_slab, None, Some(6)),
            ("stair", stair, Some(5), Some(7)),
        ] {
            let (chunks, profile) = fluid_ledge_world(shape.clone());
            let mut forward_only = upright_water_player_before_ledge();
            let mut forward_trace = Vec::new();
            for _ in 0..12 {
                let result = forward_only
                    .tick(
                        MovementInput {
                            forward: true,
                            ..MovementInput::default()
                        },
                        &chunks,
                        geometry(),
                        &profile,
                    )
                    .unwrap();
                forward_trace.push((
                    result.applied,
                    forward_only.pose,
                    forward_only.velocity,
                    forward_only.horizontal_collision,
                ));
                assert!(
                    (forward_only.velocity.y - 0.300_000_011_920_928_96).abs() > COLLISION_EPSILON,
                    "{name} applied an unsolicited fluid-exit impulse: {forward_trace:?}"
                );
            }

            let stopped_at_z = forward_only.pose.z;
            for _ in 0..4 {
                forward_only
                    .tick(MovementInput::default(), &chunks, geometry(), &profile)
                    .unwrap();
                assert!(
                    (forward_only.velocity.y - 0.300_000_011_920_928_96).abs() > COLLISION_EPSILON,
                    "{name} hopped after forward input stopped"
                );
            }
            if name == "full block" {
                assert_close(forward_only.pose.z, stopped_at_z);
            }

            let (chunks, profile) = fluid_ledge_world(shape);
            let mut deliberate_exit = upright_water_player_before_ledge();
            let mut exit_impulse_tick = None;
            let mut left_water_tick = None;
            let mut maximum_y = deliberate_exit.pose.y;
            for tick in 0..24 {
                deliberate_exit
                    .tick(
                        MovementInput {
                            forward: true,
                            jump: true,
                            ..MovementInput::default()
                        },
                        &chunks,
                        geometry(),
                        &profile,
                    )
                    .unwrap();
                maximum_y = maximum_y.max(deliberate_exit.pose.y);
                if (deliberate_exit.velocity.y - 0.300_000_011_920_928_96).abs()
                    <= COLLISION_EPSILON
                {
                    exit_impulse_tick.get_or_insert(tick);
                }
                if !deliberate_exit.in_water {
                    left_water_tick.get_or_insert(tick);
                }
            }
            assert!(
                exit_impulse_tick.is_some() || left_water_tick.is_some(),
                "{name} did not permit a deliberate forward-plus-jump exit: pose={:?} velocity={:?} maximum_y={maximum_y}",
                deliberate_exit.pose,
                deliberate_exit.velocity
            );
            assert_eq!(exit_impulse_tick, expected_impulse_tick, "{name}");
            assert_eq!(left_water_tick, expected_left_water_tick, "{name}");
            assert!(
                maximum_y > 1.25,
                "{name} deliberate exit made no vertical progress: maximum_y={maximum_y}"
            );
        }
    }

    #[test]
    fn source_cell_shapes_extending_above_one_block_are_collected_generically() {
        let tall_narrow = boxes(&[Aabb::new(
            Vec3d::new(6.0 / 16.0, 0.0, 6.0 / 16.0),
            Vec3d::new(10.0 / 16.0, 1.5, 10.0 / 16.0),
        )]);
        let (chunks, profile) = single_shape_world(8, 0, 8, RuntimeBlockStateId(9), tall_narrow);

        let upper_half = Aabb::new(Vec3d::new(7.6, 1.1, 8.4), Vec3d::new(8.2, 2.9, 9.0));
        let movement = collide(
            upper_half,
            Vec3d::new(0.5, 0.0, 0.0),
            &chunks,
            geometry(),
            &profile,
            false,
        )
        .unwrap()
        .0;
        assert_close(movement.x, 8.375 - 8.2);

        let beside = Aabb::new(Vec3d::new(7.6, 1.1, 8.7), Vec3d::new(8.2, 2.9, 9.3));
        let movement = collide(
            beside,
            Vec3d::new(0.5, 0.0, 0.0),
            &chunks,
            geometry(),
            &profile,
            false,
        )
        .unwrap()
        .0;
        assert_close(movement.x, 0.5);

        let falling = Aabb::new(Vec3d::new(8.4, 2.0, 8.4), Vec3d::new(9.0, 3.8, 9.0));
        let movement = collide(
            falling,
            Vec3d::new(0.0, -1.0, 0.0),
            &chunks,
            geometry(),
            &profile,
            false,
        )
        .unwrap()
        .0;
        assert_close(movement.y, -0.5);

        let supported = Aabb::new(Vec3d::new(8.4, 1.5, 8.4), Vec3d::new(9.0, 3.3, 9.0));
        let support = collide(
            supported,
            Vec3d::new(0.0, -0.1, 0.0),
            &chunks,
            geometry(),
            &profile,
            true,
        )
        .unwrap()
        .0;
        assert_close(support.y, 0.0);
        let leave_support = collide(
            supported,
            Vec3d::new(-0.5, 0.0, 0.0),
            &chunks,
            geometry(),
            &profile,
            true,
        )
        .unwrap()
        .0;
        assert_close(leave_support.x, -0.5);

        assert_eq!(source_cell_range(1.1, 2.9, 0.0, 1.5).0, 0);
        assert_eq!(source_cell_range(-0.5, 1.3, 0.0, 1.5).0, -2);
    }

    #[test]
    fn fence_geometry_blocks_upper_portion_without_becoming_a_full_cell() {
        let fence = classify_shape(
            "oak_fence",
            &BTreeMap::from([
                ("north".to_owned(), "true".to_owned()),
                ("east".to_owned(), "true".to_owned()),
                ("south".to_owned(), "false".to_owned()),
                ("west".to_owned(), "false".to_owned()),
            ]),
        );
        let (chunks, profile) = single_shape_world(-8, 0, -8, RuntimeBlockStateId(10), fence);
        let obstacles = collect_obstacles(
            Aabb::new(Vec3d::new(-8.7, 1.0, -8.7), Vec3d::new(-7.3, 2.9, -7.3)),
            &chunks,
            geometry(),
            &profile,
        )
        .unwrap();
        assert!(obstacles.iter().all(|bounds| bounds.max.y == 1.5));
        assert!(obstacles.iter().any(|bounds| {
            bounds.min.x == -7.375
                && bounds.max.x == -7.0
                && bounds.min.z == -7.625
                && bounds.max.z == -7.375
        }));

        let into_post = Aabb::new(Vec3d::new(-8.9, 1.01, -7.6), Vec3d::new(-8.3, 2.81, -7.0));
        let clipped = collide(
            into_post,
            Vec3d::new(0.8, 0.0, 0.0),
            &chunks,
            geometry(),
            &profile,
            false,
        )
        .unwrap()
        .0;
        assert!(clipped.x < 0.8);

        let parallel = collide(
            Aabb::new(
                Vec3d::new(-8.95, 1.01, -8.95),
                Vec3d::new(-8.65, 2.81, -8.65),
            ),
            Vec3d::new(0.0, 0.0, 0.5),
            &chunks,
            geometry(),
            &profile,
            false,
        )
        .unwrap()
        .0;
        assert_close(parallel.z, 0.5);
    }

    #[test]
    fn fence_gate_open_close_updates_replace_the_collision_immediately() {
        let closed = classify_shape(
            "oak_fence_gate",
            &BTreeMap::from([
                ("facing".to_owned(), "north".to_owned()),
                ("open".to_owned(), "false".to_owned()),
                ("in_wall".to_owned(), "true".to_owned()),
            ]),
        );
        let (mut chunks, mut profile) =
            single_shape_world(8, 0, 8, RuntimeBlockStateId(11), closed.clone());
        profile
            .states
            .insert(RuntimeBlockStateId(12), CollisionShape::Empty);
        profile.source_shape_bounds = collision_shape_envelope(profile.states.values(), 0.0);
        let player = Aabb::new(Vec3d::new(8.2, 1.05, 7.6), Vec3d::new(8.8, 2.85, 8.2));
        let blocked = collide(
            player,
            Vec3d::new(0.0, 0.0, 0.5),
            &chunks,
            geometry(),
            &profile,
            false,
        )
        .unwrap()
        .0;
        assert!(blocked.z < 0.5);

        assert!(chunks.update_block(
            ChunkCoordinate::new(0, 0),
            0,
            8,
            0,
            8,
            RuntimeBlockStateId(12),
        ));
        let open = collide(
            player,
            Vec3d::new(0.0, 0.0, 0.5),
            &chunks,
            geometry(),
            &profile,
            false,
        )
        .unwrap()
        .0;
        assert_close(open.z, 0.5);

        assert!(chunks.update_block(
            ChunkCoordinate::new(0, 0),
            0,
            8,
            0,
            8,
            RuntimeBlockStateId(11),
        ));
        let closed_again = collide(
            player,
            Vec3d::new(0.0, 0.0, 0.5),
            &chunks,
            geometry(),
            &profile,
            false,
        )
        .unwrap()
        .0;
        assert_eq!(closed_again, blocked);

        let east_west = classify_shape(
            "oak_fence_gate",
            &BTreeMap::from([
                ("facing".to_owned(), "east".to_owned()),
                ("open".to_owned(), "false".to_owned()),
            ]),
        );
        assert!(matches!(east_west, CollisionShape::Boxes(values) if values[0].max.y == 1.5));
    }

    #[test]
    fn releasing_sneak_restores_ordinary_edge_motion_on_the_next_tick() {
        let (mut chunks, profile) = flat_world();
        for z in 0..16 {
            for x in 9..16 {
                let _ = chunks.update_block(
                    ChunkCoordinate::new(0, 0),
                    0,
                    x,
                    0,
                    z,
                    RuntimeBlockStateId(0),
                );
            }
        }
        let make_player = || {
            let mut state = PlayerMovementState::from_authoritative(
                LocalPlayerPose::new(9.2, 1.0, 8.0, 0.0, 0.0),
                Vec3d::new(0.2, 0.0, 0.0),
            )
            .unwrap();
            state.on_ground = true;
            state
        };
        let mut sneaking = make_player();
        let protected = sneaking
            .tick(
                MovementInput {
                    sneak: true,
                    ..Default::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        let mut released = make_player();
        let ordinary = released
            .tick(MovementInput::default(), &chunks, geometry(), &profile)
            .unwrap();
        assert!(protected.applied.x <= 0.05 + COLLISION_EPSILON);
        assert!(ordinary.applied.x > protected.applied.x + 0.1);
    }

    #[test]
    fn stationary_sneak_release_at_a_full_block_edge_retains_support() {
        let (mut chunks, profile) = flat_world();
        for z in 0..16 {
            for x in 9..16 {
                let _ = chunks.update_block(
                    ChunkCoordinate::new(0, 0),
                    0,
                    x,
                    0,
                    z,
                    RuntimeBlockStateId(0),
                );
            }
        }
        let mut state = PlayerMovementState::from_authoritative(
            LocalPlayerPose::new(8.5, 1.0, 8.0, -90.0, 0.0),
            Vec3d::default(),
        )
        .unwrap();
        state.on_ground = true;
        for _ in 0..40 {
            state
                .tick(
                    MovementInput {
                        forward: true,
                        sneak: true,
                        ..Default::default()
                    },
                    &chunks,
                    geometry(),
                    &profile,
                )
                .unwrap();
        }
        state
            .tick(
                MovementInput {
                    sneak: true,
                    ..Default::default()
                },
                &chunks,
                geometry(),
                &profile,
            )
            .unwrap();
        let edge_pose = state.pose;
        let released = state
            .tick(MovementInput::default(), &chunks, geometry(), &profile)
            .unwrap();
        assert_close(released.applied.x, 0.0);
        assert_close(released.applied.z, 0.0);
        assert_close(state.pose.x, edge_pose.x);
        assert_close(state.pose.z, edge_pose.z);
        assert_close(state.pose.y, edge_pose.y);
        assert!(state.on_ground);

        for _ in 0..12 {
            state
                .tick(
                    MovementInput {
                        forward: true,
                        ..Default::default()
                    },
                    &chunks,
                    geometry(),
                    &profile,
                )
                .unwrap();
        }
        assert!(
            state.pose.y < edge_pose.y,
            "ordinary outward input should permit falling"
        );
    }

    #[test]
    fn zero_motion_ground_probe_preserves_corner_partial_and_negative_support() {
        let cases = [
            (
                8,
                8,
                CollisionShape::FullCube,
                LocalPlayerPose::new(9.299_999_8, 1.0, 9.299_999_8, 0.0, 0.0),
            ),
            (
                8,
                8,
                boxes(&[Aabb::new(
                    Vec3d::new(0.0, 0.0, 0.0),
                    Vec3d::new(1.0, 0.5, 1.0),
                )]),
                LocalPlayerPose::new(9.299_999_8, 0.5, 8.5, 0.0, 0.0),
            ),
            (
                -8,
                -8,
                CollisionShape::FullCube,
                LocalPlayerPose::new(-6.700_000_2, 1.0, -7.5, 0.0, 0.0),
            ),
        ];
        for (x, z, shape, pose) in cases {
            let (chunks, profile) = single_shape_world(x, 0, z, RuntimeBlockStateId(9), shape);
            let mut state =
                PlayerMovementState::from_authoritative(pose, Vec3d::default()).unwrap();
            state.on_ground = true;
            let before = state.pose;
            state
                .tick(MovementInput::default(), &chunks, geometry(), &profile)
                .unwrap();
            assert_eq!(state.pose, before);
            assert!(state.on_ground, "pose={pose:?}");
        }
    }
}
