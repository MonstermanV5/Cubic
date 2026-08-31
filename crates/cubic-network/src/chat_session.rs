use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, AtomicUsize, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use cubic_auth::{AuthenticatedMinecraftAccount, MinecraftSessionJoiner, PlayerCertificate};
use cubic_core::{
    ChatEvent, ChatMessage, ChatMessageKind, ChatMessageTrust, ChatSessionCommand, SafetyAlertKind,
    SessionPresentationMode, StructuredText,
};
use cubic_protocol::{
    bootstrap::v775::{self, ClientInformation, PlayClientbound, TextComponent},
    nbt::{NbtCompound, NbtTag},
};
use cubic_world::{BlockCollisionProfile, Vec3d, WorldEvent, WorldState};
use rand_core_06::{OsRng, RngCore};
use thiserror::Error;
use tokio::sync::mpsc;

use crate::{
    AuthenticatedLoginError, AuthenticatedLoginOptions, DevelopmentLoginError,
    DevelopmentLoginOptions, DevelopmentUsername, ServerAddress,
    connection::ConnectionError,
    development_login::{ConnectionState, connect_to_play, run_configuration},
    online_login::establish_authenticated_play,
    secure_chat::{SecureChatError, SecureChatSession, system_time_millis},
    world_movement::{WorldControlRunner, WorldMovementController},
    world_render::WorldRenderRunner,
};

pub const DEFAULT_EVENT_CAPACITY: usize = 128;
pub const DEFAULT_COMMAND_CAPACITY: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChatSessionOptions {
    pub login: DevelopmentLoginOptions,
    pub event_capacity: usize,
    pub command_capacity: usize,
}

impl Default for ChatSessionOptions {
    fn default() -> Self {
        Self {
            login: DevelopmentLoginOptions::default(),
            event_capacity: DEFAULT_EVENT_CAPACITY,
            command_capacity: DEFAULT_COMMAND_CAPACITY,
        }
    }
}

/// UI-side endpoint for a bounded persistent session.
pub struct ChatSessionHandle {
    commands: mpsc::Sender<ChatSessionCommand>,
    events: mpsc::Receiver<ChatEvent>,
    critical_event: Arc<Mutex<Option<ChatEvent>>>,
    dropped_events: Arc<AtomicUsize>,
    channel_closed_reported: bool,
    presentation_mode: Arc<AtomicU8>,
}

pub struct ChatSessionRunner {
    commands: mpsc::Receiver<ChatSessionCommand>,
    events: mpsc::Sender<ChatEvent>,
    critical_event: Arc<Mutex<Option<ChatEvent>>>,
    dropped_events: Arc<AtomicUsize>,
    presentation_mode: Arc<AtomicU8>,
}

impl ChatSessionHandle {
    #[must_use]
    pub fn bounded(options: &ChatSessionOptions) -> (Self, ChatSessionRunner) {
        let (command_tx, command_rx) = mpsc::channel(options.command_capacity.max(1));
        let (event_tx, event_rx) = mpsc::channel(options.event_capacity.max(1));
        let critical_event = Arc::new(Mutex::new(None));
        let dropped_events = Arc::new(AtomicUsize::new(0));
        let presentation_mode = Arc::new(AtomicU8::new(presentation_mode_value(
            SessionPresentationMode::Chat,
        )));
        (
            Self {
                commands: command_tx,
                events: event_rx,
                critical_event: Arc::clone(&critical_event),
                dropped_events: Arc::clone(&dropped_events),
                channel_closed_reported: false,
                presentation_mode: Arc::clone(&presentation_mode),
            },
            ChatSessionRunner {
                commands: command_rx,
                events: event_tx,
                critical_event,
                dropped_events,
                presentation_mode,
            },
        )
    }

    pub fn try_send_message(&self, message: String) -> Result<(), ChatSessionSendError> {
        self.commands
            .try_send(ChatSessionCommand::SendMessage(message))
            .map_err(map_send_error)
    }

    pub fn disconnect(&self) -> Result<(), ChatSessionSendError> {
        self.commands
            .try_send(ChatSessionCommand::Disconnect)
            .map_err(map_send_error)
    }

    pub fn set_presentation_mode(&self, mode: SessionPresentationMode) {
        self.presentation_mode
            .store(presentation_mode_value(mode), Ordering::Release);
    }

    pub fn try_next_event(&mut self) -> Option<ChatEvent> {
        match self.events.try_recv() {
            Ok(event) => Some(event),
            Err(mpsc::error::TryRecvError::Empty) => None,
            Err(mpsc::error::TryRecvError::Disconnected) if !self.channel_closed_reported => {
                self.channel_closed_reported = true;
                Some(ChatEvent::Disconnected {
                    reason: "network task stopped".to_owned(),
                })
            }
            Err(mpsc::error::TryRecvError::Disconnected) => None,
        }
    }

    pub fn take_critical_event(&self) -> Option<ChatEvent> {
        self.critical_event
            .lock()
            .ok()
            .and_then(|mut event| event.take())
    }

    #[must_use]
    pub fn dropped_event_count(&self) -> usize {
        self.dropped_events.swap(0, Ordering::AcqRel)
    }
}

const fn presentation_mode_value(mode: SessionPresentationMode) -> u8 {
    match mode {
        SessionPresentationMode::Play => 0,
        SessionPresentationMode::Chat => 1,
    }
}

impl ChatSessionRunner {
    fn presentation_mode(&self) -> SessionPresentationMode {
        if self.presentation_mode.load(Ordering::Acquire)
            == presentation_mode_value(SessionPresentationMode::Chat)
        {
            SessionPresentationMode::Chat
        } else {
            SessionPresentationMode::Play
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum ChatSessionSendError {
    #[error("the outgoing Chat Mode command queue is full")]
    Full,
    #[error("the Chat Mode network task has stopped")]
    Closed,
}

#[derive(Debug, Error)]
pub enum ChatSessionError {
    #[error(transparent)]
    Login(#[from] DevelopmentLoginError),
    #[error(transparent)]
    AuthenticatedLogin(#[from] AuthenticatedLoginError),
    #[error("secure player-chat state failed: {0}")]
    SecureChat(String),
    #[error(transparent)]
    Protocol(#[from] v775::BootstrapProtocolError),
    #[error("persistent Play transport failed: {0}")]
    Transport(String),
    #[error("the system clock is before the Unix epoch")]
    InvalidSystemClock,
    #[error("invalid outgoing chat message: {reason}")]
    InvalidMessage { reason: &'static str },
    #[error("persistent Chat Mode does not support server {feature}")]
    UnsupportedServerFeature { feature: &'static str },
    #[error("world packet adaptation failed: {0}")]
    WorldAdapter(String),
    #[error("world state update failed: {0}")]
    WorldState(#[from] cubic_world::WorldError),
    #[error("local player movement failed: {0}")]
    Movement(String),
    #[error(
        "commands are not supported in Phase 8 because signable command arguments are not available"
    )]
    CommandsUnsupported,
}

pub async fn run_development_chat_session(
    address: &ServerAddress,
    username: &DevelopmentUsername,
    options: &ChatSessionOptions,
    runner: ChatSessionRunner,
) -> Result<(), ChatSessionError> {
    let mut connected = match connect_to_play(address, username, &options.login).await {
        Ok(connected) => connected,
        Err(error) => {
            runner.critical(ChatEvent::Disconnected {
                reason: error.to_string(),
            });
            return Err(error.into());
        }
    };
    run_play_session(
        &mut connected.connection,
        ChatSecurity::UnsignedDevelopment,
        runner,
        connected.initial_login,
        connected.dimension_types,
        None,
        None,
    )
    .await
}

pub async fn run_authenticated_chat_session<J: MinecraftSessionJoiner>(
    address: &ServerAddress,
    account: &AuthenticatedMinecraftAccount,
    session_joiner: &J,
    certificate: PlayerCertificate,
    login_options: &AuthenticatedLoginOptions,
    runner: ChatSessionRunner,
) -> Result<(), ChatSessionError> {
    let mut connected =
        match establish_authenticated_play(address, account, session_joiner, login_options).await {
            Ok(connected) => connected,
            Err(error) => {
                runner.critical(ChatEvent::Disconnected {
                    reason: error.to_string(),
                });
                return Err(error.into());
            }
        };
    let sender = connected.result.profile_uuid;
    let security = ChatSecurity::Authenticated(Box::new(SecureChatSession::new(
        connected.secure_chat_rules,
        certificate,
        sender,
        random_session_uuid(),
    )));
    run_play_session(
        &mut connected.connection,
        security,
        runner,
        connected.initial_login,
        connected.dimension_types,
        None,
        None,
    )
    .await
}

/// Runs the existing development Play session while publishing coalesced world deltas.
pub async fn run_development_world_session(
    address: &ServerAddress,
    username: &DevelopmentUsername,
    options: &ChatSessionOptions,
    runner: ChatSessionRunner,
    render: WorldRenderRunner,
    controls: WorldControlRunner,
    collisions: BlockCollisionProfile,
) -> Result<(), ChatSessionError> {
    let mut connected = connect_to_play(address, username, &options.login).await?;
    run_play_session(
        &mut connected.connection,
        ChatSecurity::UnsignedDevelopment,
        runner,
        connected.initial_login,
        connected.dimension_types,
        Some(render),
        Some((controls, collisions)),
    )
    .await
}

enum ChatSecurity {
    UnsignedDevelopment,
    Authenticated(Box<SecureChatSession>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RenderPoseAuthority {
    AuthoritativeWorld,
    LocalPrediction,
}

const LOW_HEALTH_THRESHOLD: f32 = 6.0;
const DANGEROUS_AIR_THRESHOLD: i32 = 60;
const LARGE_DISPLACEMENT_THRESHOLD_SQUARED: f64 = 64.0;

#[derive(Default)]
struct SessionSafetyTracker {
    health: Option<f32>,
    air: Option<i32>,
    low_health_active: bool,
    drowning_active: bool,
    dead: bool,
}

impl SessionSafetyTracker {
    fn health_alerts(&mut self, health: f32) -> Vec<ChatEvent> {
        let mut alerts = Vec::with_capacity(2);
        if health <= 0.0 {
            if !self.dead {
                alerts.push(safety_alert(SafetyAlertKind::Death, "You died"));
            }
            self.dead = true;
        } else {
            self.dead = false;
            if self.health.is_some_and(|previous| health < previous) {
                alerts.push(safety_alert(
                    SafetyAlertKind::Damage,
                    format!("Health dropped to {health:.1}"),
                ));
            }
        }
        if health <= LOW_HEALTH_THRESHOLD && health > 0.0 {
            if !self.low_health_active {
                alerts.push(safety_alert(
                    SafetyAlertKind::LowHealth,
                    format!("Dangerously low health: {health:.1}"),
                ));
            }
            self.low_health_active = true;
        } else {
            self.low_health_active = false;
        }
        self.health = Some(health);
        alerts
    }

    fn air_alert(&mut self, air: i32) -> Option<ChatEvent> {
        let dangerous = air <= DANGEROUS_AIR_THRESHOLD;
        let alert = (dangerous && !self.drowning_active).then(|| {
            safety_alert(
                SafetyAlertKind::Drowning,
                format!("Air supply is dangerously low: {air}"),
            )
        });
        self.drowning_active = dangerous;
        self.air = Some(air);
        alert
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

fn safety_alert(kind: SafetyAlertKind, message: impl Into<String>) -> ChatEvent {
    ChatEvent::SafetyAlert {
        kind,
        message: message.into(),
    }
}

fn significant_displacement(
    from: cubic_world::LocalPlayerPose,
    to: cubic_world::LocalPlayerPose,
) -> bool {
    let dx = to.x - from.x;
    let dy = to.y - from.y;
    let dz = to.z - from.z;
    dx.mul_add(dx, dy.mul_add(dy, dz * dz)) >= LARGE_DISPLACEMENT_THRESHOLD_SQUARED
}

fn publish_authoritative_pose(
    render: Option<&WorldRenderRunner>,
    world: &WorldState,
    pose_authority: RenderPoseAuthority,
) {
    if pose_authority == RenderPoseAuthority::LocalPrediction {
        return;
    }
    let (Some(render), Some(pose)) = (render, world.session().and_then(|session| session.position))
    else {
        return;
    };
    render.pose(cubic_world::LocalPlayerPose::new(
        pose.x, pose.y, pose.z, pose.yaw, pose.pitch,
    ));
}

fn publish_reset(
    render: &Option<WorldRenderRunner>,
    world: &WorldState,
    pose_authority: RenderPoseAuthority,
) {
    let (Some(render), Some(session)) = (render, world.session()) else {
        return;
    };
    render.reset(
        session.spawn_context.dimension.to_string(),
        session.dimension_geometry,
    );
    publish_authoritative_pose(Some(render), world, pose_authority);
}

async fn run_play_session(
    connection: &mut crate::connection::MinecraftConnection,
    mut security: ChatSecurity,
    mut runner: ChatSessionRunner,
    initial_login: v775::InitialPlayLogin,
    dimension_types: Vec<cubic_world::RuntimeDimensionType>,
    render: Option<WorldRenderRunner>,
    movement: Option<(WorldControlRunner, BlockCollisionProfile)>,
) -> Result<(), ChatSessionError> {
    let player_entity_id = initial_login.player_entity_id;
    let pose_authority = if movement.is_some() {
        RenderPoseAuthority::LocalPrediction
    } else {
        RenderPoseAuthority::AuthoritativeWorld
    };
    let mut world = WorldState::default();
    world.apply(WorldEvent::BeginConfiguration)?;
    world.apply(WorldEvent::RuntimeDimensionTypes(dimension_types))?;
    world.apply(crate::world_adapter::initial_world_event(initial_login)?)?;
    publish_reset(&render, &world, pose_authority);
    tracing::info!(target: "world", summary = %world.summary(), "entered authoritative world state");
    runner.event(ChatEvent::Connected);

    let information = v775::encode_play_client_information(&ClientInformation::default())?;
    connection
        .write_all(&information, "Play Client Information write")
        .await
        .map_err(transport)?;
    if let ChatSecurity::Authenticated(session) = &security {
        write_session_update(connection, session).await?;
        tracing::info!(target: "chat", "player chat session established");
    }

    let mut salt_counter = 0_i64;
    let mut sent_player_loaded = false;
    let mut safety = SessionSafetyTracker::default();
    let mut movement = movement.map(|(controls, collisions)| {
        WorldMovementController::new(controls, collisions, player_entity_id)
    });
    let mut movement_ticks = tokio::time::interval(std::time::Duration::from_millis(50));
    movement_ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            scheduled = movement_ticks.tick(), if movement.is_some() => {
                service_movement_tick(connection, &mut movement, &world, &render, scheduled).await?;
            }
            command = runner.commands.recv() => {
                match command {
                    Some(ChatSessionCommand::SendMessage(message)) => {
                        if let Err(error) = send_chat(
                            connection,
                            &message,
                            &mut salt_counter,
                            &mut security,
                        ).await {
                            runner.event(ChatEvent::Warning(error.to_string()));
                        }
                    }
                    Some(ChatSessionCommand::Disconnect) | None => {
                        world.apply(WorldEvent::Disconnect)?;
                        tracing::info!(target: "world", summary = %world.summary(), "left authoritative world state");
                        return Ok(());
                    }
                }
            }
            frame = connection.read_frame_unbounded("persistent Play packet read") => {
                let frame = match frame {
                    Ok(frame) => frame,
                    Err(error) => {
                        let reason = error.to_string();
                        runner.critical(ChatEvent::Disconnected { reason: reason.clone() });
                        return Err(ChatSessionError::Transport(reason));
                    }
                };
                let decode_started = std::time::Instant::now();
                let frame_bytes = frame.len();
                let packet = if v775::classify_play_decode_work(&frame)? == v775::PlayDecodeWork::ChunkHeavy {
                    let mut decode = tokio::task::spawn_blocking(move || v775::decode_play_clientbound(&frame));
                    loop {
                        tokio::select! {
                            result = &mut decode => {
                                break result
                                    .map_err(|_| ChatSessionError::Movement("bounded chunk decode worker stopped unexpectedly".to_owned()))??;
                            }
                            scheduled = movement_ticks.tick(), if movement.is_some() => {
                                service_movement_tick(connection, &mut movement, &world, &render, scheduled).await?;
                            }
                        }
                    }
                } else {
                    v775::decode_play_clientbound(&frame)?
                };
                let decode_elapsed = decode_started.elapsed();
                if decode_elapsed > std::time::Duration::from_millis(50) {
                    tracing::debug!(target: "movement::latency", ?decode_elapsed, frame_bytes, "completed slow Play packet decode without starving movement ticks");
                }
                let packet = match crate::world_adapter::adapt_chunk_packet(packet) {
                    crate::world_adapter::ChunkAdaptation::Load(chunk) => {
                        let coordinate = chunk.coordinate;
                        let semantic_started = std::time::Instant::now();
                        world.apply(WorldEvent::LoadChunk(chunk))?;
                        let semantic_elapsed = semantic_started.elapsed();
                        let lookup_started = std::time::Instant::now();
                        if let Some(stored) = world.loaded_chunks().get_shared(coordinate) {
                            let lookup_elapsed = lookup_started.elapsed();
                            let publication = render
                                .as_ref()
                                .map_or_default(|render| render.load(Arc::clone(&stored)));
                            let diagnostics_started = std::time::Instant::now();
                            let summary = stored.summary();
                            tracing::trace!(target: "world::chunk", x = summary.coordinate.x, z = summary.coordinate.z, sections = summary.sections, non_empty_sections = summary.non_empty_sections, single_palettes = summary.single_block_palettes, indirect_palettes = summary.indirect_block_palettes, direct_palettes = summary.direct_block_palettes, heightmaps = summary.heightmaps, block_entities = summary.block_entities, sky_layers = summary.sky_layers, block_layers = summary.block_layers, loaded_chunks = world.loaded_chunks().len(), "stored decoded chunk");
                            let diagnostics_elapsed = diagnostics_started.elapsed();
                            let publication_elapsed = publication.lock_wait + publication.bookkeeping;
                            if lookup_elapsed > std::time::Duration::from_millis(50)
                                || publication_elapsed > std::time::Duration::from_millis(50)
                                || diagnostics_elapsed > std::time::Duration::from_millis(50)
                            {
                                tracing::debug!(target: "movement::latency", ?lookup_elapsed, lock_wait = ?publication.lock_wait, bookkeeping = ?publication.bookkeeping, ?diagnostics_elapsed, x = coordinate.x, z = coordinate.z, "slow chunk-to-render handoff breakdown");
                            }
                        }
                        if semantic_elapsed > std::time::Duration::from_millis(50) {
                            tracing::debug!(target: "movement::latency", ?semantic_elapsed, x = coordinate.x, z = coordinate.z, "semantic chunk application was slow");
                        }
                        continue;
                    }
                    crate::world_adapter::ChunkAdaptation::Unload(coordinate) => {
                        world.apply(WorldEvent::UnloadChunk(coordinate))?;
                        if let Some(render) = &render {
                            render.unload(coordinate);
                        }
                        tracing::debug!(target: "world::chunk", x = coordinate.x, z = coordinate.z, loaded_chunks = world.loaded_chunks().len(), "unloaded chunk");
                        continue;
                    }
                    crate::world_adapter::ChunkAdaptation::Light { coordinate, light } => {
                        let sky_layers = light.sky_layer_count;
                        let block_layers = light.block_layer_count;
                        world.apply(WorldEvent::UpdateChunkLight { coordinate, light })?;
                        tracing::debug!(target: "world::chunk", x = coordinate.x, z = coordinate.z, sky_layers, block_layers, "applied bounded chunk light update");
                        continue;
                    }
                    crate::world_adapter::ChunkAdaptation::Blocks(updates) => {
                        let update_count = updates.len();
                        let semantic_started = std::time::Instant::now();
                        let result = world.apply_block_updates(&updates)?;
                        let semantic_elapsed = semantic_started.elapsed();
                        let publication_started = std::time::Instant::now();
                        let mut publication_lock_wait = std::time::Duration::ZERO;
                        let mut publication_bookkeeping = std::time::Duration::ZERO;
                        if let Some(render) = &render {
                            for coordinate in &result.changed_chunks {
                                if let Some(chunk) = world.loaded_chunks().get_shared(*coordinate) {
                                    let timing = render.load(chunk);
                                    publication_lock_wait += timing.lock_wait;
                                    publication_bookkeeping += timing.bookkeeping;
                                }
                            }
                        }
                        let publication_elapsed = publication_started.elapsed();
                        tracing::debug!(
                            target: "world::block_update",
                            update_count,
                            changed_chunks = result.changed_chunks.len(),
                            ignored = result.ignored_unloaded_or_out_of_bounds,
                            revision = result.revision,
                            "applied bounded authoritative live block updates"
                        );
                        if semantic_elapsed > std::time::Duration::from_millis(50) {
                            tracing::debug!(target: "movement::latency", ?semantic_elapsed, update_count, "semantic block update application was slow");
                        }
                        if publication_elapsed > std::time::Duration::from_millis(50) {
                            tracing::debug!(target: "movement::latency", ?publication_elapsed, ?publication_lock_wait, ?publication_bookkeeping, update_count, "block render-delta publication was slow");
                        }
                        continue;
                    }
                    crate::world_adapter::ChunkAdaptation::Other(packet) => packet,
                };
                let world_event = match (&packet, &movement) {
                    (PlayClientbound::PlayerPosition(position), Some(controller)) => Some(
                        WorldEvent::SynchronizePlayerPosition(
                            controller
                                .absolute_position_update(*position, &world)
                                .map_err(|error| ChatSessionError::Movement(error.to_string()))?,
                        ),
                    ),
                    (PlayClientbound::PlayerRotation(rotation), Some(controller)) => Some(
                        WorldEvent::SynchronizePlayerRotation(
                            controller
                                .absolute_rotation_update(
                                    rotation.yaw,
                                    rotation.relative_yaw,
                                    rotation.pitch,
                                    rotation.relative_pitch,
                                    &world,
                                )
                                .map_err(|error| ChatSessionError::Movement(error.to_string()))?,
                        ),
                    ),
                    _ => crate::world_adapter::play_world_event(&packet)?,
                };
                if let Some(event) = world_event {
                    let transition = world.apply(event)?;
                    if transition.reset != cubic_world::ResetScope::None {
                        publish_reset(&render, &world, pose_authority);
                        if let Some(controller) = &mut movement {
                            let entity_id = world.session().map_or(player_entity_id, |session| session.player_entity_id);
                            controller.reset(entity_id);
                        }
                    } else {
                        publish_authoritative_pose(render.as_ref(), &world, pose_authority);
                    }
                    tracing::debug!(target: "world", summary = %world.summary(), reset = ?transition.reset, dimension_changed = transition.dimension_changed, "applied authoritative world update");
                }
                match packet {
                    PlayClientbound::KeepAlive { id } => {
                        write(connection, v775::encode_play_keep_alive(id)?, "Play Keep Alive response write").await?;
                    }
                    PlayClientbound::Ping { id } => {
                        write(connection, v775::encode_play_pong(id)?, "Play Pong write").await?;
                    }
                    PlayClientbound::PlayerPosition(position) => {
                        write(connection, v775::encode_play_teleport_confirmation(position.teleport_id)?, "Play Teleport Confirmation write").await?;
                        if let Some(controller) = &mut movement
                            && let Some(authoritative) = world.session().and_then(|session| session.position)
                        {
                            let predicted_before = controller.predicted_pose();
                            let pose = controller
                                .reconcile(position, authoritative, &world)
                                .map_err(|error| ChatSessionError::Movement(error.to_string()))?;
                            if let Some(render) = &render {
                                render.pose_discontinuity(
                                    pose,
                                    std::time::Instant::now(),
                                    controller.presentation_look(),
                                );
                            }
                            if runner.presentation_mode() == SessionPresentationMode::Chat
                                && predicted_before
                                    .is_some_and(|predicted| significant_displacement(predicted, pose))
                            {
                                runner.critical(safety_alert(
                                    SafetyAlertKind::LargeDisplacement,
                                    "The server moved you a significant distance",
                                ));
                            }
                        }
                    }
                    PlayClientbound::PlayerRotation(rotation) => {
                        if let Some(controller) = &mut movement
                            && let Some((pose, look)) = controller.rotate(
                                rotation.yaw,
                                rotation.relative_yaw,
                                rotation.pitch,
                                rotation.relative_pitch,
                            )
                            && let Some(render) = &render
                        {
                            render.pose_discontinuity(pose, std::time::Instant::now(), look);
                        }
                    }
                    PlayClientbound::SetEntityMotion(motion) => {
                        if let Some(controller) = &mut movement {
                            controller.apply_velocity(
                                motion.entity_id,
                                Vec3d::new(motion.delta_x, motion.delta_y, motion.delta_z),
                            );
                        }
                    }
                    PlayClientbound::PlayerAbilities(abilities) => {
                        if let Some(controller) = &mut movement {
                            controller
                                .apply_abilities(abilities)
                                .map_err(|error| ChatSessionError::Movement(error.to_string()))?;
                        }
                    }
                    PlayClientbound::ChunkBatchFinished { .. } => {
                        write(connection, v775::encode_play_chunk_batch_received(1.0)?, "Play Chunk Batch Received write").await?;
                        if !sent_player_loaded {
                            write(connection, v775::encode_play_player_loaded()?, "Play Player Loaded write").await?;
                            sent_player_loaded = true;
                        }
                    }
                    PlayClientbound::ChunkBatchStart => {}
                    PlayClientbound::LevelChunkWithLight(_)
                    | PlayClientbound::ForgetLevelChunk { .. }
                    | PlayClientbound::LightUpdate(_)
                    | PlayClientbound::BlockUpdate(_)
                    | PlayClientbound::SectionBlocksUpdate(_) => {}
                    PlayClientbound::CookieRequest { key } => {
                        write(connection, v775::encode_play_cookie_response(&key)?, "Play Cookie Response write").await?;
                    }
                    PlayClientbound::PlayerChat {
                        sender_name,
                        signed_content,
                        unsigned_content,
                        message,
                        global_index,
                        signature,
                        modified,
                        sender_index,
                        sender_uuid,
                    } => {
                        let trust = match (signature.is_some(), modified) {
                            (true, true) => ChatMessageTrust::Modified,
                            (true, false) => ChatMessageTrust::SignedUnverified,
                            (false, _) => ChatMessageTrust::Unsigned,
                        };
                        tracing::info!(
                            target: "chat",
                            category = "PlayerChat",
                            sender = %sender_name,
                            sender_uuid = ?sender_uuid,
                            signed_content = %signed_content,
                            global_index,
                            sender_index,
                            signature_present = signature.is_some(),
                            trust = ?trust,
                            "received decoded player chat"
                        );
                        tracing::debug!(
                            target: "chat",
                            unsigned_component = ?unsigned_content,
                            projected_component = ?message,
                            "decoded player-chat components before presentation"
                        );
                        let event = message_event(ChatMessageKind::Player, Some(sender_name), message, trust);
                        match &mut security {
                            ChatSecurity::UnsignedDevelopment if signature.is_some() => {
                                if !runner.event(event) {
                                    tracing::warn!(target: "chat", "decoded player chat dropped by bounded UI event queue");
                                }
                                write(connection, v775::encode_play_chat_acknowledgement(1)?, "Play Chat Acknowledgement write").await?;
                            }
                            ChatSecurity::Authenticated(session) => {
                                session.accept_incoming(global_index, signature, || {
                                    let delivered = runner.event(event);
                                    if !delivered {
                                        tracing::warn!(target: "chat", "decoded player chat dropped by bounded UI event queue");
                                    }
                                    delivered
                                })?;
                                if let Some(count) = session.standalone_acknowledgement() {
                                    write(connection, v775::encode_play_chat_acknowledgement(count)?, "Play Chat Acknowledgement write").await?;
                                }
                            }
                            ChatSecurity::UnsignedDevelopment => {
                                if !runner.event(event) {
                                    tracing::warn!(target: "chat", "decoded player chat dropped by bounded UI event queue");
                                }
                            }
                        }
                    }
                    PlayClientbound::DisguisedChat { sender_name, message } => {
                        tracing::info!(target: "chat", category = "DisguisedChat", sender = %sender_name, text = %message.plain_text, "received decoded disguised chat");
                        tracing::debug!(target: "chat", component = ?message.value, "decoded disguised-chat component before presentation");
                        if !runner.event(message_event(ChatMessageKind::Player, Some(sender_name), message, ChatMessageTrust::Unsigned)) {
                            tracing::warn!(target: "chat", "decoded disguised chat dropped by bounded UI event queue");
                        }
                    }
                    PlayClientbound::SystemChat { message, overlay } => {
                        tracing::info!(target: "chat", category = "SystemChat", text = %message.plain_text, overlay, "received decoded system chat");
                        tracing::debug!(target: "chat", component = ?message.value, "decoded system-chat component before presentation");
                        let kind = if overlay { ChatMessageKind::ServerNotice } else { ChatMessageKind::System };
                        if !runner.event(message_event(kind, None, message, ChatMessageTrust::NotApplicable)) {
                            tracing::warn!(target: "chat", "decoded system chat dropped by bounded UI event queue");
                        }
                    }
                    PlayClientbound::Disconnect { reason } => {
                        world.apply(WorldEvent::Disconnect)?;
                        tracing::info!(target: "world", summary = %world.summary(), "server disconnected world state");
                        runner.critical(ChatEvent::Disconnected { reason: reason.plain_text });
                        return Ok(());
                    }
                    PlayClientbound::Health { health } => {
                        for event in safety.health_alerts(health) {
                            runner.critical(event);
                        }
                    }
                    PlayClientbound::EntityData { entity_id, air_supply, .. } => {
                        let current_player_entity_id = world
                            .session()
                            .map_or(player_entity_id, |session| session.player_entity_id);
                        if entity_id == current_player_entity_id
                            && let Some(air) = air_supply
                            && let Some(event) = safety.air_alert(air)
                        {
                            runner.critical(event);
                        }
                    }
                    PlayClientbound::Login(_)
                    | PlayClientbound::Respawn(_)
                    | PlayClientbound::SetDefaultSpawnPosition(_)
                    | PlayClientbound::SetTime(_)
                    | PlayClientbound::ChangeDifficulty { .. }
                    | PlayClientbound::GameEvent { .. }
                    | PlayClientbound::InitializeBorder(_)
                    | PlayClientbound::CustomPayload { .. }
                    | PlayClientbound::Ignored { .. } => {}
                    PlayClientbound::ResourcePackPush => {
                        return Err(ChatSessionError::UnsupportedServerFeature {
                            feature: "resource-pack pushes",
                        });
                    }
                    PlayClientbound::Transfer => {
                        return Err(ChatSessionError::UnsupportedServerFeature {
                            feature: "transfers",
                        });
                    }
                    PlayClientbound::StartConfiguration => {
                        world.apply(WorldEvent::BeginConfiguration)?;
                        write(connection, v775::encode_play_acknowledge_configuration()?, "Play Configuration Acknowledged write").await?;
                        write(connection, v775::encode_client_information(&ClientInformation::default())?, "Reconfiguration Client Information write").await?;
                        let mut state = ConnectionState::Configuration;
                        let configuration = run_configuration(connection, &mut state).await?;
                        world.apply(WorldEvent::RuntimeDimensionTypes(configuration.dimension_types))?;
                        world.apply(crate::world_adapter::initial_world_event(configuration.initial_login)?)?;
                        publish_reset(&render, &world, pose_authority);
                        if let Some(controller) = &mut movement {
                            let entity_id = world.session().map_or(player_entity_id, |session| session.player_entity_id);
                            controller.reset(entity_id);
                        }
                        safety.reset();
                        tracing::info!(target: "world", summary = %world.summary(), "reconfiguration replaced authoritative world state");
                        if let ChatSecurity::Authenticated(session) = &mut security {
                            session.reset(random_session_uuid());
                            write_session_update(connection, session).await?;
                        }
                        runner.event(ChatEvent::Warning("Server reconfiguration completed".to_owned()));
                    }
                }
            }
        }
    }
}

async fn service_movement_tick(
    connection: &mut crate::connection::MinecraftConnection,
    movement: &mut Option<WorldMovementController>,
    world: &WorldState,
    render: &Option<WorldRenderRunner>,
    scheduled: tokio::time::Instant,
) -> Result<(), ChatSessionError> {
    let started = tokio::time::Instant::now();
    let lateness = started.saturating_duration_since(scheduled);
    if lateness > std::time::Duration::from_millis(50) {
        tracing::debug!(
            target: "movement::latency",
            ?lateness,
            skipped_tick_intervals = lateness.as_millis() / 50,
            "20 Hz movement tick woke late"
        );
    }
    let tick = if let Some(controller) = movement {
        controller
            .tick(world)
            .map_err(|error| ChatSessionError::Movement(error.to_string()))?
    } else {
        None
    };
    if let Some(tick) = tick {
        let simulated_at = tokio::time::Instant::now();
        if tick.jumped {
            tracing::debug!(
                target: "movement::latency",
                ?scheduled,
                ?started,
                ?tick.input_sampled_at,
                space_state_age_at_sample = ?tick.jump_changed_at.map(|at| tick.input_sampled_at.saturating_duration_since(at)),
                sample_to_jump_complete = ?simulated_at.into_std().saturating_duration_since(tick.input_sampled_at),
                grounded_at_start = tick.grounded_at_start,
                horizontal_acceleration = tick.horizontal_acceleration,
                horizontal_drag = tick.horizontal_drag,
                sprint_jump_impulse = tick.sprint_jump_impulse,
                "grounded held-Space jump applied in fixed movement tick"
            );
        }
        if let Some(render) = render {
            render.pose_tick(tick.pose, scheduled.into_std(), tick.look, tick.jumped);
        }
        for frame in tick.frames {
            write(connection, frame, "Play movement write").await?;
        }
        tracing::trace!(
            target: "movement",
            x = tick.pose.x,
            y = tick.pose.y,
            z = tick.pose.z,
            velocity_x = tick.velocity.x,
            velocity_y = tick.velocity.y,
            velocity_z = tick.velocity.z,
            on_ground = tick.on_ground,
            horizontal_collision = tick.horizontal_collision,
            sprinting = tick.sprinting,
            sneaking = tick.sneaking,
            flying = tick.flying,
            grounded_at_start = tick.grounded_at_start,
            horizontal_acceleration = tick.horizontal_acceleration,
            horizontal_drag = tick.horizontal_drag,
            sprint_jump_impulse = tick.sprint_jump_impulse,
            "completed fixed player movement tick"
        );
    }
    write(
        connection,
        v775::encode_play_client_tick_end()?,
        "Play client tick end write",
    )
    .await?;
    let elapsed = started.elapsed();
    if elapsed > std::time::Duration::from_millis(50) {
        tracing::debug!(target: "movement::latency", ?elapsed, "movement tick execution exceeded its 50 ms budget");
    }
    Ok(())
}

async fn send_chat(
    connection: &mut crate::connection::MinecraftConnection,
    message: &str,
    salt_counter: &mut i64,
    security: &mut ChatSecurity,
) -> Result<(), ChatSessionError> {
    validate_outgoing_chat(message)?;
    tracing::info!(target: "chat", message, "sending outgoing chat plaintext");
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ChatSessionError::InvalidSystemClock)?;
    let timestamp = i64::try_from(duration.as_millis()).unwrap_or(i64::MAX);
    *salt_counter = salt_counter.wrapping_add(1);
    let salt = match security {
        ChatSecurity::UnsignedDevelopment => timestamp ^ *salt_counter,
        ChatSecurity::Authenticated(_) => random_salt(),
    };
    let (signature, last_seen) = match security {
        ChatSecurity::UnsignedDevelopment => (
            None,
            v775::ChatLastSeenUpdate::empty_with_disabled_checksum(),
        ),
        ChatSecurity::Authenticated(session) => {
            let prepared = session.prepare_outgoing(message, timestamp, salt)?;
            (Some(prepared.signature), prepared.last_seen_update)
        }
    };
    let packet = v775::encode_play_chat_message(message, timestamp, salt, signature, last_seen)?;
    write(connection, packet, "Play Chat Message write").await
}

impl From<SecureChatError> for ChatSessionError {
    fn from(error: SecureChatError) -> Self {
        Self::SecureChat(error.to_string())
    }
}

impl From<crate::world_adapter::WorldAdapterError> for ChatSessionError {
    fn from(error: crate::world_adapter::WorldAdapterError) -> Self {
        Self::WorldAdapter(error.to_string())
    }
}

async fn write_session_update(
    connection: &mut crate::connection::MinecraftConnection,
    session: &SecureChatSession,
) -> Result<(), ChatSessionError> {
    let expires_at = system_time_millis(session.certificate().expires_at())?;
    let packet = v775::encode_play_chat_session_update(
        session.session_id(),
        expires_at,
        session.certificate().public_key_der(),
        session.certificate().public_key_signature(),
    )?;
    write(connection, packet, "Player Chat Session Update write").await
}

fn random_session_uuid() -> cubic_protocol::ProtocolUuid {
    let mut bytes = [0_u8; 16];
    OsRng.fill_bytes(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    cubic_protocol::ProtocolUuid::from_bytes(bytes)
}

fn random_salt() -> i64 {
    let mut bytes = [0_u8; 8];
    OsRng.fill_bytes(&mut bytes);
    i64::from_ne_bytes(bytes)
}

fn validate_outgoing_chat(message: &str) -> Result<(), ChatSessionError> {
    if message.is_empty() {
        return Err(ChatSessionError::InvalidMessage {
            reason: "the message is empty",
        });
    }
    if message.starts_with('/') {
        return Err(ChatSessionError::CommandsUnsupported);
    }
    if message.chars().any(char::is_control) {
        return Err(ChatSessionError::InvalidMessage {
            reason: "control characters are not permitted",
        });
    }
    if message.encode_utf16().count() > v775::MAX_CHAT_UTF16_UNITS {
        return Err(ChatSessionError::InvalidMessage {
            reason: "the message exceeds 256 Java UTF-16 units",
        });
    }
    Ok(())
}

async fn write(
    connection: &mut crate::connection::MinecraftConnection,
    packet: Vec<u8>,
    operation: &'static str,
) -> Result<(), ChatSessionError> {
    connection
        .write_all(&packet, operation)
        .await
        .map_err(transport)
}

fn message_event(
    kind: ChatMessageKind,
    sender: Option<String>,
    component: TextComponent,
    trust: ChatMessageTrust,
) -> ChatEvent {
    ChatEvent::Message {
        kind,
        sender,
        message: ChatMessage {
            plain_text: component.plain_text,
            structured: structured(&component.value),
            trust,
        },
    }
}

fn structured(tag: &NbtTag) -> StructuredText {
    match tag {
        NbtTag::Byte(value) => StructuredText::Boolean(*value != 0),
        NbtTag::Short(value) => StructuredText::Number(f64::from(*value)),
        NbtTag::Int(value) => StructuredText::Number(f64::from(*value)),
        NbtTag::Long(value) => StructuredText::Number(*value as f64),
        NbtTag::Float(value) => StructuredText::Number(f64::from(*value)),
        NbtTag::Double(value) => StructuredText::Number(*value),
        NbtTag::String(value) => StructuredText::String(value.to_string_lossy()),
        NbtTag::List(list) => {
            StructuredText::List(list.elements().iter().map(structured).collect())
        }
        NbtTag::Compound(compound) => StructuredText::Compound(structured_compound(compound)),
        NbtTag::ByteArray(_) | NbtTag::IntArray(_) | NbtTag::LongArray(_) => {
            StructuredText::Unsupported
        }
    }
}

fn structured_compound(compound: &NbtCompound) -> BTreeMap<String, StructuredText> {
    compound
        .iter()
        .map(|(key, value)| (key.to_string_lossy(), structured(value)))
        .collect()
}

fn transport(error: ConnectionError) -> ChatSessionError {
    ChatSessionError::Transport(error.to_string())
}

fn map_send_error(error: mpsc::error::TrySendError<ChatSessionCommand>) -> ChatSessionSendError {
    match error {
        mpsc::error::TrySendError::Full(_) => ChatSessionSendError::Full,
        mpsc::error::TrySendError::Closed(_) => ChatSessionSendError::Closed,
    }
}

impl ChatSessionRunner {
    fn event(&self, event: ChatEvent) -> bool {
        match self.events.try_send(event) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.dropped_events.fetch_add(1, Ordering::Relaxed);
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }

    fn critical(&self, event: ChatEvent) {
        if let Ok(mut slot) = self.critical_event.lock() {
            *slot = Some(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{str::FromStr, time::Duration};

    use cubic_auth::AuthError;
    use cubic_protocol::{CodecReader, FrameDecoder, FrameLimits, split_raw_packet};
    use tokio::{
        io::AsyncReadExt,
        net::{TcpListener, TcpStream},
    };

    use crate::{
        secure_chat::{ChatCertificate, SecureChatRules},
        world_movement::WorldControlHandle,
        world_render::WorldRenderHandle,
    };

    struct SyntheticCertificate;

    impl ChatCertificate for SyntheticCertificate {
        fn public_key_der(&self) -> &[u8] {
            &[0xaa, 0xbb]
        }

        fn public_key_signature(&self) -> &[u8] {
            &[0xcc]
        }

        fn expires_at(&self) -> SystemTime {
            SystemTime::now() + Duration::from_secs(60)
        }

        fn is_expired(&self, _now: SystemTime) -> bool {
            false
        }

        fn sign_chat(&self, _input: &[u8]) -> Result<[u8; 256], AuthError> {
            Ok([0x5a; 256])
        }
    }

    fn initial_login() -> v775::InitialPlayLogin {
        v775::InitialPlayLogin {
            player_entity_id: 7,
            hardcore: false,
            known_dimensions: vec!["minecraft:overworld".to_owned()],
            max_players: 20,
            view_distance: 10,
            simulation_distance: 10,
            reduced_debug_info: false,
            show_death_screen: true,
            limited_crafting: false,
            spawn: v775::SpawnInfo {
                dimension_type_raw_id: 0,
                dimension: "minecraft:overworld".to_owned(),
                hashed_seed: 0,
                game_mode: 0,
                previous_game_mode: u8::MAX,
                debug_world: false,
                flat_world: false,
                last_death_location: None,
                portal_cooldown_ticks: 0,
                sea_level: 63,
            },
            secure_chat_enforced: true,
        }
    }

    fn dimension_types() -> Vec<cubic_world::RuntimeDimensionType> {
        vec![cubic_world::RuntimeDimensionType {
            raw_id: 0,
            identifier: cubic_version::MinecraftIdentifier::new("minecraft:overworld").unwrap(),
            geometry: cubic_world::DimensionGeometry {
                min_y: -64,
                height: 384,
            },
        }]
    }

    async fn read_test_frame(stream: &mut TcpStream, decoder: &mut FrameDecoder) -> Vec<u8> {
        let mut buffer = [0_u8; 1024];
        loop {
            if let Some(frame) = decoder.next_frame().unwrap() {
                return frame;
            }
            let read = stream.read(&mut buffer).await.unwrap();
            assert_ne!(read, 0, "test connection closed before a complete frame");
            decoder.push(&buffer[..read]).unwrap();
        }
    }

    #[test]
    fn event_channel_is_bounded_and_reports_drops() {
        let options = ChatSessionOptions {
            event_capacity: 1,
            ..ChatSessionOptions::default()
        };
        let (mut handle, runner) = ChatSessionHandle::bounded(&options);
        runner.event(ChatEvent::Connected);
        assert!(!runner.event(ChatEvent::Warning("dropped".to_owned())));
        assert_eq!(handle.try_next_event(), Some(ChatEvent::Connected));
        assert_eq!(handle.dropped_event_count(), 1);
    }

    #[test]
    fn predicted_camera_is_not_rewound_by_generic_authoritative_updates() {
        let mut world = WorldState::default();
        world.apply(WorldEvent::BeginConfiguration).unwrap();
        world
            .apply(WorldEvent::RuntimeDimensionTypes(dimension_types()))
            .unwrap();
        world
            .apply(crate::world_adapter::initial_world_event(initial_login()).unwrap())
            .unwrap();
        world
            .apply(WorldEvent::SynchronizePlayerPosition(
                cubic_world::PlayerPositionUpdate {
                    teleport_id: 1,
                    x: 1.0,
                    y: 70.0,
                    z: 2.0,
                    yaw: 0.0,
                    pitch: 0.0,
                    relative: cubic_world::RelativeTransformFlags::default(),
                },
            ))
            .unwrap();

        let (mut handle, runner) = WorldRenderHandle::new();
        let predicted = cubic_world::LocalPlayerPose::new(8.0, 70.0, 9.0, 45.0, 5.0);
        runner.pose(predicted);
        assert_eq!(
            handle.take_update().unwrap().pose.map(|sample| sample.pose),
            Some(predicted)
        );

        for _ in 0..3 {
            publish_authoritative_pose(Some(&runner), &world, RenderPoseAuthority::LocalPrediction);
            assert!(handle.take_update().is_none());
        }

        let (_controls, control_runner) = WorldControlHandle::new();
        let mut controller =
            WorldMovementController::new(control_runner, BlockCollisionProfile::synthetic([]), 7);
        let corrected = controller
            .reconcile(
                v775::PlayerPosition {
                    teleport_id: 2,
                    x: 7.5,
                    y: 70.0,
                    z: 8.5,
                    delta_x: 0.0,
                    delta_y: 0.0,
                    delta_z: 0.0,
                    yaw: 40.0,
                    pitch: 4.0,
                    relative_flags: 0,
                },
                cubic_world::AuthoritativeTransform {
                    teleport_id: 2,
                    x: 7.5,
                    y: 70.0,
                    z: 8.5,
                    yaw: 40.0,
                    pitch: 4.0,
                },
                &world,
            )
            .unwrap();
        runner.pose(corrected);
        assert_eq!(
            handle.take_update().unwrap().pose.map(|sample| sample.pose),
            Some(corrected)
        );
        assert!(handle.take_update().is_none());
    }

    #[test]
    fn critical_disconnect_survives_a_full_event_queue() {
        let options = ChatSessionOptions {
            event_capacity: 1,
            ..ChatSessionOptions::default()
        };
        let (handle, runner) = ChatSessionHandle::bounded(&options);
        runner.event(ChatEvent::Connected);
        runner.critical(ChatEvent::Disconnected {
            reason: "bye".to_owned(),
        });
        assert_eq!(
            handle.take_critical_event(),
            Some(ChatEvent::Disconnected {
                reason: "bye".to_owned()
            })
        );
    }

    #[test]
    fn safety_alert_survives_a_full_event_queue() {
        let options = ChatSessionOptions {
            event_capacity: 1,
            ..ChatSessionOptions::default()
        };
        let (handle, runner) = ChatSessionHandle::bounded(&options);
        runner.event(ChatEvent::Connected);
        runner.critical(safety_alert(
            SafetyAlertKind::LowHealth,
            "Health is critically low",
        ));
        assert_eq!(
            handle.take_critical_event(),
            Some(ChatEvent::SafetyAlert {
                kind: SafetyAlertKind::LowHealth,
                message: "Health is critically low".to_owned(),
            })
        );
    }

    #[tokio::test]
    async fn world_mode_ends_each_protocol_tick_even_before_prediction_is_seeded() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let accept = tokio::spawn(async move { listener.accept().await.unwrap().0 });
        let address = ServerAddress::from_str(&format!("127.0.0.1:{port}")).unwrap();
        let limits = FrameLimits::new(v775::MAX_BOOTSTRAP_FRAME_SIZE, 4 * 1024 * 1024).unwrap();
        let mut connection = crate::connection::MinecraftConnection::connect(
            &address,
            Duration::from_secs(1),
            Duration::from_secs(1),
            limits,
        )
        .await
        .unwrap();
        let mut server = accept.await.unwrap();
        let options = ChatSessionOptions::default();
        let (handle, runner) = ChatSessionHandle::bounded(&options);
        let (_controls, control_runner) = WorldControlHandle::new();
        let session = tokio::spawn(async move {
            run_play_session(
                &mut connection,
                ChatSecurity::UnsignedDevelopment,
                runner,
                initial_login(),
                dimension_types(),
                None,
                Some((control_runner, BlockCollisionProfile::synthetic([]))),
            )
            .await
        });

        let mut decoder = FrameDecoder::new(limits);
        assert_eq!(
            split_raw_packet(&read_test_frame(&mut server, &mut decoder).await)
                .unwrap()
                .id,
            0x0e
        );
        assert_eq!(
            split_raw_packet(&read_test_frame(&mut server, &mut decoder).await)
                .unwrap()
                .id,
            0x0d
        );

        handle.disconnect().unwrap();
        session.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn blocking_chunk_work_isolated_from_the_fixed_tick_timer() {
        let mut blocked_interval = tokio::time::interval(Duration::from_millis(10));
        blocked_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        blocked_interval.tick().await;
        std::thread::sleep(Duration::from_millis(120));
        let blocked_scheduled = blocked_interval.tick().await;
        let blocked_lateness =
            tokio::time::Instant::now().saturating_duration_since(blocked_scheduled);
        assert!(blocked_lateness >= Duration::from_millis(90));

        let mut isolated_interval = tokio::time::interval(Duration::from_millis(10));
        isolated_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        isolated_interval.tick().await;
        let mut work =
            tokio::task::spawn_blocking(|| std::thread::sleep(Duration::from_millis(120)));
        let mut serviced = 0_u32;
        let mut maximum_lateness = Duration::ZERO;
        loop {
            tokio::select! {
                result = &mut work => {
                    result.unwrap();
                    break;
                }
                scheduled = isolated_interval.tick() => {
                    serviced += 1;
                    maximum_lateness = maximum_lateness.max(
                        tokio::time::Instant::now().saturating_duration_since(scheduled)
                    );
                }
            }
        }
        assert!(serviced >= 5, "serviced only {serviced} fixed ticks");
        assert!(
            maximum_lateness < Duration::from_millis(80),
            "isolated maximum lateness was {maximum_lateness:?}"
        );
        eprintln!(
            "synthetic fixed-tick lateness: blocking={blocked_lateness:?} isolated_max={maximum_lateness:?} serviced={serviced}"
        );
    }

    #[test]
    fn outgoing_chat_policy_is_explicit_and_unicode_aware() {
        assert!(validate_outgoing_chat("hello").is_ok());
        assert!(validate_outgoing_chat(&"😀".repeat(128)).is_ok());
        assert!(matches!(
            validate_outgoing_chat("/say hello"),
            Err(ChatSessionError::CommandsUnsupported)
        ));
        for invalid in ["", "bad\nline", &"x".repeat(257)] {
            assert!(matches!(
                validate_outgoing_chat(invalid),
                Err(ChatSessionError::InvalidMessage { .. })
            ));
        }
    }

    #[tokio::test]
    async fn authenticated_play_loop_sends_session_and_signed_chat_then_closes_cleanly() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let accept = tokio::spawn(async move { listener.accept().await.unwrap().0 });
        let address = ServerAddress::from_str(&format!("127.0.0.1:{port}")).unwrap();
        let limits = FrameLimits::new(v775::MAX_BOOTSTRAP_FRAME_SIZE, 4 * 1024 * 1024).unwrap();
        let mut connection = crate::connection::MinecraftConnection::connect(
            &address,
            Duration::from_secs(1),
            Duration::from_secs(1),
            limits,
        )
        .await
        .unwrap();
        let mut server = accept.await.unwrap();

        let options = ChatSessionOptions::default();
        let (mut handle, runner) = ChatSessionHandle::bounded(&options);
        let security = ChatSecurity::Authenticated(Box::new(SecureChatSession::with_certificate(
            SecureChatRules::new(1, 20, 64),
            Box::new(SyntheticCertificate),
            cubic_protocol::ProtocolUuid::from_u128(1),
            cubic_protocol::ProtocolUuid::from_u128(2),
        )));
        let session = tokio::spawn(async move {
            run_play_session(
                &mut connection,
                security,
                runner,
                initial_login(),
                dimension_types(),
                None,
                None,
            )
            .await
        });

        let mut decoder = FrameDecoder::new(limits);
        assert_eq!(
            split_raw_packet(&read_test_frame(&mut server, &mut decoder).await)
                .unwrap()
                .id,
            0x0e
        );
        let update = read_test_frame(&mut server, &mut decoder).await;
        let update = split_raw_packet(&update).unwrap();
        assert_eq!(update.id, 0x0a);
        let mut update_reader = CodecReader::new(update.payload);
        assert_eq!(update_reader.read_uuid().unwrap().as_u128(), 2);
        assert!(update_reader.read_i64().unwrap() > 0);
        assert_eq!(update_reader.read_byte_array(512).unwrap(), [0xaa, 0xbb]);
        assert_eq!(update_reader.read_byte_array(4096).unwrap(), [0xcc]);
        assert_eq!(update_reader.remaining(), 0);

        assert_eq!(handle.try_next_event(), Some(ChatEvent::Connected));
        handle.try_send_message("signed smoke".to_owned()).unwrap();
        let chat = read_test_frame(&mut server, &mut decoder).await;
        let chat = split_raw_packet(&chat).unwrap();
        assert_eq!(chat.id, 0x09);
        let mut chat_reader = CodecReader::new(chat.payload);
        assert_eq!(
            chat_reader
                .read_string(cubic_protocol::StringLimits::new(256, 768))
                .unwrap(),
            "signed smoke"
        );
        let _timestamp = chat_reader.read_i64().unwrap();
        let _salt = chat_reader.read_i64().unwrap();
        assert!(chat_reader.read_bool().unwrap());
        assert_eq!(
            chat_reader.read_bytes(256, "test signature").unwrap(),
            [0x5a; 256]
        );
        assert_eq!(chat_reader.read_var_int().unwrap(), 0);
        assert_eq!(chat_reader.read_bytes(3, "test last seen").unwrap(), [0; 3]);
        assert_eq!(chat_reader.read_u8().unwrap(), 1);
        assert_eq!(chat_reader.remaining(), 0);

        handle.disconnect().unwrap();
        session.await.unwrap().unwrap();
    }

    #[test]
    fn presentation_mode_changes_without_replacing_session_channels() {
        let (mut handle, runner) = ChatSessionHandle::bounded(&ChatSessionOptions::default());
        let identity = Arc::as_ptr(&handle.critical_event);
        assert_eq!(runner.presentation_mode(), SessionPresentationMode::Chat);
        handle.set_presentation_mode(SessionPresentationMode::Play);
        assert_eq!(runner.presentation_mode(), SessionPresentationMode::Play);
        handle.set_presentation_mode(SessionPresentationMode::Chat);
        assert_eq!(runner.presentation_mode(), SessionPresentationMode::Chat);
        assert_eq!(identity, Arc::as_ptr(&handle.critical_event));
        assert!(handle.try_next_event().is_none());
    }

    #[test]
    fn safety_alerts_are_transition_based_and_do_not_spam() {
        let mut tracker = SessionSafetyTracker::default();
        assert!(tracker.health_alerts(20.0).is_empty());
        let damage = tracker.health_alerts(10.0);
        assert_eq!(damage.len(), 1);
        assert!(matches!(
            damage[0],
            ChatEvent::SafetyAlert {
                kind: SafetyAlertKind::Damage,
                ..
            }
        ));
        let low = tracker.health_alerts(6.0);
        assert_eq!(low.len(), 2, "damage and low-health crossing are distinct");
        assert_eq!(tracker.health_alerts(6.0).len(), 0);
        let death = tracker.health_alerts(0.0);
        assert!(death.iter().any(|event| matches!(
            event,
            ChatEvent::SafetyAlert {
                kind: SafetyAlertKind::Death,
                ..
            }
        )));
        assert!(tracker.health_alerts(0.0).is_empty());
        assert!(tracker.air_alert(300).is_none());
        assert!(matches!(
            tracker.air_alert(60),
            Some(ChatEvent::SafetyAlert {
                kind: SafetyAlertKind::Drowning,
                ..
            })
        ));
        assert!(tracker.air_alert(40).is_none());
        assert!(tracker.air_alert(300).is_none());
        assert!(tracker.air_alert(59).is_some());
    }

    #[test]
    fn displacement_warning_ignores_reconciliation_jitter() {
        let origin = cubic_world::LocalPlayerPose::new(0.0, 64.0, 0.0, 0.0, 0.0);
        assert!(!significant_displacement(
            origin,
            cubic_world::LocalPlayerPose::new(0.05, 64.01, -0.03, 90.0, 20.0)
        ));
        assert!(significant_displacement(
            origin,
            cubic_world::LocalPlayerPose::new(8.0, 64.0, 0.0, 0.0, 0.0)
        ));
    }
}
