//! Public-client Microsoft and Minecraft authentication primitives.

mod error;
mod http;
mod model;
mod oauth;
mod player_certificate;
mod provider;
mod secret;
mod store;
mod xal;

pub use error::{AuthError, AuthStage};
pub use http::{AuthClient, AuthClientOptions};
pub use model::{
    AuthenticatedMinecraftAccount, MicrosoftClientId, MinecraftProfile, MinecraftProfileId,
};
pub use oauth::{LoopbackAuthorization, OAuthAuthorizationCode, OAuthCallback};
pub use player_certificate::{PlayerCertificate, PlayerCertificateClient};
pub use provider::{AuthBackend, MinecraftSessionJoiner};
pub use secret::SecretString;
pub use store::{CredentialStore, StoredAccount, SystemCredentialStore, XalDeviceCredential};
pub use xal::{
    XalAuthClient, XalAuthorizationCode, XalDeviceIdentity, XalInteractiveAuthorization,
    XalRedirectValidator,
};
