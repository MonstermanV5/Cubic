//! Verified official Minecraft metadata and artifact bootstrap.

use std::{
    collections::BTreeMap,
    fmt,
    fs::{self, File, OpenOptions},
    future::Future,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use cubic_version::{
    MinecraftVersionId, SelectedVersionMetadata, Sha1Digest, parse_official_manifest,
    parse_selected_version_metadata,
};
use reqwest::{Client, Url, redirect::Policy};
use serde::Deserialize;
use sha1::{Digest, Sha1};
use thiserror::Error;
use zip::ZipArchive;

pub const OFFICIAL_MANIFEST_URL: &str =
    "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
pub const MAX_MANIFEST_BYTES: u64 = 2 * 1024 * 1024;
pub const MAX_VERSION_METADATA_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_ASSET_INDEX_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_ASSET_OBJECT_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_CLIENT_JAR_BYTES: u64 = 1024 * 1024 * 1024;
pub const MAX_VANILLA_RESOURCE_BYTES: u64 = 4 * 1024 * 1024;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Error)]
pub enum ResourceError {
    #[error("invalid official metadata: {0}")]
    Version(#[from] cubic_version::VersionError),
    #[error("requested Minecraft version `{version}` was not found in the official manifest")]
    VersionNotFound { version: String },
    #[error("untrusted official-resource URL: {url}")]
    UntrustedUrl { url: String },
    #[error("HTTP transport failed during {operation}: {source}")]
    Http {
        operation: &'static str,
        source: reqwest::Error,
    },
    #[error("official service returned HTTP {status} during {operation}")]
    HttpStatus {
        operation: &'static str,
        status: u16,
    },
    #[error("{context} exceeds the {maximum}-byte limit")]
    Oversized { context: &'static str, maximum: u64 },
    #[error("{context} declared {declared} bytes but received {actual}")]
    SizeMismatch {
        context: &'static str,
        declared: u64,
        actual: u64,
    },
    #[error("{context} SHA-1 did not match its official descriptor")]
    HashMismatch { context: &'static str },
    #[error("invalid asset index: {reason}")]
    InvalidAssetIndex { reason: String },
    #[error("filesystem operation `{operation}` failed for {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    #[error("symbolic links are not accepted as Cubic cache files: {path}")]
    CacheSymlink { path: PathBuf },
    #[error("network unavailable and no valid cached {artifact} exists: {reason}")]
    OfflineUnavailable {
        artifact: &'static str,
        reason: String,
    },
    #[error("invalid vanilla resource path `{path}`")]
    InvalidResourcePath { path: String },
    #[error("could not open verified client resource archive: {source}")]
    InvalidClientArchive { source: zip::result::ZipError },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootstrapSource {
    Network,
    Cache,
    Mixed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetObjectDescriptor {
    logical_name: String,
    sha1: Sha1Digest,
    size: u64,
}

impl AssetObjectDescriptor {
    pub fn logical_name(&self) -> &str {
        &self.logical_name
    }
    pub const fn sha1(&self) -> Sha1Digest {
        self.sha1
    }
    pub const fn size(&self) -> u64 {
        self.size
    }
}

#[derive(Clone, Debug)]
pub struct VerifiedAssetIndex {
    objects: BTreeMap<String, AssetObjectDescriptor>,
}

impl VerifiedAssetIndex {
    pub fn len(&self) -> usize {
        self.objects.len()
    }
    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }
    pub fn resolve(&self, logical_name: &str) -> Option<&AssetObjectDescriptor> {
        self.objects.get(logical_name)
    }
    pub fn objects(&self) -> impl Iterator<Item = &AssetObjectDescriptor> {
        self.objects.values()
    }
}

#[derive(Clone, Debug)]
pub struct BootstrapResult {
    pub metadata: SelectedVersionMetadata,
    pub assets: VerifiedAssetIndex,
    pub source: BootstrapSource,
    pub cache_root: PathBuf,
    pub client_jar_cached: bool,
}

#[derive(Clone, Debug)]
pub struct VerifiedArtifact {
    path: PathBuf,
}
impl VerifiedArtifact {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// A validated logical resource path inside an official client archive.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct VanillaResourcePath(String);

impl VanillaResourcePath {
    pub fn new(path: impl Into<String>) -> Result<Self, ResourceError> {
        let path = path.into();
        let valid = !path.is_empty()
            && path.len() <= 512
            && !path.starts_with('/')
            && !path.starts_with('\\')
            && !path.contains('\\')
            && !path.contains(':')
            && !path.chars().any(char::is_control)
            && path
                .split('/')
                .all(|component| !component.is_empty() && component != "." && component != "..");
        if !valid {
            return Err(ResourceError::InvalidResourcePath { path });
        }
        Ok(Self(path))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Read-only bounded access to resources in a hash-verified official client JAR.
pub trait VanillaResourceSource {
    fn read_resource(
        &mut self,
        path: &VanillaResourcePath,
        maximum: u64,
    ) -> Result<Option<Vec<u8>>, ResourceError>;
}

pub struct VanillaClientResources {
    archive: ZipArchive<File>,
}

impl fmt::Debug for VanillaClientResources {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VanillaClientResources")
            .field("entries", &self.archive.len())
            .finish_non_exhaustive()
    }
}

impl VanillaClientResources {
    pub fn open(artifact: &VerifiedArtifact) -> Result<Self, ResourceError> {
        let metadata = fs::symlink_metadata(artifact.path()).map_err(|source| {
            io_error("inspect verified client archive", artifact.path(), source)
        })?;
        if metadata.file_type().is_symlink() {
            return Err(ResourceError::CacheSymlink {
                path: artifact.path().to_owned(),
            });
        }
        let file = File::open(artifact.path())
            .map_err(|source| io_error("open verified client archive", artifact.path(), source))?;
        let archive = ZipArchive::new(file)
            .map_err(|source| ResourceError::InvalidClientArchive { source })?;
        Ok(Self { archive })
    }
}

impl VanillaResourceSource for VanillaClientResources {
    fn read_resource(
        &mut self,
        path: &VanillaResourcePath,
        maximum: u64,
    ) -> Result<Option<Vec<u8>>, ResourceError> {
        let maximum = maximum.min(MAX_VANILLA_RESOURCE_BYTES);
        let entry = match self.archive.by_name(path.as_str()) {
            Ok(entry) => entry,
            Err(zip::result::ZipError::FileNotFound) => return Ok(None),
            Err(source) => return Err(ResourceError::InvalidClientArchive { source }),
        };
        if entry.is_dir() || entry.size() > maximum {
            return Err(ResourceError::Oversized {
                context: "vanilla client resource",
                maximum,
            });
        }
        let capacity = usize::try_from(entry.size()).map_err(|_| ResourceError::Oversized {
            context: "vanilla client resource",
            maximum,
        })?;
        let mut bytes = Vec::with_capacity(capacity);
        entry
            .take(maximum.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|source| {
                io_error(
                    "read verified client resource",
                    Path::new(path.as_str()),
                    source,
                )
            })?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > maximum {
            return Err(ResourceError::Oversized {
                context: "vanilla client resource",
                maximum,
            });
        }
        Ok(Some(bytes))
    }
}

trait Fetcher: Send + Sync {
    fn bytes<'a>(
        &'a self,
        url: &'a Url,
        limit: u64,
        operation: &'static str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, ResourceError>> + Send + 'a>>;
    fn artifact<'a>(
        &'a self,
        url: &'a Url,
        path: &'a Path,
        size: u64,
        sha1: Sha1Digest,
        context: &'static str,
    ) -> Pin<Box<dyn Future<Output = Result<(), ResourceError>> + Send + 'a>>;
}

struct ReqwestFetcher {
    client: Client,
}

impl ReqwestFetcher {
    fn new() -> Result<Self, ResourceError> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .redirect(Policy::none())
            .user_agent("Cubic/0.1")
            .build()
            .map_err(|source| ResourceError::Http {
                operation: "build official-resource client",
                source,
            })?;
        Ok(Self { client })
    }
}

impl Fetcher for ReqwestFetcher {
    fn bytes<'a>(
        &'a self,
        url: &'a Url,
        limit: u64,
        operation: &'static str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, ResourceError>> + Send + 'a>> {
        Box::pin(async move {
            validate_url(url, ResourceKind::Metadata)?;
            let mut response = self
                .client
                .get(url.clone())
                .send()
                .await
                .map_err(|source| ResourceError::Http { operation, source })?;
            if !response.status().is_success() {
                return Err(ResourceError::HttpStatus {
                    operation,
                    status: response.status().as_u16(),
                });
            }
            if response
                .content_length()
                .is_some_and(|length| length > limit)
            {
                return Err(ResourceError::Oversized {
                    context: operation,
                    maximum: limit,
                });
            }
            let mut body = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|source| ResourceError::Http { operation, source })?
            {
                let new_len =
                    body.len()
                        .checked_add(chunk.len())
                        .ok_or(ResourceError::Oversized {
                            context: operation,
                            maximum: limit,
                        })?;
                if u64::try_from(new_len).unwrap_or(u64::MAX) > limit {
                    return Err(ResourceError::Oversized {
                        context: operation,
                        maximum: limit,
                    });
                }
                body.extend_from_slice(&chunk);
            }
            Ok(body)
        })
    }

    fn artifact<'a>(
        &'a self,
        url: &'a Url,
        path: &'a Path,
        size: u64,
        sha1: Sha1Digest,
        context: &'static str,
    ) -> Pin<Box<dyn Future<Output = Result<(), ResourceError>> + Send + 'a>> {
        Box::pin(async move {
            validate_url(url, ResourceKind::Artifact)?;
            let mut response = self
                .client
                .get(url.clone())
                .send()
                .await
                .map_err(|source| ResourceError::Http {
                    operation: context,
                    source,
                })?;
            if !response.status().is_success() {
                return Err(ResourceError::HttpStatus {
                    operation: context,
                    status: response.status().as_u16(),
                });
            }
            if let Some(actual) = response.content_length().filter(|actual| *actual != size) {
                return Err(ResourceError::SizeMismatch {
                    context,
                    declared: size,
                    actual,
                });
            }
            let mut file = create_new(path)?;
            let mut hasher = Sha1::new();
            let mut received = 0_u64;
            while let Some(chunk) =
                response
                    .chunk()
                    .await
                    .map_err(|source| ResourceError::Http {
                        operation: context,
                        source,
                    })?
            {
                received = received
                    .checked_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX))
                    .ok_or(ResourceError::Oversized {
                        context,
                        maximum: size,
                    })?;
                if received > size {
                    return Err(ResourceError::SizeMismatch {
                        context,
                        declared: size,
                        actual: received,
                    });
                }
                file.write_all(&chunk)
                    .map_err(|source| io_error("write temporary artifact", path, source))?;
                hasher.update(&chunk);
            }
            file.sync_all()
                .map_err(|source| io_error("sync temporary artifact", path, source))?;
            if received != size {
                return Err(ResourceError::SizeMismatch {
                    context,
                    declared: size,
                    actual: received,
                });
            }
            if hasher.finalize().as_slice() != sha1.as_bytes() {
                return Err(ResourceError::HashMismatch { context });
            }
            Ok(())
        })
    }
}

pub struct OfficialVersionBootstrap {
    root: PathBuf,
    fetcher: Arc<dyn Fetcher>,
}

impl fmt::Debug for OfficialVersionBootstrap {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OfficialVersionBootstrap")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl OfficialVersionBootstrap {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, ResourceError> {
        Ok(Self {
            root: root.into(),
            fetcher: Arc::new(ReqwestFetcher::new()?),
        })
    }

    #[cfg(test)]
    fn with_fetcher(root: impl Into<PathBuf>, fetcher: Arc<dyn Fetcher>) -> Self {
        Self {
            root: root.into(),
            fetcher,
        }
    }

    pub async fn bootstrap(
        &self,
        requested: &MinecraftVersionId,
    ) -> Result<BootstrapResult, ResourceError> {
        ensure_layout(&self.root)?;
        let manifest_url =
            Url::parse(OFFICIAL_MANIFEST_URL).map_err(|_| ResourceError::UntrustedUrl {
                url: OFFICIAL_MANIFEST_URL.to_owned(),
            })?;
        let manifest_path = self.root.join("manifests").join("version_manifest_v2.json");
        let (manifest_bytes, manifest_network) =
            match read_bounded(&manifest_path, MAX_MANIFEST_BYTES) {
                Ok(bytes) if parse_official_manifest(&bytes).is_ok() => (bytes, false),
                _ => {
                    let bytes = self
                        .fetcher
                        .bytes(&manifest_url, MAX_MANIFEST_BYTES, "version manifest fetch")
                        .await
                        .map_err(|error| ResourceError::OfflineUnavailable {
                            artifact: "version manifest",
                            reason: error.to_string(),
                        })?;
                    parse_official_manifest(&bytes)?;
                    atomic_write(&manifest_path, &bytes)?;
                    (bytes, true)
                }
            };
        let manifest = parse_official_manifest(&manifest_bytes)?;
        let entry = manifest
            .find(requested)
            .ok_or_else(|| ResourceError::VersionNotFound {
                version: requested.as_str().to_owned(),
            })?;
        validate_url(&entry.metadata_url, ResourceKind::Metadata)?;
        let version_dir = self.root.join("versions").join(requested.as_str());
        fs::create_dir_all(&version_dir)
            .map_err(|source| io_error("create version cache directory", &version_dir, source))?;
        let (metadata_bytes, metadata_network) = self
            .load_or_fetch_bytes(
                &version_dir.join("metadata.json"),
                &entry.metadata_url,
                MAX_VERSION_METADATA_BYTES,
                Some(entry.metadata_sha1),
                None,
                "version metadata",
            )
            .await?;
        let metadata = parse_selected_version_metadata(&metadata_bytes)?;
        if metadata.id != *requested {
            return Err(ResourceError::InvalidAssetIndex {
                reason: "selected metadata version ID does not match the request".to_owned(),
            });
        }
        validate_url(&metadata.asset_index.url, ResourceKind::Metadata)?;
        validate_url(&metadata.client.url, ResourceKind::Artifact)?;
        if metadata.client.size > MAX_CLIENT_JAR_BYTES {
            return Err(ResourceError::Oversized {
                context: "client JAR",
                maximum: MAX_CLIENT_JAR_BYTES,
            });
        }
        let (asset_bytes, asset_network) = self
            .load_or_fetch_bytes(
                &version_dir.join("asset-index.json"),
                &metadata.asset_index.url,
                MAX_ASSET_INDEX_BYTES,
                Some(metadata.asset_index.sha1),
                Some(metadata.asset_index.size),
                "asset index",
            )
            .await?;
        let assets = parse_asset_index(&asset_bytes)?;
        let client_jar_cached = verify_file(
            &version_dir.join("client.jar"),
            metadata.client.size,
            metadata.client.sha1,
            "client JAR",
        )
        .unwrap_or(false);
        let network_count = usize::from(manifest_network)
            + usize::from(metadata_network)
            + usize::from(asset_network);
        let source = match network_count {
            0 => BootstrapSource::Cache,
            3 => BootstrapSource::Network,
            _ => BootstrapSource::Mixed,
        };
        tracing::info!(target: "resources", version = %requested, ?source, assets = assets.len(), "official Minecraft version bootstrap complete");
        Ok(BootstrapResult {
            metadata,
            assets,
            source,
            cache_root: self.root.clone(),
            client_jar_cached,
        })
    }

    pub async fn ensure_client_jar(
        &self,
        metadata: &SelectedVersionMetadata,
    ) -> Result<VerifiedArtifact, ResourceError> {
        let path = self
            .root
            .join("versions")
            .join(metadata.id.as_str())
            .join("client.jar");
        self.ensure_artifact(
            &path,
            &metadata.client.url,
            metadata.client.size,
            metadata.client.sha1,
            "client JAR",
        )
        .await
    }

    pub async fn ensure_asset_object(
        &self,
        descriptor: &AssetObjectDescriptor,
    ) -> Result<VerifiedArtifact, ResourceError> {
        if descriptor.size > MAX_ASSET_OBJECT_BYTES {
            return Err(ResourceError::Oversized {
                context: "asset object",
                maximum: MAX_ASSET_OBJECT_BYTES,
            });
        }
        let hash = descriptor.sha1.to_string();
        let prefix = hash
            .get(..2)
            .ok_or_else(|| ResourceError::InvalidAssetIndex {
                reason: "asset SHA-1 prefix is missing".to_owned(),
            })?;
        let path = self.root.join("objects").join(prefix).join(&hash);
        let url = Url::parse(&format!(
            "https://resources.download.minecraft.net/{prefix}/{hash}"
        ))
        .map_err(|_| ResourceError::UntrustedUrl { url: hash.clone() })?;
        self.ensure_artifact(
            &path,
            &url,
            descriptor.size,
            descriptor.sha1,
            "asset object",
        )
        .await
    }

    async fn load_or_fetch_bytes(
        &self,
        path: &Path,
        url: &Url,
        limit: u64,
        sha1: Option<Sha1Digest>,
        size: Option<u64>,
        context: &'static str,
    ) -> Result<(Vec<u8>, bool), ResourceError> {
        if let Ok(bytes) = read_bounded(path, limit)
            && verify_bytes(&bytes, size, sha1, context).is_ok()
        {
            tracing::debug!(target: "resources", %context, path = %path.display(), "verified cache hit");
            return Ok((bytes, false));
        }
        tracing::debug!(target: "resources", %context, "cache miss; fetching official artifact");
        let bytes = self.fetcher.bytes(url, limit, context).await?;
        verify_bytes(&bytes, size, sha1, context)?;
        atomic_write(path, &bytes)?;
        Ok((bytes, true))
    }

    async fn ensure_artifact(
        &self,
        path: &Path,
        url: &Url,
        size: u64,
        sha1: Sha1Digest,
        context: &'static str,
    ) -> Result<VerifiedArtifact, ResourceError> {
        if verify_file(path, size, sha1, context).unwrap_or(false) {
            tracing::debug!(target: "resources", %context, path = %path.display(), "verified immutable artifact cache hit");
            return Ok(VerifiedArtifact {
                path: path.to_owned(),
            });
        }
        tracing::info!(target: "resources", %context, expected_size = size, "fetching official immutable artifact");
        if path.exists() {
            fs::remove_file(path)
                .map_err(|source| io_error("remove corrupt cache artifact", path, source))?;
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|source| io_error("create artifact cache directory", parent, source))?;
        }
        let temporary = temporary_path(path);
        if let Err(error) = self
            .fetcher
            .artifact(url, &temporary, size, sha1, context)
            .await
        {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        fs::rename(&temporary, path)
            .map_err(|source| io_error("promote verified artifact", path, source))?;
        Ok(VerifiedArtifact {
            path: path.to_owned(),
        })
    }
}

#[derive(Deserialize)]
struct AssetIndexWire {
    objects: BTreeMap<String, AssetWire>,
}
#[derive(Deserialize)]
struct AssetWire {
    hash: String,
    size: u64,
}

pub fn parse_asset_index(bytes: &[u8]) -> Result<VerifiedAssetIndex, ResourceError> {
    let wire: AssetIndexWire =
        serde_json::from_slice(bytes).map_err(|error| ResourceError::InvalidAssetIndex {
            reason: error.to_string(),
        })?;
    if wire.objects.len() > 200_000 {
        return Err(ResourceError::InvalidAssetIndex {
            reason: "asset count exceeds 200000".to_owned(),
        });
    }
    let mut objects = BTreeMap::new();
    for (logical_name, value) in wire.objects {
        if logical_name.is_empty()
            || logical_name.len() > 1_024
            || logical_name.chars().any(char::is_control)
        {
            return Err(ResourceError::InvalidAssetIndex {
                reason: "logical asset name is empty, overlong, or contains controls".to_owned(),
            });
        }
        let descriptor = AssetObjectDescriptor {
            logical_name: logical_name.clone(),
            sha1: value.hash.parse()?,
            size: value.size,
        };
        objects.insert(logical_name, descriptor);
    }
    Ok(VerifiedAssetIndex { objects })
}

#[derive(Clone, Copy)]
enum ResourceKind {
    Metadata,
    Artifact,
}
fn validate_url(url: &Url, kind: ResourceKind) -> Result<(), ResourceError> {
    let allowed = match kind {
        ResourceKind::Metadata => &["piston-meta.mojang.com", "launchermeta.mojang.com"][..],
        ResourceKind::Artifact => &[
            "piston-data.mojang.com",
            "launcher.mojang.com",
            "resources.download.minecraft.net",
        ][..],
    };
    if url.scheme() != "https"
        || url.port().is_some()
        || url.username() != ""
        || url.password().is_some()
        || !url.host_str().is_some_and(|host| allowed.contains(&host))
    {
        return Err(ResourceError::UntrustedUrl {
            url: sanitized_url(url),
        });
    }
    Ok(())
}
fn sanitized_url(url: &Url) -> String {
    format!(
        "{}://{}{}",
        url.scheme(),
        url.host_str().unwrap_or("<missing>"),
        url.path()
    )
}

fn verify_bytes(
    bytes: &[u8],
    size: Option<u64>,
    sha1: Option<Sha1Digest>,
    context: &'static str,
) -> Result<(), ResourceError> {
    let actual = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if let Some(declared) = size.filter(|declared| *declared != actual) {
        return Err(ResourceError::SizeMismatch {
            context,
            declared,
            actual,
        });
    }
    if let Some(expected) = sha1
        && Sha1::digest(bytes).as_slice() != expected.as_bytes()
    {
        return Err(ResourceError::HashMismatch { context });
    }
    Ok(())
}

fn verify_file(
    path: &Path,
    size: u64,
    sha1: Sha1Digest,
    context: &'static str,
) -> Result<bool, ResourceError> {
    if path.exists()
        && fs::symlink_metadata(path)
            .map_err(|source| io_error("inspect cached artifact", path, source))?
            .file_type()
            .is_symlink()
    {
        return Err(ResourceError::CacheSymlink {
            path: path.to_owned(),
        });
    }
    if !path.is_file() {
        return Ok(false);
    }
    let metadata =
        fs::metadata(path).map_err(|source| io_error("inspect cached artifact", path, source))?;
    if metadata.len() != size {
        return Ok(false);
    }
    let mut file =
        File::open(path).map_err(|source| io_error("open cached artifact", path, source))?;
    let mut hasher = Sha1::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|source| io_error("verify cached artifact", path, source))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let valid = hasher.finalize().as_slice() == sha1.as_bytes();
    if !valid {
        tracing::warn!(target: "resources", %context, path = %path.display(), "cached artifact failed integrity verification");
    }
    Ok(valid)
}

fn ensure_layout(root: &Path) -> Result<(), ResourceError> {
    for directory in [
        root.join("manifests"),
        root.join("versions"),
        root.join("objects"),
    ] {
        fs::create_dir_all(&directory)
            .map_err(|source| io_error("create cache directory", &directory, source))?;
    }
    Ok(())
}

fn read_bounded(path: &Path, maximum: u64) -> Result<Vec<u8>, ResourceError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| io_error("inspect cached metadata", path, source))?;
    if metadata.file_type().is_symlink() {
        return Err(ResourceError::CacheSymlink {
            path: path.to_owned(),
        });
    }
    if metadata.len() > maximum {
        return Err(ResourceError::Oversized {
            context: "cached metadata",
            maximum,
        });
    }
    let file = File::open(path).map_err(|source| io_error("open cached metadata", path, source))?;
    let mut bytes = Vec::new();
    file.take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| io_error("read cached metadata", path, source))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > maximum {
        return Err(ResourceError::Oversized {
            context: "cached metadata",
            maximum,
        });
    }
    Ok(bytes)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), ResourceError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|source| io_error("create metadata directory", parent, source))?;
    }
    let temporary = temporary_path(path);
    let mut file = create_new(&temporary)?;
    file.write_all(bytes)
        .map_err(|source| io_error("write temporary metadata", &temporary, source))?;
    file.sync_all()
        .map_err(|source| io_error("sync temporary metadata", &temporary, source))?;
    if path.exists() {
        fs::remove_file(path)
            .map_err(|source| io_error("replace cached metadata", path, source))?;
    }
    if let Err(source) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(io_error("promote cached metadata", path, source));
    }
    Ok(())
}

fn temporary_path(path: &Path) -> PathBuf {
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("artifact");
    path.with_file_name(format!(".{name}.{}.{}.part", std::process::id(), sequence))
}
fn create_new(path: &Path) -> Result<File, ResourceError> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| io_error("create temporary cache file", path, source))
}
fn io_error(operation: &'static str, path: &Path, source: io::Error) -> ResourceError {
    ResourceError::Io {
        operation,
        path: path.to_owned(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::HashMap, sync::Mutex};

    #[derive(Default)]
    struct FakeFetcher {
        bodies: Mutex<HashMap<String, Vec<u8>>>,
        calls: AtomicU64,
    }

    impl Fetcher for FakeFetcher {
        fn bytes<'a>(
            &'a self,
            url: &'a Url,
            limit: u64,
            operation: &'static str,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, ResourceError>> + Send + 'a>> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::Relaxed);
                let value = self
                    .bodies
                    .lock()
                    .unwrap()
                    .get(url.as_str())
                    .cloned()
                    .ok_or_else(|| ResourceError::OfflineUnavailable {
                        artifact: operation,
                        reason: "synthetic offline".to_owned(),
                    })?;
                if value.len() as u64 > limit {
                    return Err(ResourceError::Oversized {
                        context: operation,
                        maximum: limit,
                    });
                }
                Ok(value)
            })
        }
        fn artifact<'a>(
            &'a self,
            url: &'a Url,
            path: &'a Path,
            size: u64,
            sha1: Sha1Digest,
            context: &'static str,
        ) -> Pin<Box<dyn Future<Output = Result<(), ResourceError>> + Send + 'a>> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::Relaxed);
                let value = self
                    .bodies
                    .lock()
                    .unwrap()
                    .get(url.as_str())
                    .cloned()
                    .ok_or_else(|| ResourceError::OfflineUnavailable {
                        artifact: context,
                        reason: "synthetic offline".to_owned(),
                    })?;
                verify_bytes(&value, Some(size), Some(sha1), context)?;
                fs::write(path, value)
                    .map_err(|source| io_error("write synthetic artifact", path, source))
            })
        }
    }

    struct Fixture {
        root: tempfile::TempDir,
        fetcher: Arc<FakeFetcher>,
        bootstrap: OfficialVersionBootstrap,
        client: Vec<u8>,
    }
    fn digest(bytes: &[u8]) -> String {
        Sha1::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn fixture() -> Fixture {
        let client = b"synthetic client".to_vec();
        let asset = b"synthetic asset";
        let asset_hash = digest(asset);
        let assets = format!(
            r#"{{"objects":{{"minecraft/lang/test.json":{{"hash":"{asset_hash}","size":{}}}}}}}"#,
            asset.len()
        )
        .into_bytes();
        let metadata_text = format!(
            r#"{{"id":"release-a","type":"release","assetIndex":{{"id":"assets-a","url":"https://piston-meta.mojang.com/assets.json","sha1":"{}","size":{}}},"downloads":{{"client":{{"url":"https://piston-data.mojang.com/client.jar","sha1":"{}","size":{}}}}}}}"#,
            digest(&assets),
            assets.len(),
            digest(&client),
            client.len()
        );
        let metadata = metadata_text.as_bytes().to_vec();
        let snapshot_metadata = metadata_text
            .replace("release-a", "snapshot-a")
            .replace("\"release\"", "\"snapshot\"")
            .into_bytes();
        let manifest = format!(r#"{{"latest":{{"release":"release-a","snapshot":"snapshot-a"}},"versions":[{{"id":"release-a","type":"release","url":"https://piston-meta.mojang.com/version.json","sha1":"{}","releaseTime":"2026-01-01T00:00:00Z","time":"2026-01-01T00:00:00Z"}},{{"id":"snapshot-a","type":"snapshot","url":"https://piston-meta.mojang.com/snapshot.json","sha1":"{}","releaseTime":"2026-01-02T00:00:00Z","time":"2026-01-02T00:00:00Z"}}]}}"#, digest(&metadata), digest(&snapshot_metadata)).into_bytes();
        let fetcher = Arc::new(FakeFetcher::default());
        let mut bodies = fetcher.bodies.lock().unwrap();
        bodies.insert(OFFICIAL_MANIFEST_URL.to_owned(), manifest);
        bodies.insert(
            "https://piston-meta.mojang.com/version.json".to_owned(),
            metadata.clone(),
        );
        bodies.insert(
            "https://piston-meta.mojang.com/snapshot.json".to_owned(),
            snapshot_metadata,
        );
        bodies.insert(
            "https://piston-meta.mojang.com/assets.json".to_owned(),
            assets,
        );
        bodies.insert(
            "https://piston-data.mojang.com/client.jar".to_owned(),
            client.clone(),
        );
        bodies.insert(
            format!(
                "https://resources.download.minecraft.net/{}/{asset_hash}",
                &asset_hash[..2]
            ),
            asset.to_vec(),
        );
        drop(bodies);
        let root = tempfile::tempdir().unwrap();
        let bootstrap = OfficialVersionBootstrap::with_fetcher(root.path(), fetcher.clone());
        Fixture {
            root,
            fetcher,
            bootstrap,
            client,
        }
    }

    #[tokio::test]
    async fn online_bootstrap_then_offline_verified_cache_reuse() {
        let fixture = fixture();
        let id = MinecraftVersionId::new("release-a").unwrap();
        let first = fixture.bootstrap.bootstrap(&id).await.unwrap();
        assert_eq!(first.source, BootstrapSource::Network);
        assert_eq!(first.assets.len(), 1);
        let calls = fixture.fetcher.calls.load(Ordering::Relaxed);
        let second = fixture.bootstrap.bootstrap(&id).await.unwrap();
        assert_eq!(second.source, BootstrapSource::Cache);
        assert_eq!(fixture.fetcher.calls.load(Ordering::Relaxed), calls);
    }

    #[tokio::test]
    async fn release_and_snapshot_cache_directories_coexist() {
        let fixture = fixture();
        fixture
            .bootstrap
            .bootstrap(&MinecraftVersionId::new("release-a").unwrap())
            .await
            .unwrap();
        fixture
            .bootstrap
            .bootstrap(&MinecraftVersionId::new("snapshot-a").unwrap())
            .await
            .unwrap();
        assert!(
            fixture
                .root
                .path()
                .join("versions/release-a/metadata.json")
                .is_file()
        );
        assert!(
            fixture
                .root
                .path()
                .join("versions/snapshot-a/metadata.json")
                .is_file()
        );
    }

    #[tokio::test]
    async fn corruption_refetches_and_client_artifact_is_verified() {
        let fixture = fixture();
        let id = MinecraftVersionId::new("release-a").unwrap();
        let first = fixture.bootstrap.bootstrap(&id).await.unwrap();
        fs::write(
            fixture
                .root
                .path()
                .join("versions/release-a/asset-index.json"),
            b"corrupt",
        )
        .unwrap();
        assert_eq!(
            fixture.bootstrap.bootstrap(&id).await.unwrap().source,
            BootstrapSource::Mixed
        );
        let artifact = fixture
            .bootstrap
            .ensure_client_jar(&first.metadata)
            .await
            .unwrap();
        assert_eq!(fs::read(artifact.path()).unwrap(), fixture.client);
        let calls = fixture.fetcher.calls.load(Ordering::Relaxed);
        fixture
            .bootstrap
            .ensure_client_jar(&first.metadata)
            .await
            .unwrap();
        assert_eq!(fixture.fetcher.calls.load(Ordering::Relaxed), calls);
    }

    #[tokio::test]
    async fn content_addressed_asset_deduplicates() {
        let fixture = fixture();
        let result = fixture
            .bootstrap
            .bootstrap(&MinecraftVersionId::new("release-a").unwrap())
            .await
            .unwrap();
        let descriptor = result.assets.resolve("minecraft/lang/test.json").unwrap();
        let artifact = fixture
            .bootstrap
            .ensure_asset_object(descriptor)
            .await
            .unwrap();
        assert!(artifact.path().ends_with(descriptor.sha1().to_string()));
        let calls = fixture.fetcher.calls.load(Ordering::Relaxed);
        fixture
            .bootstrap
            .ensure_asset_object(descriptor)
            .await
            .unwrap();
        assert_eq!(fixture.fetcher.calls.load(Ordering::Relaxed), calls);
    }

    #[tokio::test]
    async fn missing_and_traversal_versions_are_safe() {
        let fixture = fixture();
        assert!(matches!(
            fixture
                .bootstrap
                .bootstrap(&MinecraftVersionId::new("missing").unwrap())
                .await,
            Err(ResourceError::VersionNotFound { .. })
        ));
        assert!(MinecraftVersionId::new("../escape").is_err());
        assert!(!fixture.root.path().join("escape").exists());
    }

    #[test]
    fn asset_index_hash_size_and_url_boundaries_are_strict() {
        let index = parse_asset_index(br#"{"objects":{"b":{"hash":"0123456789abcdef0123456789abcdef01234567","size":2},"a":{"hash":"0123456789abcdef0123456789abcdef01234567","size":1}}}"#).unwrap();
        assert_eq!(
            index
                .objects()
                .map(AssetObjectDescriptor::logical_name)
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert!(parse_asset_index(b"{").is_err());
        assert!(parse_asset_index(br#"{"objects":{"a":{"hash":"bad","size":1}}}"#).is_err());
        let hash: Sha1Digest = "0123456789abcdef0123456789abcdef01234567".parse().unwrap();
        assert!(verify_bytes(b"x", Some(2), None, "test").is_err());
        assert!(verify_bytes(b"x", None, Some(hash), "test").is_err());
        for value in [
            "http://piston-meta.mojang.com/x",
            "https://piston-meta.mojang.com.attacker.example/x",
            "https://evil.example/x",
        ] {
            assert!(validate_url(&Url::parse(value).unwrap(), ResourceKind::Metadata).is_err());
        }
    }

    #[test]
    fn atomic_temporary_path_is_not_a_valid_cache_entry() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("client.jar");
        let temporary = temporary_path(&target);
        fs::write(&temporary, b"partial").unwrap();
        assert!(!target.exists());
        fs::remove_file(temporary).unwrap();
    }

    #[test]
    fn vanilla_resource_paths_reject_traversal_absolute_and_drive_escapes() {
        assert_eq!(
            VanillaResourcePath::new("assets/minecraft/models/block/cube.json")
                .unwrap()
                .as_str(),
            "assets/minecraft/models/block/cube.json"
        );
        for invalid in [
            "../client.json",
            "assets/../client.json",
            "/assets/client.json",
            r"C:\assets\client.json",
            "assets//client.json",
        ] {
            assert!(VanillaResourcePath::new(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn verified_client_resource_reads_are_bounded() {
        use zip::{ZipWriter, write::SimpleFileOptions};

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("client.jar");
        let file = File::create(&path).unwrap();
        let mut writer = ZipWriter::new(file);
        writer
            .start_file(
                "assets/minecraft/models/block/test.json",
                SimpleFileOptions::default(),
            )
            .unwrap();
        writer.write_all(br#"{"parent":"block/cube"}"#).unwrap();
        writer.finish().unwrap();
        let artifact = VerifiedArtifact { path };
        let resource = VanillaResourcePath::new("assets/minecraft/models/block/test.json").unwrap();
        let mut source = VanillaClientResources::open(&artifact).unwrap();

        assert!(source.read_resource(&resource, 128).unwrap().is_some());
        assert!(matches!(
            source.read_resource(&resource, 4),
            Err(ResourceError::Oversized { .. })
        ));
    }
}
