mod common;

use std::fs;

use common::TestDirectory;
use cubic_version::{
    MAX_VERSION_FILE_BYTES, MinecraftVersionId, MinecraftVersionKind, ProtocolVersion,
    VersionDataStore, VersionError, build_catalog, serialize_catalog, write_catalog,
};

#[test]
fn exact_lookup_and_unknown_lookup_are_explicit() {
    let root = TestDirectory::fixture("exact");
    let store = VersionDataStore::open(root.path()).unwrap();
    let release = store
        .load_exact(&MinecraftVersionId::new("cubic-test-release-a").unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(release.kind(), MinecraftVersionKind::Release);
    assert!(
        store
            .load_exact(&MinecraftVersionId::new("not-installed").unwrap())
            .unwrap()
            .is_none()
    );
}

#[test]
fn protocol_lookup_returns_zero_one_or_multiple_in_version_id_order() {
    let root = TestDirectory::fixture("protocol");
    let store = VersionDataStore::open(root.path()).unwrap();
    assert!(
        store
            .find_by_protocol(ProtocolVersion::new(8999))
            .unwrap()
            .is_empty()
    );
    let one = store.find_by_protocol(ProtocolVersion::new(9001)).unwrap();
    assert_eq!(one.len(), 1);
    let shared = store.find_by_protocol(ProtocolVersion::new(9000)).unwrap();
    let ids: Vec<_> = shared
        .iter()
        .map(|data| data.minecraft_version().as_str())
        .collect();
    assert_eq!(ids, ["cubic-test-release-a", "cubic-test-snapshot"]);
}

#[test]
fn releases_and_snapshots_coexist_in_deterministic_order() {
    let root = TestDirectory::fixture("coexist");
    let store = VersionDataStore::open(root.path()).unwrap();
    let kinds: Vec<_> = store
        .available_versions()
        .iter()
        .map(|entry| entry.kind())
        .collect();
    assert_eq!(
        kinds,
        [
            MinecraftVersionKind::Release,
            MinecraftVersionKind::Release,
            MinecraftVersionKind::Snapshot
        ]
    );
}

#[test]
fn generated_catalog_is_identical_across_repeated_runs() {
    let root = TestDirectory::fixture("repeat");
    fs::remove_file(root.path().join("catalog.json")).unwrap();
    let first_catalog = write_catalog(root.path()).unwrap();
    let first = fs::read(root.path().join("catalog.json")).unwrap();
    let second_catalog = write_catalog(root.path()).unwrap();
    let second = fs::read(root.path().join("catalog.json")).unwrap();
    assert_eq!(first_catalog, second_catalog);
    assert_eq!(first, second);
    assert_eq!(
        first,
        serialize_catalog(&build_catalog(root.path()).unwrap()).unwrap()
    );
}

#[test]
fn catalog_dataset_mismatch_is_rejected() {
    let root = TestDirectory::fixture("mismatch");
    let path = root
        .path()
        .join("versions/cubic-test-release-b/version.json");
    let changed = fs::read_to_string(&path).unwrap().replace("9001", "9002");
    fs::write(path, changed).unwrap();
    assert!(matches!(
        VersionDataStore::open(root.path()),
        Err(VersionError::CatalogDatasetMismatch {
            field: "protocol",
            ..
        })
    ));
}

#[test]
fn directory_and_declared_version_id_must_match() {
    let root = TestDirectory::fixture("directory-mismatch");
    let path = root
        .path()
        .join("versions/cubic-test-release-a/version.json");
    let changed = fs::read_to_string(&path)
        .unwrap()
        .replace("cubic-test-release-a", "different-id");
    fs::write(path, changed).unwrap();
    assert!(matches!(
        build_catalog(root.path()),
        Err(VersionError::DatasetDirectoryMismatch { .. })
    ));
}

#[test]
fn oversized_metadata_is_rejected_before_json_parsing() {
    let root = TestDirectory::fixture("oversized");
    let path = root
        .path()
        .join("versions/cubic-test-release-a/version.json");
    let size = usize::try_from(MAX_VERSION_FILE_BYTES).unwrap() + 1;
    fs::write(path, vec![b' '; size]).unwrap();
    assert!(matches!(
        VersionDataStore::open(root.path()),
        Err(VersionError::FileTooLarge { .. })
    ));
}
