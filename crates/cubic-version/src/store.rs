use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use crate::{
    CatalogEntry, MinecraftVersionId, ProtocolVersion, VersionCatalog, VersionData, VersionError,
    deserialize_catalog, deserialize_version_data, serialize_catalog,
};

pub const CATALOG_FILE_NAME: &str = "catalog.json";
pub const VERSIONS_DIRECTORY_NAME: &str = "versions";
pub const VERSION_FILE_NAME: &str = "version.json";
pub const MAX_VERSION_FILE_BYTES: u64 = 64 * 1024;
pub const MAX_CATALOG_FILE_BYTES: u64 = 1024 * 1024;
pub const MAX_INSTALLED_VERSIONS: usize = 4096;

#[derive(Debug)]
pub struct VersionDataStore {
    root: PathBuf,
    catalog: VersionCatalog,
}

impl VersionDataStore {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, VersionError> {
        let root = root.as_ref().to_path_buf();
        require_directory(&root)?;
        require_directory(&root.join(VERSIONS_DIRECTORY_NAME))?;
        let catalog_path = root.join(CATALOG_FILE_NAME);
        let catalog = deserialize_catalog(&read_bounded(&catalog_path, MAX_CATALOG_FILE_BYTES)?)?;
        let store = Self { root, catalog };
        store.validate_catalog_datasets()?;
        Ok(store)
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn available_versions(&self) -> &[CatalogEntry] {
        self.catalog.entries()
    }

    pub fn load_exact(
        &self,
        version: &MinecraftVersionId,
    ) -> Result<Option<VersionData>, VersionError> {
        if self.catalog.exact(version).is_none() {
            return Ok(None);
        }
        self.load_dataset(version).map(Some)
    }

    pub fn find_by_protocol(
        &self,
        protocol: ProtocolVersion,
    ) -> Result<Vec<VersionData>, VersionError> {
        self.catalog
            .find_by_protocol(protocol)
            .map(|entry| self.load_dataset(entry.minecraft_version()))
            .collect()
    }

    fn validate_catalog_datasets(&self) -> Result<(), VersionError> {
        for entry in self.catalog.entries() {
            let data = self.load_dataset(entry.minecraft_version())?;
            if data.kind() != entry.kind() {
                return Err(VersionError::CatalogDatasetMismatch {
                    id: entry.minecraft_version().to_string(),
                    field: "kind",
                });
            }
            if data.protocol() != entry.protocol() {
                return Err(VersionError::CatalogDatasetMismatch {
                    id: entry.minecraft_version().to_string(),
                    field: "protocol",
                });
            }
        }
        Ok(())
    }

    fn load_dataset(&self, version: &MinecraftVersionId) -> Result<VersionData, VersionError> {
        load_dataset_from_root(&self.root, version)
    }
}

pub fn build_catalog(root: impl AsRef<Path>) -> Result<VersionCatalog, VersionError> {
    let root = root.as_ref();
    require_directory(root)?;
    let versions_path = root.join(VERSIONS_DIRECTORY_NAME);
    require_directory(&versions_path)?;
    let directory = fs::read_dir(&versions_path).map_err(|source| VersionError::Io {
        operation: "reading versions directory",
        path: versions_path.clone(),
        source,
    })?;
    let mut datasets = Vec::new();
    for entry_result in directory {
        if datasets.len() >= MAX_INSTALLED_VERSIONS {
            return Err(VersionError::TooManyVersions {
                max: MAX_INSTALLED_VERSIONS,
            });
        }
        let entry = entry_result.map_err(|source| VersionError::Io {
            operation: "reading versions directory entry",
            path: versions_path.clone(),
            source,
        })?;
        let entry_path = entry.path();
        let file_type = entry.file_type().map_err(|source| VersionError::Io {
            operation: "inspecting versions directory entry",
            path: entry_path.clone(),
            source,
        })?;
        if file_type.is_symlink() {
            return Err(VersionError::SymlinkNotAllowed { path: entry_path });
        }
        if !file_type.is_dir() {
            return Err(VersionError::UnexpectedVersionsEntry { path: entry_path });
        }
        let directory_id =
            entry
                .file_name()
                .into_string()
                .map_err(|_| VersionError::UnexpectedVersionsEntry {
                    path: entry_path.clone(),
                })?;
        let version_id = MinecraftVersionId::new(directory_id.clone())?;
        let data = load_dataset_from_root(root, &version_id)?;
        if data.minecraft_version() != &version_id {
            return Err(VersionError::DatasetDirectoryMismatch {
                directory_id,
                declared_id: data.minecraft_version().to_string(),
            });
        }
        datasets.push(data);
    }
    VersionCatalog::from_versions(&datasets)
}

pub fn write_catalog(root: impl AsRef<Path>) -> Result<VersionCatalog, VersionError> {
    let root = root.as_ref();
    let catalog = build_catalog(root)?;
    let bytes = serialize_catalog(&catalog)?;
    let path = root.join(CATALOG_FILE_NAME);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(VersionError::SymlinkNotAllowed { path });
        }
        Ok(_) => {}
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(VersionError::Io {
                operation: "inspecting catalog destination",
                path,
                source,
            });
        }
    }
    let mut file = File::create(&path).map_err(|source| VersionError::Io {
        operation: "creating catalog",
        path: path.clone(),
        source,
    })?;
    file.write_all(&bytes).map_err(|source| VersionError::Io {
        operation: "writing catalog",
        path: path.clone(),
        source,
    })?;
    file.flush().map_err(|source| VersionError::Io {
        operation: "flushing catalog",
        path,
        source,
    })?;
    Ok(catalog)
}

fn load_dataset_from_root(
    root: &Path,
    version: &MinecraftVersionId,
) -> Result<VersionData, VersionError> {
    let directory = root.join(VERSIONS_DIRECTORY_NAME).join(version.as_str());
    require_directory(&directory)?;
    let path = directory.join(VERSION_FILE_NAME);
    let data = deserialize_version_data(&read_bounded(&path, MAX_VERSION_FILE_BYTES)?)?;
    if data.minecraft_version() != version {
        return Err(VersionError::DatasetDirectoryMismatch {
            directory_id: version.to_string(),
            declared_id: data.minecraft_version().to_string(),
        });
    }
    Ok(data)
}

fn read_bounded(path: &Path, max: u64) -> Result<Vec<u8>, VersionError> {
    reject_symlink(path)?;
    let mut file = File::open(path).map_err(|source| VersionError::Io {
        operation: "opening version-data file",
        path: path.to_path_buf(),
        source,
    })?;
    let metadata = file.metadata().map_err(|source| VersionError::Io {
        operation: "inspecting version-data file",
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.len() > max {
        return Err(VersionError::FileTooLarge {
            path: path.to_path_buf(),
            size: metadata.len(),
            max,
        });
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(max.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| VersionError::Io {
            operation: "reading version-data file",
            path: path.to_path_buf(),
            source,
        })?;
    let actual = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if actual > max {
        return Err(VersionError::FileTooLarge {
            path: path.to_path_buf(),
            size: actual,
            max,
        });
    }
    Ok(bytes)
}

fn require_directory(path: &Path) -> Result<(), VersionError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| VersionError::Io {
        operation: "inspecting version-data directory",
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() {
        return Err(VersionError::SymlinkNotAllowed {
            path: path.to_path_buf(),
        });
    }
    if metadata.is_dir() {
        Ok(())
    } else {
        Err(VersionError::RootNotDirectory {
            path: path.to_path_buf(),
        })
    }
}

fn reject_symlink(path: &Path) -> Result<(), VersionError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| VersionError::Io {
        operation: "inspecting version-data path",
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() {
        Err(VersionError::SymlinkNotAllowed {
            path: path.to_path_buf(),
        })
    } else {
        Ok(())
    }
}
