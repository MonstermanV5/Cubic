use std::{fmt, str::FromStr, time::SystemTime};

use serde::{Deserialize, Serialize};

use crate::{AuthBackend, AuthError, SecretString};

const MAX_PROFILE_NAME_BYTES: usize = 16;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MicrosoftClientId(String);

impl MicrosoftClientId {
    pub fn new(value: impl Into<String>) -> Result<Self, AuthError> {
        let value = value.into();
        let valid = value.len() == 36
            && value.bytes().enumerate().all(|(index, byte)| {
                matches!(index, 8 | 13 | 18 | 23) && byte == b'-'
                    || !matches!(index, 8 | 13 | 18 | 23) && byte.is_ascii_hexdigit()
            });
        if !valid {
            return Err(AuthError::InvalidClientId);
        }
        Ok(Self(value.to_ascii_lowercase()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MicrosoftClientId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct MinecraftProfileId([u8; 16]);

impl MinecraftProfileId {
    #[must_use]
    pub const fn bytes(self) -> [u8; 16] {
        self.0
    }

    #[must_use]
    pub fn compact(self) -> String {
        self.0.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

impl FromStr for MinecraftProfileId {
    type Err = AuthError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let compact: String = value
            .chars()
            .filter(|character| *character != '-')
            .collect();
        if compact.len() != 32 || !compact.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(AuthError::InvalidMinecraftProfile);
        }
        let mut bytes = [0_u8; 16];
        for (index, slot) in bytes.iter_mut().enumerate() {
            let start = index * 2;
            let pair = compact
                .get(start..start + 2)
                .ok_or(AuthError::InvalidMinecraftProfile)?;
            *slot = u8::from_str_radix(pair, 16).map_err(|_| AuthError::InvalidMinecraftProfile)?;
        }
        Ok(Self(bytes))
    }
}

impl fmt::Display for MinecraftProfileId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, byte) in self.0.iter().enumerate() {
            if matches!(index, 4 | 6 | 8 | 10) {
                formatter.write_str("-")?;
            }
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MinecraftProfile {
    pub id: MinecraftProfileId,
    pub name: String,
}

impl MinecraftProfile {
    pub(crate) fn validate(self) -> Result<Self, AuthError> {
        if self.name.is_empty()
            || self.name.len() > MAX_PROFILE_NAME_BYTES
            || !self
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err(AuthError::InvalidMinecraftProfile);
        }
        Ok(self)
    }
}

#[derive(Debug)]
pub struct AuthenticatedMinecraftAccount {
    pub backend: AuthBackend,
    pub profile: MinecraftProfile,
    pub minecraft_access_token: SecretString,
    pub expires_at: SystemTime,
    pub refresh_token: SecretString,
}

#[cfg(test)]
mod tests {
    use super::{MicrosoftClientId, MinecraftProfile, MinecraftProfileId};
    use std::str::FromStr;

    #[test]
    fn client_id_and_profile_uuid_are_strict() {
        assert!(MicrosoftClientId::new("00000000-1111-2222-3333-444444444444").is_ok());
        assert!(MicrosoftClientId::new("borrowed-launcher-id").is_err());
        let id = MinecraftProfileId::from_str("0123456789abcdef0123456789abcdef").unwrap();
        assert_eq!(id.to_string(), "01234567-89ab-cdef-0123-456789abcdef");
        assert!(MinecraftProfileId::from_str("not-a-profile").is_err());
    }

    #[test]
    fn profile_names_are_validated_independently_of_provider() {
        let id = MinecraftProfileId::from_str("0123456789abcdef0123456789abcdef").unwrap();
        assert!(
            MinecraftProfile {
                id,
                name: "Valid_Name".into()
            }
            .validate()
            .is_ok()
        );
        assert!(
            MinecraftProfile {
                id,
                name: "invalid name".into()
            }
            .validate()
            .is_err()
        );
    }
}
