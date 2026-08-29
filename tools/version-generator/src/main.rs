//! Offline validation and deterministic generation for Cubic version data.

use std::{
    ffi::OsString,
    fs::{self, File},
    io::{BufReader, Read},
    path::{Path, PathBuf},
    process::ExitCode,
};

use cubic_protocol::{
    bootstrap::v775,
    packet_schema::{
        MAX_PACKET_SCHEMA_BYTES, PacketDirection, PacketLayout, PacketRegistry,
        ProtoDefIdentityAlias, ProtoDefSource, ProtocolState, generate_packet_schema_from_report,
        merge_protodef_layouts, parse_packet_schema, serialize_packet_schema,
    },
};
use cubic_version::{
    GameData, GameDataProvenance, MAX_GAME_DATA_BYTES, MinecraftIdentifier, MinecraftVersionId,
    Sha1Digest, VersionDataStore, generate_game_data_from_reports, parse_game_data,
    parse_selected_version_metadata, serialize_game_data, write_catalog,
};
use sha1::{Digest, Sha1};
use sha2::Sha256;

const MAX_METADATA_BYTES: u64 = 4 * 1024 * 1024;
const REGISTRIES_REPORT: &str = "registries.json";
const BLOCKS_REPORT: &str = "blocks.json";
const GAME_DATA_FILE: &str = "game-data.json";
const PACKETS_REPORT: &str = "packets.json";
const PACKET_SCHEMA_FILE: &str = "packet-schema.json";
const PRISMARINE_REVISION: &str = "8a80816cbfb3fe2b609f2cde4e57796c8033af61";
const PRISMARINE_PROTOCOL_SHA256: &str =
    "2dd1dcde27d5a48e8658ae3333179370a589fdbb69e6c78aadf64f7485e4723f";
const PRISMARINE_VERSION_INDEX_SHA256: &str =
    "ce7bd7523c8e3a2b27f7e84cf961ab86519426db1fcfa4c82d82cfa61eb85913";
const PRISMARINE_PROTOCOL_PATH: &str = "data/pc/26.1/protocol.json";
const PRISMARINE_VERSION_INDEX_PATH: &str = "data/pc/common/protocolVersions.json";

const V775_ALIASES: &[ProtoDefIdentityAlias] = &[
    alias(
        ProtocolState::Handshake,
        PacketDirection::Serverbound,
        "set_protocol",
        "minecraft:intention",
    ),
    alias(
        ProtocolState::Status,
        PacketDirection::Serverbound,
        "ping_start",
        "minecraft:status_request",
    ),
    alias(
        ProtocolState::Status,
        PacketDirection::Serverbound,
        "ping",
        "minecraft:ping_request",
    ),
    alias(
        ProtocolState::Status,
        PacketDirection::Clientbound,
        "server_info",
        "minecraft:status_response",
    ),
    alias(
        ProtocolState::Status,
        PacketDirection::Clientbound,
        "ping",
        "minecraft:pong_response",
    ),
    alias(
        ProtocolState::Login,
        PacketDirection::Serverbound,
        "login_start",
        "minecraft:hello",
    ),
    alias(
        ProtocolState::Login,
        PacketDirection::Clientbound,
        "encryption_begin",
        "minecraft:hello",
    ),
    alias(
        ProtocolState::Login,
        PacketDirection::Clientbound,
        "success",
        "minecraft:login_finished",
    ),
    alias(
        ProtocolState::Login,
        PacketDirection::Clientbound,
        "compress",
        "minecraft:login_compression",
    ),
    alias(
        ProtocolState::Configuration,
        PacketDirection::Serverbound,
        "settings",
        "minecraft:client_information",
    ),
    alias(
        ProtocolState::Play,
        PacketDirection::Serverbound,
        "teleport_confirm",
        "minecraft:accept_teleportation",
    ),
    alias(
        ProtocolState::Play,
        PacketDirection::Serverbound,
        "message_acknowledgement",
        "minecraft:chat_ack",
    ),
    alias(
        ProtocolState::Play,
        PacketDirection::Serverbound,
        "chat_message",
        "minecraft:chat",
    ),
    alias(
        ProtocolState::Play,
        PacketDirection::Clientbound,
        "profileless_chat",
        "minecraft:disguised_chat",
    ),
    alias(
        ProtocolState::Play,
        PacketDirection::Clientbound,
        "update_health",
        "minecraft:set_health",
    ),
];

const fn alias(
    state: ProtocolState,
    direction: PacketDirection,
    source: &'static str,
    official: &'static str,
) -> ProtoDefIdentityAlias {
    ProtoDefIdentityAlias {
        state,
        direction,
        source,
        official,
    }
}

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
        Some("packet-schema") => run_packet_schema(arguments),
        Some("validate-packet-schema") => run_validate_packet_schema(arguments),
        Some("inspect-packet-schema") => run_inspect_packet_schema(arguments),
        _ => Err(usage()),
    }
}

fn run_packet_schema(mut arguments: impl Iterator<Item = OsString>) -> Result<String, String> {
    let cache_root = required_path(&mut arguments)?;
    let version = required_utf8(&mut arguments, "Minecraft version ID")?
        .parse::<MinecraftVersionId>()
        .map_err(|error| error.to_string())?;
    let protocol = required_utf8(&mut arguments, "protocol version")?
        .parse::<i32>()
        .map_err(|error| format!("protocol version is invalid: {error}"))?;
    let reports_root = required_path(&mut arguments)?;
    let structural_root = required_path(&mut arguments)?;
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

    let report_path = reports_root.join(PACKETS_REPORT);
    let report = read_bounded(&report_path, MAX_PACKET_SCHEMA_BYTES as u64)?;
    let artifact = generate_packet_schema_from_report(
        version.clone(),
        cubic_version::ProtocolVersion::new(protocol),
        hash_bytes(&report)?,
        &report,
    )
    .map_err(|error| error.to_string())?;
    let protocol_path = structural_root.join(PRISMARINE_PROTOCOL_PATH);
    let version_index_path = structural_root.join(PRISMARINE_VERSION_INDEX_PATH);
    reject_symlink(&structural_root)?;
    reject_symlink(&protocol_path)?;
    reject_symlink(&version_index_path)?;
    let structural = read_bounded(&protocol_path, MAX_PACKET_SCHEMA_BYTES as u64)?;
    let version_index = read_bounded(&version_index_path, MAX_PACKET_SCHEMA_BYTES as u64)?;
    verify_sha256(
        &structural,
        PRISMARINE_PROTOCOL_SHA256,
        "structural protocol source",
    )?;
    verify_sha256(
        &version_index,
        PRISMARINE_VERSION_INDEX_SHA256,
        "protocol version index",
    )?;
    validate_prismarine_version_index(&version_index, &version, protocol)?;
    let artifact = merge_protodef_layouts(
        artifact,
        ProtoDefSource {
            bytes: &structural,
            source: "PrismarineJS minecraft-data",
            revision: PRISMARINE_REVISION,
            source_schema: "ProtoDef protocol.json; major version 26.1",
            content_sha256: PRISMARINE_PROTOCOL_SHA256,
            license: "MIT",
            aliases: V775_ALIASES,
        },
    )
    .map_err(|error| error.to_string())?;
    let registry = PacketRegistry::new(artifact).map_err(|error| error.to_string())?;
    let structural_cross_checks = if version.as_str() == v775::MINECRAFT_VERSION_ID
        && protocol == v775::PROTOCOL_VERSION
    {
        registry
            .cross_check(v775::packet_identity_cross_checks())
            .map_err(|error| error.to_string())?;
        Some(v775::cross_check_generated_layouts(&registry).map_err(|error| error.to_string())?)
    } else {
        None
    };
    let bytes = serialize_packet_schema(registry.artifact()).map_err(|error| error.to_string())?;
    let output_directory = output_root.join(version.as_str());
    fs::create_dir_all(&output_directory)
        .map_err(|error| format!("failed to create {}: {error}", output_directory.display()))?;
    reject_symlink(&output_directory)?;
    let output = output_directory.join(PACKET_SCHEMA_FILE);
    write_packet_schema_if_changed(&output, &bytes)?;
    packet_schema_summary(&registry, &bytes, &output, structural_cross_checks)
}

fn run_validate_packet_schema(
    mut arguments: impl Iterator<Item = OsString>,
) -> Result<String, String> {
    let artifact = required_path(&mut arguments)?;
    ensure_finished(arguments)?;
    let bytes = read_bounded(&artifact, MAX_PACKET_SCHEMA_BYTES as u64)?;
    let registry = parse_packet_schema(&bytes).map_err(|error| error.to_string())?;
    Ok(format!(
        "validated packet schema for {}: {} packet definitions",
        registry.artifact().minecraft_version,
        registry.artifact().packets.len()
    ))
}

fn run_inspect_packet_schema(
    mut arguments: impl Iterator<Item = OsString>,
) -> Result<String, String> {
    let artifact = required_path(&mut arguments)?;
    let state = parse_cli_state(&required_utf8(&mut arguments, "protocol state")?)?;
    let direction = parse_cli_direction(&required_utf8(&mut arguments, "packet direction")?)?;
    let selector = required_utf8(&mut arguments, "packet identity or numeric ID")?;
    ensure_finished(arguments)?;
    let bytes = read_bounded(&artifact, MAX_PACKET_SCHEMA_BYTES as u64)?;
    let registry = parse_packet_schema(&bytes).map_err(|error| error.to_string())?;
    let definition = if let Ok(id) = selector.parse::<u32>() {
        registry.by_id(state, direction, id)
    } else {
        let identity = MinecraftIdentifier::new(selector).map_err(|error| error.to_string())?;
        registry.by_identity(state, direction, &identity)
    }
    .ok_or_else(|| "packet is not present in that state/direction".to_owned())?;
    match &definition.layout {
        PacketLayout::Unsupported { reason } => Ok(format!(
            "Version: {}\nProtocol: {}\nState: {:?}\nDirection: {:?}\nPacket: {}\nID: {} (0x{:02x})\nLayout: identity-only\nUnsupported reason: {reason:?}",
            registry.artifact().minecraft_version,
            registry.artifact().protocol_version,
            definition.state,
            definition.direction,
            definition.identity,
            definition.id,
            definition.id
        )),
        PacketLayout::Fields { fields } => Ok(format!(
            "Version: {}\nProtocol: {}\nState: {:?}\nDirection: {:?}\nPacket: {}\nID: {} (0x{:02x})\nLayout: generated field codec\nFields: {fields:#?}",
            registry.artifact().minecraft_version,
            registry.artifact().protocol_version,
            definition.state,
            definition.direction,
            definition.identity,
            definition.id,
            definition.id
        )),
    }
}

fn packet_schema_summary(
    registry: &PacketRegistry,
    bytes: &[u8],
    output: &Path,
    structural_cross_checks: Option<usize>,
) -> Result<String, String> {
    let mut lines = vec![
        format!("Version: {}", registry.artifact().minecraft_version),
        format!("Protocol: {}", registry.artifact().protocol_version),
        format!("Schema: {}", registry.artifact().schema_version.value()),
    ];
    for state in [
        ProtocolState::Handshake,
        ProtocolState::Status,
        ProtocolState::Login,
        ProtocolState::Configuration,
        ProtocolState::Play,
    ] {
        let serverbound = count_packets(registry, state, PacketDirection::Serverbound);
        let clientbound = count_packets(registry, state, PacketDirection::Clientbound);
        lines.push(format!(
            "{state:?}: serverbound {serverbound}, clientbound {clientbound}"
        ));
    }
    let supported = registry
        .artifact()
        .packets
        .iter()
        .filter(|packet| matches!(packet.layout, PacketLayout::Fields { .. }))
        .count();
    let mut reasons = std::collections::BTreeMap::<String, usize>::new();
    for packet in &registry.artifact().packets {
        if let PacketLayout::Unsupported { reason } = &packet.layout {
            *reasons.entry(reason.category().to_owned()).or_default() += 1;
        }
    }
    lines.extend([
        format!("Total packets: {}", registry.artifact().packets.len()),
        format!("Fully generated codec layouts: {supported}"),
        format!(
            "Identity-only/unsupported definitions: {}",
            registry.artifact().packets.len() - supported
        ),
        format!("Artifact bytes: {}", bytes.len()),
        format!(
            "Official packets.json SHA-1: {}",
            registry.artifact().provenance.official_report_sha1
        ),
        format!("Content SHA-1: {}", hash_bytes(bytes)?),
        "Structural source: PrismarineJS minecraft-data".to_owned(),
        format!("Structural source revision: {PRISMARINE_REVISION}"),
        format!("Structural source SHA-256: {PRISMARINE_PROTOCOL_SHA256}"),
        format!("Unsupported reasons: {reasons:?}"),
        format!(
            "Bootstrap v775 cross-check: {}",
            if structural_cross_checks.is_some() {
                "passed"
            } else {
                "not applicable"
            }
        ),
        format!(
            "Bootstrap v775 structural cross-check: {}",
            structural_cross_checks
                .map(|count| format!("passed ({count} layouts)"))
                .unwrap_or_else(|| "not applicable".to_owned())
        ),
        format!("Output: {}", output.display()),
        "Validation: passed".to_owned(),
    ]);
    Ok(lines.join("\n"))
}

fn count_packets(
    registry: &PacketRegistry,
    state: ProtocolState,
    direction: PacketDirection,
) -> usize {
    registry
        .artifact()
        .packets
        .iter()
        .filter(|packet| packet.state == state && packet.direction == direction)
        .count()
}

fn verify_sha256(bytes: &[u8], expected: &str, label: &str) -> Result<(), String> {
    let actual = format!("{:x}", Sha256::digest(bytes));
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "{label} SHA-256 mismatch: expected {expected}, found {actual}"
        ))
    }
}

fn validate_prismarine_version_index(
    bytes: &[u8],
    version: &MinecraftVersionId,
    protocol: i32,
) -> Result<(), String> {
    let entries: Vec<serde_json::Value> = serde_json::from_slice(bytes)
        .map_err(|error| format!("protocol version index is malformed: {error}"))?;
    let matching: Vec<_> = entries
        .iter()
        .filter(|entry| {
            entry
                .get("minecraftVersion")
                .and_then(serde_json::Value::as_str)
                == Some(version.as_str())
        })
        .collect();
    if matching.len() != 1 {
        return Err(format!(
            "pinned protocol version index contains {} entries for exact version {version}",
            matching.len()
        ));
    }
    let entry = matching[0];
    if entry.get("version").and_then(serde_json::Value::as_i64) != Some(i64::from(protocol))
        || entry
            .get("majorVersion")
            .and_then(serde_json::Value::as_str)
            != Some("26.1")
    {
        return Err(format!(
            "pinned structural source does not map exact version {version} to protocol {protocol} and major schema 26.1"
        ));
    }
    Ok(())
}

fn parse_cli_state(value: &str) -> Result<ProtocolState, String> {
    match value {
        "handshake" => Ok(ProtocolState::Handshake),
        "status" => Ok(ProtocolState::Status),
        "login" => Ok(ProtocolState::Login),
        "configuration" => Ok(ProtocolState::Configuration),
        "play" => Ok(ProtocolState::Play),
        _ => Err("state must be handshake, status, login, configuration, or play".to_owned()),
    }
}
fn parse_cli_direction(value: &str) -> Result<PacketDirection, String> {
    match value {
        "serverbound" => Ok(PacketDirection::Serverbound),
        "clientbound" => Ok(PacketDirection::Clientbound),
        _ => Err("direction must be serverbound or clientbound".to_owned()),
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

fn write_packet_schema_if_changed(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if path.exists() {
        reject_symlink(path)?;
        if read_bounded(path, MAX_PACKET_SCHEMA_BYTES as u64)? == bytes {
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
    "Usage:\n  version-generator validate <version-data-root>\n  version-generator build-catalog <version-data-root>\n  version-generator game-data <minecraft-cache-root> <version-id> <reports-root> <output-root>\n  version-generator validate-game-data <game-data.json>\n  version-generator inspect-game-data <game-data.json> <identifier>\n  version-generator packet-schema <minecraft-cache-root> <version-id> <protocol> <reports-root> <pinned-minecraft-data-checkout> <output-root>\n  version-generator validate-packet-schema <packet-schema.json>\n  version-generator inspect-packet-schema <packet-schema.json> <state> <direction> <identity-or-id>".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cubic_protocol::packet_schema::{
        PacketDefinition, PacketLayout, PacketSchemaArtifact, PacketSchemaFormatVersion,
        PacketSchemaProvenance,
    };

    fn artifact_bytes() -> Vec<u8> {
        serialize_packet_schema(&PacketSchemaArtifact {
            schema_version: PacketSchemaFormatVersion::CURRENT,
            minecraft_version: MinecraftVersionId::new("synthetic").unwrap(),
            protocol_version: cubic_version::ProtocolVersion::new(99),
            provenance: PacketSchemaProvenance {
                official_report_sha1: "0123456789abcdef0123456789abcdef01234567".parse().unwrap(),
                supplemental: None,
            },
            packets: vec![PacketDefinition {
                state: ProtocolState::Status,
                direction: PacketDirection::Serverbound,
                identity: MinecraftIdentifier::new("minecraft:status_request").unwrap(),
                id: 0,
                layout: PacketLayout::Unsupported {
                    reason: cubic_protocol::packet_schema::UnsupportedLayoutReason::NoStructuralSourceEntry,
                },
            }],
        })
        .unwrap()
    }

    #[test]
    fn packet_schema_validate_and_inspect_commands_are_offline_and_deterministic() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("packet-schema.json");
        let bytes = artifact_bytes();
        fs::write(&path, &bytes).unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);
        let validated = run([
            OsString::from("validate-packet-schema"),
            path.clone().into_os_string(),
        ])
        .unwrap();
        assert!(validated.contains("1 packet definitions"));
        let inspected = run([
            OsString::from("inspect-packet-schema"),
            path.into_os_string(),
            OsString::from("status"),
            OsString::from("serverbound"),
            OsString::from("minecraft:status_request"),
        ])
        .unwrap();
        assert!(inspected.contains("ID: 0 (0x00)"));
        assert!(inspected.contains("identity-only"));
    }

    #[test]
    fn packet_schema_cli_rejects_bad_state_direction_and_extra_arguments() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("packet-schema.json");
        fs::write(&path, artifact_bytes()).unwrap();
        assert!(
            run([
                OsString::from("inspect-packet-schema"),
                path.clone().into_os_string(),
                OsString::from("future"),
                OsString::from("serverbound"),
                OsString::from("0"),
            ])
            .is_err()
        );
        assert!(
            run([
                OsString::from("validate-packet-schema"),
                path.into_os_string(),
                OsString::from("extra"),
            ])
            .is_err()
        );
    }

    #[test]
    fn pinned_structural_version_index_requires_exact_version_protocol_and_family() {
        let version = MinecraftVersionId::new("26.1.2").unwrap();
        let valid = br#"[{"minecraftVersion":"26.1.2","version":775,"majorVersion":"26.1"}]"#;
        validate_prismarine_version_index(valid, &version, 775).unwrap();
        assert!(validate_prismarine_version_index(valid, &version, 774).is_err());
        assert!(
            validate_prismarine_version_index(
                br#"[{"minecraftVersion":"26.1.2","version":775,"majorVersion":"future"}]"#,
                &version,
                775,
            )
            .is_err()
        );
        assert!(validate_prismarine_version_index(b"[]", &version, 775).is_err());
    }

    #[test]
    fn pinned_structural_hash_check_fails_closed() {
        let expected = format!("{:x}", Sha256::digest(b"schema"));
        verify_sha256(b"schema", &expected, "test source").unwrap();
        assert!(verify_sha256(b"changed", &expected, "test source").is_err());
    }
}
