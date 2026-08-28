use std::time::Duration;

use cubic_auth::{AuthError, AuthenticatedMinecraftAccount, MinecraftSessionJoiner};
use cubic_protocol::{
    FrameLimits, ProtocolUuid,
    bootstrap::v775::{
        self, ClientInformation, LoginClientbound, MAX_BOOTSTRAP_FRAME_SIZE,
        MAX_CONFIGURATION_BUFFERED_BYTES,
    },
    handshake::{Handshake, HandshakeNextState, encode_handshake},
};
use thiserror::Error;
use tokio::time::timeout;

use crate::{
    ConnectionState, ServerAddress,
    connection::MinecraftConnection,
    development_login::{profile::DevLoginProtocolProfile, run_configuration, transition},
    online_crypto::{OnlineCryptoError, minecraft_server_hash, prepare_encryption},
};

const MAX_LOGIN_PACKETS: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthenticatedLoginOptions {
    pub connect_timeout: Duration,
    pub io_timeout: Duration,
    pub overall_timeout: Duration,
}

impl Default for AuthenticatedLoginOptions {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(5),
            io_timeout: Duration::from_secs(10),
            overall_timeout: Duration::from_secs(45),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthenticatedLoginResult {
    pub address: ServerAddress,
    pub profile_name: String,
    pub profile_uuid: ProtocolUuid,
    pub state: ConnectionState,
    pub compression_enabled: bool,
}

#[derive(Debug, Error)]
pub enum AuthenticatedLoginError {
    #[error("invalid protocol-775 bootstrap profile")]
    InvalidProfile,
    #[error("authenticated Minecraft transport failed: {0}")]
    Transport(String),
    #[error(transparent)]
    Protocol(#[from] v775::BootstrapProtocolError),
    #[error(transparent)]
    Authentication(#[from] AuthError),
    #[error(transparent)]
    Cryptography(#[from] OnlineCryptoError),
    #[error("server disconnected during authenticated Login: {0}")]
    ServerDisconnect(String),
    #[error("authenticated Login Success profile did not match Minecraft Services")]
    ProfileMismatch,
    #[error("authenticated Login exceeded {0} packets")]
    PacketLimit(usize),
    #[error("authenticated Login/Configuration timed out after {0:?}")]
    OverallTimeout(Duration),
}

pub async fn authenticated_login<J: MinecraftSessionJoiner>(
    address: &ServerAddress,
    account: &AuthenticatedMinecraftAccount,
    session_joiner: &J,
    options: &AuthenticatedLoginOptions,
) -> Result<AuthenticatedLoginResult, AuthenticatedLoginError> {
    match timeout(
        options.overall_timeout,
        establish_authenticated_play_inner(address, account, session_joiner, options),
    )
    .await
    {
        Ok(result) => result.map(|connected| connected.result),
        Err(_) => Err(AuthenticatedLoginError::OverallTimeout(
            options.overall_timeout,
        )),
    }
}

pub(crate) struct AuthenticatedPlayConnection {
    pub(crate) connection: MinecraftConnection,
    pub(crate) result: AuthenticatedLoginResult,
    pub(crate) secure_chat_rules: crate::secure_chat::SecureChatRules,
}

pub(crate) async fn establish_authenticated_play<J: MinecraftSessionJoiner>(
    address: &ServerAddress,
    account: &AuthenticatedMinecraftAccount,
    session_joiner: &J,
    options: &AuthenticatedLoginOptions,
) -> Result<AuthenticatedPlayConnection, AuthenticatedLoginError> {
    match timeout(
        options.overall_timeout,
        establish_authenticated_play_inner(address, account, session_joiner, options),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(AuthenticatedLoginError::OverallTimeout(
            options.overall_timeout,
        )),
    }
}

async fn establish_authenticated_play_inner<J: MinecraftSessionJoiner>(
    address: &ServerAddress,
    account: &AuthenticatedMinecraftAccount,
    session_joiner: &J,
    options: &AuthenticatedLoginOptions,
) -> Result<AuthenticatedPlayConnection, AuthenticatedLoginError> {
    let profile = DevLoginProtocolProfile::protocol_775()
        .map_err(|_| AuthenticatedLoginError::InvalidProfile)?;
    let limits = FrameLimits::new(MAX_BOOTSTRAP_FRAME_SIZE, MAX_CONFIGURATION_BUFFERED_BYTES)
        .map_err(|error| AuthenticatedLoginError::Transport(error.to_string()))?;
    let mut connection =
        MinecraftConnection::connect(address, options.connect_timeout, options.io_timeout, limits)
            .await
            .map_err(|error| AuthenticatedLoginError::Transport(error.to_string()))?;
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
    .map_err(|error| AuthenticatedLoginError::Transport(error.to_string()))?;
    connection
        .write_all(&handshake, "authenticated Handshake write")
        .await
        .map_err(|error| AuthenticatedLoginError::Transport(error.to_string()))?;
    transition(
        &mut state,
        ConnectionState::Handshake,
        ConnectionState::Login,
    )
    .map_err(|error| AuthenticatedLoginError::Transport(error.to_string()))?;
    let expected_uuid = ProtocolUuid::from_u128(u128::from_be_bytes(account.profile.id.bytes()));
    let login_start = v775::encode_login_start(&account.profile.name, expected_uuid)?;
    connection
        .write_all(&login_start, "authenticated Login Start write")
        .await
        .map_err(|error| AuthenticatedLoginError::Transport(error.to_string()))?;

    let mut compression_enabled = false;
    for _ in 0..MAX_LOGIN_PACKETS {
        let frame = connection
            .read_frame("authenticated Login packet read")
            .await
            .map_err(|error| AuthenticatedLoginError::Transport(error.to_string()))?;
        match v775::decode_login_clientbound(&frame)? {
            LoginClientbound::Disconnect { reason_json } => {
                return Err(AuthenticatedLoginError::ServerDisconnect(bounded(
                    reason_json,
                )));
            }
            LoginClientbound::EncryptionRequest(request) => {
                let material = prepare_encryption(request.public_key_der, request.verify_token)?;
                if request.should_authenticate {
                    let hash = minecraft_server_hash(
                        request.server_id,
                        &material.shared_secret,
                        request.public_key_der,
                    );
                    session_joiner.join_server(account, &hash).await?;
                }
                let response = v775::encode_encryption_response(
                    &material.encrypted_secret,
                    &material.encrypted_verify_token,
                )?;
                connection
                    .write_all(&response, "Encryption Response write")
                    .await
                    .map_err(|error| AuthenticatedLoginError::Transport(error.to_string()))?;
                connection
                    .enable_encryption(&material.shared_secret)
                    .map_err(|error| AuthenticatedLoginError::Transport(error.to_string()))?;
            }
            LoginClientbound::SetCompression { threshold } => {
                connection
                    .enable_compression(threshold)
                    .map_err(|error| AuthenticatedLoginError::Transport(error.to_string()))?;
                compression_enabled = true;
            }
            LoginClientbound::PluginRequest { transaction_id, .. } => {
                let response = v775::encode_login_plugin_response(transaction_id)?;
                connection
                    .write_all(&response, "Login Plugin Response write")
                    .await
                    .map_err(|error| AuthenticatedLoginError::Transport(error.to_string()))?;
            }
            LoginClientbound::CookieRequest { key } => {
                let response = v775::encode_login_cookie_response(key)?;
                connection
                    .write_all(&response, "Login Cookie Response write")
                    .await
                    .map_err(|error| AuthenticatedLoginError::Transport(error.to_string()))?;
            }
            LoginClientbound::Success(success) => {
                if success.username != account.profile.name || success.uuid != expected_uuid {
                    return Err(AuthenticatedLoginError::ProfileMismatch);
                }
                connection
                    .write_all(
                        &v775::encode_login_acknowledged()?,
                        "Login Acknowledged write",
                    )
                    .await
                    .map_err(|error| AuthenticatedLoginError::Transport(error.to_string()))?;
                transition(
                    &mut state,
                    ConnectionState::Login,
                    ConnectionState::Configuration,
                )
                .map_err(|error| AuthenticatedLoginError::Transport(error.to_string()))?;
                connection
                    .write_all(
                        &v775::encode_client_information(&ClientInformation::default())?,
                        "Client Information write",
                    )
                    .await
                    .map_err(|error| AuthenticatedLoginError::Transport(error.to_string()))?;
                run_configuration(&mut connection, &mut state)
                    .await
                    .map_err(|error| AuthenticatedLoginError::Transport(error.to_string()))?;
                return Ok(AuthenticatedPlayConnection {
                    connection,
                    secure_chat_rules: profile.secure_chat_rules(),
                    result: AuthenticatedLoginResult {
                        address: address.clone(),
                        profile_name: account.profile.name.clone(),
                        profile_uuid: expected_uuid,
                        state,
                        compression_enabled,
                    },
                });
            }
        }
    }
    Err(AuthenticatedLoginError::PacketLimit(MAX_LOGIN_PACKETS))
}

fn bounded(value: &str) -> String {
    let mut result: String = value
        .chars()
        .filter(|character| !character.is_control())
        .take(512)
        .collect();
    if result.is_empty() {
        result.push_str("<no safe reason>");
    }
    result
}
