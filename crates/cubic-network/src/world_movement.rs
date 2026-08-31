use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use cubic_protocol::bootstrap::v775::{self, PlayerCommandAction, PlayerInput, PlayerPosition};
use cubic_world::{
    AuthoritativeTransform, BlockCollisionProfile, ChunkCoordinate, LocalPlayerPose, MovementInput,
    PlayerMovementState, PlayerPositionUpdate, PlayerRotationUpdate, RelativeTransformFlags,
    RenderLookSample, SimulationError, Vec3d, WorldState,
};
use thiserror::Error;

const POSITION_EPSILON_SQUARED: f64 = 4.0e-8;
const FORCED_POSITION_INTERVAL: u8 = 20;
const MAX_INPUT_TRANSITIONS: usize = 64;
const SLOW_INPUT_PATH: Duration = Duration::from_millis(50);
const FLIGHT_TOGGLE_WINDOW: Duration = Duration::from_millis(350);

#[derive(Clone, Copy, Debug)]
struct InputTransition {
    sequence: u64,
    input: MovementInput,
    recorded_at: Instant,
    reset: bool,
}

#[derive(Debug, Default)]
struct ControlState {
    input: MovementInput,
    input_sequence: u64,
    transitions: VecDeque<InputTransition>,
    coalesced_transitions: u64,
    look: RenderLookSample,
    pending_yaw: f64,
    pending_pitch: f64,
    intended_yaw: Option<f32>,
    intended_pitch: Option<f32>,
    jump_changed_at: Option<Instant>,
}

#[derive(Debug, Default)]
struct ControlSample {
    input: MovementInput,
    transitions: Vec<InputTransition>,
    coalesced_transitions: u64,
    yaw_delta: f32,
    pitch_delta: f32,
    look: RenderLookSample,
    intended_yaw: Option<f32>,
    intended_pitch: Option<f32>,
    jump_changed_at: Option<Instant>,
}

/// UI-side movement endpoint. Held input and look deltas coalesce, while a
/// small bounded transition journal preserves press/release edges until the
/// 20 Hz simulation consumes them.
#[derive(Clone)]
pub struct WorldControlHandle(Arc<Mutex<ControlState>>);

/// Network/simulation-side endpoint for the coalesced World Mode controls.
pub struct WorldControlRunner(Arc<Mutex<ControlState>>);

impl WorldControlHandle {
    #[must_use]
    pub fn new() -> (Self, WorldControlRunner) {
        let state = Arc::new(Mutex::new(ControlState::default()));
        (Self(Arc::clone(&state)), WorldControlRunner(state))
    }

    pub fn set_input(&self, sequence: u64, input: MovementInput) {
        let started = Instant::now();
        if let Ok(mut state) = self.0.lock() {
            record_input_transition(&mut state, sequence, input, Instant::now(), false);
        }
        let elapsed = started.elapsed();
        if elapsed > SLOW_INPUT_PATH {
            tracing::warn!(target: "movement::latency", ?elapsed, "movement input mailbox update was slow");
        }
    }

    pub fn reset_input(&self, sequence: u64) {
        let started = Instant::now();
        if let Ok(mut state) = self.0.lock() {
            record_input_transition(
                &mut state,
                sequence,
                MovementInput::default(),
                Instant::now(),
                true,
            );
        }
        let elapsed = started.elapsed();
        if elapsed > SLOW_INPUT_PATH {
            tracing::warn!(target: "movement::latency", ?elapsed, "movement focus-release cleanup was slow");
        }
    }

    pub fn add_look_delta(&self, sequence: u64, yaw: f32, pitch: f32) {
        if !yaw.is_finite() || !pitch.is_finite() {
            return;
        }
        let started = Instant::now();
        if let Ok(mut state) = self.0.lock() {
            if sequence <= state.look.sequence {
                return;
            }
            state.look.sequence = sequence;
            state.look.yaw_total += f64::from(yaw);
            state.look.pitch_total += f64::from(pitch);
            state.pending_yaw += f64::from(yaw);
            state.pending_pitch += f64::from(pitch);
            if let (Some(intended_yaw), Some(intended_pitch)) =
                (state.intended_yaw, state.intended_pitch)
            {
                state.intended_yaw = Some((intended_yaw + yaw).rem_euclid(360.0));
                state.intended_pitch = Some((intended_pitch + pitch).clamp(-90.0, 90.0));
            }
        }
        let elapsed = started.elapsed();
        if elapsed > SLOW_INPUT_PATH {
            tracing::warn!(target: "movement::latency", ?elapsed, "movement look mailbox update was slow");
        }
    }

    pub fn clear(&self) {
        let started = Instant::now();
        if let Ok(mut state) = self.0.lock() {
            let sequence = state.input_sequence.saturating_add(1);
            record_input_transition(
                &mut state,
                sequence,
                MovementInput::default(),
                Instant::now(),
                true,
            );
        }
        let elapsed = started.elapsed();
        if elapsed > SLOW_INPUT_PATH {
            tracing::warn!(target: "movement::latency", ?elapsed, "movement focus-release cleanup was slow");
        }
    }
}

impl WorldControlRunner {
    fn take(&self) -> ControlSample {
        let Ok(mut state) = self.0.lock() else {
            return ControlSample::default();
        };
        ControlSample {
            input: state.input,
            transitions: state.transitions.drain(..).collect(),
            coalesced_transitions: std::mem::take(&mut state.coalesced_transitions),
            yaw_delta: std::mem::take(&mut state.pending_yaw) as f32,
            pitch_delta: std::mem::take(&mut state.pending_pitch) as f32,
            look: state.look,
            intended_yaw: state.intended_yaw,
            intended_pitch: state.intended_pitch,
            jump_changed_at: state.jump_changed_at,
        }
    }

    fn rebase_look(&self, yaw: f32, pitch: f32) -> RenderLookSample {
        let Ok(mut state) = self.0.lock() else {
            return RenderLookSample::default();
        };
        state.pending_yaw = 0.0;
        state.pending_pitch = 0.0;
        state.intended_yaw = Some(yaw.rem_euclid(360.0));
        state.intended_pitch = Some(pitch.clamp(-90.0, 90.0));
        state.look
    }

    fn clear_look_intent(&self) -> RenderLookSample {
        let Ok(mut state) = self.0.lock() else {
            return RenderLookSample::default();
        };
        state.pending_yaw = 0.0;
        state.pending_pitch = 0.0;
        state.intended_yaw = None;
        state.intended_pitch = None;
        state.look
    }
}

fn record_input_transition(
    state: &mut ControlState,
    sequence: u64,
    input: MovementInput,
    now: Instant,
    reset: bool,
) {
    if sequence <= state.input_sequence {
        tracing::debug!(target: "movement::input", sequence, current_sequence = state.input_sequence, "ignored stale movement input snapshot");
        return;
    }
    state.input_sequence = sequence;
    if state.input == input {
        return;
    }
    if state.input.jump != input.jump {
        state.jump_changed_at = Some(now);
    }
    state.input = input;
    if state.transitions.len() == MAX_INPUT_TRANSITIONS {
        state.transitions.pop_front();
        state.coalesced_transitions = state.coalesced_transitions.saturating_add(1);
    }
    state.transitions.push_back(InputTransition {
        sequence,
        input,
        recorded_at: now,
        reset,
    });
    tracing::trace!(
        target: "movement::input",
        sequence,
        journal_depth = state.transitions.len(),
        forward = input.forward,
        backward = input.backward,
        left = input.left,
        right = input.right,
        jump = input.jump,
        sneak = input.sneak,
        sprint = input.sprint,
        "recorded movement input transition in bounded journal"
    );
}

#[derive(Debug, Error)]
pub enum MovementError {
    #[error(transparent)]
    Simulation(#[from] SimulationError),
    #[error(transparent)]
    Protocol(#[from] v775::BootstrapProtocolError),
    #[error("relative server correction has no local {field} baseline")]
    MissingCorrectionBaseline { field: &'static str },
}

pub(crate) struct WorldMovementController {
    controls: WorldControlRunner,
    collisions: BlockCollisionProfile,
    simulation: Option<PlayerMovementState>,
    packets: MovementPacketTracker,
    entity_id: i32,
    correction_count: u64,
    rapid_correction_count: u64,
    approximate_collision_count: u64,
    last_correction_at: Option<Instant>,
    last_look: RenderLookSample,
    flight_toggle: FlightToggleTracker,
    abilities: Option<v775::PlayerAbilities>,
}

impl WorldMovementController {
    pub(crate) fn new(
        controls: WorldControlRunner,
        collisions: BlockCollisionProfile,
        entity_id: i32,
    ) -> Self {
        Self {
            controls,
            collisions,
            simulation: None,
            packets: MovementPacketTracker::default(),
            entity_id,
            correction_count: 0,
            rapid_correction_count: 0,
            approximate_collision_count: 0,
            last_correction_at: None,
            last_look: RenderLookSample::default(),
            flight_toggle: FlightToggleTracker::default(),
            abilities: None,
        }
    }

    pub(crate) fn reset(&mut self, entity_id: i32) {
        self.simulation = None;
        self.packets = MovementPacketTracker::default();
        self.entity_id = entity_id;
        self.correction_count = 0;
        self.rapid_correction_count = 0;
        self.approximate_collision_count = 0;
        self.last_correction_at = None;
        self.flight_toggle.reset();
        self.last_look = self.controls.clear_look_intent();
        if let Some(abilities) = &mut self.abilities {
            abilities.flying = false;
        }
    }

    pub(crate) fn tick(
        &mut self,
        world: &WorldState,
    ) -> Result<Option<MovementTick>, MovementError> {
        let controls = self.controls.take();
        let input_sampled_at = Instant::now();
        self.last_look = controls.look;
        if let Some(oldest) = controls.transitions.first() {
            let latency = oldest.recorded_at.elapsed();
            if latency > SLOW_INPUT_PATH || controls.coalesced_transitions != 0 {
                tracing::debug!(
                    target: "movement::latency",
                    ?latency,
                    transitions = controls.transitions.len(),
                    coalesced_transitions = controls.coalesced_transitions,
                    "movement tick consumed delayed input transitions"
                );
            }
        }
        for transition in &controls.transitions {
            tracing::trace!(
                target: "movement::input",
                sequence = transition.sequence,
                age = ?transition.recorded_at.elapsed(),
                forward = transition.input.forward,
                backward = transition.input.backward,
                left = transition.input.left,
                right = transition.input.right,
                jump = transition.input.jump,
                sneak = transition.input.sneak,
                sprint = transition.input.sprint,
                "movement controller consumed input transition"
            );
        }
        if let Some(last) = controls.transitions.last() {
            tracing::debug!(
                target: "movement::input",
                last_sequence = last.sequence,
                oldest_transition_age = ?controls.transitions.first().map(|transition| transition.recorded_at.elapsed()),
                newest_transition_age = ?last.recorded_at.elapsed(),
                forward = controls.input.forward,
                backward = controls.input.backward,
                left = controls.input.left,
                right = controls.input.right,
                jump = controls.input.jump,
                sneak = controls.input.sneak,
                sprint = controls.input.sprint,
                "fixed-tick physics selected final held input state"
            );
        }
        let Some(simulation) = &mut self.simulation else {
            return Ok(None);
        };
        let mut flight_changes = self.flight_toggle.consume(
            &controls.transitions,
            controls.coalesced_transitions,
            controls.input.jump,
            simulation,
        );
        if let (Some(yaw), Some(pitch)) = (controls.intended_yaw, controls.intended_pitch) {
            simulation.set_rotation(yaw, pitch)?;
        } else {
            simulation.rotate(controls.yaw_delta, controls.pitch_delta)?;
        }
        let session = world.session().ok_or(SimulationError::NonFinite {
            field: "missing active world session",
        })?;
        let player_chunk = ChunkCoordinate::new(
            (simulation.pose.x / 16.0).floor() as i32,
            (simulation.pose.z / 16.0).floor() as i32,
        );
        if world.loaded_chunks().get(player_chunk).is_none() {
            let frames = self.packets.frames_with_flight(
                simulation,
                &controls.transitions,
                controls.input,
                self.entity_id,
                &flight_changes,
            )?;
            return Ok(Some(MovementTick {
                pose: simulation.pose,
                velocity: simulation.velocity,
                on_ground: simulation.on_ground,
                horizontal_collision: simulation.horizontal_collision,
                sprinting: simulation.sprinting,
                sneaking: controls.input.sneak,
                flying: simulation.flying,
                look: controls.look,
                jumped: false,
                grounded_at_start: simulation.on_ground,
                horizontal_acceleration: 0.0,
                horizontal_drag: 1.0,
                sprint_jump_impulse: 0.0,
                input_sampled_at,
                jump_changed_at: controls.jump_changed_at,
                frames,
            }));
        }
        let result = simulation.tick(
            controls.input,
            world.loaded_chunks(),
            session.dimension_geometry,
            &self.collisions,
        )?;
        if let Some(flying) = result.flight_changed {
            flight_changes.push(flying);
        }
        if result.approximate_collision {
            self.approximate_collision_count = self.approximate_collision_count.saturating_add(1);
        }
        let frames = self.packets.frames_with_flight(
            simulation,
            &controls.transitions,
            controls.input,
            self.entity_id,
            &flight_changes,
        )?;
        Ok(Some(MovementTick {
            pose: simulation.pose,
            velocity: simulation.velocity,
            on_ground: simulation.on_ground,
            horizontal_collision: simulation.horizontal_collision,
            sprinting: simulation.sprinting,
            sneaking: controls.input.sneak,
            flying: simulation.flying,
            look: controls.look,
            jumped: result.jumped,
            grounded_at_start: result.grounded_at_start,
            horizontal_acceleration: result.horizontal_acceleration,
            horizontal_drag: result.horizontal_drag,
            sprint_jump_impulse: result.sprint_jump_impulse,
            input_sampled_at,
            jump_changed_at: controls.jump_changed_at,
            frames,
        }))
    }

    pub(crate) const fn presentation_look(&self) -> RenderLookSample {
        self.last_look
    }

    pub(crate) fn predicted_pose(&self) -> Option<LocalPlayerPose> {
        self.simulation.as_ref().map(|state| state.pose)
    }

    pub(crate) fn reconcile(
        &mut self,
        packet: PlayerPosition,
        authoritative: AuthoritativeTransform,
        world: &WorldState,
    ) -> Result<LocalPlayerPose, MovementError> {
        let old_velocity = self
            .simulation
            .as_ref()
            .map_or(Vec3d::default(), |state| state.velocity);
        let old_pose = self
            .simulation
            .as_ref()
            .map_or(LocalPlayerPose::new(0.0, 0.0, 0.0, 0.0, 0.0), |state| {
                state.pose
            });
        let velocity = correction_velocity(old_velocity, old_pose, packet, authoritative);
        let pose = LocalPlayerPose::new(
            authoritative.x,
            authoritative.y,
            authoritative.z,
            authoritative.yaw,
            authoritative.pitch,
        );
        self.correction_count = self.correction_count.saturating_add(1);
        let now = Instant::now();
        if self
            .last_correction_at
            .is_some_and(|previous| now.duration_since(previous) <= Duration::from_secs(1))
        {
            self.rapid_correction_count = self.rapid_correction_count.saturating_add(1);
        }
        self.last_correction_at = Some(now);
        let pre_bounds = self
            .simulation
            .as_ref()
            .map(PlayerMovementState::bounding_box);
        let pre_grounded = self
            .simulation
            .as_ref()
            .is_some_and(|state| state.on_ground);
        let pre_horizontal_collision = self
            .simulation
            .as_ref()
            .is_some_and(|state| state.horizontal_collision);
        let collision_diagnostics = self.simulation.as_ref().and_then(|state| {
            world.session().map(|session| {
                state.collision_diagnostics(
                    world.loaded_chunks(),
                    session.dimension_geometry,
                    &self.collisions,
                )
            })
        });
        let last_movement_family = self.packets.last_movement_family;
        let last_position_sent = self.packets.last_position_sent;
        if let Some(simulation) = &mut self.simulation {
            simulation.reconcile(pose, velocity)?;
        } else {
            let mut simulation = PlayerMovementState::from_authoritative(pose, velocity)?;
            if let Some(abilities) = self.abilities {
                simulation.apply_flight_abilities(
                    abilities.may_fly,
                    abilities.flying,
                    abilities.flying_speed,
                )?;
            }
            self.simulation = Some(simulation);
        }
        self.last_look = self.controls.rebase_look(pose.yaw, pose.pitch);
        self.packets.reset(pose);
        tracing::debug!(
            target: "movement",
            teleport_id = packet.teleport_id,
            relative_flags = format_args!("{:#05x}", packet.relative_flags),
            predicted_x = old_pose.x,
            predicted_y = old_pose.y,
            predicted_z = old_pose.z,
            authoritative_x = pose.x,
            authoritative_y = pose.y,
            authoritative_z = pose.z,
            delta_x = pose.x - old_pose.x,
            delta_y = pose.y - old_pose.y,
            delta_z = pose.z - old_pose.z,
            predicted_velocity_x = old_velocity.x,
            predicted_velocity_y = old_velocity.y,
            predicted_velocity_z = old_velocity.z,
            resulting_velocity_x = velocity.x,
            resulting_velocity_y = velocity.y,
            resulting_velocity_z = velocity.z,
            grounded = pre_grounded,
            horizontal_collision = pre_horizontal_collision,
            ?last_movement_family,
            ?last_position_sent,
            ?pre_bounds,
            ?collision_diagnostics,
            corrections_received = self.correction_count,
            rapid_corrections = self.rapid_correction_count,
            approximate_collision_ticks = self.approximate_collision_count,
            "reconciled local prediction to authoritative player position"
        );
        Ok(pose)
    }

    pub(crate) fn absolute_position_update(
        &self,
        packet: PlayerPosition,
        world: &WorldState,
    ) -> Result<PlayerPositionUpdate, MovementError> {
        let local = self.simulation.as_ref().map(|state| state.pose);
        let session = world.session();
        let authoritative = session.and_then(|session| session.position);
        let rotation = session.and_then(|session| session.rotation);
        let f64_component =
            |value: f64, relative: bool, local: Option<f64>, authoritative: Option<f64>, field| {
                if relative {
                    local
                        .or(authoritative)
                        .map(|base| base + value)
                        .ok_or(MovementError::MissingCorrectionBaseline { field })
                } else {
                    Ok(value)
                }
            };
        let f32_component =
            |value: f32, relative: bool, local: Option<f32>, authoritative: Option<f32>, field| {
                if relative {
                    local
                        .or(authoritative)
                        .map(|base| base + value)
                        .ok_or(MovementError::MissingCorrectionBaseline { field })
                } else {
                    Ok(value)
                }
            };
        Ok(PlayerPositionUpdate {
            teleport_id: packet.teleport_id,
            x: f64_component(
                packet.x,
                packet.relative_flags & 0x01 != 0,
                local.map(|pose| pose.x),
                authoritative.map(|pose| pose.x),
                "x",
            )?,
            y: f64_component(
                packet.y,
                packet.relative_flags & 0x02 != 0,
                local.map(|pose| pose.y),
                authoritative.map(|pose| pose.y),
                "y",
            )?,
            z: f64_component(
                packet.z,
                packet.relative_flags & 0x04 != 0,
                local.map(|pose| pose.z),
                authoritative.map(|pose| pose.z),
                "z",
            )?,
            yaw: f32_component(
                packet.yaw,
                packet.relative_flags & 0x08 != 0,
                local.map(|pose| pose.yaw),
                rotation.map(|value| value.yaw),
                "yaw",
            )?,
            pitch: f32_component(
                packet.pitch,
                packet.relative_flags & 0x10 != 0,
                local.map(|pose| pose.pitch),
                rotation.map(|value| value.pitch),
                "pitch",
            )?,
            relative: RelativeTransformFlags::default(),
        })
    }

    pub(crate) fn absolute_rotation_update(
        &self,
        yaw: f32,
        relative_yaw: bool,
        pitch: f32,
        relative_pitch: bool,
        world: &WorldState,
    ) -> Result<PlayerRotationUpdate, MovementError> {
        let local = self.simulation.as_ref().map(|state| state.pose);
        let authoritative = world.session().and_then(|session| session.rotation);
        let resolve =
            |value: f32, relative: bool, local: Option<f32>, prior: Option<f32>, field| {
                if relative {
                    local
                        .or(prior)
                        .map(|base| base + value)
                        .ok_or(MovementError::MissingCorrectionBaseline { field })
                } else {
                    Ok(value)
                }
            };
        Ok(PlayerRotationUpdate {
            yaw: resolve(
                yaw,
                relative_yaw,
                local.map(|pose| pose.yaw),
                authoritative.map(|value| value.yaw),
                "yaw",
            )?,
            pitch: resolve(
                pitch,
                relative_pitch,
                local.map(|pose| pose.pitch),
                authoritative.map(|value| value.pitch),
                "pitch",
            )?,
            relative_yaw: false,
            relative_pitch: false,
        })
    }

    pub(crate) fn rotate(
        &mut self,
        yaw: f32,
        relative_yaw: bool,
        pitch: f32,
        relative_pitch: bool,
    ) -> Option<(LocalPlayerPose, RenderLookSample)> {
        let Some(simulation) = &mut self.simulation else {
            return None;
        };
        simulation.pose.yaw = if relative_yaw {
            simulation.pose.yaw + yaw
        } else {
            yaw
        }
        .rem_euclid(360.0);
        simulation.pose.pitch = if relative_pitch {
            simulation.pose.pitch + pitch
        } else {
            pitch
        }
        .clamp(-90.0, 90.0);
        self.last_look = self
            .controls
            .rebase_look(simulation.pose.yaw, simulation.pose.pitch);
        self.packets.reset(simulation.pose);
        Some((simulation.pose, self.last_look))
    }

    pub(crate) fn apply_velocity(&mut self, entity_id: i32, velocity: Vec3d) -> bool {
        if entity_id != self.entity_id || !velocity.is_finite() {
            return false;
        }
        let Some(simulation) = &mut self.simulation else {
            return false;
        };
        simulation.velocity = velocity;
        tracing::debug!(
            target: "movement",
            entity_id,
            x = velocity.x,
            y = velocity.y,
            z = velocity.z,
            "applied authoritative player velocity"
        );
        true
    }

    pub(crate) fn apply_abilities(
        &mut self,
        abilities: v775::PlayerAbilities,
    ) -> Result<(), MovementError> {
        self.abilities = Some(abilities);
        self.flight_toggle.retain_only_if_capable(abilities.may_fly);
        if let Some(simulation) = &mut self.simulation {
            simulation.apply_flight_abilities(
                abilities.may_fly,
                abilities.flying,
                abilities.flying_speed,
            )?;
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
struct FlightToggleTracker {
    first_press_at: Option<Instant>,
    jump_held: bool,
}

impl FlightToggleTracker {
    fn reset(&mut self) {
        self.first_press_at = None;
        self.jump_held = false;
    }

    fn retain_only_if_capable(&mut self, may_fly: bool) {
        if !may_fly {
            self.reset();
        }
    }

    fn consume(
        &mut self,
        transitions: &[InputTransition],
        coalesced: u64,
        final_jump: bool,
        state: &mut PlayerMovementState,
    ) -> Vec<bool> {
        if coalesced != 0 {
            self.first_press_at = None;
            self.jump_held = final_jump;
            return Vec::new();
        }
        let mut changes = Vec::new();
        for transition in transitions {
            if transition.reset {
                self.reset();
                continue;
            }
            if transition.input.jump && !self.jump_held && state.may_fly {
                let completes_toggle = self.first_press_at.is_some_and(|first| {
                    transition.recorded_at.saturating_duration_since(first) <= FLIGHT_TOGGLE_WINDOW
                });
                if completes_toggle {
                    self.first_press_at = None;
                    if state.set_flying(!state.flying) {
                        changes.push(state.flying);
                    }
                } else {
                    self.first_press_at = Some(transition.recorded_at);
                }
            }
            self.jump_held = transition.input.jump;
        }
        if self
            .first_press_at
            .is_some_and(|first| first.elapsed() > FLIGHT_TOGGLE_WINDOW)
        {
            self.first_press_at = None;
        }
        self.jump_held = final_jump;
        changes
    }
}

pub(crate) struct MovementTick {
    pub(crate) pose: LocalPlayerPose,
    pub(crate) velocity: Vec3d,
    pub(crate) on_ground: bool,
    pub(crate) horizontal_collision: bool,
    pub(crate) sprinting: bool,
    pub(crate) sneaking: bool,
    pub(crate) flying: bool,
    pub(crate) look: RenderLookSample,
    pub(crate) jumped: bool,
    pub(crate) grounded_at_start: bool,
    pub(crate) horizontal_acceleration: f64,
    pub(crate) horizontal_drag: f64,
    pub(crate) sprint_jump_impulse: f64,
    pub(crate) input_sampled_at: Instant,
    pub(crate) jump_changed_at: Option<Instant>,
    pub(crate) frames: Vec<Vec<u8>>,
}

#[derive(Default)]
struct MovementPacketTracker {
    last_pose: Option<LocalPlayerPose>,
    last_on_ground: bool,
    last_horizontal_collision: bool,
    last_input: PlayerInput,
    sprinting: bool,
    position_reminder: u8,
    last_movement_family: Option<MovementPacketFamily>,
    last_position_sent: Option<Vec3d>,
    diagnose_next_movement: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MovementPacketFamily {
    Position,
    PositionRotation,
    Rotation,
    StatusOnly,
}

impl MovementPacketTracker {
    fn reset(&mut self, pose: LocalPlayerPose) {
        self.last_pose = Some(pose);
        self.position_reminder = 0;
        self.last_movement_family = None;
        self.last_position_sent = None;
        self.diagnose_next_movement = true;
    }

    fn frames_with_flight(
        &mut self,
        state: &PlayerMovementState,
        transitions: &[InputTransition],
        input: MovementInput,
        entity_id: i32,
        flight_changes: &[bool],
    ) -> Result<Vec<Vec<u8>>, v775::BootstrapProtocolError> {
        let to_packet = |input: MovementInput| PlayerInput {
            forward: input.forward,
            backward: input.backward,
            left: input.left,
            right: input.right,
            jump: input.jump,
            sneak: input.sneak,
            sprint: input.sprint,
        };
        let mut result =
            Vec::with_capacity(transitions.len().saturating_add(flight_changes.len() + 3));
        for transition in transitions {
            self.push_input_transition(&mut result, to_packet(transition.input))?;
        }
        self.push_input_transition(&mut result, to_packet(input))?;
        for flying in flight_changes {
            result.push(v775::encode_play_player_abilities(*flying)?);
        }
        if state.sprinting != self.sprinting {
            tracing::debug!(
                target: "movement",
                sprinting = state.sprinting,
                "changed transmitted sprint state"
            );
            result.push(v775::encode_play_player_command(
                entity_id,
                if state.sprinting {
                    PlayerCommandAction::StartSprinting
                } else {
                    PlayerCommandAction::StopSprinting
                },
            )?);
            self.sprinting = state.sprinting;
        }

        let Some(last) = self.last_pose else {
            self.reset(state.pose);
            return Ok(result);
        };
        self.position_reminder = self.position_reminder.saturating_add(1);
        let dx = state.pose.x - last.x;
        let dy = state.pose.y - last.y;
        let dz = state.pose.z - last.z;
        let position_changed = dx.mul_add(dx, dy.mul_add(dy, dz * dz)) > POSITION_EPSILON_SQUARED
            || self.position_reminder >= FORCED_POSITION_INTERVAL;
        let rotation_changed = state.pose.yaw != last.yaw || state.pose.pitch != last.pitch;
        let flags_changed = state.on_ground != self.last_on_ground
            || state.horizontal_collision != self.last_horizontal_collision;
        let movement = match (position_changed, rotation_changed) {
            (true, true) => Some((
                MovementPacketFamily::PositionRotation,
                v775::encode_play_move_position_rotation(
                    state.pose.x,
                    state.pose.y,
                    state.pose.z,
                    state.pose.yaw,
                    state.pose.pitch,
                    state.on_ground,
                    state.horizontal_collision,
                )?,
            )),
            (true, false) => Some((
                MovementPacketFamily::Position,
                v775::encode_play_move_position(
                    state.pose.x,
                    state.pose.y,
                    state.pose.z,
                    state.on_ground,
                    state.horizontal_collision,
                )?,
            )),
            (false, true) => Some((
                MovementPacketFamily::Rotation,
                v775::encode_play_move_rotation(
                    state.pose.yaw,
                    state.pose.pitch,
                    state.on_ground,
                    state.horizontal_collision,
                )?,
            )),
            (false, false) if flags_changed => Some((
                MovementPacketFamily::StatusOnly,
                v775::encode_play_move_status(state.on_ground, state.horizontal_collision)?,
            )),
            (false, false) => None,
        };
        if let Some((family, movement)) = movement {
            self.last_movement_family = Some(family);
            if position_changed {
                self.last_position_sent =
                    Some(Vec3d::new(state.pose.x, state.pose.y, state.pose.z));
            }
            if self.diagnose_next_movement {
                tracing::debug!(
                    target: "movement",
                    ?family,
                    x = state.pose.x,
                    y = state.pose.y,
                    z = state.pose.z,
                    yaw = state.pose.yaw,
                    pitch = state.pose.pitch,
                    on_ground = state.on_ground,
                    horizontal_collision = state.horizontal_collision,
                    "sent first movement packet after authoritative correction"
                );
                self.diagnose_next_movement = false;
            }
            result.push(movement);
        }
        if position_changed {
            self.last_pose = Some(LocalPlayerPose {
                x: state.pose.x,
                y: state.pose.y,
                z: state.pose.z,
                ..last
            });
            self.position_reminder = 0;
        }
        if rotation_changed {
            let mut updated = self.last_pose.unwrap_or(last);
            updated.yaw = state.pose.yaw;
            updated.pitch = state.pose.pitch;
            self.last_pose = Some(updated);
        }
        self.last_on_ground = state.on_ground;
        self.last_horizontal_collision = state.horizontal_collision;
        Ok(result)
    }

    #[cfg(test)]
    fn frames(
        &mut self,
        state: &PlayerMovementState,
        transitions: &[InputTransition],
        input: MovementInput,
        entity_id: i32,
    ) -> Result<Vec<Vec<u8>>, v775::BootstrapProtocolError> {
        self.frames_with_flight(state, transitions, input, entity_id, &[])
    }

    fn push_input_transition(
        &mut self,
        result: &mut Vec<Vec<u8>>,
        input_packet: PlayerInput,
    ) -> Result<(), v775::BootstrapProtocolError> {
        if input_packet != self.last_input {
            if input_packet.sneak != self.last_input.sneak {
                tracing::debug!(
                    target: "movement",
                    sneaking = input_packet.sneak,
                    "changed transmitted sneak input"
                );
            }
            result.push(v775::encode_play_player_input(input_packet)?);
            self.last_input = input_packet;
        }
        Ok(())
    }
}

fn correction_velocity(
    velocity: Vec3d,
    old_pose: LocalPlayerPose,
    packet: PlayerPosition,
    authoritative: AuthoritativeTransform,
) -> Vec3d {
    let rotated = if packet.relative_flags & 0x100 != 0 {
        let pitch = f64::from(old_pose.pitch - authoritative.pitch).to_radians();
        let yaw = f64::from(old_pose.yaw - authoritative.yaw).to_radians();
        let (sin_pitch, cos_pitch) = pitch.sin_cos();
        let x_rotated = Vec3d::new(
            velocity.x,
            velocity.y * cos_pitch + velocity.z * sin_pitch,
            velocity.z * cos_pitch - velocity.y * sin_pitch,
        );
        let (sin_yaw, cos_yaw) = yaw.sin_cos();
        Vec3d::new(
            x_rotated.x * cos_yaw + x_rotated.z * sin_yaw,
            x_rotated.y,
            x_rotated.z * cos_yaw - x_rotated.x * sin_yaw,
        )
    } else {
        velocity
    };
    Vec3d::new(
        if packet.relative_flags & 0x20 != 0 {
            rotated.x + packet.delta_x
        } else {
            packet.delta_x
        },
        if packet.relative_flags & 0x40 != 0 {
            rotated.y + packet.delta_y
        } else {
            packet.delta_y
        },
        if packet.relative_flags & 0x80 != 0 {
            rotated.z + packet.delta_z
        } else {
            packet.delta_z
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use cubic_protocol::{FrameDecoder, FrameLimits, split_raw_packet};

    fn packet_ids(frames: &[Vec<u8>]) -> Vec<i32> {
        frames
            .iter()
            .map(|frame| {
                let mut decoder =
                    FrameDecoder::new(FrameLimits::new(2_097_151, 4 * 1024 * 1024).unwrap());
                decoder.push(frame).unwrap();
                let body = decoder.next_frame().unwrap().unwrap();
                split_raw_packet(&body).unwrap().id
            })
            .collect()
    }

    fn test_state() -> PlayerMovementState {
        PlayerMovementState::from_authoritative(
            LocalPlayerPose::new(1.0, 64.0, 2.0, 0.0, 0.0),
            Vec3d::default(),
        )
        .unwrap()
    }

    #[test]
    fn control_mailbox_preserves_edges_and_clear_releases_all_input() {
        let (handle, runner) = WorldControlHandle::new();
        handle.set_input(
            1,
            MovementInput {
                forward: true,
                sprint: true,
                ..MovementInput::default()
            },
        );
        handle.add_look_delta(1, 2.0, 3.0);
        handle.add_look_delta(2, 4.0, -1.0);
        let value = runner.take();
        assert!(value.input.forward && value.input.sprint);
        assert_eq!(value.transitions.len(), 1);
        assert_eq!(value.transitions[0].input, value.input);
        assert!(!value.transitions[0].reset);
        assert_eq!((value.yaw_delta, value.pitch_delta), (6.0, 2.0));
        assert_eq!(runner.take().yaw_delta, 0.0);
        handle.clear();
        let cleared = runner.take();
        assert_eq!(cleared.input, MovementInput::default());
        assert_eq!(cleared.transitions.len(), 1);
        assert_eq!(cleared.transitions[0].input, MovementInput::default());
        assert!(cleared.transitions[0].reset);
    }

    #[test]
    fn press_and_release_survive_a_delayed_consumer_in_order() {
        let (handle, runner) = WorldControlHandle::new();
        let pressed = MovementInput {
            forward: true,
            ..MovementInput::default()
        };
        handle.set_input(1, pressed);
        handle.set_input(2, MovementInput::default());
        std::thread::sleep(Duration::from_millis(60));

        let sample = runner.take();
        assert_eq!(sample.input, MovementInput::default());
        assert_eq!(
            sample
                .transitions
                .iter()
                .map(|transition| transition.input)
                .collect::<Vec<_>>(),
            vec![pressed, MovementInput::default()]
        );
        assert!(sample.transitions[0].recorded_at.elapsed() >= Duration::from_millis(50));
    }

    #[test]
    fn held_space_is_sampled_as_level_state_until_release() {
        let (handle, runner) = WorldControlHandle::new();
        let held = MovementInput {
            jump: true,
            ..MovementInput::default()
        };
        handle.set_input(1, held);

        let first_tick = runner.take();
        assert!(first_tick.input.jump);
        assert_eq!(first_tick.transitions.len(), 1);

        let airborne_tick = runner.take();
        assert!(airborne_tick.input.jump);
        assert!(airborne_tick.transitions.is_empty());

        handle.set_input(2, MovementInput::default());
        let released_before_landing_tick = runner.take();
        assert!(!released_before_landing_tick.input.jump);
        assert_eq!(released_before_landing_tick.transitions.len(), 1);
    }

    #[test]
    fn held_state_survives_delay_and_transition_journal_is_bounded() {
        let (handle, runner) = WorldControlHandle::new();
        for index in 0..(MAX_INPUT_TRANSITIONS + 20) {
            handle.set_input(
                (index + 1) as u64,
                MovementInput {
                    left: index % 2 == 0,
                    right: index % 2 != 0,
                    ..MovementInput::default()
                },
            );
        }
        let sample = runner.take();
        assert_eq!(sample.transitions.len(), MAX_INPUT_TRANSITIONS);
        assert_eq!(sample.coalesced_transitions, 20);
        assert!(sample.input.right);
    }

    #[test]
    fn final_input_cannot_be_latched_by_delay_overflow_focus_loss_or_stale_snapshots() {
        let (handle, runner) = WorldControlHandle::new();
        let pressed = MovementInput {
            right: true,
            jump: true,
            sprint: true,
            ..MovementInput::default()
        };
        handle.set_input(10, pressed);
        std::thread::sleep(Duration::from_millis(60));
        handle.set_input(9, MovementInput::default());
        assert_eq!(
            runner.take().input,
            pressed,
            "stale release must be ignored"
        );

        handle.set_input(11, MovementInput::default());
        assert_eq!(runner.take().input, MovementInput::default());
        for offset in 0..(MAX_INPUT_TRANSITIONS + 20) {
            let input = if offset % 2 == 0 {
                MovementInput {
                    left: true,
                    ..MovementInput::default()
                }
            } else {
                MovementInput {
                    right: true,
                    ..MovementInput::default()
                }
            };
            handle.set_input(12 + offset as u64, input);
        }
        handle.set_input(1000, MovementInput::default());
        let overflowed = runner.take();
        assert_eq!(overflowed.transitions.len(), MAX_INPUT_TRANSITIONS);
        assert!(overflowed.coalesced_transitions > 0);
        assert_eq!(overflowed.input, MovementInput::default());
        assert_eq!(
            overflowed
                .transitions
                .last()
                .map(|transition| transition.input),
            Some(MovementInput::default())
        );

        handle.set_input(1001, pressed);
        handle.clear();
        let focused_out = runner.take();
        assert_eq!(focused_out.input, MovementInput::default());
        assert_eq!(
            focused_out
                .transitions
                .last()
                .map(|transition| transition.input),
            Some(MovementInput::default())
        );
    }

    #[test]
    fn continuous_mouse_input_is_consumed_once_without_saturation_or_backlog() {
        let (handle, runner) = WorldControlHandle::new();
        let mut consumed_yaw = 0.0_f64;
        let mut consumed_pitch = 0.0_f64;
        let mut latest = RenderLookSample::default();
        for sequence in 1..=10_000_u64 {
            handle.add_look_delta(
                sequence,
                0.125,
                if sequence % 2 == 0 { 0.025 } else { -0.025 },
            );
            if sequence % 20 == 0 {
                let sample = runner.take();
                consumed_yaw += f64::from(sample.yaw_delta);
                consumed_pitch += f64::from(sample.pitch_delta);
                latest = sample.look;
                assert_eq!(latest.sequence, sequence);
            }
        }
        assert!((consumed_yaw - 1_250.0).abs() < 1.0e-6);
        assert!(consumed_pitch.abs() < 1.0e-6);
        assert_eq!(latest.yaw_total, consumed_yaw);
        assert!((latest.pitch_total - consumed_pitch).abs() < 1.0e-6);
        let empty = runner.take();
        assert_eq!((empty.yaw_delta, empty.pitch_delta), (0.0, 0.0));
        assert_eq!(empty.look, latest);
    }

    #[test]
    fn keyboard_activity_cannot_change_mouse_totals_or_acknowledge_pending_look() {
        #[derive(Clone, Copy)]
        enum Pattern {
            MouseOnly,
            HeldForward,
            RepeatedForward,
            AlternatingStrafe,
            RepeatedSprint,
            RepeatedSneak,
            RepeatedJump,
            RapidMixed,
            CreativeFlightControls,
        }

        let patterns = [
            Pattern::MouseOnly,
            Pattern::HeldForward,
            Pattern::RepeatedForward,
            Pattern::AlternatingStrafe,
            Pattern::RepeatedSprint,
            Pattern::RepeatedSneak,
            Pattern::RepeatedJump,
            Pattern::RapidMixed,
            Pattern::CreativeFlightControls,
        ];
        for pattern in patterns {
            let (handle, runner) = WorldControlHandle::new();
            let mut input_sequence = 0_u64;
            let mut input = MovementInput::default();
            let mut produced_yaw = 0.0_f64;
            let mut produced_pitch = 0.0_f64;
            let mut consumed_yaw = 0.0_f64;
            let mut consumed_pitch = 0.0_f64;

            for mouse_sequence in 1..=2_000_u64 {
                let yaw = if mouse_sequence % 3 == 0 {
                    -0.075
                } else {
                    0.125
                };
                let pitch = if mouse_sequence % 2 == 0 {
                    0.025
                } else {
                    -0.025
                };
                handle.add_look_delta(mouse_sequence, yaw, pitch);
                produced_yaw += f64::from(yaw);
                produced_pitch += f64::from(pitch);

                let changed = match pattern {
                    Pattern::MouseOnly => false,
                    Pattern::HeldForward => {
                        let changed = mouse_sequence == 1;
                        input.forward = true;
                        changed
                    }
                    Pattern::RepeatedForward => {
                        input.forward = !input.forward;
                        true
                    }
                    Pattern::AlternatingStrafe => {
                        input.left = mouse_sequence % 2 == 0;
                        input.right = !input.left;
                        true
                    }
                    Pattern::RepeatedSprint => {
                        input.sprint = !input.sprint;
                        true
                    }
                    Pattern::RepeatedSneak => {
                        input.sneak = !input.sneak;
                        true
                    }
                    Pattern::RepeatedJump => {
                        input.jump = !input.jump;
                        true
                    }
                    Pattern::RapidMixed => {
                        input.forward = mouse_sequence % 2 == 0;
                        input.backward = mouse_sequence % 5 == 0;
                        input.left = mouse_sequence % 3 == 0;
                        input.right = mouse_sequence % 7 == 0;
                        input.sprint = mouse_sequence % 11 == 0;
                        true
                    }
                    Pattern::CreativeFlightControls => {
                        input.forward = true;
                        input.jump = mouse_sequence % 4 < 2;
                        input.sneak = mouse_sequence % 8 >= 6;
                        input.sprint = mouse_sequence % 6 < 3;
                        true
                    }
                };
                if changed {
                    input_sequence += 1;
                    let look_before = handle.0.lock().unwrap().look;
                    handle.set_input(input_sequence, input);
                    let state = handle.0.lock().unwrap();
                    assert_eq!(state.look, look_before);
                    drop(state);
                }

                if mouse_sequence % 17 == 0 {
                    let sample = runner.take();
                    consumed_yaw += f64::from(sample.yaw_delta);
                    consumed_pitch += f64::from(sample.pitch_delta);
                    let state = handle.0.lock().unwrap();
                    assert!((produced_yaw - consumed_yaw - state.pending_yaw).abs() < 1.0e-5);
                    assert!((produced_pitch - consumed_pitch - state.pending_pitch).abs() < 1.0e-5);
                    assert_eq!(sample.look.yaw_total, produced_yaw);
                    assert_eq!(sample.look.pitch_total, produced_pitch);
                }
            }

            let final_sample = runner.take();
            consumed_yaw += f64::from(final_sample.yaw_delta);
            consumed_pitch += f64::from(final_sample.pitch_delta);
            assert!((produced_yaw - consumed_yaw).abs() < 1.0e-5);
            assert!((produced_pitch - consumed_pitch).abs() < 1.0e-5);
            assert_eq!(final_sample.input, input);

            let input_before = handle.0.lock().unwrap().input;
            handle.add_look_delta(2_001, 1.0, -1.0);
            assert_eq!(handle.0.lock().unwrap().input, input_before);
        }
    }

    #[test]
    fn look_intent_is_sampled_absolutely_and_authoritative_rebase_discards_old_pending_delta() {
        let (handle, runner) = WorldControlHandle::new();
        let rebased = runner.rebase_look(350.0, 80.0);
        assert_eq!(rebased, RenderLookSample::default());
        handle.add_look_delta(1, 20.0, 20.0);
        handle.add_look_delta(2, -5.0, -3.0);
        let sample = runner.take();
        assert_eq!(
            (sample.intended_yaw, sample.intended_pitch),
            (Some(5.0), Some(87.0))
        );
        assert_eq!((sample.yaw_delta, sample.pitch_delta), (15.0, 17.0));

        handle.add_look_delta(3, 9.0, 7.0);
        let at_correction = runner.rebase_look(90.0, -20.0);
        assert_eq!(at_correction.sequence, 3);
        let corrected = runner.take();
        assert_eq!((corrected.yaw_delta, corrected.pitch_delta), (0.0, 0.0));
        assert_eq!(
            (corrected.intended_yaw, corrected.intended_pitch),
            (Some(90.0), Some(-20.0))
        );

        handle.add_look_delta(4, -4.0, 2.0);
        let post_correction = runner.take();
        assert_eq!(
            (post_correction.intended_yaw, post_correction.intended_pitch),
            (Some(86.0), Some(-18.0))
        );
        assert_eq!(
            (post_correction.yaw_delta, post_correction.pitch_delta),
            (-4.0, 2.0)
        );
    }

    #[test]
    fn packet_tracker_emits_a_short_press_and_release_even_between_ticks() {
        let pressed = MovementInput {
            forward: true,
            ..MovementInput::default()
        };
        let now = Instant::now();
        let transitions = [
            InputTransition {
                sequence: 1,
                input: pressed,
                recorded_at: now,
                reset: false,
            },
            InputTransition {
                sequence: 2,
                input: MovementInput::default(),
                recorded_at: now,
                reset: false,
            },
        ];
        let mut tracker = MovementPacketTracker::default();
        let state = test_state();
        tracker.reset(state.pose);
        assert_eq!(
            packet_ids(
                &tracker
                    .frames(&state, &transitions, MovementInput::default(), 9)
                    .unwrap()
            ),
            vec![0x2b, 0x2b]
        );
    }

    #[test]
    fn relative_correction_velocity_matches_current_rotation_and_delta_rules() {
        let old = LocalPlayerPose::new(0.0, 0.0, 0.0, 90.0, 0.0);
        let authoritative = AuthoritativeTransform {
            x: 1.0,
            y: 2.0,
            z: 3.0,
            yaw: 0.0,
            pitch: 0.0,
            teleport_id: 4,
        };
        let packet = PlayerPosition {
            teleport_id: 4,
            x: 1.0,
            y: 2.0,
            z: 3.0,
            delta_x: 0.25,
            delta_y: 0.5,
            delta_z: 0.75,
            yaw: 0.0,
            pitch: 0.0,
            relative_flags: 0x20 | 0x40 | 0x80 | 0x100,
        };
        let result = correction_velocity(Vec3d::new(1.0, 2.0, 0.0), old, packet, authoritative);
        assert!((result.x - 0.25).abs() < 1.0e-9);
        assert!((result.y - 2.5).abs() < 1.0e-9);
        assert!((result.z + 0.25).abs() < 1.0e-9);
    }

    #[test]
    fn packet_tracker_selects_exact_movement_families_and_periodic_position() {
        let mut tracker = MovementPacketTracker::default();
        let mut state = test_state();
        tracker.reset(state.pose);

        state.pose.x += 1.0;
        assert_eq!(
            packet_ids(
                &tracker
                    .frames(&state, &[], MovementInput::default(), 9)
                    .unwrap()
            ),
            vec![0x1e]
        );
        state.pose.yaw = 20.0;
        assert_eq!(
            packet_ids(
                &tracker
                    .frames(&state, &[], MovementInput::default(), 9)
                    .unwrap()
            ),
            vec![0x20]
        );
        state.pose.z += 1.0;
        state.pose.pitch = 5.0;
        assert_eq!(
            packet_ids(
                &tracker
                    .frames(&state, &[], MovementInput::default(), 9)
                    .unwrap()
            ),
            vec![0x1f]
        );
        state.on_ground = true;
        assert_eq!(
            packet_ids(
                &tracker
                    .frames(&state, &[], MovementInput::default(), 9)
                    .unwrap()
            ),
            vec![0x21]
        );

        let mut periodic = MovementPacketTracker::default();
        let unchanged = test_state();
        periodic.reset(unchanged.pose);
        for _ in 0..19 {
            assert!(
                periodic
                    .frames(&unchanged, &[], MovementInput::default(), 9)
                    .unwrap()
                    .is_empty()
            );
        }
        assert_eq!(
            packet_ids(
                &periodic
                    .frames(&unchanged, &[], MovementInput::default(), 9)
                    .unwrap()
            ),
            vec![0x1e]
        );
    }

    #[test]
    fn input_and_sprint_transitions_are_sent_once_without_teleport_packets() {
        let mut tracker = MovementPacketTracker::default();
        let mut state = test_state();
        tracker.reset(state.pose);
        let input = MovementInput {
            forward: true,
            sprint: true,
            ..MovementInput::default()
        };
        state.sprinting = true;
        let first = packet_ids(&tracker.frames(&state, &[], input, 300).unwrap());
        assert_eq!(first, vec![0x2b, 0x2a]);
        assert!(!first.contains(&0));
        assert!(tracker.frames(&state, &[], input, 300).unwrap().is_empty());

        state.sprinting = false;
        let stopped = packet_ids(
            &tracker
                .frames(&state, &[], MovementInput::default(), 300)
                .unwrap(),
        );
        assert_eq!(stopped, vec![0x2b, 0x2a]);
    }

    #[test]
    fn raw_sprint_key_and_resolved_sprint_command_are_independent() {
        let mut tracker = MovementPacketTracker::default();
        let mut state = test_state();
        tracker.reset(state.pose);

        let backward_with_sprint_key = MovementInput {
            backward: true,
            sprint: true,
            ..MovementInput::default()
        };
        assert_eq!(
            packet_ids(
                &tracker
                    .frames(&state, &[], backward_with_sprint_key, 300)
                    .unwrap()
            ),
            vec![0x2b]
        );

        state.sprinting = true;
        let forward_after_key_release = MovementInput {
            forward: true,
            ..MovementInput::default()
        };
        assert_eq!(
            packet_ids(
                &tracker
                    .frames(&state, &[], forward_after_key_release, 300)
                    .unwrap()
            ),
            vec![0x2b, 0x2a]
        );
    }

    #[test]
    fn relative_position_correction_uses_predicted_pose_not_stale_server_pose() {
        let (_handle, runner) = WorldControlHandle::new();
        let mut controller =
            WorldMovementController::new(runner, BlockCollisionProfile::synthetic([]), 7);
        controller.simulation = Some(
            PlayerMovementState::from_authoritative(
                LocalPlayerPose::new(100.0, 70.0, -40.0, 30.0, 10.0),
                Vec3d::default(),
            )
            .unwrap(),
        );
        let update = controller
            .absolute_position_update(
                PlayerPosition {
                    teleport_id: 3,
                    x: 1.0,
                    y: 2.0,
                    z: 3.0,
                    delta_x: 0.0,
                    delta_y: 0.0,
                    delta_z: 0.0,
                    yaw: 5.0,
                    pitch: -2.0,
                    relative_flags: 0x1f,
                },
                &WorldState::default(),
            )
            .unwrap();
        assert_eq!((update.x, update.y, update.z), (101.0, 72.0, -37.0));
        assert_eq!((update.yaw, update.pitch), (35.0, 8.0));
        assert_eq!(update.relative, RelativeTransformFlags::default());
    }

    #[test]
    fn correction_metrics_count_distinct_and_rapid_packets_and_zero_replaced_velocity() {
        let (_handle, runner) = WorldControlHandle::new();
        let mut controller =
            WorldMovementController::new(runner, BlockCollisionProfile::synthetic([]), 7);
        controller.simulation = Some(
            PlayerMovementState::from_authoritative(
                LocalPlayerPose::new(-2.25, 69.0, -4.5, 0.0, 0.0),
                Vec3d::new(1.0, 2.0, 3.0),
            )
            .unwrap(),
        );
        let packet = |teleport_id| PlayerPosition {
            teleport_id,
            x: -2.3,
            y: 69.0,
            z: -4.5,
            delta_x: 0.0,
            delta_y: 0.0,
            delta_z: 0.0,
            yaw: 0.0,
            pitch: 0.0,
            relative_flags: 0,
        };
        let authoritative = |teleport_id| AuthoritativeTransform {
            x: -2.3,
            y: 69.0,
            z: -4.5,
            yaw: 0.0,
            pitch: 0.0,
            teleport_id,
        };
        let world = WorldState::default();

        controller
            .reconcile(packet(2), authoritative(2), &world)
            .unwrap();
        controller
            .reconcile(packet(3), authoritative(3), &world)
            .unwrap();

        assert_eq!(controller.correction_count, 2);
        assert_eq!(controller.rapid_correction_count, 1);
        assert_eq!(
            controller.simulation.as_ref().unwrap().velocity,
            Vec3d::default()
        );
    }

    fn jump_transition(sequence: u64, jump: bool, at: Instant) -> InputTransition {
        InputTransition {
            sequence,
            input: MovementInput {
                jump,
                ..MovementInput::default()
            },
            recorded_at: at,
            reset: false,
        }
    }

    #[test]
    fn durable_double_space_toggles_flight_even_when_consumed_late() {
        let base = Instant::now() - Duration::from_secs(1);
        let transitions = [
            jump_transition(1, true, base),
            jump_transition(2, false, base + Duration::from_millis(50)),
            jump_transition(3, true, base + Duration::from_millis(300)),
        ];
        let mut state = test_state();
        state.apply_flight_abilities(true, false, 0.05).unwrap();
        let mut tracker = FlightToggleTracker::default();
        assert_eq!(
            tracker.consume(&transitions, 0, true, &mut state),
            vec![true]
        );
        assert!(state.flying);

        let mut packets = MovementPacketTracker::default();
        packets.reset(state.pose);
        assert_eq!(
            packet_ids(
                &packets
                    .frames_with_flight(&state, &transitions, transitions[2].input, 7, &[true],)
                    .unwrap()
            ),
            vec![0x2b, 0x2b, 0x2b, 0x28]
        );
    }

    #[test]
    fn flight_gesture_rejects_slow_hold_focus_loss_and_journal_overflow() {
        let base = Instant::now() - Duration::from_secs(2);
        let mut state = test_state();
        state.apply_flight_abilities(true, false, 0.05).unwrap();
        let mut tracker = FlightToggleTracker::default();
        assert!(
            tracker
                .consume(
                    &[
                        jump_transition(1, true, base),
                        jump_transition(2, false, base + Duration::from_millis(50)),
                        jump_transition(3, true, base + Duration::from_millis(500)),
                    ],
                    0,
                    true,
                    &mut state,
                )
                .is_empty()
        );
        assert!(!state.flying);
        assert!(tracker.consume(&[], 0, true, &mut state).is_empty());

        let reset = InputTransition {
            reset: true,
            ..jump_transition(4, false, base + Duration::from_millis(550))
        };
        assert!(tracker.consume(&[reset], 0, false, &mut state).is_empty());
        assert!(
            tracker
                .consume(
                    &[jump_transition(5, true, base + Duration::from_millis(600))],
                    1,
                    true,
                    &mut state,
                )
                .is_empty()
        );
        assert!(!state.flying);
    }

    #[test]
    fn server_ability_revocation_clears_flight_and_partial_toggle_state() {
        let (_handle, runner) = WorldControlHandle::new();
        let mut controller =
            WorldMovementController::new(runner, BlockCollisionProfile::synthetic([]), 7);
        controller.simulation = Some(test_state());
        controller
            .apply_abilities(v775::PlayerAbilities {
                invulnerable: true,
                flying: true,
                may_fly: true,
                instant_build: true,
                flying_speed: 0.05,
                walking_speed: 0.1,
            })
            .unwrap();
        assert!(controller.simulation.as_ref().unwrap().flying);
        controller
            .apply_abilities(v775::PlayerAbilities {
                invulnerable: false,
                flying: false,
                may_fly: false,
                instant_build: false,
                flying_speed: 0.05,
                walking_speed: 0.1,
            })
            .unwrap();
        assert!(!controller.simulation.as_ref().unwrap().flying);
        assert!(!controller.simulation.as_ref().unwrap().may_fly);
        assert!(controller.flight_toggle.first_press_at.is_none());

        controller
            .apply_abilities(v775::PlayerAbilities {
                invulnerable: true,
                flying: true,
                may_fly: true,
                instant_build: true,
                flying_speed: 0.05,
                walking_speed: 0.1,
            })
            .unwrap();
        controller.reset(9);
        assert!(controller.simulation.is_none());
        assert_eq!(controller.abilities.map(|value| value.flying), Some(false));
    }
}
