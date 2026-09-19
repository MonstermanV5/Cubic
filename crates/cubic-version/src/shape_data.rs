//! Generated, exact-version block collision and outline shapes.
//!
//! The checked-in facts are produced by `tools/ShapeDump.java` from a locally
//! installed vanilla client. Mojang classes are never linked into Cubic.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::{MinecraftVersionId, VersionError};

pub const SHAPE_DATA_SCHEMA_VERSION: u32 = 1;
const MAX_SHAPE_DATA_BYTES: usize = 16 * 1024 * 1024;
const MAX_STATES: usize = 65_536;
const MAX_SHAPES: usize = 4_096;
const MAX_BOXES_PER_SHAPE: usize = 256;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(transparent)]
pub struct ShapeDataBox(pub [f64; 6]);

impl ShapeDataBox {
    #[must_use]
    pub const fn min(self) -> [f64; 3] {
        [self.0[0], self.0[1], self.0[2]]
    }

    #[must_use]
    pub const fn max(self) -> [f64; 3] {
        [self.0[3], self.0[4], self.0[5]]
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum DynamicCollisionKind {
    Scaffolding,
    PowderSnow,
    MovingPistonBlockEntity,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct BlockStateShape {
    pub runtime_id: u32,
    pub block: String,
    pub properties: BTreeMap<String, String>,
    pub collision_shape: usize,
    pub outline_shape: usize,
    #[serde(default, alias = "dynamic_collision")]
    pub dynamic_context: Option<DynamicCollisionKind>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ShapeData {
    schema_version: u32,
    minecraft_version: MinecraftVersionId,
    state_count: usize,
    shapes: Vec<Vec<ShapeDataBox>>,
    states: Vec<BlockStateShape>,
}

impl ShapeData {
    #[must_use]
    pub fn states(&self) -> &[BlockStateShape] {
        &self.states
    }

    #[must_use]
    pub fn state(&self, runtime_id: u32) -> Option<&BlockStateShape> {
        self.states
            .binary_search_by_key(&runtime_id, |state| state.runtime_id)
            .ok()
            .and_then(|index| self.states.get(index))
    }

    #[must_use]
    pub fn shape(&self, index: usize) -> Option<&[ShapeDataBox]> {
        self.shapes.get(index).map(Vec::as_slice)
    }

    #[must_use]
    pub const fn minecraft_version(&self) -> &MinecraftVersionId {
        &self.minecraft_version
    }
}

pub fn shape_data_for(version: &MinecraftVersionId) -> Result<Option<ShapeData>, VersionError> {
    match version.as_str() {
        "26.1.2" => parse_shape_data(include_bytes!("../data/26.1.2/shape-data.json")).map(Some),
        _ => Ok(None),
    }
}

pub fn parse_shape_data(bytes: &[u8]) -> Result<ShapeData, VersionError> {
    if bytes.len() > MAX_SHAPE_DATA_BYTES {
        return invalid("shape artifact exceeds its byte limit");
    }
    let data: ShapeData =
        serde_json::from_slice(bytes).map_err(|error| VersionError::InvalidShapeData {
            reason: format!(
                "JSON is malformed at line {}, column {}",
                error.line(),
                error.column()
            ),
        })?;
    if data.schema_version != SHAPE_DATA_SCHEMA_VERSION {
        return Err(VersionError::UnsupportedShapeDataFormat {
            found: data.schema_version,
            supported: SHAPE_DATA_SCHEMA_VERSION,
        });
    }
    if data.state_count != data.states.len()
        || data.states.len() > MAX_STATES
        || data.shapes.len() > MAX_SHAPES
    {
        return invalid("shape artifact has invalid state or shape counts");
    }
    if data
        .shapes
        .iter()
        .any(|shape| shape.len() > MAX_BOXES_PER_SHAPE)
    {
        return invalid("shape contains too many boxes");
    }
    for shape in &data.shapes {
        for bounds in shape {
            let values = bounds.0;
            if values.iter().any(|value| !value.is_finite())
                || values[..3]
                    .iter()
                    .zip(&values[3..])
                    .any(|(min, max)| min >= max)
            {
                return invalid("shape contains non-finite or inverted bounds");
            }
        }
    }
    let mut ids = BTreeSet::new();
    let mut previous = None;
    for state in &data.states {
        if previous.is_some_and(|prior| prior >= state.runtime_id)
            || !ids.insert(state.runtime_id)
            || state.collision_shape >= data.shapes.len()
            || state.outline_shape >= data.shapes.len()
            || state.block.is_empty()
            || state.block.len() > 256
        {
            return invalid("shape state is unsorted or references invalid data");
        }
        previous = Some(state.runtime_id);
    }
    Ok(data)
}

fn invalid<T>(reason: impl Into<String>) -> Result<T, VersionError> {
    Err(VersionError::InvalidShapeData {
        reason: reason.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_26_1_2_oracle_is_complete_and_deterministic() {
        let data = shape_data_for(&MinecraftVersionId::new("26.1.2").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(data.states.len(), 29_873);
        assert_eq!(data.states.first().unwrap().runtime_id, 0);
        assert_eq!(data.states.last().unwrap().runtime_id, 29_872);
        assert!(data.shapes.len() > 500);
        assert!(data.state(0).is_some());
        for (expected, state) in data.states.iter().enumerate() {
            assert_eq!(state.runtime_id as usize, expected);
            assert!(data.shape(state.collision_shape).is_some());
            assert!(data.shape(state.outline_shape).is_some());
        }
        let dynamic = data
            .states
            .iter()
            .fold(BTreeMap::new(), |mut counts, state| {
                if let Some(kind) = state.dynamic_context {
                    *counts.entry(kind).or_insert(0_usize) += 1;
                }
                counts
            });
        assert_eq!(dynamic.get(&DynamicCollisionKind::Scaffolding), Some(&32));
        assert_eq!(dynamic.get(&DynamicCollisionKind::PowderSnow), Some(&1));
        assert_eq!(
            dynamic.get(&DynamicCollisionKind::MovingPistonBlockEntity),
            Some(&12)
        );
        assert_eq!(
            data.states
                .iter()
                .filter(|state| state.dynamic_context.is_none())
                .count(),
            29_828
        );
    }

    #[test]
    fn malformed_and_future_shape_artifacts_are_rejected() {
        assert!(parse_shape_data(b"{").is_err());
        let future = br#"{"schema_version":2,"minecraft_version":"x","state_count":0,"shapes":[],"states":[]}"#;
        assert!(matches!(
            parse_shape_data(future),
            Err(VersionError::UnsupportedShapeDataFormat { found: 2, .. })
        ));
    }
}
