use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use cubic_core::{ChatEvent, ChatMessage, ChatMessageKind, ChatSessionCommand, StructuredText};
use cubic_protocol::{
    bootstrap::v775::{self, ClientInformation, PlayClientbound, TextComponent},
    nbt::{NbtCompound, NbtTag},
};
use thiserror::Error;
use tokio::sync::mpsc;

use crate::{
    DevelopmentLoginError, DevelopmentLoginOptions, DevelopmentUsername, ServerAddress,
    connection::ConnectionError,
    development_login::{ConnectionState, connect_to_play, run_configuration},
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
}

pub struct ChatSessionRunner {
    commands: mpsc::Receiver<ChatSessionCommand>,
    events: mpsc::Sender<ChatEvent>,
    critical_event: Arc<Mutex<Option<ChatEvent>>>,
    dropped_events: Arc<AtomicUsize>,
}

impl ChatSessionHandle {
    #[must_use]
    pub fn bounded(options: &ChatSessionOptions) -> (Self, ChatSessionRunner) {
        let (command_tx, command_rx) = mpsc::channel(options.command_capacity.max(1));
        let (event_tx, event_rx) = mpsc::channel(options.event_capacity.max(1));
        let critical_event = Arc::new(Mutex::new(None));
        let dropped_events = Arc::new(AtomicUsize::new(0));
        (
            Self {
                commands: command_tx,
                events: event_rx,
                critical_event: Arc::clone(&critical_event),
                dropped_events: Arc::clone(&dropped_events),
                channel_closed_reported: false,
            },
            ChatSessionRunner {
                commands: command_rx,
                events: event_tx,
                critical_event,
                dropped_events,
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
    Protocol(#[from] v775::BootstrapProtocolError),
    #[error("persistent Play transport failed: {0}")]
    Transport(String),
    #[error("the system clock is before the Unix epoch")]
    InvalidSystemClock,
    #[error("invalid outgoing chat message: {reason}")]
    InvalidMessage { reason: &'static str },
    #[error(
        "commands are not supported in Phase 8 because signable command arguments are not available"
    )]
    CommandsUnsupported,
}

pub async fn run_development_chat_session(
    address: &ServerAddress,
    username: &DevelopmentUsername,
    options: &ChatSessionOptions,
    mut runner: ChatSessionRunner,
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
    runner.event(ChatEvent::Connected);

    let information = v775::encode_play_client_information(&ClientInformation::default())?;
    connected
        .connection
        .write_all(&information, "Play Client Information write")
        .await
        .map_err(transport)?;

    let mut salt_counter = 0_i64;
    let mut sent_player_loaded = false;

    loop {
        tokio::select! {
            command = runner.commands.recv() => {
                match command {
                    Some(ChatSessionCommand::SendMessage(message)) => {
                        if let Err(error) = send_chat(
                            &mut connected.connection,
                            &message,
                            &mut salt_counter,
                        ).await {
                            runner.event(ChatEvent::Warning(error.to_string()));
                        }
                    }
                    Some(ChatSessionCommand::Disconnect) | None => return Ok(()),
                }
            }
            frame = connected.connection.read_frame_unbounded("persistent Play packet read") => {
                let frame = match frame {
                    Ok(frame) => frame,
                    Err(error) => {
                        let reason = error.to_string();
                        runner.critical(ChatEvent::Disconnected { reason: reason.clone() });
                        return Err(ChatSessionError::Transport(reason));
                    }
                };
                match v775::decode_play_clientbound(&frame)? {
                    PlayClientbound::KeepAlive { id } => {
                        write(&mut connected.connection, v775::encode_play_keep_alive(id)?, "Play Keep Alive response write").await?;
                    }
                    PlayClientbound::Ping { id } => {
                        write(&mut connected.connection, v775::encode_play_pong(id)?, "Play Pong write").await?;
                    }
                    PlayClientbound::PlayerPosition { teleport_id } => {
                        write(&mut connected.connection, v775::encode_play_teleport_confirmation(teleport_id)?, "Play Teleport Confirmation write").await?;
                    }
                    PlayClientbound::ChunkBatchFinished { .. } => {
                        write(&mut connected.connection, v775::encode_play_chunk_batch_received(1.0)?, "Play Chunk Batch Received write").await?;
                        if !sent_player_loaded {
                            write(&mut connected.connection, v775::encode_play_player_loaded()?, "Play Player Loaded write").await?;
                            sent_player_loaded = true;
                        }
                    }
                    PlayClientbound::CookieRequest { key } => {
                        write(&mut connected.connection, v775::encode_play_cookie_response(&key)?, "Play Cookie Response write").await?;
                    }
                    PlayClientbound::PlayerChat {
                        sender_name,
                        message,
                        acknowledgement_required,
                        ..
                    } => {
                        if acknowledgement_required {
                            write(&mut connected.connection, v775::encode_play_chat_acknowledgement(1)?, "Play Chat Acknowledgement write").await?;
                        }
                        runner.event(message_event(ChatMessageKind::Player, Some(sender_name), message));
                    }
                    PlayClientbound::DisguisedChat { sender_name, message } => {
                        runner.event(message_event(ChatMessageKind::Player, Some(sender_name), message));
                    }
                    PlayClientbound::SystemChat { message, overlay } => {
                        let kind = if overlay { ChatMessageKind::ServerNotice } else { ChatMessageKind::System };
                        runner.event(message_event(kind, None, message));
                    }
                    PlayClientbound::Disconnect { reason } => {
                        runner.critical(ChatEvent::Disconnected { reason: reason.plain_text });
                        return Ok(());
                    }
                    PlayClientbound::Health { health } if health <= 6.0 => {
                        runner.event(ChatEvent::Warning(format!("Low health: {health:.1}")));
                    }
                    PlayClientbound::Health { .. } | PlayClientbound::Ignored { .. } => {}
                    PlayClientbound::StartConfiguration => {
                        write(&mut connected.connection, v775::encode_play_acknowledge_configuration()?, "Play Configuration Acknowledged write").await?;
                        write(&mut connected.connection, v775::encode_client_information(&ClientInformation::default())?, "Reconfiguration Client Information write").await?;
                        let mut state = ConnectionState::Configuration;
                        let _skipped = run_configuration(&mut connected.connection, &mut state).await?;
                        runner.event(ChatEvent::Warning("Server reconfiguration completed".to_owned()));
                    }
                }
            }
        }
    }
}

async fn send_chat(
    connection: &mut crate::connection::MinecraftConnection,
    message: &str,
    salt_counter: &mut i64,
) -> Result<(), ChatSessionError> {
    validate_outgoing_chat(message)?;
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ChatSessionError::InvalidSystemClock)?;
    let timestamp = i64::try_from(duration.as_millis()).unwrap_or(i64::MAX);
    *salt_counter = salt_counter.wrapping_add(1);
    let packet = v775::encode_play_chat_message(
        message,
        timestamp,
        timestamp ^ *salt_counter,
        v775::ChatLastSeenUpdate::empty_with_disabled_checksum(),
    )?;
    write(connection, packet, "Play Chat Message write").await
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
) -> ChatEvent {
    ChatEvent::Message {
        kind,
        sender,
        message: ChatMessage {
            plain_text: component.plain_text,
            structured: structured(&component.value),
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
    fn event(&self, event: ChatEvent) {
        match self.events.try_send(event) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.dropped_events.fetch_add(1, Ordering::Relaxed);
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {}
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

    #[test]
    fn event_channel_is_bounded_and_reports_drops() {
        let options = ChatSessionOptions {
            event_capacity: 1,
            ..ChatSessionOptions::default()
        };
        let (mut handle, runner) = ChatSessionHandle::bounded(&options);
        runner.event(ChatEvent::Connected);
        runner.event(ChatEvent::Warning("dropped".to_owned()));
        assert_eq!(handle.try_next_event(), Some(ChatEvent::Connected));
        assert_eq!(handle.dropped_event_count(), 1);
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
}
