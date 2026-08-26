use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    CompatibilityProfileId, MinecraftVersionId, ProtocolVersion, VersionDataFormatVersion,
    VersionError,
};

pub const MAX_COMPATIBILITY_PROFILES: usize = 32;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum MinecraftVersionKind {
    Release,
    Snapshot,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct VersionData {
    format_version: VersionDataFormatVersion,
    minecraft_version: MinecraftVersionId,
    kind: MinecraftVersionKind,
    protocol: ProtocolVersion,
    compatibility_profiles: Vec<CompatibilityProfileId>,
}

impl VersionData {
    pub fn new(
        minecraft_version: MinecraftVersionId,
        kind: MinecraftVersionKind,
        protocol: ProtocolVersion,
        compatibility_profiles: Vec<CompatibilityProfileId>,
    ) -> Result<Self, VersionError> {
        validate_protocol(protocol)?;
        let compatibility_profiles = normalize_profiles(compatibility_profiles)?;
        Ok(Self {
            format_version: VersionDataFormatVersion::CURRENT,
            minecraft_version,
            kind,
            protocol,
            compatibility_profiles,
        })
    }

    #[must_use]
    pub const fn format_version(&self) -> VersionDataFormatVersion {
        self.format_version
    }

    #[must_use]
    pub fn minecraft_version(&self) -> &MinecraftVersionId {
        &self.minecraft_version
    }

    #[must_use]
    pub const fn kind(&self) -> MinecraftVersionKind {
        self.kind
    }

    #[must_use]
    pub const fn protocol(&self) -> ProtocolVersion {
        self.protocol
    }

    #[must_use]
    pub fn compatibility_profiles(&self) -> &[CompatibilityProfileId] {
        &self.compatibility_profiles
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CatalogEntry {
    minecraft_version: MinecraftVersionId,
    kind: MinecraftVersionKind,
    protocol: ProtocolVersion,
}

impl CatalogEntry {
    #[must_use]
    pub fn minecraft_version(&self) -> &MinecraftVersionId {
        &self.minecraft_version
    }

    #[must_use]
    pub const fn kind(&self) -> MinecraftVersionKind {
        self.kind
    }

    #[must_use]
    pub const fn protocol(&self) -> ProtocolVersion {
        self.protocol
    }
}

impl From<&VersionData> for CatalogEntry {
    fn from(data: &VersionData) -> Self {
        Self {
            minecraft_version: data.minecraft_version.clone(),
            kind: data.kind,
            protocol: data.protocol,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct VersionCatalog {
    format_version: VersionDataFormatVersion,
    versions: Vec<CatalogEntry>,
}

impl VersionCatalog {
    pub fn from_versions<'a>(
        versions: impl IntoIterator<Item = &'a VersionData>,
    ) -> Result<Self, VersionError> {
        Self::from_entries(versions.into_iter().map(CatalogEntry::from).collect())
    }

    #[must_use]
    pub const fn format_version(&self) -> VersionDataFormatVersion {
        self.format_version
    }

    #[must_use]
    pub fn entries(&self) -> &[CatalogEntry] {
        &self.versions
    }

    #[must_use]
    pub fn exact(&self, version: &MinecraftVersionId) -> Option<&CatalogEntry> {
        self.versions
            .binary_search_by(|entry| entry.minecraft_version.cmp(version))
            .ok()
            .and_then(|index| self.versions.get(index))
    }

    pub fn find_by_protocol(
        &self,
        protocol: ProtocolVersion,
    ) -> impl Iterator<Item = &CatalogEntry> {
        self.versions
            .iter()
            .filter(move |entry| entry.protocol == protocol)
    }

    pub(crate) fn from_entries(mut versions: Vec<CatalogEntry>) -> Result<Self, VersionError> {
        if versions.len() > crate::MAX_INSTALLED_VERSIONS {
            return Err(VersionError::TooManyVersions {
                max: crate::MAX_INSTALLED_VERSIONS,
            });
        }
        for entry in &versions {
            validate_protocol(entry.protocol)?;
        }
        versions.sort_by(|left, right| left.minecraft_version.cmp(&right.minecraft_version));
        for pair in versions.windows(2) {
            let [left, right] = pair else {
                continue;
            };
            if left.minecraft_version == right.minecraft_version {
                return Err(VersionError::DuplicateVersionId {
                    id: left.minecraft_version.to_string(),
                });
            }
        }
        Ok(Self {
            format_version: VersionDataFormatVersion::CURRENT,
            versions,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireVersionData {
    format_version: VersionDataFormatVersion,
    minecraft_version: MinecraftVersionId,
    kind: MinecraftVersionKind,
    protocol: ProtocolVersion,
    #[serde(default)]
    compatibility_profiles: Vec<CompatibilityProfileId>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireCatalog {
    format_version: VersionDataFormatVersion,
    versions: Vec<WireCatalogEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireCatalogEntry {
    minecraft_version: MinecraftVersionId,
    kind: MinecraftVersionKind,
    protocol: ProtocolVersion,
}

pub fn deserialize_version_data(bytes: &[u8]) -> Result<VersionData, VersionError> {
    let value = parse_and_check_format(bytes)?;
    let wire: WireVersionData =
        serde_json::from_value(value).map_err(|_| VersionError::InvalidField {
            field: "required or typed version metadata",
        })?;
    check_current_format(wire.format_version)?;
    VersionData::new(
        wire.minecraft_version,
        wire.kind,
        wire.protocol,
        wire.compatibility_profiles,
    )
}

pub fn deserialize_catalog(bytes: &[u8]) -> Result<VersionCatalog, VersionError> {
    let value = parse_and_check_format(bytes)?;
    let wire: WireCatalog =
        serde_json::from_value(value).map_err(|_| VersionError::InvalidField {
            field: "required or typed catalog metadata",
        })?;
    check_current_format(wire.format_version)?;
    VersionCatalog::from_entries(
        wire.versions
            .into_iter()
            .map(|entry| CatalogEntry {
                minecraft_version: entry.minecraft_version,
                kind: entry.kind,
                protocol: entry.protocol,
            })
            .collect(),
    )
}

pub fn serialize_version_data(data: &VersionData) -> Result<Vec<u8>, VersionError> {
    serialize_pretty(data)
}

pub fn serialize_catalog(catalog: &VersionCatalog) -> Result<Vec<u8>, VersionError> {
    serialize_pretty(catalog)
}

fn parse_and_check_format(bytes: &[u8]) -> Result<Value, VersionError> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|error| VersionError::MalformedJson {
            line: error.line(),
            column: error.column(),
        })?;
    let format = value
        .as_object()
        .and_then(|object| object.get("format_version"))
        .and_then(Value::as_u64)
        .and_then(|number| u32::try_from(number).ok())
        .ok_or(VersionError::InvalidField {
            field: "format_version",
        })?;
    check_current_format(VersionDataFormatVersion::new(format))?;
    Ok(value)
}

fn check_current_format(format: VersionDataFormatVersion) -> Result<(), VersionError> {
    if format == VersionDataFormatVersion::CURRENT {
        Ok(())
    } else {
        Err(VersionError::UnsupportedFormatVersion {
            found: format.value(),
            supported: VersionDataFormatVersion::CURRENT.value(),
        })
    }
}

fn serialize_pretty(value: &impl Serialize) -> Result<Vec<u8>, VersionError> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(VersionError::Serialization)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn validate_protocol(protocol: ProtocolVersion) -> Result<(), VersionError> {
    if protocol.value() < 0 {
        Err(VersionError::InvalidProtocolVersion {
            value: protocol.value(),
        })
    } else {
        Ok(())
    }
}

fn normalize_profiles(
    mut profiles: Vec<CompatibilityProfileId>,
) -> Result<Vec<CompatibilityProfileId>, VersionError> {
    if profiles.len() > MAX_COMPATIBILITY_PROFILES {
        return Err(VersionError::InvalidField {
            field: "compatibility_profiles count",
        });
    }
    profiles.sort();
    for pair in profiles.windows(2) {
        let [left, right] = pair else {
            continue;
        };
        if left == right {
            return Err(VersionError::DuplicateCompatibilityProfile {
                id: left.to_string(),
            });
        }
    }
    Ok(profiles)
}
