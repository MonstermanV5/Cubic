use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    marker::PhantomData,
    ops::Deref,
    path::Path,
    str::FromStr,
};

use serde::{
    Deserialize, Serialize,
    de::{MapAccess, Visitor},
};
use serde_json::Value;

use crate::{MinecraftVersionId, Sha1Digest, VersionError};

pub const MAX_GAME_DATA_BYTES: usize = 64 * 1024 * 1024;
pub const GAME_DATA_FILE_NAME: &str = "game-data.json";
const MAX_IDENTIFIER_BYTES: usize = 256;
const MAX_REGISTRIES: usize = 512;
const MAX_ENTRIES_PER_REGISTRY: usize = 65_536;
const MAX_BLOCKS: usize = 8_192;
const MAX_BLOCK_STATES: usize = 1_048_576;
const MAX_PROPERTIES_PER_BLOCK: usize = 64;
const MAX_VALUES_PER_PROPERTY: usize = 256;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct GameDataFormatVersion(u32);

impl GameDataFormatVersion {
    pub const CURRENT: Self = Self(1);

    #[must_use]
    pub const fn value(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct MinecraftIdentifier(String);

impl MinecraftIdentifier {
    pub fn new(value: impl Into<String>) -> Result<Self, VersionError> {
        let value = value.into();
        validate_identifier(&value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MinecraftIdentifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for MinecraftIdentifier {
    type Err = VersionError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for MinecraftIdentifier {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GameDataProvenance {
    pub source: String,
    pub client_jar_sha1: Sha1Digest,
    pub registries_report_sha1: Sha1Digest,
    pub blocks_report_sha1: Sha1Digest,
}

impl GameDataProvenance {
    pub fn mojang_data_generator(
        client_jar_sha1: Sha1Digest,
        registries_report_sha1: Sha1Digest,
        blocks_report_sha1: Sha1Digest,
    ) -> Self {
        Self {
            source: "mojang_data_generator_reports".to_owned(),
            client_jar_sha1,
            registries_report_sha1,
            blocks_report_sha1,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RegistryEntry {
    pub identifier: MinecraftIdentifier,
    pub raw_id: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RegistryTable {
    pub identifier: MinecraftIdentifier,
    pub entries: Vec<RegistryEntry>,
}

impl RegistryTable {
    #[must_use]
    pub fn by_identifier(&self, identifier: &MinecraftIdentifier) -> Option<&RegistryEntry> {
        self.entries
            .binary_search_by(|entry| entry.identifier.cmp(identifier))
            .ok()
            .and_then(|index| self.entries.get(index))
    }

    #[must_use]
    pub fn by_raw_id(&self, raw_id: u32) -> Option<&RegistryEntry> {
        self.entries.iter().find(|entry| entry.raw_id == raw_id)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BlockProperty {
    pub name: String,
    pub values: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BlockState {
    pub state_id: u32,
    pub properties: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BlockDefinition {
    pub identifier: MinecraftIdentifier,
    pub raw_id: u32,
    pub default_state_id: u32,
    pub properties: Vec<BlockProperty>,
    pub states: Vec<BlockState>,
}

impl BlockDefinition {
    #[must_use]
    pub fn state(&self, state_id: u32) -> Option<&BlockState> {
        self.states
            .binary_search_by_key(&state_id, |state| state.state_id)
            .ok()
            .and_then(|index| self.states.get(index))
    }

    #[must_use]
    pub fn state_for_properties(
        &self,
        properties: &BTreeMap<String, String>,
    ) -> Option<&BlockState> {
        self.states
            .iter()
            .find(|state| &state.properties == properties)
    }
}

/// A generated vanilla baseline. Runtime server-supplied registries remain a
/// separate future overlay and must not mutate this immutable artifact.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GameDataArtifact {
    pub schema_version: GameDataFormatVersion,
    pub minecraft_version: MinecraftVersionId,
    pub provenance: GameDataProvenance,
    pub registries: Vec<RegistryTable>,
    pub blocks: Vec<BlockDefinition>,
}

#[derive(Clone, Debug)]
pub struct GameData {
    artifact: GameDataArtifact,
}

impl GameData {
    pub fn new(artifact: GameDataArtifact) -> Result<Self, VersionError> {
        validate_artifact(&artifact)?;
        Ok(Self { artifact })
    }

    pub fn load(root: &Path, version: &MinecraftVersionId) -> Result<Self, VersionError> {
        let directory = root.join(version.as_str());
        let path = directory.join(GAME_DATA_FILE_NAME);
        for candidate in [&directory, &path] {
            if fs::symlink_metadata(candidate)
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
            {
                return Err(VersionError::SymlinkNotAllowed {
                    path: candidate.to_path_buf(),
                });
            }
        }
        let metadata = fs::metadata(&path).map_err(|source| VersionError::Io {
            operation: "inspecting generated game data",
            path: path.clone(),
            source,
        })?;
        if metadata.len() > MAX_GAME_DATA_BYTES as u64 {
            return Err(VersionError::FileTooLarge {
                path,
                size: metadata.len(),
                max: MAX_GAME_DATA_BYTES as u64,
            });
        }
        let bytes = fs::read(&path).map_err(|source| VersionError::Io {
            operation: "reading generated game data",
            path,
            source,
        })?;
        let data = parse_game_data(&bytes)?;
        if &data.artifact.minecraft_version != version {
            return Err(VersionError::DatasetDirectoryMismatch {
                directory_id: version.to_string(),
                declared_id: data.artifact.minecraft_version.to_string(),
            });
        }
        Ok(data)
    }

    #[must_use]
    pub fn artifact(&self) -> &GameDataArtifact {
        &self.artifact
    }

    #[must_use]
    pub fn registry(&self, identifier: &MinecraftIdentifier) -> Option<&RegistryTable> {
        self.artifact
            .registries
            .binary_search_by(|registry| registry.identifier.cmp(identifier))
            .ok()
            .and_then(|index| self.artifact.registries.get(index))
    }

    #[must_use]
    pub fn block(&self, identifier: &MinecraftIdentifier) -> Option<&BlockDefinition> {
        self.artifact
            .blocks
            .binary_search_by(|block| block.identifier.cmp(identifier))
            .ok()
            .and_then(|index| self.artifact.blocks.get(index))
    }

    #[must_use]
    pub fn block_by_raw_id(&self, raw_id: u32) -> Option<&BlockDefinition> {
        self.artifact
            .blocks
            .iter()
            .find(|block| block.raw_id == raw_id)
    }

    #[must_use]
    pub fn block_state(&self, state_id: u32) -> Option<(&BlockDefinition, &BlockState)> {
        self.artifact
            .blocks
            .iter()
            .find_map(|block| block.state(state_id).map(|state| (block, state)))
    }

    #[must_use]
    pub fn item(&self, identifier: &MinecraftIdentifier) -> Option<&RegistryEntry> {
        self.registry(&MinecraftIdentifier("minecraft:item".to_owned()))?
            .by_identifier(identifier)
    }

    #[must_use]
    pub fn item_by_raw_id(&self, raw_id: u32) -> Option<&RegistryEntry> {
        self.registry(&MinecraftIdentifier("minecraft:item".to_owned()))?
            .by_raw_id(raw_id)
    }

    #[must_use]
    pub fn entity_type(&self, identifier: &MinecraftIdentifier) -> Option<&RegistryEntry> {
        self.registry(&MinecraftIdentifier("minecraft:entity_type".to_owned()))?
            .by_identifier(identifier)
    }

    #[must_use]
    pub fn entity_type_by_raw_id(&self, raw_id: u32) -> Option<&RegistryEntry> {
        self.registry(&MinecraftIdentifier("minecraft:entity_type".to_owned()))?
            .by_raw_id(raw_id)
    }
}

#[derive(Deserialize)]
struct ReportRegistry {
    entries: UniqueMap<String, ReportRegistryEntry>,
}

#[derive(Deserialize)]
struct ReportRegistryEntry {
    protocol_id: i64,
}

#[derive(Deserialize)]
struct ReportBlock {
    #[serde(default)]
    properties: UniqueMap<String, Vec<String>>,
    states: Vec<ReportBlockState>,
}

#[derive(Deserialize)]
struct ReportBlockState {
    id: i64,
    #[serde(default)]
    default: bool,
    #[serde(default)]
    properties: UniqueMap<String, String>,
}

struct UniqueMap<K, V>(BTreeMap<K, V>);

impl<K, V> Default for UniqueMap<K, V> {
    fn default() -> Self {
        Self(BTreeMap::new())
    }
}

impl<K, V> Deref for UniqueMap<K, V> {
    type Target = BTreeMap<K, V>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<K, V> IntoIterator for UniqueMap<K, V> {
    type Item = (K, V);
    type IntoIter = std::collections::btree_map::IntoIter<K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'de, K, V> Deserialize<'de> for UniqueMap<K, V>
where
    K: Deserialize<'de> + Ord + fmt::Display,
    V: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct UniqueMapVisitor<K, V>(PhantomData<(K, V)>);

        impl<'de, K, V> Visitor<'de> for UniqueMapVisitor<K, V>
        where
            K: Deserialize<'de> + Ord + fmt::Display,
            V: Deserialize<'de>,
        {
            type Value = UniqueMap<K, V>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON object without duplicate keys")
            }

            fn visit_map<A>(self, mut access: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut values = BTreeMap::new();
                while let Some((key, value)) = access.next_entry()? {
                    if values.insert(key, value).is_some() {
                        return Err(serde::de::Error::custom("duplicate JSON object key"));
                    }
                }
                Ok(UniqueMap(values))
            }
        }

        deserializer.deserialize_map(UniqueMapVisitor(PhantomData))
    }
}

pub fn generate_game_data_from_reports(
    minecraft_version: MinecraftVersionId,
    provenance: GameDataProvenance,
    registries_json: &[u8],
    blocks_json: &[u8],
) -> Result<GameDataArtifact, VersionError> {
    bounded(registries_json, "registries report")?;
    bounded(blocks_json, "blocks report")?;
    let report_registries: UniqueMap<String, ReportRegistry> = parse_report(registries_json)?;
    let report_blocks: UniqueMap<String, ReportBlock> = parse_report(blocks_json)?;
    if report_registries.len() > MAX_REGISTRIES || report_blocks.len() > MAX_BLOCKS {
        return invalid("report contains too many registries or blocks");
    }

    let mut registries = Vec::with_capacity(report_registries.len());
    for (name, report) in report_registries {
        if report.entries.len() > MAX_ENTRIES_PER_REGISTRY {
            return invalid("registry contains too many entries");
        }
        let mut entries = Vec::with_capacity(report.entries.len());
        for (entry_name, entry) in report.entries {
            entries.push(RegistryEntry {
                identifier: MinecraftIdentifier::new(entry_name)?,
                raw_id: u32::try_from(entry.protocol_id).map_err(|_| {
                    VersionError::InvalidGameData {
                        reason: "registry raw ID is negative or too large".to_owned(),
                    }
                })?,
            });
        }
        entries.sort_by(|left, right| left.identifier.cmp(&right.identifier));
        registries.push(RegistryTable {
            identifier: MinecraftIdentifier::new(name)?,
            entries,
        });
    }
    registries.sort_by(|left, right| left.identifier.cmp(&right.identifier));

    let block_registry_id = MinecraftIdentifier::new("minecraft:block")?;
    let block_registry = registries
        .iter()
        .find(|registry| registry.identifier == block_registry_id)
        .ok_or_else(|| VersionError::InvalidGameData {
            reason: "registries report has no minecraft:block registry".to_owned(),
        })?;
    let mut blocks = Vec::with_capacity(report_blocks.len());
    let mut state_count = 0_usize;
    for (name, report) in report_blocks {
        state_count = state_count
            .checked_add(report.states.len())
            .ok_or_else(|| VersionError::InvalidGameData {
                reason: "block-state count overflow".to_owned(),
            })?;
        if state_count > MAX_BLOCK_STATES || report.properties.len() > MAX_PROPERTIES_PER_BLOCK {
            return invalid("report contains too many block states or properties");
        }
        let identifier = MinecraftIdentifier::new(name)?;
        let raw_id = block_registry
            .by_identifier(&identifier)
            .ok_or_else(|| VersionError::InvalidGameData {
                reason: format!("block {identifier} is absent from minecraft:block"),
            })?
            .raw_id;
        let mut properties = Vec::with_capacity(report.properties.len());
        for (name, mut values) in report.properties {
            validate_property_token(&name)?;
            if values.is_empty() || values.len() > MAX_VALUES_PER_PROPERTY {
                return invalid("block property has an invalid value count");
            }
            for value in &values {
                validate_property_token(value)?;
            }
            values.sort();
            values.dedup();
            properties.push(BlockProperty { name, values });
        }
        properties.sort_by(|left, right| left.name.cmp(&right.name));
        let defaults: Vec<_> = report.states.iter().filter(|state| state.default).collect();
        if defaults.len() != 1 {
            return invalid("block must have exactly one default state");
        }
        let default_state_id =
            u32::try_from(defaults[0].id).map_err(|_| VersionError::InvalidGameData {
                reason: "default block-state ID is negative or too large".to_owned(),
            })?;
        let mut states = Vec::with_capacity(report.states.len());
        for state in report.states {
            states.push(BlockState {
                state_id: u32::try_from(state.id).map_err(|_| VersionError::InvalidGameData {
                    reason: "block-state ID is negative or too large".to_owned(),
                })?,
                properties: state.properties.0,
            });
        }
        states.sort_by_key(|state| state.state_id);
        blocks.push(BlockDefinition {
            identifier,
            raw_id,
            default_state_id,
            properties,
            states,
        });
    }
    blocks.sort_by(|left, right| left.identifier.cmp(&right.identifier));
    let artifact = GameDataArtifact {
        schema_version: GameDataFormatVersion::CURRENT,
        minecraft_version,
        provenance,
        registries,
        blocks,
    };
    validate_artifact(&artifact)?;
    Ok(artifact)
}

pub fn serialize_game_data(artifact: &GameDataArtifact) -> Result<Vec<u8>, VersionError> {
    validate_artifact(artifact)?;
    let mut bytes = serde_json::to_vec_pretty(artifact).map_err(VersionError::Serialization)?;
    bytes.push(b'\n');
    Ok(bytes)
}

pub fn parse_game_data(bytes: &[u8]) -> Result<GameData, VersionError> {
    bounded(bytes, "generated artifact")?;
    let value: Value = parse_report(bytes)?;
    let found = value
        .get("schema_version")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| VersionError::InvalidGameData {
            reason: "missing or invalid schema_version".to_owned(),
        })?;
    if found != GameDataFormatVersion::CURRENT.value() {
        return Err(VersionError::UnsupportedGameDataFormat {
            found,
            supported: GameDataFormatVersion::CURRENT.value(),
        });
    }
    let artifact: GameDataArtifact =
        serde_json::from_value(value).map_err(|error| VersionError::InvalidGameData {
            reason: format!("generated artifact shape is invalid: {error}"),
        })?;
    GameData::new(artifact)
}

fn validate_artifact(artifact: &GameDataArtifact) -> Result<(), VersionError> {
    if artifact.schema_version != GameDataFormatVersion::CURRENT {
        return Err(VersionError::UnsupportedGameDataFormat {
            found: artifact.schema_version.value(),
            supported: GameDataFormatVersion::CURRENT.value(),
        });
    }
    if artifact.provenance.source != "mojang_data_generator_reports" {
        return invalid("unsupported or missing provenance source");
    }
    if artifact.registries.len() > MAX_REGISTRIES || artifact.blocks.len() > MAX_BLOCKS {
        return invalid("artifact exceeds registry or block count limits");
    }
    ensure_sorted_unique(
        artifact
            .registries
            .iter()
            .map(|registry| &registry.identifier),
        "registry identifier",
    )?;
    let mut global_state_ids = BTreeSet::new();
    for registry in &artifact.registries {
        if registry.entries.len() > MAX_ENTRIES_PER_REGISTRY {
            return invalid("registry exceeds entry count limit");
        }
        ensure_sorted_unique(
            registry.entries.iter().map(|entry| &entry.identifier),
            "registry entry identifier",
        )?;
        let mut raw_ids = BTreeSet::new();
        if registry
            .entries
            .iter()
            .any(|entry| !raw_ids.insert(entry.raw_id))
        {
            return invalid("registry contains duplicate raw IDs");
        }
    }
    ensure_sorted_unique(
        artifact.blocks.iter().map(|block| &block.identifier),
        "block identifier",
    )?;
    let block_registry = artifact
        .registries
        .iter()
        .find(|registry| registry.identifier.as_str() == "minecraft:block")
        .ok_or_else(|| VersionError::InvalidGameData {
            reason: "artifact has no minecraft:block registry".to_owned(),
        })?;
    if block_registry.entries.len() != artifact.blocks.len() {
        return invalid("minecraft:block entries and block definitions differ in count");
    }
    for block in &artifact.blocks {
        if block.properties.len() > MAX_PROPERTIES_PER_BLOCK {
            return invalid("block exceeds property count limit");
        }
        let registry_entry = block_registry
            .by_identifier(&block.identifier)
            .ok_or_else(|| VersionError::InvalidGameData {
                reason: format!("block {} is absent from minecraft:block", block.identifier),
            })?;
        if registry_entry.raw_id != block.raw_id {
            return invalid("block raw ID does not match minecraft:block registry");
        }
        ensure_sorted_unique(
            block.properties.iter().map(|property| &property.name),
            "block property",
        )?;
        for property in &block.properties {
            validate_property_token(&property.name)?;
            if property.values.is_empty() || property.values.len() > MAX_VALUES_PER_PROPERTY {
                return invalid("block property has an invalid value count");
            }
            ensure_sorted_unique(property.values.iter(), "block property value")?;
            for value in &property.values {
                validate_property_token(value)?;
            }
        }
        if block.states.is_empty() || block.state(block.default_state_id).is_none() {
            return invalid("block has no states or its default state is missing");
        }
        let mut previous = None;
        for state in &block.states {
            if previous.is_some_and(|value| value >= state.state_id)
                || !global_state_ids.insert(state.state_id)
            {
                return invalid("duplicate or unsorted block-state ID");
            }
            previous = Some(state.state_id);
            if state.properties.len() != block.properties.len() {
                return invalid("block state does not define every property exactly once");
            }
            for property in &block.properties {
                let value = state.properties.get(&property.name).ok_or_else(|| {
                    VersionError::InvalidGameData {
                        reason: "block state is missing a property".to_owned(),
                    }
                })?;
                if !property.values.iter().any(|allowed| allowed == value) {
                    return invalid("block state uses an impossible property value");
                }
            }
        }
    }
    Ok(())
}

fn validate_identifier(value: &str) -> Result<(), VersionError> {
    if value.is_empty() || value.len() > MAX_IDENTIFIER_BYTES || value.chars().any(char::is_control)
    {
        return invalid("Minecraft identifier is empty, overlong, or contains controls");
    }
    let (namespace, path) = value
        .split_once(':')
        .ok_or_else(|| VersionError::InvalidGameData {
            reason: "Minecraft identifier must contain an explicit namespace".to_owned(),
        })?;
    if namespace.is_empty()
        || path.is_empty()
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return invalid("Minecraft identifier has an invalid namespace or path");
    }
    if !namespace
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte))
        || !path.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"/._-".contains(&byte)
        })
    {
        return invalid("Minecraft identifier contains unsupported characters");
    }
    Ok(())
}

fn validate_property_token(value: &str) -> Result<(), VersionError> {
    if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        return invalid("block property name/value is empty, overlong, or contains controls");
    }
    Ok(())
}

fn ensure_sorted_unique<'a, T: Ord + ?Sized + 'a>(
    values: impl Iterator<Item = &'a T>,
    label: &str,
) -> Result<(), VersionError> {
    let mut previous: Option<&T> = None;
    for value in values {
        if previous.is_some_and(|prior| prior >= value) {
            return invalid(format!("duplicate or unsorted {label}"));
        }
        previous = Some(value);
    }
    Ok(())
}

fn bounded(bytes: &[u8], label: &str) -> Result<(), VersionError> {
    if bytes.len() > MAX_GAME_DATA_BYTES {
        return invalid(format!("{label} exceeds {MAX_GAME_DATA_BYTES} bytes"));
    }
    Ok(())
}

fn parse_report<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, VersionError> {
    serde_json::from_slice(bytes).map_err(|error| VersionError::InvalidGameData {
        reason: format!(
            "JSON is malformed at line {}, column {}",
            error.line(),
            error.column()
        ),
    })
}

fn invalid<T>(reason: impl Into<String>) -> Result<T, VersionError> {
    Err(VersionError::InvalidGameData {
        reason: reason.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH_A: &str = "0123456789abcdef0123456789abcdef01234567";
    const HASH_B: &str = "89abcdef0123456789abcdef0123456789abcdef";

    fn provenance() -> GameDataProvenance {
        GameDataProvenance::mojang_data_generator(
            HASH_A.parse().unwrap(),
            HASH_B.parse().unwrap(),
            HASH_A.parse().unwrap(),
        )
    }

    fn reports(block_raw: u32, stone_state: u32) -> (Vec<u8>, Vec<u8>) {
        (format!(r#"{{"minecraft:block":{{"protocol_id":0,"entries":{{"minecraft:air":{{"protocol_id":0}},"minecraft:stone":{{"protocol_id":{block_raw}}}}}}},"minecraft:item":{{"entries":{{"minecraft:air":{{"protocol_id":0}},"example:widget":{{"protocol_id":9}}}}}},"minecraft:entity_type":{{"entries":{{"minecraft:pig":{{"protocol_id":4}}}}}},"future:registry":{{"entries":{{"other:entry":{{"protocol_id":12}}}}}}}}"#).into_bytes(),
         format!(r#"{{"minecraft:air":{{"states":[{{"id":0,"default":true}}]}},"minecraft:stone":{{"properties":{{"variant":["smooth","rough"]}},"states":[{{"id":{stone_state},"default":true,"properties":{{"variant":"smooth"}}}},{{"id":{},"properties":{{"variant":"rough"}}}}]}}}}"#, stone_state + 1).into_bytes())
    }

    fn generated(version: &str, block_raw: u32, stone_state: u32) -> GameDataArtifact {
        let (registries, blocks) = reports(block_raw, stone_state);
        generate_game_data_from_reports(
            MinecraftVersionId::new(version).unwrap(),
            provenance(),
            &registries,
            &blocks,
        )
        .unwrap()
    }

    #[test]
    fn generation_is_deterministic_and_lookups_cover_core_registries() {
        let artifact = generated("test-a", 1, 10);
        let first = serialize_game_data(&artifact).unwrap();
        let second = serialize_game_data(&generated("test-a", 1, 10)).unwrap();
        assert_eq!(first, second);
        let data = parse_game_data(&first).unwrap();
        let stone = MinecraftIdentifier::new("minecraft:stone").unwrap();
        assert_eq!(data.block(&stone).unwrap().raw_id, 1);
        assert_eq!(data.block_by_raw_id(1).unwrap().identifier, stone);
        assert_eq!(
            data.block_state(10).unwrap().1.properties["variant"],
            "smooth"
        );
        assert_eq!(
            data.item(&MinecraftIdentifier::new("example:widget").unwrap())
                .unwrap()
                .raw_id,
            9
        );
        assert_eq!(
            data.item_by_raw_id(9).unwrap().identifier.as_str(),
            "example:widget"
        );
        assert_eq!(
            data.entity_type(&MinecraftIdentifier::new("minecraft:pig").unwrap())
                .unwrap()
                .raw_id,
            4
        );
        assert_eq!(
            data.entity_type_by_raw_id(4).unwrap().identifier.as_str(),
            "minecraft:pig"
        );
        assert!(
            data.registry(&MinecraftIdentifier::new("future:registry").unwrap())
                .is_some()
        );
    }

    #[test]
    fn two_versions_keep_different_sparse_ids_and_layouts_isolated() {
        let first = GameData::new(generated("test-a", 7, 100)).unwrap();
        let mut second_artifact = generated("test-b", 42, 900);
        second_artifact
            .registries
            .retain(|registry| registry.identifier.as_str() != "future:registry");
        let second = GameData::new(second_artifact).unwrap();
        let stone = MinecraftIdentifier::new("minecraft:stone").unwrap();
        assert_eq!(first.block(&stone).unwrap().raw_id, 7);
        assert_eq!(second.block(&stone).unwrap().raw_id, 42);
        assert!(first.block_state(900).is_none());
        assert!(second.block_state(100).is_none());
        assert!(
            first
                .registry(&MinecraftIdentifier::new("future:registry").unwrap())
                .is_some()
        );
        assert!(
            second
                .registry(&MinecraftIdentifier::new("future:registry").unwrap())
                .is_none()
        );
    }

    #[test]
    fn block_property_lookup_and_default_are_validated() {
        let data = GameData::new(generated("test-a", 1, 10)).unwrap();
        let block = data
            .block(&MinecraftIdentifier::new("minecraft:stone").unwrap())
            .unwrap();
        let properties = BTreeMap::from([("variant".to_owned(), "rough".to_owned())]);
        assert_eq!(
            block.state_for_properties(&properties).unwrap().state_id,
            11
        );
        let (_, blocks) = reports(1, 10);
        let invalid_blocks = String::from_utf8(blocks)
            .unwrap()
            .replace("\"default\":true,", "");
        let (registries, _) = reports(1, 10);
        assert!(
            generate_game_data_from_reports(
                MinecraftVersionId::new("x").unwrap(),
                provenance(),
                &registries,
                invalid_blocks.as_bytes()
            )
            .is_err()
        );
    }

    #[test]
    fn duplicates_invalid_properties_and_missing_blocks_are_rejected() {
        let mut artifact = generated("test-a", 1, 10);
        let duplicate = artifact.registries[0].entries[0].clone();
        artifact.registries[0].entries.push(duplicate);
        assert!(serialize_game_data(&artifact).is_err());
        let mut artifact = generated("test-a", 1, 10);
        let block_registry = artifact
            .registries
            .iter_mut()
            .find(|registry| registry.identifier.as_str() == "minecraft:block")
            .unwrap();
        block_registry.entries[1].raw_id = block_registry.entries[0].raw_id;
        assert!(serialize_game_data(&artifact).is_err());
        let mut artifact = generated("test-a", 1, 10);
        artifact.blocks[1].states[1].state_id = artifact.blocks[1].states[0].state_id;
        assert!(serialize_game_data(&artifact).is_err());
        let mut artifact = generated("test-a", 1, 10);
        artifact.blocks[1].states[0]
            .properties
            .insert("variant".to_owned(), "impossible".to_owned());
        assert!(serialize_game_data(&artifact).is_err());
        let (registries, blocks) = reports(1, 10);
        let missing = String::from_utf8(registries)
            .unwrap()
            .replace(",\"minecraft:stone\":{\"protocol_id\":1}", "");
        assert!(
            generate_game_data_from_reports(
                MinecraftVersionId::new("x").unwrap(),
                provenance(),
                missing.as_bytes(),
                &blocks
            )
            .is_err()
        );
    }

    #[test]
    fn malformed_unsupported_and_pathological_inputs_are_rejected() {
        assert!(MinecraftIdentifier::new("minecraft:../stone").is_err());
        assert!(MinecraftIdentifier::new("Other:stone").is_err());
        assert!(parse_game_data(b"{").is_err());
        let duplicate_registry =
            br#"{"minecraft:block":{"entries":{}},"minecraft:block":{"entries":{}}}"#;
        assert!(
            generate_game_data_from_reports(
                MinecraftVersionId::new("x").unwrap(),
                provenance(),
                duplicate_registry,
                b"{}"
            )
            .is_err()
        );
        let bytes = serialize_game_data(&generated("test-a", 1, 10)).unwrap();
        let future = String::from_utf8(bytes)
            .unwrap()
            .replace("\"schema_version\": 1", "\"schema_version\": 99");
        assert!(matches!(
            parse_game_data(future.as_bytes()),
            Err(VersionError::UnsupportedGameDataFormat { found: 99, .. })
        ));
    }
}
