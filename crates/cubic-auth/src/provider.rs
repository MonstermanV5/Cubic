use std::{fmt, future::Future, str::FromStr};

use serde::{Deserialize, Serialize};

use crate::{AuthError, AuthenticatedMinecraftAccount};

/// Selects an authentication protocol without changing the account model used by networking.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthBackend {
    /// Cubic's own Microsoft Entra public-client registration.
    #[default]
    CubicEntra,
    /// Experimental interoperability with the first-party Xbox/Minecraft launcher XAL flow.
    XalInterop,
}

impl AuthBackend {
    #[cfg(windows)]
    pub(crate) const fn credential_account(self) -> &'static str {
        match self {
            Self::CubicEntra => "default",
            Self::XalInterop => "xal-interop-account",
        }
    }

    #[must_use]
    pub const fn cli_name(self) -> &'static str {
        match self {
            Self::CubicEntra => "cubic-entra",
            Self::XalInterop => "xal",
        }
    }
}

impl fmt::Display for AuthBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.cli_name())
    }
}

impl FromStr for AuthBackend {
    type Err = AuthError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "cubic-entra" | "entra" => Ok(Self::CubicEntra),
            "xal" | "xal-interop" => Ok(Self::XalInterop),
            _ => Err(AuthError::InvalidBackend(value.to_owned())),
        }
    }
}

/// Provider-neutral capability needed by Minecraft's encrypted Login flow.
pub trait MinecraftSessionJoiner: Send + Sync {
    fn join_server<'a>(
        &'a self,
        account: &'a AuthenticatedMinecraftAccount,
        server_hash: &'a str,
    ) -> impl Future<Output = Result<(), AuthError>> + Send + 'a;
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::AuthBackend;

    #[test]
    fn provider_selection_is_explicit_and_stable() {
        assert_eq!(AuthBackend::default(), AuthBackend::CubicEntra);
        assert_eq!(
            AuthBackend::from_str("cubic-entra").unwrap(),
            AuthBackend::CubicEntra
        );
        assert_eq!(
            AuthBackend::from_str("xal").unwrap(),
            AuthBackend::XalInterop
        );
        assert!(AuthBackend::from_str("another-launcher").is_err());
    }
}
