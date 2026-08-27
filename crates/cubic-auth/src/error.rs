use std::{io, time::Duration};

use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthStage {
    MicrosoftOAuth,
    XboxUserToken,
    XboxXsts,
    MinecraftToken,
    MinecraftEntitlements,
    MinecraftProfile,
    MinecraftSessionJoin,
    XboxDevice,
    SisuAuthenticate,
    SisuAuthorize,
    MinecraftLauncher,
    SecureStore,
}

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("unknown authentication backend {0:?}; expected cubic-entra or xal")]
    InvalidBackend(String),
    #[error("stored credential belongs to a different authentication backend")]
    BackendMismatch,
    #[error(
        "CUBIC_MSA_CLIENT_ID is not configured; register Cubic as a public native Microsoft application and set its application (client) ID"
    )]
    MissingClientId,
    #[error("the Microsoft application client ID is not a canonical UUID")]
    InvalidClientId,
    #[error("could not bind the OAuth callback listener to IPv4 loopback")]
    CallbackBind(#[source] io::Error),
    #[error("the OAuth callback timed out after {timeout:?}")]
    CallbackTimeout { timeout: Duration },
    #[error("the OAuth callback was malformed")]
    MalformedCallback,
    #[error("the OAuth callback state did not match the pending authorization")]
    StateMismatch,
    #[error("the XAL browser result must be the complete Microsoft desktop redirect URL")]
    InvalidXalCallback,
    #[error("Microsoft authorization was not completed: {code}")]
    OAuthRejected { code: String },
    #[error("authentication transport failed during {stage:?}")]
    Transport {
        stage: AuthStage,
        #[source]
        source: reqwest::Error,
    },
    #[error("{stage:?} returned HTTP {status}: {message}")]
    Http {
        stage: AuthStage,
        status: u16,
        message: String,
    },
    #[error(
        "Minecraft Services rejected Cubic's client ID with HTTP 403 Invalid app registration; Cubic's own app registration requires external approval"
    )]
    InvalidAppRegistration,
    #[error("{stage:?} returned an oversized response (limit {limit} bytes)")]
    ResponseTooLarge { stage: AuthStage, limit: usize },
    #[error("{stage:?} returned malformed or incomplete JSON")]
    InvalidResponse { stage: AuthStage },
    #[error("{stage:?} response omitted required header {name}")]
    MissingHeader {
        stage: AuthStage,
        name: &'static str,
    },
    #[error("Xbox/XSTS rejected the account ({code}): {guidance}")]
    XboxAccount { code: u64, guidance: &'static str },
    #[error("the account does not have a Minecraft Java entitlement")]
    NoJavaEntitlement,
    #[error("Minecraft Services returned an invalid profile")]
    InvalidMinecraftProfile,
    #[error("secure credential storage is unavailable")]
    SecureStoreUnavailable,
    #[error("stored Cubic credentials are corrupt; sign in again")]
    CorruptStoredCredential,
    #[error("this platform has no Phase 9 secure-store implementation yet")]
    SecureStoreUnsupported,
    #[error("the stored XAL device identity is malformed")]
    InvalidXalDevice,
    #[error("XAL request timestamp is outside the supported Windows FILETIME range")]
    InvalidXalTimestamp,
    #[error("XAL request signing failed")]
    XalSigning,
}
