pub(crate) mod profile;

use std::{fmt, time::Duration};

use cubic_protocol::{
    FrameLimits, ProtocolUuid,
    bootstrap::v775::{
        self, ClientInformation, ConfigurationClientbound, LoginClientbound,
        MAX_BOOTSTRAP_FRAME_SIZE, MAX_CONFIGURATION_BUFFERED_BYTES, PlayClientbound,
    },
    handshake::{Handshake, HandshakeNextState, encode_handshake},
};
use cubic_version::{MinecraftVersionId, ProtocolVersion};
use tokio::time::timeout;

use self::profile::DevLoginProtocolProfile;
use crate::{
    DevelopmentLoginError, ServerAddress, UnsupportedPhase7Feature,
    connection::{ConnectionError, MinecraftConnection},
};

const MAX_LOGIN_PACKETS: usize = 64;
const MAX_CONFIGURATION_PACKETS: usize = 2_048;
const MAX_PLAY_ACCEPTANCE_PACKETS: usize = 256;
const MAX_RECONFIGURATIONS_DURING_ACCEPTANCE: usize = 8;
const MAX_ERROR_PREVIEW_CHARS: usize = 512;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionState {
    Handshake,
    Login,
    Configuration,
    Play,
    Closed,
}

impl ConnectionState {
    const fn name(self) -> &'static str {
        match self {
            Self::Handshake => "Handshake",
            Self::Login => "Login",
            Self::Configuration => "Configuration",
            Self::Play => "Play",
            Self::Closed => "Closed",
        }
    }
}

impl fmt::Display for ConnectionState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DevelopmentUsername(String);

impl DevelopmentUsername {
    pub fn new(value: impl Into<String>) -> Result<Self, DevelopmentLoginError> {
        let value = value.into();
        if value.is_empty() {
            return Err(DevelopmentLoginError::InvalidUsername {
                reason: "the value is empty",
            });
        }
        if value.len() > v775::MAX_USERNAME_UTF16_UNITS {
            return Err(DevelopmentLoginError::InvalidUsername {
                reason: "the value exceeds 16 ASCII characters",
            });
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err(DevelopmentLoginError::InvalidUsername {
                reason: "only ASCII letters, digits, and underscores are permitted",
            });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DevelopmentUsername {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DevelopmentLoginOptions {
    pub connect_timeout: Duration,
    pub io_timeout: Duration,
    pub overall_timeout: Duration,
}

impl Default for DevelopmentLoginOptions {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(3),
            io_timeout: Duration::from_secs(5),
            overall_timeout: Duration::from_secs(20),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DevelopmentLoginResult {
    pub address: ServerAddress,
    pub minecraft_version: MinecraftVersionId,
    pub protocol_version: ProtocolVersion,
    pub username: DevelopmentUsername,
    pub profile_uuid: ProtocolUuid,
    pub state: ConnectionState,
    pub skipped_configuration_packets: usize,
}

pub async fn development_login(
    address: &ServerAddress,
    username: &DevelopmentUsername,
    options: &DevelopmentLoginOptions,
) -> Result<DevelopmentLoginResult, DevelopmentLoginError> {
    match timeout(
        options.overall_timeout,
        connect_to_play_inner(address, username, options),
    )
    .await
    {
        Ok(result) => result.map(|connected| connected.result),
        Err(_) => Err(DevelopmentLoginError::OverallTimeout {
            timeout: options.overall_timeout,
        }),
    }
}

pub(crate) struct ConnectedPlay {
    pub(crate) connection: MinecraftConnection,
    pub(crate) initial_login: v775::InitialPlayLogin,
    pub(crate) result: DevelopmentLoginResult,
}

pub(crate) async fn connect_to_play(
    address: &ServerAddress,
    username: &DevelopmentUsername,
    options: &DevelopmentLoginOptions,
) -> Result<ConnectedPlay, DevelopmentLoginError> {
    match timeout(
        options.overall_timeout,
        connect_to_play_inner(address, username, options),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(DevelopmentLoginError::OverallTimeout {
            timeout: options.overall_timeout,
        }),
    }
}

async fn connect_to_play_inner(
    address: &ServerAddress,
    username: &DevelopmentUsername,
    options: &DevelopmentLoginOptions,
) -> Result<ConnectedPlay, DevelopmentLoginError> {
    let profile = DevLoginProtocolProfile::protocol_775()
        .map_err(DevelopmentLoginError::InvalidBootstrapProfile)?;
    let limits = FrameLimits::new(MAX_BOOTSTRAP_FRAME_SIZE, MAX_CONFIGURATION_BUFFERED_BYTES)
        .map_err(DevelopmentLoginError::Framing)?;
    let mut connection =
        MinecraftConnection::connect(address, options.connect_timeout, options.io_timeout, limits)
            .await
            .map_err(map_connection_error)?;
    let mut state = ConnectionState::Handshake;

    let handshake = encode_handshake(
        &Handshake {
            protocol_version: profile.protocol_version().value(),
            server_address: address.host(),
            server_port: address.port(),
            next_state: HandshakeNextState::Login,
        },
        MAX_BOOTSTRAP_FRAME_SIZE,
    )
    .map_err(DevelopmentLoginError::Framing)?;
    connection
        .write_all(&handshake, "Login Handshake write")
        .await
        .map_err(map_connection_error)?;
    transition(
        &mut state,
        ConnectionState::Handshake,
        ConnectionState::Login,
    )?;

    // The all-zero UUID requests normal offline-mode UUID assignment by the server.
    let login_start = v775::encode_login_start(username.as_str(), ProtocolUuid::from_u128(0))?;
    connection
        .write_all(&login_start, "Login Start write")
        .await
        .map_err(map_connection_error)?;

    let profile_uuid = run_login(&mut connection, username, &mut state).await?;
    let configuration = run_configuration(&mut connection, &mut state).await?;

    Ok(ConnectedPlay {
        connection,
        initial_login: configuration.initial_login,
        result: DevelopmentLoginResult {
            address: address.clone(),
            minecraft_version: profile.minecraft_version().clone(),
            protocol_version: profile.protocol_version(),
            username: username.clone(),
            profile_uuid,
            state,
            skipped_configuration_packets: configuration.skipped_packets,
        },
    })
}

async fn run_login(
    connection: &mut MinecraftConnection,
    username: &DevelopmentUsername,
    state: &mut ConnectionState,
) -> Result<ProtocolUuid, DevelopmentLoginError> {
    for _ in 0..MAX_LOGIN_PACKETS {
        let frame = connection
            .read_frame("Login packet read")
            .await
            .map_err(map_connection_error)?;
        match v775::decode_login_clientbound(&frame)? {
            LoginClientbound::Disconnect { reason_json } => {
                return Err(DevelopmentLoginError::ServerDisconnect {
                    state: state.name(),
                    reason: bounded_preview(reason_json),
                });
            }
            LoginClientbound::EncryptionRequest(_) => {
                return Err(DevelopmentLoginError::UnsupportedForPhase7 {
                    feature: UnsupportedPhase7Feature::Encryption,
                    required_setting: "online-mode=false",
                });
            }
            LoginClientbound::SetCompression { .. } => {
                return Err(DevelopmentLoginError::UnsupportedForPhase7 {
                    feature: UnsupportedPhase7Feature::Compression,
                    required_setting: "network-compression-threshold=-1",
                });
            }
            LoginClientbound::PluginRequest { transaction_id, .. } => {
                let response = v775::encode_login_plugin_response(transaction_id)?;
                connection
                    .write_all(&response, "Login Plugin Response write")
                    .await
                    .map_err(map_connection_error)?;
            }
            LoginClientbound::CookieRequest { key } => {
                let response = v775::encode_login_cookie_response(key)?;
                connection
                    .write_all(&response, "Login Cookie Response write")
                    .await
                    .map_err(map_connection_error)?;
            }
            LoginClientbound::Success(success) => {
                if success.username != username.as_str() {
                    return Err(DevelopmentLoginError::LoginUsernameMismatch {
                        expected: username.as_str().to_owned(),
                        received: success.username.to_owned(),
                    });
                }
                if success.uuid.as_u128() == 0 {
                    return Err(DevelopmentLoginError::InvalidLoginUuid);
                }
                let acknowledged = v775::encode_login_acknowledged()?;
                connection
                    .write_all(&acknowledged, "Login Acknowledged write")
                    .await
                    .map_err(map_connection_error)?;
                transition(
                    state,
                    ConnectionState::Login,
                    ConnectionState::Configuration,
                )?;
                let client_information =
                    v775::encode_client_information(&ClientInformation::default())?;
                connection
                    .write_all(&client_information, "Client Information write")
                    .await
                    .map_err(map_connection_error)?;
                return Ok(success.uuid);
            }
        }
    }
    Err(DevelopmentLoginError::PacketLimitExceeded {
        state: state.name(),
        max_packets: MAX_LOGIN_PACKETS,
    })
}

pub(crate) struct ConfigurationOutcome {
    pub(crate) skipped_packets: usize,
    pub(crate) initial_login: v775::InitialPlayLogin,
}

pub(crate) async fn run_configuration(
    connection: &mut MinecraftConnection,
    state: &mut ConnectionState,
) -> Result<ConfigurationOutcome, DevelopmentLoginError> {
    let mut skipped_packets = 0_usize;
    for _ in 0..=MAX_RECONFIGURATIONS_DURING_ACCEPTANCE {
        skipped_packets =
            skipped_packets.saturating_add(run_configuration_phase(connection, state).await?);
        match await_initial_play_login(connection, state).await? {
            PlayAcceptance::Login(initial_login) => {
                return Ok(ConfigurationOutcome {
                    skipped_packets,
                    initial_login,
                });
            }
            PlayAcceptance::Reconfigure => {}
        }
    }
    Err(DevelopmentLoginError::PacketLimitExceeded {
        state: "Play/Configuration transitions",
        max_packets: MAX_RECONFIGURATIONS_DURING_ACCEPTANCE,
    })
}

async fn run_configuration_phase(
    connection: &mut MinecraftConnection,
    state: &mut ConnectionState,
) -> Result<usize, DevelopmentLoginError> {
    let mut skipped_packets = 0_usize;
    for _ in 0..MAX_CONFIGURATION_PACKETS {
        let frame = connection
            .read_frame("Configuration packet read")
            .await
            .map_err(map_connection_error)?;
        match v775::decode_configuration_clientbound(&frame)? {
            ConfigurationClientbound::CookieRequest { key } => {
                let response = v775::encode_configuration_cookie_response(key)?;
                connection
                    .write_all(&response, "Configuration Cookie Response write")
                    .await
                    .map_err(map_connection_error)?;
            }
            ConfigurationClientbound::CustomPayload { .. }
            | ConfigurationClientbound::Skipped { .. } => {
                skipped_packets = skipped_packets.saturating_add(1);
            }
            ConfigurationClientbound::Disconnect { reason } => {
                let text = reason.get_string("text").map_or_else(
                    || "<structured NBT reason>".to_owned(),
                    |value| value.to_string_lossy(),
                );
                return Err(DevelopmentLoginError::ServerDisconnect {
                    state: state.name(),
                    reason: bounded_preview(&text),
                });
            }
            ConfigurationClientbound::Finish => {
                let response = v775::encode_finish_configuration()?;
                connection
                    .write_all(&response, "Finish Configuration acknowledgement write")
                    .await
                    .map_err(map_connection_error)?;
                transition(state, ConnectionState::Configuration, ConnectionState::Play)?;
                return Ok(skipped_packets);
            }
            ConfigurationClientbound::KeepAlive { id } => {
                let response = v775::encode_configuration_keep_alive(id)?;
                connection
                    .write_all(&response, "Configuration Keep Alive response write")
                    .await
                    .map_err(map_connection_error)?;
            }
            ConfigurationClientbound::Ping { id } => {
                let response = v775::encode_configuration_pong(id)?;
                connection
                    .write_all(&response, "Configuration Pong write")
                    .await
                    .map_err(map_connection_error)?;
            }
            ConfigurationClientbound::KnownPacks(_) => {
                let response = v775::encode_known_packs_response_empty()?;
                connection
                    .write_all(&response, "Known Packs response write")
                    .await
                    .map_err(map_connection_error)?;
            }
            ConfigurationClientbound::ResourcePackPush => {
                return Err(DevelopmentLoginError::UnsupportedForPhase7 {
                    feature: UnsupportedPhase7Feature::ResourcePack,
                    required_setting: "resource-pack= and require-resource-pack=false",
                });
            }
            ConfigurationClientbound::Transfer => {
                return Err(DevelopmentLoginError::UnsupportedForPhase7 {
                    feature: UnsupportedPhase7Feature::Transfer,
                    required_setting: "connect directly to the development server",
                });
            }
            ConfigurationClientbound::CodeOfConduct => {
                return Err(DevelopmentLoginError::UnsupportedForPhase7 {
                    feature: UnsupportedPhase7Feature::CodeOfConduct,
                    required_setting: "enable-code-of-conduct=false",
                });
            }
        }
    }
    Err(DevelopmentLoginError::PacketLimitExceeded {
        state: state.name(),
        max_packets: MAX_CONFIGURATION_PACKETS,
    })
}

enum PlayAcceptance {
    Login(v775::InitialPlayLogin),
    Reconfigure,
}

async fn await_initial_play_login(
    connection: &mut MinecraftConnection,
    state: &mut ConnectionState,
) -> Result<PlayAcceptance, DevelopmentLoginError> {
    let mut sent_player_loaded = false;
    for _ in 0..MAX_PLAY_ACCEPTANCE_PACKETS {
        let frame = connection
            .read_frame("initial Play packet read")
            .await
            .map_err(map_connection_error)?;
        match v775::decode_play_clientbound(&frame)? {
            PlayClientbound::Login(login) => return Ok(PlayAcceptance::Login(login)),
            PlayClientbound::KeepAlive { id } => {
                connection
                    .write_all(
                        &v775::encode_play_keep_alive(id)?,
                        "Play Keep Alive response write",
                    )
                    .await
                    .map_err(map_connection_error)?;
            }
            PlayClientbound::Ping { id } => {
                connection
                    .write_all(&v775::encode_play_pong(id)?, "Play Pong write")
                    .await
                    .map_err(map_connection_error)?;
            }
            PlayClientbound::PlayerPosition(position) => {
                connection
                    .write_all(
                        &v775::encode_play_teleport_confirmation(position.teleport_id)?,
                        "Play Teleport Confirmation write",
                    )
                    .await
                    .map_err(map_connection_error)?;
            }
            PlayClientbound::ChunkBatchFinished { .. } => {
                connection
                    .write_all(
                        &v775::encode_play_chunk_batch_received(1.0)?,
                        "Play Chunk Batch Received write",
                    )
                    .await
                    .map_err(map_connection_error)?;
                if !sent_player_loaded {
                    connection
                        .write_all(
                            &v775::encode_play_player_loaded()?,
                            "Play Player Loaded write",
                        )
                        .await
                        .map_err(map_connection_error)?;
                    sent_player_loaded = true;
                }
            }
            PlayClientbound::CookieRequest { key } => {
                connection
                    .write_all(
                        &v775::encode_play_cookie_response(&key)?,
                        "Play Cookie Response write",
                    )
                    .await
                    .map_err(map_connection_error)?;
            }
            PlayClientbound::Disconnect { reason } => {
                return Err(DevelopmentLoginError::ServerDisconnect {
                    state: state.name(),
                    reason: bounded_preview(&reason.plain_text),
                });
            }
            PlayClientbound::StartConfiguration => {
                connection
                    .write_all(
                        &v775::encode_play_acknowledge_configuration()?,
                        "Play Configuration Acknowledged write",
                    )
                    .await
                    .map_err(map_connection_error)?;
                transition(state, ConnectionState::Play, ConnectionState::Configuration)?;
                connection
                    .write_all(
                        &v775::encode_client_information(&ClientInformation::default())?,
                        "Reconfiguration Client Information write",
                    )
                    .await
                    .map_err(map_connection_error)?;
                return Ok(PlayAcceptance::Reconfigure);
            }
            PlayClientbound::ResourcePackPush => {
                return Err(DevelopmentLoginError::UnsupportedForPhase7 {
                    feature: UnsupportedPhase7Feature::ResourcePack,
                    required_setting: "resource-pack= and require-resource-pack=false",
                });
            }
            PlayClientbound::Transfer => {
                return Err(DevelopmentLoginError::UnsupportedForPhase7 {
                    feature: UnsupportedPhase7Feature::Transfer,
                    required_setting: "connect directly to the development server",
                });
            }
            PlayClientbound::PlayerChat { .. }
            | PlayClientbound::DisguisedChat { .. }
            | PlayClientbound::SystemChat { .. }
            | PlayClientbound::Respawn(_)
            | PlayClientbound::SetDefaultSpawnPosition(_)
            | PlayClientbound::SetTime(_)
            | PlayClientbound::ChangeDifficulty { .. }
            | PlayClientbound::GameEvent { .. }
            | PlayClientbound::InitializeBorder(_)
            | PlayClientbound::Health { .. }
            | PlayClientbound::CustomPayload { .. }
            | PlayClientbound::ChunkBatchStart
            | PlayClientbound::LevelChunkWithLight(_)
            | PlayClientbound::ForgetLevelChunk { .. }
            | PlayClientbound::LightUpdate(_)
            | PlayClientbound::Ignored { .. } => {}
        }
    }
    Err(DevelopmentLoginError::PacketLimitExceeded {
        state: "Play acceptance",
        max_packets: MAX_PLAY_ACCEPTANCE_PACKETS,
    })
}

pub(crate) fn transition(
    state: &mut ConnectionState,
    expected: ConnectionState,
    next: ConnectionState,
) -> Result<(), DevelopmentLoginError> {
    if *state != expected {
        return Err(DevelopmentLoginError::InvalidStateTransition {
            from: state.name(),
            to: next.name(),
        });
    }
    *state = next;
    Ok(())
}

fn bounded_preview(value: &str) -> String {
    let mut preview: String = value.chars().take(MAX_ERROR_PREVIEW_CHARS).collect();
    if value.chars().count() > MAX_ERROR_PREVIEW_CHARS {
        preview.push('…');
    }
    preview
}

fn map_connection_error(error: ConnectionError) -> DevelopmentLoginError {
    match error {
        ConnectionError::ConnectTimeout { timeout } => {
            DevelopmentLoginError::ConnectTimeout { timeout }
        }
        ConnectionError::ConnectFailed { source } => {
            DevelopmentLoginError::ConnectFailed { source }
        }
        ConnectionError::IoTimeout { operation, timeout } => {
            DevelopmentLoginError::IoTimeout { operation, timeout }
        }
        ConnectionError::Io { operation, source } => {
            DevelopmentLoginError::Io { operation, source }
        }
        ConnectionError::PrematureDisconnect {
            phase,
            buffered_bytes,
        } => DevelopmentLoginError::PrematureDisconnect {
            phase,
            buffered_bytes,
        },
        ConnectionError::Framing(error) => DevelopmentLoginError::Framing(error),
        ConnectionError::Transform(error) => DevelopmentLoginError::WireTransform {
            reason: error.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{ConnectionState, DevelopmentUsername, transition};

    #[test]
    fn username_validation_is_deliberately_narrow() {
        assert!(DevelopmentUsername::new("CubicTest_7").is_ok());
        for invalid in ["", "abcdefghijklmnopq", "has space", "José", "dash-name"] {
            assert!(DevelopmentUsername::new(invalid).is_err());
        }
    }

    #[test]
    fn state_transitions_are_explicit() {
        let mut state = ConnectionState::Handshake;
        transition(
            &mut state,
            ConnectionState::Handshake,
            ConnectionState::Login,
        )
        .unwrap();
        assert_eq!(state, ConnectionState::Login);
        assert!(
            transition(
                &mut state,
                ConnectionState::Configuration,
                ConnectionState::Play
            )
            .is_err()
        );
    }
}
