//! Reproducible, exact-version default entity dimensions from `tools/EntityDump.java`.
use std::collections::BTreeSet;

use serde::Deserialize;

use crate::{MinecraftIdentifier, MinecraftVersionId, VersionError};

pub const ENTITY_DATA_SCHEMA_VERSION: u32 = 1;
const MAX_ENTITY_DATA_BYTES: usize = 256 * 1024;
const MAX_ENTITY_TYPES: usize = 1_024;

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct EntityDimension {
    pub raw_id: u32,
    pub identifier: MinecraftIdentifier,
    pub width: f32,
    pub height: f32,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct EntityData {
    schema_version: u32,
    minecraft_version: MinecraftVersionId,
    entity_count: usize,
    entities: Vec<EntityDimension>,
}

impl EntityData {
    #[must_use]
    pub fn entities(&self) -> &[EntityDimension] {
        &self.entities
    }

    #[must_use]
    pub fn get(&self, identifier: &str) -> Option<&EntityDimension> {
        self.entities
            .iter()
            .find(|entity| entity.identifier.as_str() == identifier)
    }

    #[must_use]
    pub const fn minecraft_version(&self) -> &MinecraftVersionId {
        &self.minecraft_version
    }
}

pub fn entity_data_for(version: &MinecraftVersionId) -> Result<Option<EntityData>, VersionError> {
    match version.as_str() {
        "26.1.2" => parse_entity_data(include_bytes!("../data/26.1.2/entity-data.json")).map(Some),
        _ => Ok(None),
    }
}

pub fn parse_entity_data(bytes: &[u8]) -> Result<EntityData, VersionError> {
    if bytes.len() > MAX_ENTITY_DATA_BYTES {
        return invalid("entity artifact exceeds byte limit");
    }
    let data: EntityData =
        serde_json::from_slice(bytes).map_err(|error| VersionError::InvalidEntityData {
            reason: format!(
                "JSON is malformed at line {}, column {}",
                error.line(),
                error.column()
            ),
        })?;
    if data.schema_version != ENTITY_DATA_SCHEMA_VERSION {
        return Err(VersionError::UnsupportedEntityDataFormat {
            found: data.schema_version,
            supported: ENTITY_DATA_SCHEMA_VERSION,
        });
    }
    if data.entities.len() != data.entity_count || data.entity_count > MAX_ENTITY_TYPES {
        return invalid("entity count is invalid");
    }
    let mut seen = BTreeSet::new();
    for (expected_raw, entity) in data.entities.iter().enumerate() {
        if usize::try_from(entity.raw_id).ok() != Some(expected_raw)
            || !seen.insert(entity.identifier.clone())
            || !entity.width.is_finite()
            || !entity.height.is_finite()
            || entity.width < 0.0
            || entity.height < 0.0
            || entity.width > 128.0
            || entity.height > 128.0
        {
            return invalid("entity IDs, identities, or dimensions are invalid");
        }
    }
    Ok(data)
}

fn invalid<T>(reason: impl Into<String>) -> Result<T, VersionError> {
    Err(VersionError::InvalidEntityData {
        reason: reason.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_26_1_2_entity_dimensions_cover_registry() {
        let data = entity_data_for(&MinecraftVersionId::new("26.1.2").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(data.entities().len(), 157);
        for (identifier, width, height) in [
            ("minecraft:player", 0.6, 1.8),
            ("minecraft:zombie", 0.6, 1.95),
            ("minecraft:cow", 0.9, 1.4),
            ("minecraft:spider", 1.4, 0.9),
            ("minecraft:item", 0.25, 0.25),
            ("minecraft:block_display", 0.0, 0.0),
        ] {
            let entry = data.get(identifier).unwrap();
            assert!((entry.width - width).abs() < 1e-5, "{identifier}");
            assert!((entry.height - height).abs() < 1e-5, "{identifier}");
        }
        assert!(data.get("minecraft:boat").is_none());
        assert!(data.get("minecraft:oak_boat").is_some());
    }

    #[test]
    fn malformed_dimensions_and_duplicate_ids_are_rejected() {
        assert!(parse_entity_data(b"{").is_err());
        assert!(parse_entity_data(b"{\"schema_version\":1,\"minecraft_version\":\"26.1.2\",\"entity_count\":1,\"entities\":[{\"raw_id\":0,\"identifier\":\"minecraft:test\",\"width\":-1,\"height\":1}]}").is_err());
    }
}
