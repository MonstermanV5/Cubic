use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

use sha1::{Digest, Sha1};

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

    fn empty(label: &str) -> Self {
        let unique = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "cubic-version-generator-{label}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
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

#[test]
fn game_data_generation_is_deterministic_and_inspectable() {
    let root = TestDirectory::empty("game-data");
    let fixture = GameDataFixture::create(root.path());
    let first = game_data_command(&fixture).output().unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let output = fixture.output_file();
    let first_bytes = fs::read(&output).unwrap();
    assert!(game_data_command(&fixture).status().unwrap().success());
    assert_eq!(first_bytes, fs::read(&output).unwrap());

    let validation = command_args(["validate-game-data".as_ref(), output.as_os_str()])
        .output()
        .unwrap();
    assert!(validation.status.success());
    let inspection = command_args([
        "inspect-game-data".as_ref(),
        output.as_os_str(),
        "minecraft:stone".as_ref(),
    ])
    .output()
    .unwrap();
    assert!(inspection.status.success());
    assert!(String::from_utf8_lossy(&inspection.stdout).contains("Default state: 1"));
}

#[test]
fn game_data_rejects_unverified_client_and_malformed_reports() {
    let root = TestDirectory::empty("game-data-invalid");
    let fixture = GameDataFixture::create(root.path());
    fs::write(&fixture.client, b"tampered").unwrap();
    let bad_client = game_data_command(&fixture).output().unwrap();
    assert!(!bad_client.status.success());
    assert!(String::from_utf8_lossy(&bad_client.stderr).contains("size"));

    fs::write(&fixture.client, GameDataFixture::CLIENT_BYTES).unwrap();
    fs::write(fixture.reports.join("blocks.json"), b"{").unwrap();
    let bad_report = game_data_command(&fixture).output().unwrap();
    assert!(!bad_report.status.success());
    assert!(String::from_utf8_lossy(&bad_report.stderr).contains("malformed"));
}

struct GameDataFixture {
    cache: PathBuf,
    reports: PathBuf,
    output: PathBuf,
    client: PathBuf,
}

impl GameDataFixture {
    const CLIENT_BYTES: &'static [u8] = b"synthetic verified client bytes";

    fn create(root: &Path) -> Self {
        let cache = root.join("cache");
        let reports = root.join("reports");
        let output = root.join("generated");
        let version = cache.join("versions/test-version");
        fs::create_dir_all(&version).unwrap();
        fs::create_dir_all(&reports).unwrap();
        let client = version.join("client.jar");
        fs::write(&client, Self::CLIENT_BYTES).unwrap();
        let hash = hex_sha1(Self::CLIENT_BYTES);
        fs::write(
            version.join("metadata.json"),
            format!(
                r#"{{"id":"test-version","type":"release","assetIndex":{{"id":"synthetic","url":"https://piston-meta.mojang.com/assets","sha1":"{hash}","size":1}},"downloads":{{"client":{{"url":"https://piston-data.mojang.com/client.jar","sha1":"{hash}","size":{}}}}}}}"#,
                Self::CLIENT_BYTES.len()
            ),
        )
        .unwrap();
        fs::write(
            reports.join("registries.json"),
            r#"{"minecraft:block":{"entries":{"minecraft:air":{"protocol_id":0},"minecraft:stone":{"protocol_id":1}}},"minecraft:item":{"entries":{"minecraft:stone":{"protocol_id":2}}},"minecraft:entity_type":{"entries":{"minecraft:pig":{"protocol_id":3}}}}"#,
        )
        .unwrap();
        fs::write(
            reports.join("blocks.json"),
            r#"{"minecraft:air":{"states":[{"id":0,"default":true}]},"minecraft:stone":{"properties":{"variant":["plain","smooth"]},"states":[{"id":1,"default":true,"properties":{"variant":"plain"}},{"id":2,"properties":{"variant":"smooth"}}]}}"#,
        )
        .unwrap();
        Self {
            cache,
            reports,
            output,
            client,
        }
    }

    fn output_file(&self) -> PathBuf {
        self.output.join("test-version/game-data.json")
    }
}

fn game_data_command(fixture: &GameDataFixture) -> Command {
    command_args([
        "game-data".as_ref(),
        fixture.cache.as_os_str(),
        "test-version".as_ref(),
        fixture.reports.as_os_str(),
        fixture.output.as_os_str(),
    ])
}

fn command_args(arguments: impl IntoIterator<Item = impl AsRef<std::ffi::OsStr>>) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_version-generator"));
    command.args(arguments);
    command
}

fn hex_sha1(bytes: &[u8]) -> String {
    Sha1::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
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
