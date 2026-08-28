use std::{io, path::PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum VersionError {
    #[error("invalid official Minecraft metadata: {reason}")]
    InvalidOfficialMetadata { reason: String },
    #[error("invalid Minecraft version ID: {reason}")]
    InvalidVersionId { reason: &'static str },
    #[error("invalid compatibility profile ID: {reason}")]
    InvalidCompatibilityProfileId { reason: &'static str },
    #[error("Minecraft protocol version {value} must not be negative in installed version data")]
    InvalidProtocolVersion { value: i32 },
    #[error("unsupported Cubic version-data format {found}; supported format is {supported}")]
    UnsupportedFormatVersion { found: u32, supported: u32 },
    #[error("version-data JSON is malformed at line {line}, column {column}")]
    MalformedJson { line: usize, column: usize },
    #[error("version-data JSON is missing or has an invalid {field} field")]
    InvalidField { field: &'static str },
    #[error("version-data serialization failed")]
    Serialization(#[source] serde_json::Error),
    #[error("version-data root is not a directory: {path}")]
    RootNotDirectory { path: PathBuf },
    #[error("symbolic links are not accepted in version-data paths: {path}")]
    SymlinkNotAllowed { path: PathBuf },
    #[error("unexpected entry in the versions directory: {path}")]
    UnexpectedVersionsEntry { path: PathBuf },
    #[error("version-data file {path} is {size} bytes, exceeding limit {max}")]
    FileTooLarge { path: PathBuf, size: u64, max: u64 },
    #[error("version-data catalog contains more than {max} entries")]
    TooManyVersions { max: usize },
    #[error("duplicate Minecraft version ID {id}")]
    DuplicateVersionId { id: String },
    #[error("duplicate compatibility profile ID {id}")]
    DuplicateCompatibilityProfile { id: String },
    #[error("dataset directory {directory_id} contains version ID {declared_id}")]
    DatasetDirectoryMismatch {
        directory_id: String,
        declared_id: String,
    },
    #[error("catalog metadata for {id} does not match its dataset field {field}")]
    CatalogDatasetMismatch { id: String, field: &'static str },
    #[error("version-data I/O failed while {operation}: {path}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}
