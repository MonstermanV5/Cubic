use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

use crate::VersionError;

pub const MAX_VERSION_ID_BYTES: usize = 128;
pub const MAX_COMPATIBILITY_PROFILE_ID_BYTES: usize = 128;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct MinecraftVersionId(String);

impl MinecraftVersionId {
    pub fn new(value: impl Into<String>) -> Result<Self, VersionError> {
        let value = value.into();
        validate_version_id(&value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MinecraftVersionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for MinecraftVersionId {
    type Err = VersionError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for MinecraftVersionId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct CompatibilityProfileId(String);

impl CompatibilityProfileId {
    pub fn new(value: impl Into<String>) -> Result<Self, VersionError> {
        let value = value.into();
        validate_profile_id(&value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CompatibilityProfileId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for CompatibilityProfileId {
    type Err = VersionError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for CompatibilityProfileId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProtocolVersion(i32);

impl ProtocolVersion {
    #[must_use]
    pub const fn new(value: i32) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn value(self) -> i32 {
        self.0
    }
}

impl fmt::Display for ProtocolVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct VersionDataFormatVersion(u32);

impl VersionDataFormatVersion {
    pub const CURRENT: Self = Self(1);

    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn value(self) -> u32 {
        self.0
    }
}

fn validate_version_id(value: &str) -> Result<(), VersionError> {
    if value.is_empty() {
        return Err(VersionError::InvalidVersionId {
            reason: "the value is empty",
        });
    }
    if value.len() > MAX_VERSION_ID_BYTES {
        return Err(VersionError::InvalidVersionId {
            reason: "the value exceeds 128 UTF-8 bytes",
        });
    }
    if value == "." || value == ".." {
        return Err(VersionError::InvalidVersionId {
            reason: "dot path components are forbidden",
        });
    }
    if value.chars().any(char::is_control) {
        return Err(VersionError::InvalidVersionId {
            reason: "control characters are forbidden",
        });
    }
    if value.contains(['/', '\\']) {
        return Err(VersionError::InvalidVersionId {
            reason: "path separators are forbidden",
        });
    }
    if value.contains(['<', '>', ':', '"', '|', '?', '*']) {
        return Err(VersionError::InvalidVersionId {
            reason: "filesystem-reserved characters are forbidden",
        });
    }
    if value.ends_with([' ', '.']) {
        return Err(VersionError::InvalidVersionId {
            reason: "trailing spaces and dots are forbidden",
        });
    }
    if is_windows_device_name(value) {
        return Err(VersionError::InvalidVersionId {
            reason: "reserved Windows device names are forbidden",
        });
    }
    Ok(())
}

fn validate_profile_id(value: &str) -> Result<(), VersionError> {
    if value.is_empty() {
        return Err(VersionError::InvalidCompatibilityProfileId {
            reason: "the value is empty",
        });
    }
    if value.len() > MAX_COMPATIBILITY_PROFILE_ID_BYTES {
        return Err(VersionError::InvalidCompatibilityProfileId {
            reason: "the value exceeds 128 bytes",
        });
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte))
    {
        return Err(VersionError::InvalidCompatibilityProfileId {
            reason: "only lowercase ASCII letters, digits, dots, underscores, and hyphens are allowed",
        });
    }
    Ok(())
}

fn is_windows_device_name(value: &str) -> bool {
    let stem = value.split('.').next().unwrap_or(value);
    let upper = stem.to_ascii_uppercase();
    matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || matches!(
            upper.strip_prefix("COM"),
            Some("1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        )
        || matches!(
            upper.strip_prefix("LPT"),
            Some("1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        )
}
