//! Offline validation and deterministic generation for Cubic version data.

use std::{
    ffi::OsString,
    fs::{self, File},
    io::{BufReader, Read},
    path::{Path, PathBuf},
    process::ExitCode,
};

use cubic_version::{
    GameData, GameDataProvenance, MAX_GAME_DATA_BYTES, MinecraftIdentifier, MinecraftVersionId,
    Sha1Digest, VersionDataStore, generate_game_data_from_reports, parse_game_data,
    parse_selected_version_metadata, serialize_game_data, write_catalog,
};
use sha1::{Digest, Sha1};

const MAX_METADATA_BYTES: u64 = 4 * 1024 * 1024;
const REGISTRIES_REPORT: &str = "registries.json";
const BLOCKS_REPORT: &str = "blocks.json";
const GAME_DATA_FILE: &str = "game-data.json";

fn main() -> ExitCode {
    match run(std::env::args_os().skip(1)) {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn run(arguments: impl IntoIterator<Item = OsString>) -> Result<String, String> {
    let mut arguments = arguments.into_iter();
    let command = arguments.next().ok_or_else(usage)?;
    match command.to_str() {
        Some("validate") | Some("build-catalog") => run_version_data(command, arguments),
        Some("game-data") => run_game_data(arguments),
        Some("validate-game-data") => run_validate_game_data(arguments),
        Some("inspect-game-data") => run_inspect_game_data(arguments),
        _ => Err(usage()),
    }
}

fn run_version_data(
    command: OsString,
    mut arguments: impl Iterator<Item = OsString>,
) -> Result<String, String> {
    let root = arguments.next().map(PathBuf::from).ok_or_else(usage)?;
    ensure_finished(arguments)?;
    match command.to_str() {
        Some("validate") => {
            let store = VersionDataStore::open(&root).map_err(|error| error.to_string())?;
            Ok(format!(
                "validated {} installed version dataset(s)",
                store.available_versions().len()
            ))
        }
        Some("build-catalog") => {
            let catalog = write_catalog(&root).map_err(|error| error.to_string())?;
            VersionDataStore::open(&root).map_err(|error| error.to_string())?;
            Ok(format!(
                "wrote deterministic catalog for {} version dataset(s)",
                catalog.entries().len()
            ))
        }
        _ => Err(usage()),
    }
}

fn run_game_data(mut arguments: impl Iterator<Item = OsString>) -> Result<String, String> {
    let cache_root = required_path(&mut arguments)?;
    let version = required_utf8(&mut arguments, "Minecraft version ID")?
        .parse::<MinecraftVersionId>()
        .map_err(|error| error.to_string())?;
    let reports_root = required_path(&mut arguments)?;
    let output_root = required_path(&mut arguments)?;
    ensure_finished(arguments)?;

    let version_cache = cache_root.join("versions").join(version.as_str());
    reject_symlink(&version_cache)?;
    let metadata_bytes = read_bounded(&version_cache.join("metadata.json"), MAX_METADATA_BYTES)?;
    let metadata =
        parse_selected_version_metadata(&metadata_bytes).map_err(|error| error.to_string())?;
    if metadata.id != version {
        return Err(format!(
            "cached metadata declares version {}, expected {version}",
            metadata.id
        ));
    }
    verify_file(
        &version_cache.join("client.jar"),
        metadata.client.size,
        metadata.client.sha1,
    )?;

    let registries_bytes = read_bounded(
        &reports_root.join(REGISTRIES_REPORT),
        MAX_GAME_DATA_BYTES as u64,
    )?;
    let blocks_bytes = read_bounded(
        &reports_root.join(BLOCKS_REPORT),
        MAX_GAME_DATA_BYTES as u64,
    )?;
    let provenance = GameDataProvenance::mojang_data_generator(
        metadata.client.sha1,
        hash_bytes(&registries_bytes)?,
        hash_bytes(&blocks_bytes)?,
    );
    let artifact = generate_game_data_from_reports(
        version.clone(),
        provenance,
        &registries_bytes,
        &blocks_bytes,
    )
    .map_err(|error| error.to_string())?;
    let bytes = serialize_game_data(&artifact).map_err(|error| error.to_string())?;
    let data = parse_game_data(&bytes).map_err(|error| error.to_string())?;

    let output_directory = output_root.join(version.as_str());
    fs::create_dir_all(&output_directory)
        .map_err(|error| format!("failed to create {}: {error}", output_directory.display()))?;
    reject_symlink(&output_directory)?;
    let output = output_directory.join(GAME_DATA_FILE);
    write_if_changed(&output, &bytes)?;
    let summary = summary(&data);
    Ok(format!(
        "Version: {version}\nSchema: {}\nSource: Mojang Data Generator reports; verified client {}\nRegistries: {}\nBlocks: {}\nBlock states: {}\nItems: {}\nEntity types: {}\nArtifact bytes: {}\nApproximate loaded bytes: {}\nOutput: {}\nContent SHA-1: {}\nValidation: passed",
        artifact.schema_version.value(),
        metadata.client.sha1,
        summary.registries,
        summary.blocks,
        summary.block_states,
        summary.items,
        summary.entity_types,
        bytes.len(),
        summary.approximate_loaded_bytes,
        output.display(),
        hash_bytes(&bytes)?,
    ))
}

fn run_validate_game_data(mut arguments: impl Iterator<Item = OsString>) -> Result<String, String> {
    let artifact = required_path(&mut arguments)?;
    ensure_finished(arguments)?;
    let bytes = read_bounded(&artifact, MAX_GAME_DATA_BYTES as u64)?;
    let data = parse_game_data(&bytes).map_err(|error| error.to_string())?;
    let summary = summary(&data);
    Ok(format!(
        "validated game data for {}: {} registries, {} blocks, {} block states",
        data.artifact().minecraft_version,
        summary.registries,
        summary.blocks,
        summary.block_states
    ))
}

fn run_inspect_game_data(mut arguments: impl Iterator<Item = OsString>) -> Result<String, String> {
    let artifact = required_path(&mut arguments)?;
    let identifier = required_utf8(&mut arguments, "Minecraft identifier")?
        .parse::<MinecraftIdentifier>()
        .map_err(|error| error.to_string())?;
    ensure_finished(arguments)?;
    let bytes = read_bounded(&artifact, MAX_GAME_DATA_BYTES as u64)?;
    let data = parse_game_data(&bytes).map_err(|error| error.to_string())?;
    if let Some(block) = data.block(&identifier) {
        return Ok(format!(
            "Block: {}\nRaw ID: {}\nDefault state: {}\nProperties: {}\nStates: {}",
            block.identifier,
            block.raw_id,
            block.default_state_id,
            block.properties.len(),
            block.states.len()
        ));
    }
    for registry_name in ["minecraft:item", "minecraft:entity_type"] {
        let registry_id =
            MinecraftIdentifier::new(registry_name).map_err(|error| error.to_string())?;
        if let Some(entry) = data
            .registry(&registry_id)
            .and_then(|registry| registry.by_identifier(&identifier))
        {
            return Ok(format!(
                "Registry: {registry_name}\nEntry: {}\nRaw ID: {}",
                entry.identifier, entry.raw_id
            ));
        }
    }
    Err(format!(
        "identifier {identifier} is not a block, item, or entity type in {}",
        data.artifact().minecraft_version
    ))
}

struct Summary {
    registries: usize,
    blocks: usize,
    block_states: usize,
    items: usize,
    entity_types: usize,
    approximate_loaded_bytes: usize,
}

fn summary(data: &GameData) -> Summary {
    let artifact = data.artifact();
    let items = registry_len(data, "minecraft:item");
    let entity_types = registry_len(data, "minecraft:entity_type");
    let block_states = artifact.blocks.iter().map(|block| block.states.len()).sum();
    let strings = artifact
        .registries
        .iter()
        .map(|registry| {
            registry.identifier.as_str().len()
                + registry
                    .entries
                    .iter()
                    .map(|entry| entry.identifier.as_str().len())
                    .sum::<usize>()
        })
        .sum::<usize>()
        + artifact
            .blocks
            .iter()
            .map(|block| {
                block.identifier.as_str().len()
                    + block
                        .properties
                        .iter()
                        .map(|property| {
                            property.name.len()
                                + property.values.iter().map(String::len).sum::<usize>()
                        })
                        .sum::<usize>()
                    + block
                        .states
                        .iter()
                        .flat_map(|state| state.properties.iter())
                        .map(|(name, value)| name.len() + value.len())
                        .sum::<usize>()
            })
            .sum::<usize>();
    let collection_storage = std::mem::size_of_val(artifact.registries.as_slice())
        + artifact
            .registries
            .iter()
            .map(|registry| std::mem::size_of_val(registry.entries.as_slice()))
            .sum::<usize>()
        + std::mem::size_of_val(artifact.blocks.as_slice())
        + artifact
            .blocks
            .iter()
            .map(|block| {
                std::mem::size_of_val(block.properties.as_slice())
                    + block
                        .properties
                        .iter()
                        .map(|property| std::mem::size_of_val(property.values.as_slice()))
                        .sum::<usize>()
                    + std::mem::size_of_val(block.states.as_slice())
                    + block
                        .states
                        .iter()
                        .map(|state| {
                            state.properties.len()
                                * (std::mem::size_of::<(String, String)>()
                                    + 3 * std::mem::size_of::<usize>())
                        })
                        .sum::<usize>()
            })
            .sum::<usize>();
    Summary {
        registries: artifact.registries.len(),
        blocks: artifact.blocks.len(),
        block_states,
        items,
        entity_types,
        approximate_loaded_bytes: std::mem::size_of_val(artifact) + collection_storage + strings,
    }
}

fn registry_len(data: &GameData, name: &str) -> usize {
    MinecraftIdentifier::new(name)
        .ok()
        .and_then(|identifier| data.registry(&identifier))
        .map_or(0, |registry| registry.entries.len())
}

fn verify_file(path: &Path, expected_size: u64, expected_hash: Sha1Digest) -> Result<(), String> {
    reject_symlink(path)?;
    let file =
        File::open(path).map_err(|error| format!("failed to open {}: {error}", path.display()))?;
    let actual_size = file
        .metadata()
        .map_err(|error| format!("failed to inspect {}: {error}", path.display()))?
        .len();
    if actual_size != expected_size {
        return Err(format!(
            "{} has size {actual_size}, expected {expected_size}",
            path.display()
        ));
    }
    let mut reader = BufReader::new(file);
    let mut digest = Sha1::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    let actual = digest_to_sha1(digest.finalize().as_slice())?;
    if actual != expected_hash {
        return Err(format!(
            "{} failed published SHA-1 verification",
            path.display()
        ));
    }
    Ok(())
}

fn read_bounded(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    reject_symlink(path)?;
    let file =
        File::open(path).map_err(|error| format!("failed to open {}: {error}", path.display()))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("failed to inspect {}: {error}", path.display()))?;
    if !metadata.is_file() || metadata.len() > max {
        return Err(format!("{} is not a bounded regular file", path.display()));
    }
    let capacity = usize::try_from(metadata.len()).unwrap_or(0);
    let mut bytes = Vec::with_capacity(capacity);
    file.take(max.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    if bytes.len() as u64 > max {
        return Err(format!("{} grew beyond its bounded size", path.display()));
    }
    Ok(bytes)
}

fn write_if_changed(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if path.exists() {
        reject_symlink(path)?;
        let existing = read_bounded(path, MAX_GAME_DATA_BYTES as u64)?;
        if existing == bytes {
            return Ok(());
        }
    }
    let temporary = path.with_extension("json.part");
    if temporary.exists() {
        fs::remove_file(&temporary)
            .map_err(|error| format!("failed to remove stale {}: {error}", temporary.display()))?;
    }
    fs::write(&temporary, bytes)
        .map_err(|error| format!("failed to write {}: {error}", temporary.display()))?;
    if path.exists() {
        fs::remove_file(path)
            .map_err(|error| format!("failed to replace {}: {error}", path.display()))?;
    }
    fs::rename(&temporary, path)
        .map_err(|error| format!("failed to promote {}: {error}", path.display()))
}

fn reject_symlink(path: &Path) -> Result<(), String> {
    if path
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(format!(
            "symbolic links are not accepted: {}",
            path.display()
        ));
    }
    Ok(())
}

fn hash_bytes(bytes: &[u8]) -> Result<Sha1Digest, String> {
    digest_to_sha1(Sha1::digest(bytes).as_slice())
}

fn digest_to_sha1(bytes: &[u8]) -> Result<Sha1Digest, String> {
    if bytes.len() != 20 {
        return Err("SHA-1 implementation returned an unexpected length".to_owned());
    }
    let text = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    text.parse::<Sha1Digest>()
        .map_err(|error| error.to_string())
}

fn required_path(arguments: &mut impl Iterator<Item = OsString>) -> Result<PathBuf, String> {
    arguments.next().map(PathBuf::from).ok_or_else(usage)
}

fn required_utf8(
    arguments: &mut impl Iterator<Item = OsString>,
    label: &str,
) -> Result<String, String> {
    arguments
        .next()
        .ok_or_else(usage)?
        .into_string()
        .map_err(|_| format!("{label} must be valid UTF-8"))
}

fn ensure_finished(mut arguments: impl Iterator<Item = OsString>) -> Result<(), String> {
    if arguments.next().is_some() {
        Err(usage())
    } else {
        Ok(())
    }
}

fn usage() -> String {
    "Usage:\n  version-generator validate <version-data-root>\n  version-generator build-catalog <version-data-root>\n  version-generator game-data <minecraft-cache-root> <version-id> <reports-root> <output-root>\n  version-generator validate-game-data <game-data.json>\n  version-generator inspect-game-data <game-data.json> <identifier>".to_owned()
}
