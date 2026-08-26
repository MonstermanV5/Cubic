use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn fixture(label: &str) -> Self {
        let unique = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "cubic-version-generator-{label}-{}-{unique}",
            std::process::id()
        ));
        copy_directory(&fixture_root(), &path);
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn validate_accepts_consistent_synthetic_data() {
    let root = TestDirectory::fixture("valid");
    let output = command("validate", root.path()).output().unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("validated 3"));
}

#[test]
fn validate_rejects_inconsistent_data() {
    let root = TestDirectory::fixture("invalid");
    let path = root
        .path()
        .join("versions/cubic-test-release-b/version.json");
    let changed = fs::read_to_string(&path).unwrap().replace("9001", "9002");
    fs::write(path, changed).unwrap();
    let output = command("validate", root.path()).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("does not match"));
}

#[test]
fn build_catalog_is_byte_identical_across_runs() {
    let root = TestDirectory::fixture("catalog");
    fs::remove_file(root.path().join("catalog.json")).unwrap();
    assert!(
        command("build-catalog", root.path())
            .status()
            .unwrap()
            .success()
    );
    let first = fs::read(root.path().join("catalog.json")).unwrap();
    assert!(
        command("build-catalog", root.path())
            .status()
            .unwrap()
            .success()
    );
    let second = fs::read(root.path().join("catalog.json")).unwrap();
    assert_eq!(first, second);
}

fn command(action: &str, root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_version-generator"));
    command.arg(action).arg(root);
    command
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/cubic-version/tests/fixtures/version-data")
}

fn copy_directory(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let destination_entry = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_directory(&entry.path(), &destination_entry);
        } else {
            fs::copy(entry.path(), destination_entry).unwrap();
        }
    }
}
