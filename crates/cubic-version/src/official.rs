use std::{fmt, str::FromStr};

use serde::Deserialize;
use time::OffsetDateTime;
use url::Url;

use crate::{MinecraftVersionId, VersionError};

const MAX_ID_BYTES: usize = 128;
const MAX_URL_BYTES: usize = 2_048;
const MAX_TIMESTAMP_BYTES: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OfficialVersionKind {
    Release,
    Snapshot,
    Other(String),
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct Sha1Digest([u8; 20]);

impl Sha1Digest {
    pub const fn as_bytes(&self) -> &[u8; 20] {
        &self.0
    }
}

impl fmt::Debug for Sha1Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for Sha1Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl FromStr for Sha1Digest {
    type Err = VersionError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 40 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(VersionError::InvalidOfficialMetadata {
                reason: "SHA-1 must contain exactly 40 hexadecimal characters".to_owned(),
            });
        }
        let mut bytes = [0_u8; 20];
        for (index, pair) in value.as_bytes().as_chunks::<2>().0.iter().enumerate() {
            let text =
                std::str::from_utf8(pair).map_err(|_| VersionError::InvalidOfficialMetadata {
                    reason: "SHA-1 is not ASCII".to_owned(),
                })?;
            bytes[index] = u8::from_str_radix(text, 16).map_err(|_| {
                VersionError::InvalidOfficialMetadata {
                    reason: "SHA-1 contains invalid hexadecimal data".to_owned(),
                }
            })?;
        }
        Ok(Self(bytes))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OfficialVersionEntry {
    pub id: MinecraftVersionId,
    pub kind: OfficialVersionKind,
    pub metadata_url: Url,
    pub metadata_sha1: Sha1Digest,
    pub released_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub compliance_level: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OfficialVersionManifest {
    pub latest_release: MinecraftVersionId,
    pub latest_snapshot: MinecraftVersionId,
    pub versions: Vec<OfficialVersionEntry>,
}

impl OfficialVersionManifest {
    pub fn find(&self, id: &MinecraftVersionId) -> Option<&OfficialVersionEntry> {
        self.versions.iter().find(|entry| &entry.id == id)
    }

    pub fn latest_release(&self) -> Option<&OfficialVersionEntry> {
        self.find(&self.latest_release)
    }

    pub fn latest_snapshot(&self) -> Option<&OfficialVersionEntry> {
        self.find(&self.latest_snapshot)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetIndexDescriptor {
    pub id: String,
    pub url: Url,
    pub sha1: Sha1Digest,
    pub size: u64,
    pub total_size: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientDownloadDescriptor {
    pub url: Url,
    pub sha1: Sha1Digest,
    pub size: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectedVersionMetadata {
    pub id: MinecraftVersionId,
    pub kind: OfficialVersionKind,
    pub asset_index: AssetIndexDescriptor,
    pub client: ClientDownloadDescriptor,
    pub java_major_version: Option<u32>,
    pub main_class: Option<String>,
    pub inherits_from: Option<MinecraftVersionId>,
}

#[derive(Deserialize)]
struct ManifestWire {
    latest: LatestWire,
    versions: Vec<VersionWire>,
}

#[derive(Deserialize)]
struct LatestWire {
    release: String,
    snapshot: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct VersionWire {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    url: String,
    sha1: String,
    release_time: String,
    time: String,
    compliance_level: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MetadataWire {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    asset_index: AssetIndexWire,
    downloads: DownloadsWire,
    java_version: Option<JavaVersionWire>,
    main_class: Option<String>,
    inherits_from: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AssetIndexWire {
    id: String,
    url: String,
    sha1: String,
    size: u64,
    total_size: Option<u64>,
}

#[derive(Deserialize)]
struct DownloadsWire {
    client: DownloadWire,
}

#[derive(Deserialize)]
struct DownloadWire {
    url: String,
    sha1: String,
    size: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JavaVersionWire {
    major_version: u32,
}

pub fn parse_official_manifest(bytes: &[u8]) -> Result<OfficialVersionManifest, VersionError> {
    let wire: ManifestWire =
        serde_json::from_slice(bytes).map_err(|source| VersionError::MalformedJson {
            line: source.line(),
            column: source.column(),
        })?;
    let versions = wire
        .versions
        .into_iter()
        .map(convert_entry)
        .collect::<Result<Vec<_>, _>>()?;
    let manifest = OfficialVersionManifest {
        latest_release: MinecraftVersionId::new(wire.latest.release)?,
        latest_snapshot: MinecraftVersionId::new(wire.latest.snapshot)?,
        versions,
    };
    if manifest.latest_release().is_none() || manifest.latest_snapshot().is_none() {
        return Err(VersionError::InvalidOfficialMetadata {
            reason: "latest version identifiers are absent from the manifest entries".to_owned(),
        });
    }
    Ok(manifest)
}

pub fn parse_selected_version_metadata(
    bytes: &[u8],
) -> Result<SelectedVersionMetadata, VersionError> {
    let wire: MetadataWire =
        serde_json::from_slice(bytes).map_err(|source| VersionError::MalformedJson {
            line: source.line(),
            column: source.column(),
        })?;
    validate_short(&wire.asset_index.id, "asset-index ID", MAX_ID_BYTES)?;
    if let Some(main_class) = &wire.main_class {
        validate_short(main_class, "main class", 512)?;
    }
    Ok(SelectedVersionMetadata {
        id: MinecraftVersionId::new(wire.id)?,
        kind: kind(wire.kind)?,
        asset_index: AssetIndexDescriptor {
            id: wire.asset_index.id,
            url: https_url(&wire.asset_index.url)?,
            sha1: wire.asset_index.sha1.parse()?,
            size: wire.asset_index.size,
            total_size: wire.asset_index.total_size,
        },
        client: ClientDownloadDescriptor {
            url: https_url(&wire.downloads.client.url)?,
            sha1: wire.downloads.client.sha1.parse()?,
            size: wire.downloads.client.size,
        },
        java_major_version: wire.java_version.map(|value| value.major_version),
        main_class: wire.main_class,
        inherits_from: wire
            .inherits_from
            .map(MinecraftVersionId::new)
            .transpose()?,
    })
}

fn convert_entry(wire: VersionWire) -> Result<OfficialVersionEntry, VersionError> {
    Ok(OfficialVersionEntry {
        id: MinecraftVersionId::new(wire.id)?,
        kind: kind(wire.kind)?,
        metadata_url: https_url(&wire.url)?,
        metadata_sha1: wire.sha1.parse()?,
        released_at: timestamp(&wire.release_time)?,
        updated_at: timestamp(&wire.time)?,
        compliance_level: wire.compliance_level,
    })
}

fn kind(value: String) -> Result<OfficialVersionKind, VersionError> {
    validate_short(&value, "version type", 64)?;
    Ok(match value.as_str() {
        "release" => OfficialVersionKind::Release,
        "snapshot" => OfficialVersionKind::Snapshot,
        _ => OfficialVersionKind::Other(value),
    })
}

fn timestamp(value: &str) -> Result<OffsetDateTime, VersionError> {
    validate_short(value, "timestamp", MAX_TIMESTAMP_BYTES)?;
    OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).map_err(|_| {
        VersionError::InvalidOfficialMetadata {
            reason: "timestamp is not valid RFC 3339".to_owned(),
        }
    })
}

fn https_url(value: &str) -> Result<Url, VersionError> {
    validate_short(value, "URL", MAX_URL_BYTES)?;
    let url = Url::parse(value).map_err(|_| VersionError::InvalidOfficialMetadata {
        reason: "invalid official metadata URL".to_owned(),
    })?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || url.username() != ""
        || url.password().is_some()
    {
        return Err(VersionError::InvalidOfficialMetadata {
            reason: "official metadata URL must be an absolute credential-free HTTPS URL"
                .to_owned(),
        });
    }
    Ok(url)
}

fn validate_short(value: &str, field: &'static str, max: usize) -> Result<(), VersionError> {
    if value.is_empty() || value.len() > max || value.chars().any(char::is_control) {
        return Err(VersionError::InvalidOfficialMetadata {
            reason: format!("{field} is empty, overlong, or contains control characters"),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn release_snapshot_latest_selection_and_unknown_fields_work() {
        let json = format!(
            r#"{{"latest":{{"release":"r","snapshot":"s"}},"versions":[{{"id":"r","type":"release","url":"https://piston-meta.mojang.com/r","sha1":"{HASH}","releaseTime":"2026-01-01T00:00:00Z","time":"2026-01-01T00:00:00Z","complianceLevel":1,"future":true}},{{"id":"s","type":"snapshot","url":"https://piston-meta.mojang.com/s","sha1":"{HASH}","releaseTime":"2026-01-02T00:00:00Z","time":"2026-01-02T00:00:00Z"}}]}}"#
        );
        let manifest = parse_official_manifest(json.as_bytes()).unwrap();
        assert_eq!(
            manifest.latest_release().unwrap().kind,
            OfficialVersionKind::Release
        );
        assert_eq!(
            manifest.latest_snapshot().unwrap().kind,
            OfficialVersionKind::Snapshot
        );
    }

    #[test]
    fn unknown_kind_is_preserved() {
        let json = format!(
            r#"{{"latest":{{"release":"x","snapshot":"x"}},"versions":[{{"id":"x","type":"historical","url":"https://piston-meta.mojang.com/x","sha1":"{HASH}","releaseTime":"2026-01-01T00:00:00Z","time":"2026-01-01T00:00:00Z"}}]}}"#
        );
        assert_eq!(
            parse_official_manifest(json.as_bytes()).unwrap().versions[0].kind,
            OfficialVersionKind::Other("historical".to_owned())
        );
    }

    #[test]
    fn selected_metadata_parses_descriptors() {
        let json = format!(
            r#"{{"id":"r","type":"release","assetIndex":{{"id":"a","url":"https://piston-meta.mojang.com/a","sha1":"{HASH}","size":12,"totalSize":34}},"downloads":{{"client":{{"url":"https://piston-data.mojang.com/c","sha1":"{HASH}","size":56}}}},"javaVersion":{{"majorVersion":21}},"unknown":true}}"#
        );
        let value = parse_selected_version_metadata(json.as_bytes()).unwrap();
        assert_eq!(value.id.as_str(), "r");
        assert_eq!(value.asset_index.total_size, Some(34));
        assert_eq!(value.client.size, 56);
    }

    #[test]
    fn malformed_json_hash_timestamp_and_non_https_are_rejected() {
        assert!(parse_official_manifest(b"{").is_err());
        assert!("short".parse::<Sha1Digest>().is_err());
        let json = format!(
            r#"{{"latest":{{"release":"x","snapshot":"x"}},"versions":[{{"id":"x","type":"release","url":"http://example.com/x","sha1":"{HASH}","releaseTime":"bad","time":"bad"}}]}}"#
        );
        assert!(parse_official_manifest(json.as_bytes()).is_err());
    }
}
