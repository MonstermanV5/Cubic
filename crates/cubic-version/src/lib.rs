//! Runtime model and offline catalog support for Cubic version data.
//!
//! The crate is synchronous, filesystem-backed, transport-independent, and
//! performs no implicit network access.

mod error;
mod identity;
mod model;
mod store;

pub use error::VersionError;
pub use identity::{
    CompatibilityProfileId, MinecraftVersionId, ProtocolVersion, VersionDataFormatVersion,
};
pub use model::{
    CatalogEntry, MinecraftVersionKind, VersionCatalog, VersionData, deserialize_catalog,
    deserialize_version_data, serialize_catalog, serialize_version_data,
};
pub use store::{
    CATALOG_FILE_NAME, MAX_CATALOG_FILE_BYTES, MAX_INSTALLED_VERSIONS, MAX_VERSION_FILE_BYTES,
    VERSION_FILE_NAME, VERSIONS_DIRECTORY_NAME, VersionDataStore, build_catalog, write_catalog,
};
