//! Runtime model and offline catalog support for Cubic version data.
//!
//! The crate is synchronous, filesystem-backed, transport-independent, and
//! performs no implicit network access.

mod error;
mod game_data;
mod identity;
mod model;
mod official;
mod store;

pub use error::VersionError;
pub use game_data::{
    BlockDefinition, BlockProperty, BlockState, GAME_DATA_FILE_NAME, GameData, GameDataArtifact,
    GameDataFormatVersion, GameDataProvenance, MAX_GAME_DATA_BYTES, MinecraftIdentifier,
    RegistryEntry, RegistryTable, generate_game_data_from_reports, parse_game_data,
    serialize_game_data,
};
pub use identity::{
    CompatibilityProfileId, MinecraftVersionId, ProtocolVersion, VersionDataFormatVersion,
};
pub use model::{
    CatalogEntry, MinecraftVersionKind, VersionCatalog, VersionData, deserialize_catalog,
    deserialize_version_data, serialize_catalog, serialize_version_data,
};
pub use official::{
    AssetIndexDescriptor, ClientDownloadDescriptor, OfficialVersionEntry, OfficialVersionKind,
    OfficialVersionManifest, SelectedVersionMetadata, Sha1Digest, parse_official_manifest,
    parse_selected_version_metadata,
};
pub use store::{
    CATALOG_FILE_NAME, MAX_CATALOG_FILE_BYTES, MAX_INSTALLED_VERSIONS, MAX_VERSION_FILE_BYTES,
    VERSION_FILE_NAME, VERSIONS_DIRECTORY_NAME, VersionDataStore, build_catalog, write_catalog,
};
