use std::{
    collections::{BTreeMap, BTreeSet},
    io::Cursor,
};

use cubic_resources::{
    MAX_VANILLA_RESOURCE_BYTES, ResourceError, VanillaResourcePath, VanillaResourceSource,
};
use cubic_version::{GameData, MinecraftIdentifier};
use cubic_world::RuntimeBlockStateId;
use png::{ColorType, Transformations};
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

const MAX_JSON_BYTES: u64 = 1024 * 1024;
const MAX_MODEL_DEPTH: usize = 32;
const MAX_ELEMENTS: usize = 256;
const MAX_ATLAS_SIDE: u32 = 4096;
const ATLAS_GUTTER: u32 = 1;

#[derive(Debug, Error)]
pub enum BlockResourceError {
    #[error(transparent)]
    Source(#[from] ResourceError),
    #[error("invalid block resource identifier `{value}`")]
    Identifier { value: String },
    #[error("malformed {kind} `{identifier}`: {reason}")]
    Malformed {
        kind: &'static str,
        identifier: String,
        reason: String,
    },
    #[error("texture atlas exceeds the {maximum}-pixel side limit")]
    AtlasTooLarge { maximum: u32 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RenderLayer {
    Opaque,
    Cutout,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum Direction {
    Down,
    Up,
    North,
    South,
    West,
    East,
}

impl Direction {
    pub(crate) const ALL: [Self; 6] = [
        Self::Down,
        Self::Up,
        Self::North,
        Self::South,
        Self::West,
        Self::East,
    ];

    pub(crate) const fn offset(self) -> [i32; 3] {
        match self {
            Self::Down => [0, -1, 0],
            Self::Up => [0, 1, 0],
            Self::North => [0, 0, -1],
            Self::South => [0, 0, 1],
            Self::West => [-1, 0, 0],
            Self::East => [1, 0, 0],
        }
    }

    pub(crate) const fn index(self) -> usize {
        match self {
            Self::Down => 0,
            Self::Up => 1,
            Self::North => 2,
            Self::South => 3,
            Self::West => 4,
            Self::East => 5,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ModelFace {
    pub direction: Direction,
    pub corners: [[f32; 3]; 4],
    pub uv: [[f32; 2]; 4],
    pub texture: String,
    pub atlas_region: AtlasRegion,
    pub cullface: Option<Direction>,
    pub tint_index: Option<u32>,
    pub shade: f32,
}

#[derive(Clone, Debug)]
pub(crate) struct ModelApplication {
    pub faces: Vec<ModelFace>,
    pub x_rotation: u16,
    pub y_rotation: u16,
    pub uvlock: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct WeightedApplications {
    pub entries: Vec<(u32, ModelApplication)>,
    pub total_weight: u32,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct StateModels {
    pub parts: Vec<WeightedApplications>,
    pub full_opaque_cube: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct AtlasRegion {
    pub min: [f32; 2],
    pub max: [f32; 2],
    pub layer: RenderLayer,
}

#[derive(Clone, Debug)]
pub struct TextureAtlasData {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    regions: BTreeMap<String, AtlasRegion>,
}

impl TextureAtlasData {
    pub(crate) fn region(&self, texture: &str) -> AtlasRegion {
        self.regions
            .get(texture)
            .copied()
            .or_else(|| self.regions.get("cubic:missing").copied())
            .unwrap_or(AtlasRegion {
                min: [0.0, 0.0],
                max: [1.0, 1.0],
                layer: RenderLayer::Opaque,
            })
    }
}

#[derive(Clone, Debug)]
pub struct BlockResources {
    states: Vec<Option<StateModels>>,
    fallback: StateModels,
    pub atlas: TextureAtlasData,
    pub blockstate_count: usize,
    pub model_count: usize,
    pub texture_count: usize,
    pub fallback_count: usize,
}

impl BlockResources {
    pub fn load(
        data: &GameData,
        source: &mut impl VanillaResourceSource,
    ) -> Result<Self, BlockResourceError> {
        let mut loader = Loader::new(source);
        let mut states = BTreeMap::new();
        let mut fallback_count = 0;
        for block in &data.artifact().blocks {
            let is_air = matches!(
                block.identifier.as_str(),
                "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air"
            );
            let definition = match loader.load_blockstate(&block.identifier) {
                Ok(definition) => Some(definition),
                Err(error) => {
                    loader.record_failure(&error);
                    None
                }
            };
            for state in &block.states {
                let models = if is_air {
                    StateModels::default()
                } else {
                    match definition
                        .as_ref()
                        .map(|definition| loader.resolve_state(definition, &state.properties))
                    {
                        Some(Ok(models)) if !models.parts.is_empty() => models,
                        Some(Err(error)) => {
                            loader.record_failure(&error);
                            fallback_count += 1;
                            fallback_state()
                        }
                        _ => {
                            fallback_count += 1;
                            fallback_state()
                        }
                    }
                };
                states.insert(RuntimeBlockStateId(state.state_id), models);
            }
        }
        let atlas = loader.build_atlas(&states)?;
        for models in states.values_mut() {
            prepare_runtime_state(models, &atlas);
        }
        for (reason, count) in loader.failures.iter().take(16) {
            tracing::warn!(count, %reason, "vanilla block resource used bounded fallback");
        }
        if loader.failures.len() > 16 {
            tracing::warn!(
                additional_failure_kinds = loader.failures.len() - 16,
                "additional block-resource failure kinds coalesced"
            );
        }
        let texture_count = atlas.regions.len();
        let state_capacity = states
            .keys()
            .next_back()
            .and_then(|state| usize::try_from(state.0).ok())
            .and_then(|maximum| maximum.checked_add(1))
            .ok_or_else(|| malformed("game data", "block states", "state ID range overflow"))?;
        let mut indexed_states = vec![None; state_capacity];
        for (state, models) in states {
            let index = usize::try_from(state.0)
                .map_err(|_| malformed("game data", "block states", "state ID overflow"))?;
            indexed_states[index] = Some(models);
        }
        let mut fallback = fallback_state();
        prepare_runtime_state(&mut fallback, &atlas);
        Ok(Self {
            states: indexed_states,
            fallback,
            atlas,
            blockstate_count: loader.blockstates_loaded,
            model_count: loader.models.len(),
            texture_count,
            fallback_count,
        })
    }

    pub(crate) fn state(&self, state: RuntimeBlockStateId) -> &StateModels {
        self.states
            .get(usize::try_from(state.0).unwrap_or(usize::MAX))
            .and_then(Option::as_ref)
            .unwrap_or(&self.fallback)
    }

    #[cfg(test)]
    pub(crate) fn synthetic(air: impl IntoIterator<Item = RuntimeBlockStateId>) -> Self {
        let air = air.into_iter().collect::<Vec<_>>();
        let capacity = air
            .iter()
            .filter_map(|state| usize::try_from(state.0).ok())
            .max()
            .and_then(|maximum| maximum.checked_add(1))
            .unwrap_or(0);
        let mut states = vec![None; capacity];
        for state in air {
            if let Ok(index) = usize::try_from(state.0) {
                states[index] = Some(StateModels::default());
            }
        }
        let atlas = pack_atlas(BTreeMap::from([(
            "cubic:missing".to_owned(),
            missing_texture(),
        )]))
        .expect("synthetic missing atlas");
        Self {
            states,
            fallback: fallback_state(),
            atlas,
            blockstate_count: 0,
            model_count: 0,
            texture_count: 1,
            fallback_count: 0,
        }
    }

    #[cfg(test)]
    pub(crate) fn synthetic_non_full(
        air: impl IntoIterator<Item = RuntimeBlockStateId>,
        state: RuntimeBlockStateId,
    ) -> Self {
        let mut resources = Self::synthetic(air);
        let Ok(index) = usize::try_from(state.0) else {
            return resources;
        };
        if resources.states.len() <= index {
            resources.states.resize_with(index + 1, || None);
        }
        let mut models = fallback_state();
        models.full_opaque_cube = false;
        resources.states[index] = Some(models);
        resources
    }
}

fn prepare_runtime_state(models: &mut StateModels, atlas: &TextureAtlasData) {
    for part in &mut models.parts {
        for (_, model) in &mut part.entries {
            for face in &mut model.faces {
                if model.uvlock {
                    face.uv.rotate_left(uvlock_quarter_turns(
                        face.direction,
                        model.x_rotation,
                        model.y_rotation,
                    ));
                }
                face.corners = face.corners.map(|corner| {
                    rotate_blockstate_corner(corner, model.x_rotation, model.y_rotation)
                });
                face.cullface = face.cullface.map(|direction| {
                    rotate_blockstate_direction(direction, model.x_rotation, model.y_rotation)
                });
                face.atlas_region = atlas.region(&face.texture);
            }
            model.x_rotation = 0;
            model.y_rotation = 0;
            model.uvlock = false;
        }
    }
    models.full_opaque_cube = models.parts.len() == 1
        && models.parts[0].entries.iter().all(|(_, model)| {
            is_full_cube(model)
                && model
                    .faces
                    .iter()
                    .all(|face| face.atlas_region.layer == RenderLayer::Opaque)
        });
}

pub(crate) fn rotate_blockstate_corner(
    mut point: [f32; 3],
    x_rotation: u16,
    y_rotation: u16,
) -> [f32; 3] {
    for value in &mut point {
        *value -= 0.5;
    }
    for _ in 0..(x_rotation / 90) {
        point = [point[0], -point[2], point[1]];
    }
    for _ in 0..(y_rotation / 90) {
        point = [-point[2], point[1], point[0]];
    }
    for value in &mut point {
        *value += 0.5;
    }
    point
}

pub(crate) fn rotate_blockstate_direction(
    mut direction: Direction,
    x_rotation: u16,
    y_rotation: u16,
) -> Direction {
    for _ in 0..(x_rotation / 90) {
        direction = match direction {
            Direction::Up => Direction::South,
            Direction::South => Direction::Down,
            Direction::Down => Direction::North,
            Direction::North => Direction::Up,
            other => other,
        };
    }
    for _ in 0..(y_rotation / 90) {
        direction = match direction {
            Direction::North => Direction::East,
            Direction::East => Direction::South,
            Direction::South => Direction::West,
            Direction::West => Direction::North,
            other => other,
        };
    }
    direction
}

pub(crate) fn uvlock_quarter_turns(
    direction: Direction,
    x_rotation: u16,
    y_rotation: u16,
) -> usize {
    let source = face_corners([0.0; 3], [1.0; 3], direction);
    let transformed_first = rotate_blockstate_corner(source[0], x_rotation, y_rotation);
    let target_direction = rotate_blockstate_direction(direction, x_rotation, y_rotation);
    let target = face_corners([0.0; 3], [1.0; 3], target_direction);
    target
        .iter()
        .position(|corner| {
            corner
                .iter()
                .zip(transformed_first)
                .all(|(left, right)| (*left - right).abs() < 1.0e-5)
        })
        .unwrap_or(0)
}

#[derive(Clone, Debug)]
struct BlockstateDefinition {
    variants: BTreeMap<String, Vec<ModelReference>>,
    multipart: Vec<Multipart>,
}

#[derive(Clone, Debug)]
struct Multipart {
    condition: Condition,
    apply: Vec<ModelReference>,
}

#[derive(Clone, Debug)]
enum Condition {
    Always,
    Property(String, Vec<String>),
    And(Vec<Condition>),
    Or(Vec<Condition>),
}

impl Condition {
    fn matches(&self, properties: &BTreeMap<String, String>) -> bool {
        match self {
            Self::Always => true,
            Self::Property(name, values) => properties
                .get(name)
                .is_some_and(|value| values.iter().any(|candidate| candidate == value)),
            Self::And(conditions) => conditions
                .iter()
                .all(|condition| condition.matches(properties)),
            Self::Or(conditions) => conditions
                .iter()
                .any(|condition| condition.matches(properties)),
        }
    }
}

#[derive(Clone, Debug)]
struct ModelReference {
    model: MinecraftIdentifier,
    x: u16,
    y: u16,
    uvlock: bool,
    weight: u32,
}

#[derive(Clone, Debug, Deserialize)]
struct ModelWire {
    parent: Option<String>,
    #[serde(default)]
    textures: BTreeMap<String, TextureWire>,
    elements: Option<Vec<ElementWire>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
enum TextureWire {
    Simple(String),
    Extended {
        sprite: String,
        #[serde(default)]
        #[serde(rename = "force_translucent")]
        _force_translucent: bool,
    },
}

impl TextureWire {
    fn into_sprite(self) -> String {
        match self {
            Self::Simple(sprite) | Self::Extended { sprite, .. } => sprite,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct ElementWire {
    from: [f32; 3],
    to: [f32; 3],
    rotation: Option<ElementRotationWire>,
    #[serde(default = "default_true")]
    shade: bool,
    faces: BTreeMap<String, FaceWire>,
}

#[derive(Clone, Debug, Deserialize)]
struct ElementRotationWire {
    origin: [f32; 3],
    axis: String,
    angle: f32,
    #[serde(default)]
    rescale: bool,
}

#[derive(Clone, Debug, Deserialize)]
struct FaceWire {
    uv: Option<[f32; 4]>,
    texture: String,
    cullface: Option<String>,
    #[serde(default)]
    rotation: u16,
    tintindex: Option<u32>,
}

#[derive(Clone, Debug)]
struct ResolvedModel {
    textures: BTreeMap<String, String>,
    elements: Vec<ElementWire>,
}

struct Loader<'a, S> {
    source: &'a mut S,
    models: BTreeMap<MinecraftIdentifier, ResolvedModel>,
    blockstates_loaded: usize,
    failures: BTreeMap<String, usize>,
}

impl<'a, S: VanillaResourceSource> Loader<'a, S> {
    fn new(source: &'a mut S) -> Self {
        Self {
            source,
            models: BTreeMap::new(),
            blockstates_loaded: 0,
            failures: BTreeMap::new(),
        }
    }

    fn record_failure(&mut self, error: &BlockResourceError) {
        *self.failures.entry(error.to_string()).or_default() += 1;
    }

    fn load_blockstate(
        &mut self,
        identifier: &MinecraftIdentifier,
    ) -> Result<BlockstateDefinition, BlockResourceError> {
        let path = resource_path(identifier, "blockstates", "json")?;
        let bytes = self
            .source
            .read_resource(&path, MAX_JSON_BYTES)?
            .ok_or_else(|| malformed("blockstate", identifier.as_str(), "resource is missing"))?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| malformed("blockstate", identifier.as_str(), error.to_string()))?;
        let object = value
            .as_object()
            .ok_or_else(|| malformed("blockstate", identifier.as_str(), "root is not an object"))?;
        let mut variants = BTreeMap::new();
        if let Some(value) = object.get("variants") {
            for (selector, apply) in value.as_object().ok_or_else(|| {
                malformed(
                    "blockstate",
                    identifier.as_str(),
                    "variants is not an object",
                )
            })? {
                parse_selector(selector)?;
                variants.insert(selector.clone(), parse_model_references(apply)?);
            }
        }
        let mut multipart = Vec::new();
        if let Some(value) = object.get("multipart") {
            let entries = value.as_array().ok_or_else(|| {
                malformed(
                    "blockstate",
                    identifier.as_str(),
                    "multipart is not an array",
                )
            })?;
            if entries.len() > 256 {
                return Err(malformed(
                    "blockstate",
                    identifier.as_str(),
                    "too many multipart entries",
                ));
            }
            for entry in entries {
                let entry = entry.as_object().ok_or_else(|| {
                    malformed(
                        "blockstate",
                        identifier.as_str(),
                        "multipart entry is not an object",
                    )
                })?;
                let condition = entry
                    .get("when")
                    .map_or(Ok(Condition::Always), parse_condition)?;
                let apply = parse_model_references(entry.get("apply").ok_or_else(|| {
                    malformed(
                        "blockstate",
                        identifier.as_str(),
                        "multipart apply is missing",
                    )
                })?)?;
                multipart.push(Multipart { condition, apply });
            }
        }
        self.blockstates_loaded += 1;
        Ok(BlockstateDefinition {
            variants,
            multipart,
        })
    }

    fn resolve_state(
        &mut self,
        definition: &BlockstateDefinition,
        properties: &BTreeMap<String, String>,
    ) -> Result<StateModels, BlockResourceError> {
        let mut groups = Vec::new();
        if !definition.variants.is_empty() {
            let selected = definition
                .variants
                .iter()
                .filter(|(selector, _)| selector_matches(selector, properties))
                .max_by_key(|(selector, _)| {
                    selector.split(',').filter(|term| !term.is_empty()).count()
                })
                .map(|(_, references)| references);
            if let Some(references) = selected {
                groups.push(references.clone());
            }
        }
        for multipart in &definition.multipart {
            if multipart.condition.matches(properties) {
                groups.push(multipart.apply.clone());
            }
        }
        let mut parts = Vec::new();
        for references in groups {
            let mut entries = Vec::new();
            let mut total_weight = 0_u32;
            for reference in references {
                let model = self.resolve_model(&reference.model, &mut Vec::new())?;
                let faces = bake_model(&model)?;
                total_weight = total_weight.checked_add(reference.weight).ok_or_else(|| {
                    malformed(
                        "blockstate",
                        reference.model.as_str(),
                        "model weight overflow",
                    )
                })?;
                if total_weight > i32::MAX as u32 {
                    return Err(malformed(
                        "blockstate",
                        reference.model.as_str(),
                        "total model weight exceeds the vanilla signed-integer limit",
                    ));
                }
                entries.push((
                    reference.weight,
                    ModelApplication {
                        faces,
                        x_rotation: reference.x,
                        y_rotation: reference.y,
                        uvlock: reference.uvlock,
                    },
                ));
            }
            if !entries.is_empty() {
                parts.push(WeightedApplications {
                    entries,
                    total_weight,
                });
            }
        }
        let full_opaque_cube = parts.len() == 1
            && parts[0]
                .entries
                .iter()
                .all(|(_, model)| is_full_cube(model));
        Ok(StateModels {
            parts,
            full_opaque_cube,
        })
    }

    fn resolve_model(
        &mut self,
        identifier: &MinecraftIdentifier,
        stack: &mut Vec<MinecraftIdentifier>,
    ) -> Result<ResolvedModel, BlockResourceError> {
        if let Some(model) = self.models.get(identifier) {
            return Ok(model.clone());
        }
        if stack.len() >= MAX_MODEL_DEPTH || stack.contains(identifier) {
            return Err(malformed(
                "model",
                identifier.as_str(),
                "parent cycle or depth limit",
            ));
        }
        stack.push(identifier.clone());
        let path = resource_path(identifier, "models", "json")?;
        let bytes = self
            .source
            .read_resource(&path, MAX_JSON_BYTES)?
            .ok_or_else(|| malformed("model", identifier.as_str(), "resource is missing"))?;
        let wire: ModelWire = serde_json::from_slice(&bytes)
            .map_err(|error| malformed("model", identifier.as_str(), error.to_string()))?;
        let mut resolved = if let Some(parent) = wire.parent.as_deref() {
            let parent = parse_identifier(parent)?;
            self.resolve_model(&parent, stack)?
        } else {
            ResolvedModel {
                textures: BTreeMap::new(),
                elements: Vec::new(),
            }
        };
        resolved.textures.extend(
            wire.textures
                .into_iter()
                .map(|(name, value)| (name, value.into_sprite())),
        );
        if let Some(elements) = wire.elements {
            if elements.len() > MAX_ELEMENTS {
                return Err(malformed("model", identifier.as_str(), "too many elements"));
            }
            resolved.elements = elements;
        }
        stack.pop();
        self.models.insert(identifier.clone(), resolved.clone());
        Ok(resolved)
    }

    fn build_atlas(
        &mut self,
        states: &BTreeMap<RuntimeBlockStateId, StateModels>,
    ) -> Result<TextureAtlasData, BlockResourceError> {
        let mut names = BTreeSet::from(["cubic:missing".to_owned()]);
        for state in states.values() {
            for part in &state.parts {
                for (_, model) in &part.entries {
                    for face in &model.faces {
                        names.insert(face.texture.clone());
                    }
                }
            }
        }
        let mut images = BTreeMap::new();
        images.insert("cubic:missing".to_owned(), missing_texture());
        for name in names.iter().filter(|name| name.as_str() != "cubic:missing") {
            let identifier = parse_identifier(name)?;
            let path = resource_path(&identifier, "textures", "png")?;
            let image = self
                .source
                .read_resource(&path, MAX_VANILLA_RESOURCE_BYTES)?
                .and_then(|bytes| decode_png(&bytes).ok())
                .unwrap_or_else(missing_texture);
            images.insert(name.clone(), image);
        }
        pack_atlas(images)
    }
}

fn malformed(
    kind: &'static str,
    identifier: &str,
    reason: impl Into<String>,
) -> BlockResourceError {
    BlockResourceError::Malformed {
        kind,
        identifier: identifier.to_owned(),
        reason: reason.into(),
    }
}

fn parse_identifier(value: &str) -> Result<MinecraftIdentifier, BlockResourceError> {
    let value = if value.contains(':') {
        value.to_owned()
    } else {
        format!("minecraft:{value}")
    };
    MinecraftIdentifier::new(value.clone()).map_err(|_| BlockResourceError::Identifier { value })
}

fn resource_path(
    identifier: &MinecraftIdentifier,
    directory: &str,
    extension: &str,
) -> Result<VanillaResourcePath, BlockResourceError> {
    let (namespace, path) =
        identifier
            .as_str()
            .split_once(':')
            .ok_or_else(|| BlockResourceError::Identifier {
                value: identifier.to_string(),
            })?;
    Ok(VanillaResourcePath::new(format!(
        "assets/{namespace}/{directory}/{path}.{extension}"
    ))?)
}

fn parse_selector(selector: &str) -> Result<(), BlockResourceError> {
    if selector.is_empty() {
        return Ok(());
    }
    for term in selector.split(',') {
        let (name, value) = term
            .split_once('=')
            .ok_or_else(|| malformed("blockstate selector", selector, "missing equals"))?;
        if name.is_empty() || value.is_empty() {
            return Err(malformed("blockstate selector", selector, "empty property"));
        }
    }
    Ok(())
}

fn selector_matches(selector: &str, properties: &BTreeMap<String, String>) -> bool {
    selector.is_empty()
        || selector.split(',').all(|term| {
            term.split_once('=').is_some_and(|(name, value)| {
                properties.get(name).is_some_and(|actual| actual == value)
            })
        })
}

fn parse_model_references(value: &Value) -> Result<Vec<ModelReference>, BlockResourceError> {
    let values: Vec<&Value> = match value {
        Value::Array(values) => values.iter().collect(),
        _ => vec![value],
    };
    if values.is_empty() || values.len() > 256 {
        return Err(malformed(
            "blockstate",
            "apply",
            "invalid model alternative count",
        ));
    }
    values
        .into_iter()
        .map(|value| {
            let object = value.as_object().ok_or_else(|| {
                malformed("blockstate", "apply", "model reference is not an object")
            })?;
            let model = parse_identifier(
                object
                    .get("model")
                    .and_then(Value::as_str)
                    .ok_or_else(|| malformed("blockstate", "apply", "model is missing"))?,
            )?;
            let rotation = |name| object.get(name).and_then(Value::as_u64).unwrap_or(0);
            let x = u16::try_from(rotation("x"))
                .map_err(|_| malformed("blockstate", "apply", "x rotation out of range"))?;
            let y = u16::try_from(rotation("y"))
                .map_err(|_| malformed("blockstate", "apply", "y rotation out of range"))?;
            if ![0, 90, 180, 270].contains(&x) || ![0, 90, 180, 270].contains(&y) {
                return Err(malformed(
                    "blockstate",
                    "apply",
                    "rotation is not a quarter turn",
                ));
            }
            let weight = u32::try_from(object.get("weight").and_then(Value::as_u64).unwrap_or(1))
                .map_err(|_| malformed("blockstate", "apply", "weight out of range"))?;
            if weight == 0 {
                return Err(malformed("blockstate", "apply", "weight is zero"));
            }
            Ok(ModelReference {
                model,
                x,
                y,
                uvlock: object
                    .get("uvlock")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                weight,
            })
        })
        .collect()
}

fn parse_condition(value: &Value) -> Result<Condition, BlockResourceError> {
    let object = value
        .as_object()
        .ok_or_else(|| malformed("multipart condition", "when", "condition is not an object"))?;
    if let Some(or) = object.get("OR") {
        return parse_condition_list(or, false);
    }
    if let Some(and) = object.get("AND") {
        return parse_condition_list(and, true);
    }
    let mut conditions = Vec::new();
    for (name, value) in object {
        let value = value
            .as_str()
            .ok_or_else(|| malformed("multipart condition", name, "value is not a string"))?;
        conditions.push(Condition::Property(
            name.clone(),
            value.split('|').map(str::to_owned).collect(),
        ));
    }
    Ok(Condition::And(conditions))
}

fn parse_condition_list(value: &Value, and: bool) -> Result<Condition, BlockResourceError> {
    let values = value.as_array().ok_or_else(|| {
        malformed(
            "multipart condition",
            "condition",
            "logical condition is not an array",
        )
    })?;
    let conditions = values
        .iter()
        .map(parse_condition)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(if and {
        Condition::And(conditions)
    } else {
        Condition::Or(conditions)
    })
}

fn default_true() -> bool {
    true
}

fn bake_model(model: &ResolvedModel) -> Result<Vec<ModelFace>, BlockResourceError> {
    let mut faces = Vec::new();
    for element in &model.elements {
        if element
            .from
            .iter()
            .chain(&element.to)
            .any(|value| !value.is_finite() || *value < -16.0 || *value > 32.0)
        {
            return Err(malformed("model", "element", "element bounds are invalid"));
        }
        for (name, face) in &element.faces {
            let direction = parse_direction(name)
                .ok_or_else(|| malformed("model", "face", "unknown direction"))?;
            let mut corners = face_corners(element.from, element.to, direction);
            if let Some(rotation) = &element.rotation {
                for corner in &mut corners {
                    *corner = rotate_element(*corner, rotation)?;
                }
            }
            let mut uv = face
                .uv
                .unwrap_or_else(|| generated_uv(element.from, element.to, direction));
            for value in &mut uv {
                *value /= 16.0;
            }
            // Minecraft model UVs and decoded PNG rows both use a top-left origin.
            // `face_corners` follows Minecraft's direction-specific baked-quad
            // vertex order, so the UV indices match the game's CuboidFace codec.
            let mut uv_corners = [
                [uv[0], uv[1]],
                [uv[0], uv[3]],
                [uv[2], uv[3]],
                [uv[2], uv[1]],
            ];
            let rotations = usize::from((face.rotation / 90) % 4);
            uv_corners.rotate_left(rotations);
            let texture = resolve_texture(&face.texture, &model.textures)?;
            faces.push(ModelFace {
                direction,
                corners: corners
                    .map(|corner| [corner[0] / 16.0, corner[1] / 16.0, corner[2] / 16.0]),
                uv: uv_corners,
                texture,
                atlas_region: AtlasRegion {
                    min: [0.0, 0.0],
                    max: [1.0, 1.0],
                    layer: RenderLayer::Opaque,
                },
                cullface: face.cullface.as_deref().and_then(parse_direction),
                tint_index: face.tintindex,
                shade: if element.shade {
                    direction_shade(direction)
                } else {
                    1.0
                },
            });
        }
    }
    Ok(faces)
}

fn resolve_texture(
    value: &str,
    textures: &BTreeMap<String, String>,
) -> Result<String, BlockResourceError> {
    let mut current = value;
    let mut visited = BTreeSet::new();
    while let Some(variable) = current.strip_prefix('#') {
        if !visited.insert(variable.to_owned()) {
            return Err(malformed("model texture", value, "texture reference cycle"));
        }
        current = textures
            .get(variable)
            .map(String::as_str)
            .ok_or_else(|| malformed("model texture", value, "texture variable is missing"))?;
    }
    Ok(parse_identifier(current)?.to_string())
}

fn parse_direction(value: &str) -> Option<Direction> {
    match value {
        "down" => Some(Direction::Down),
        "up" => Some(Direction::Up),
        "north" => Some(Direction::North),
        "south" => Some(Direction::South),
        "west" => Some(Direction::West),
        "east" => Some(Direction::East),
        _ => None,
    }
}

fn face_corners(from: [f32; 3], to: [f32; 3], direction: Direction) -> [[f32; 3]; 4] {
    // Canonical Minecraft baked-quad order. With UV indices 0..=3 mapped to
    // (min U,min V), (min U,max V), (max U,max V), (max U,min V), the face-local
    // U axes are: down/up/south +X, north -X, west +Z, and east -Z.
    match direction {
        Direction::East => [
            [to[0], to[1], to[2]],
            [to[0], from[1], to[2]],
            [to[0], from[1], from[2]],
            [to[0], to[1], from[2]],
        ],
        Direction::West => [
            [from[0], to[1], from[2]],
            [from[0], from[1], from[2]],
            [from[0], from[1], to[2]],
            [from[0], to[1], to[2]],
        ],
        Direction::Up => [
            [from[0], to[1], from[2]],
            [from[0], to[1], to[2]],
            [to[0], to[1], to[2]],
            [to[0], to[1], from[2]],
        ],
        Direction::Down => [
            [from[0], from[1], to[2]],
            [from[0], from[1], from[2]],
            [to[0], from[1], from[2]],
            [to[0], from[1], to[2]],
        ],
        Direction::South => [
            [from[0], to[1], to[2]],
            [from[0], from[1], to[2]],
            [to[0], from[1], to[2]],
            [to[0], to[1], to[2]],
        ],
        Direction::North => [
            [to[0], to[1], from[2]],
            [to[0], from[1], from[2]],
            [from[0], from[1], from[2]],
            [from[0], to[1], from[2]],
        ],
    }
}

fn generated_uv(from: [f32; 3], to: [f32; 3], direction: Direction) -> [f32; 4] {
    match direction {
        Direction::Down | Direction::Up => [from[0], from[2], to[0], to[2]],
        Direction::North | Direction::South => [from[0], 16.0 - to[1], to[0], 16.0 - from[1]],
        Direction::West | Direction::East => [from[2], 16.0 - to[1], to[2], 16.0 - from[1]],
    }
}

fn rotate_element(
    mut point: [f32; 3],
    rotation: &ElementRotationWire,
) -> Result<[f32; 3], BlockResourceError> {
    if !rotation.angle.is_finite() || ![-45.0, -22.5, 0.0, 22.5, 45.0].contains(&rotation.angle) {
        return Err(malformed("model", "element rotation", "unsupported angle"));
    }
    for (value, origin) in point.iter_mut().zip(rotation.origin) {
        *value -= origin;
    }
    let (sin, cos) = rotation.angle.to_radians().sin_cos();
    point = match rotation.axis.as_str() {
        "x" => [
            point[0],
            point[1] * cos - point[2] * sin,
            point[1] * sin + point[2] * cos,
        ],
        "y" => [
            point[0] * cos + point[2] * sin,
            point[1],
            -point[0] * sin + point[2] * cos,
        ],
        "z" => [
            point[0] * cos - point[1] * sin,
            point[0] * sin + point[1] * cos,
            point[2],
        ],
        _ => return Err(malformed("model", "element rotation", "unknown axis")),
    };
    if rotation.rescale && cos.abs() > f32::EPSILON {
        let scale = 1.0 / cos.abs();
        for (axis, value) in point.iter_mut().enumerate() {
            if rotation.axis.as_bytes().first().copied() != Some(b"xyz"[axis]) {
                *value *= scale;
            }
        }
    }
    for (value, origin) in point.iter_mut().zip(rotation.origin) {
        *value += origin;
    }
    Ok(point)
}

fn direction_shade(direction: Direction) -> f32 {
    match direction {
        Direction::Up => 1.0,
        Direction::Down => 0.55,
        Direction::East => 0.85,
        Direction::West => 0.7,
        Direction::South => 0.8,
        Direction::North => 0.65,
    }
}

fn is_full_cube(model: &ModelApplication) -> bool {
    model.x_rotation == 0
        && model.y_rotation == 0
        && model.faces.len() == 6
        && Direction::ALL.into_iter().all(|direction| {
            model.faces.iter().any(|face| {
                face.cullface == Some(direction) && face_covers_unit_boundary(face, direction)
            })
        })
}

fn face_covers_unit_boundary(face: &ModelFace, direction: Direction) -> bool {
    const EPSILON: f32 = 1.0e-6;
    let (axis, boundary, first_span, second_span) = match direction {
        Direction::Down => (1, 0.0, 0, 2),
        Direction::Up => (1, 1.0, 0, 2),
        Direction::North => (2, 0.0, 0, 1),
        Direction::South => (2, 1.0, 0, 1),
        Direction::West => (0, 0.0, 1, 2),
        Direction::East => (0, 1.0, 1, 2),
    };
    face.corners
        .iter()
        .all(|corner| (corner[axis] - boundary).abs() <= EPSILON)
        && spans_unit_interval(&face.corners, first_span, EPSILON)
        && spans_unit_interval(&face.corners, second_span, EPSILON)
}

fn spans_unit_interval(corners: &[[f32; 3]; 4], axis: usize, epsilon: f32) -> bool {
    let (minimum, maximum) = corners.iter().map(|corner| corner[axis]).fold(
        (f32::INFINITY, f32::NEG_INFINITY),
        |(minimum, maximum), value| (minimum.min(value), maximum.max(value)),
    );
    minimum.abs() <= epsilon && (maximum - 1.0).abs() <= epsilon
}

fn fallback_state() -> StateModels {
    let element = ElementWire {
        from: [0.0; 3],
        to: [16.0; 3],
        rotation: None,
        shade: true,
        faces: ["down", "up", "north", "south", "west", "east"]
            .into_iter()
            .map(|direction| {
                (
                    direction.to_owned(),
                    FaceWire {
                        uv: None,
                        texture: "cubic:missing".to_owned(),
                        cullface: Some(direction.to_owned()),
                        rotation: 0,
                        tintindex: None,
                    },
                )
            })
            .collect(),
    };
    let model = ResolvedModel {
        textures: BTreeMap::new(),
        elements: vec![element],
    };
    let faces = bake_model(&model).unwrap_or_default();
    StateModels {
        parts: vec![WeightedApplications {
            entries: vec![(
                1,
                ModelApplication {
                    faces,
                    x_rotation: 0,
                    y_rotation: 0,
                    uvlock: false,
                },
            )],
            total_weight: 1,
        }],
        full_opaque_cube: true,
    }
}

#[derive(Clone)]
struct DecodedImage {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    cutout: bool,
}

fn decode_png(bytes: &[u8]) -> Result<DecodedImage, BlockResourceError> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(Transformations::EXPAND | Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .map_err(|error| malformed("texture", "png", error.to_string()))?;
    let info = reader.info();
    if info.width == 0 || info.height == 0 || info.width > 1024 || info.height > 16384 {
        return Err(malformed("texture", "png", "dimensions exceed limits"));
    }
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| malformed("texture", "png", "decoded size overflow"))?;
    if size > 64 * 1024 * 1024 {
        return Err(malformed("texture", "png", "decoded image exceeds limit"));
    }
    let mut output = vec![0; size];
    let frame = reader
        .next_frame(&mut output)
        .map_err(|error| malformed("texture", "png", error.to_string()))?;
    let raw = &output[..frame.buffer_size()];
    let mut rgba = Vec::with_capacity(
        usize::try_from(frame.width.saturating_mul(frame.width).saturating_mul(4)).unwrap_or(0),
    );
    let first_height = frame.width.min(frame.height);
    let channels = match frame.color_type {
        ColorType::Rgba => 4,
        ColorType::Rgb => 3,
        ColorType::GrayscaleAlpha => 2,
        ColorType::Grayscale => 1,
        _ => return Err(malformed("texture", "png", "unsupported color format")),
    };
    for pixel in raw
        .chunks_exact(channels)
        .take(usize::try_from(frame.width * first_height).unwrap_or(0))
    {
        match channels {
            4 => rgba.extend_from_slice(pixel),
            3 => rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]),
            2 => rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], pixel[1]]),
            1 => rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], 255]),
            _ => {}
        }
    }
    let cutout = rgba.as_chunks::<4>().0.iter().any(|pixel| pixel[3] < 255);
    Ok(DecodedImage {
        width: frame.width,
        height: first_height,
        rgba,
        cutout,
    })
}

fn missing_texture() -> DecodedImage {
    let mut rgba = Vec::with_capacity(16 * 16 * 4);
    for y in 0..16 {
        for x in 0..16 {
            let c = if (x / 4 + y / 4) % 2 == 0 {
                [255, 0, 255, 255]
            } else {
                [20, 0, 20, 255]
            };
            rgba.extend_from_slice(&c);
        }
    }
    DecodedImage {
        width: 16,
        height: 16,
        rgba,
        cutout: false,
    }
}

fn pack_atlas(
    images: BTreeMap<String, DecodedImage>,
) -> Result<TextureAtlasData, BlockResourceError> {
    let width = 2048_u32;
    let mut placements = BTreeMap::new();
    let mut x = ATLAS_GUTTER;
    let mut y = ATLAS_GUTTER;
    let mut row = 0;
    for (name, image) in &images {
        if image.width == 0
            || image.height == 0
            || image.width > width.saturating_sub(ATLAS_GUTTER * 2)
            || image.height > MAX_ATLAS_SIDE
            || image.rgba.len()
                != usize::try_from(image.width.saturating_mul(image.height).saturating_mul(4))
                    .unwrap_or(usize::MAX)
        {
            return Err(BlockResourceError::AtlasTooLarge {
                maximum: MAX_ATLAS_SIDE,
            });
        }
        if x + image.width + ATLAS_GUTTER > width {
            x = ATLAS_GUTTER;
            y += row + ATLAS_GUTTER * 2;
            row = 0;
        }
        placements.insert(name.clone(), (x, y));
        x += image.width + ATLAS_GUTTER * 2;
        row = row.max(image.height);
    }
    let height = (y + row + ATLAS_GUTTER).next_power_of_two();
    if height > MAX_ATLAS_SIDE {
        return Err(BlockResourceError::AtlasTooLarge {
            maximum: MAX_ATLAS_SIDE,
        });
    }
    let len =
        usize::try_from(width * height * 4).map_err(|_| BlockResourceError::AtlasTooLarge {
            maximum: MAX_ATLAS_SIDE,
        })?;
    let mut rgba = vec![0; len];
    let mut regions = BTreeMap::new();
    for (name, image) in images {
        let (px, py) = placements[&name];
        for atlas_y in (py - ATLAS_GUTTER)..(py + image.height + ATLAS_GUTTER) {
            for atlas_x in (px - ATLAS_GUTTER)..(px + image.width + ATLAS_GUTTER) {
                let source_x = atlas_x.saturating_sub(px).min(image.width - 1);
                let source_y = atlas_y.saturating_sub(py).min(image.height - 1);
                let source_offset = pixel_offset(image.width, source_x, source_y)
                    .ok_or_else(|| malformed("texture atlas", &name, "source pixel overflow"))?;
                let destination_offset =
                    pixel_offset(width, atlas_x, atlas_y).ok_or_else(|| {
                        malformed("texture atlas", &name, "destination pixel overflow")
                    })?;
                let source = image
                    .rgba
                    .get(source_offset..source_offset + 4)
                    .ok_or_else(|| malformed("texture atlas", &name, "source pixel is missing"))?;
                let destination = rgba
                    .get_mut(destination_offset..destination_offset + 4)
                    .ok_or_else(|| {
                        malformed("texture atlas", &name, "destination pixel is outside atlas")
                    })?;
                destination.copy_from_slice(source);
            }
        }
        regions.insert(
            name,
            AtlasRegion {
                min: [px as f32 / width as f32, py as f32 / height as f32],
                max: [
                    (px + image.width) as f32 / width as f32,
                    (py + image.height) as f32 / height as f32,
                ],
                layer: if image.cutout {
                    RenderLayer::Cutout
                } else {
                    RenderLayer::Opaque
                },
            },
        );
    }
    Ok(TextureAtlasData {
        width,
        height,
        rgba,
        regions,
    })
}

fn pixel_offset(row_width: u32, x: u32, y: u32) -> Option<usize> {
    y.checked_mul(row_width)
        .and_then(|row| row.checked_add(x))
        .and_then(|pixel| pixel.checked_mul(4))
        .and_then(|offset| usize::try_from(offset).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct MemorySource(BTreeMap<String, Vec<u8>>);

    impl MemorySource {
        fn insert(&mut self, path: &str, value: &str) {
            self.0.insert(path.to_owned(), value.as_bytes().to_vec());
        }
    }

    impl VanillaResourceSource for MemorySource {
        fn read_resource(
            &mut self,
            path: &VanillaResourcePath,
            maximum: u64,
        ) -> Result<Option<Vec<u8>>, ResourceError> {
            let value = self.0.get(path.as_str()).cloned();
            if value
                .as_ref()
                .is_some_and(|value| value.len() as u64 > maximum)
            {
                return Err(ResourceError::Oversized {
                    context: "synthetic resource",
                    maximum,
                });
            }
            Ok(value)
        }
    }

    fn identifier(value: &str) -> MinecraftIdentifier {
        parse_identifier(value).unwrap()
    }

    #[test]
    fn identifiers_default_namespace_and_build_safe_paths() {
        assert_eq!(
            parse_identifier("block/cube").unwrap().as_str(),
            "minecraft:block/cube"
        );
        assert_eq!(
            resource_path(&identifier("minecraft:stone"), "blockstates", "json")
                .unwrap()
                .as_str(),
            "assets/minecraft/blockstates/stone.json"
        );
        assert!(parse_identifier("../escape").is_err());
    }

    #[test]
    fn variants_select_exact_properties_and_reject_malformed_selectors() {
        let properties = BTreeMap::from([
            ("facing".to_owned(), "north".to_owned()),
            ("half".to_owned(), "bottom".to_owned()),
        ]);
        assert!(selector_matches("facing=north,half=bottom", &properties));
        assert!(!selector_matches("facing=south", &properties));
        assert!(parse_selector("").is_ok());
        assert!(parse_selector("facing").is_err());
    }

    #[test]
    fn weighted_model_references_preserve_rotation_uvlock_and_weights() {
        let references = parse_model_references(&serde_json::json!([
            {"model":"block/a", "weight":3, "x":90, "y":270, "uvlock":true},
            {"model":"block/b", "weight":1}
        ]))
        .unwrap();
        assert_eq!(references.len(), 2);
        assert_eq!(references[0].weight, 3);
        assert_eq!((references[0].x, references[0].y), (90, 270));
        assert!(references[0].uvlock);
    }

    #[test]
    fn multipart_and_or_conditions_match_deterministically() {
        let properties = BTreeMap::from([
            ("north".to_owned(), "true".to_owned()),
            ("shape".to_owned(), "left".to_owned()),
        ]);
        let condition = parse_condition(&serde_json::json!({
            "AND": [
                {"north": "true"},
                {"OR": [{"shape": "left|right"}, {"shape": "straight"}]}
            ]
        }))
        .unwrap();
        assert!(condition.matches(&properties));
    }

    #[test]
    fn default_and_exact_variants_resolve_the_expected_models() {
        let mut source = MemorySource::default();
        source.insert(
            "assets/minecraft/blockstates/test.json",
            r#"{"variants":{"":{"model":"block/default"},"facing=north":{"model":"block/north","x":90,"y":180}}}"#,
        );
        for name in ["default", "north"] {
            source.insert(
                &format!("assets/minecraft/models/block/{name}.json"),
                r##"{"textures":{"all":"block/stone"},"elements":[{"from":[0,0,0],"to":[16,16,16],"faces":{"north":{"texture":"#all"}}}]}"##,
            );
        }
        let mut loader = Loader::new(&mut source);
        let definition = loader
            .load_blockstate(&identifier("minecraft:test"))
            .unwrap();
        let default = loader.resolve_state(&definition, &BTreeMap::new()).unwrap();
        let north = loader
            .resolve_state(
                &definition,
                &BTreeMap::from([("facing".to_owned(), "north".to_owned())]),
            )
            .unwrap();
        assert_eq!(
            (
                default.parts[0].entries[0].1.x_rotation,
                default.parts[0].entries[0].1.y_rotation
            ),
            (0, 0)
        );
        assert_eq!(
            (
                north.parts[0].entries[0].1.x_rotation,
                north.parts[0].entries[0].1.y_rotation
            ),
            (90, 180)
        );
    }

    #[test]
    fn multiple_matching_multipart_entries_all_contribute_geometry() {
        let mut source = MemorySource::default();
        source.insert(
            "assets/minecraft/blockstates/test.json",
            r#"{"multipart":[{"when":{"north":"true"},"apply":{"model":"block/part"}},{"when":{"east":"true"},"apply":{"model":"block/part","y":90}}]}"#,
        );
        source.insert(
            "assets/minecraft/models/block/part.json",
            r##"{"textures":{"all":"block/stone"},"elements":[{"from":[0,0,0],"to":[4,16,4],"faces":{"north":{"texture":"#all"}}}]}"##,
        );
        let mut loader = Loader::new(&mut source);
        let definition = loader
            .load_blockstate(&identifier("minecraft:test"))
            .unwrap();
        let state = loader
            .resolve_state(
                &definition,
                &BTreeMap::from([
                    ("north".to_owned(), "true".to_owned()),
                    ("east".to_owned(), "true".to_owned()),
                ]),
            )
            .unwrap();
        assert_eq!(state.parts.len(), 2);
    }

    #[test]
    fn malformed_blockstate_json_returns_a_structured_error() {
        let mut source = MemorySource::default();
        source.insert("assets/minecraft/blockstates/test.json", "{");
        let mut loader = Loader::new(&mut source);
        assert!(matches!(
            loader.load_blockstate(&identifier("minecraft:test")),
            Err(BlockResourceError::Malformed {
                kind: "blockstate",
                ..
            })
        ));
    }

    #[test]
    fn model_parent_and_texture_inheritance_resolve_across_levels() {
        let mut source = MemorySource::default();
        source.insert(
            "assets/minecraft/models/block/base.json",
            r##"{"textures":{"all":"minecraft:block/stone"},"elements":[{"from":[0,0,0],"to":[16,16,16],"faces":{"north":{"texture":"#all","cullface":"north","tintindex":0}}}]}"##,
        );
        source.insert(
            "assets/minecraft/models/block/middle.json",
            r##"{"parent":"minecraft:block/base","textures":{"side":"#all"}}"##,
        );
        source.insert(
            "assets/minecraft/models/block/child.json",
            r#"{"parent":"minecraft:block/middle"}"#,
        );
        let mut loader = Loader::new(&mut source);
        let resolved = loader
            .resolve_model(&identifier("minecraft:block/child"), &mut Vec::new())
            .unwrap();
        let faces = bake_model(&resolved).unwrap();
        assert_eq!(faces.len(), 1);
        assert_eq!(faces[0].texture, "minecraft:block/stone");
        assert_eq!(faces[0].cullface, Some(Direction::North));
        assert_eq!(faces[0].tint_index, Some(0));
    }

    #[test]
    fn zero_thickness_cross_faces_are_coplanar_with_opposite_winding() {
        let mut source = MemorySource::default();
        source.insert(
            "assets/minecraft/models/block/test_cross.json",
            r##"{"textures":{"cross":"minecraft:block/test"},"elements":[{"from":[0.8,0,8],"to":[15.2,16,8],"faces":{"north":{"texture":"#cross"},"south":{"texture":"#cross"}}}]}"##,
        );
        let mut loader = Loader::new(&mut source);
        let resolved = loader
            .resolve_model(&identifier("minecraft:block/test_cross"), &mut Vec::new())
            .unwrap();
        let faces = bake_model(&resolved).unwrap();
        assert_eq!(faces.len(), 2);

        let sorted = |mut corners: [[f32; 3]; 4]| {
            corners.sort_by(|left, right| {
                left.iter()
                    .zip(right)
                    .find_map(|(left, right)| {
                        let ordering = left.total_cmp(right);
                        (ordering != std::cmp::Ordering::Equal).then_some(ordering)
                    })
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            corners
        };
        assert_eq!(sorted(faces[0].corners), sorted(faces[1].corners));

        let normal = |corners: [[f32; 3]; 4]| {
            let a = [
                corners[1][0] - corners[0][0],
                corners[1][1] - corners[0][1],
                corners[1][2] - corners[0][2],
            ];
            let b = [
                corners[2][0] - corners[0][0],
                corners[2][1] - corners[0][1],
                corners[2][2] - corners[0][2],
            ];
            [
                a[1] * b[2] - a[2] * b[1],
                a[2] * b[0] - a[0] * b[2],
                a[0] * b[1] - a[1] * b[0],
            ]
        };
        let first = normal(faces[0].corners);
        let second = normal(faces[1].corners);
        let dot = first[0] * second[0] + first[1] * second[1] + first[2] * second[2];
        assert!(dot < 0.0, "opposing faces must have opposite winding");
    }

    #[test]
    fn parent_and_texture_cycles_are_rejected() {
        let mut source = MemorySource::default();
        source.insert(
            "assets/minecraft/models/block/a.json",
            r#"{"parent":"minecraft:block/b"}"#,
        );
        source.insert(
            "assets/minecraft/models/block/b.json",
            r#"{"parent":"minecraft:block/a"}"#,
        );
        let mut loader = Loader::new(&mut source);
        assert!(
            loader
                .resolve_model(&identifier("minecraft:block/a"), &mut Vec::new())
                .is_err()
        );

        let textures = BTreeMap::from([
            ("a".to_owned(), "#b".to_owned()),
            ("b".to_owned(), "#a".to_owned()),
        ]);
        assert!(resolve_texture("#a", &textures).is_err());
    }

    #[test]
    fn model_inheritance_depth_and_missing_parent_are_bounded_errors() {
        let mut source = MemorySource::default();
        for index in 0..=MAX_MODEL_DEPTH {
            let parent = index + 1;
            source.insert(
                &format!("assets/minecraft/models/block/depth{index}.json"),
                &format!(r#"{{"parent":"minecraft:block/depth{parent}"}}"#),
            );
        }
        let mut loader = Loader::new(&mut source);
        assert!(
            loader
                .resolve_model(&identifier("minecraft:block/depth0"), &mut Vec::new())
                .is_err()
        );
        assert!(
            loader
                .resolve_model(&identifier("minecraft:block/missing"), &mut Vec::new())
                .is_err()
        );
    }

    #[test]
    fn face_uv_rotation_element_rotation_and_bounds_are_preserved() {
        let model: ModelWire = serde_json::from_value(serde_json::json!({
            "textures": {"face": "minecraft:block/stone"},
            "elements": [{
                "from": [0, 0, 0], "to": [8, 16, 16],
                "rotation": {"origin": [8, 8, 8], "axis": "y", "angle": 22.5, "rescale": true},
                "faces": {"east": {"texture": "#face", "uv": [1,2,3,4], "rotation": 90}}
            }]
        }))
        .unwrap();
        let resolved = ResolvedModel {
            textures: model
                .textures
                .into_iter()
                .map(|(name, value)| (name, value.into_sprite()))
                .collect(),
            elements: model.elements.unwrap(),
        };
        let faces = bake_model(&resolved).unwrap();
        assert_eq!(faces.len(), 1);
        assert_eq!(faces[0].uv[0], [1.0 / 16.0, 4.0 / 16.0]);
        assert_eq!(faces[0].uv[1], [3.0 / 16.0, 4.0 / 16.0]);
        assert!(
            faces[0]
                .corners
                .iter()
                .flatten()
                .all(|value| value.is_finite())
        );
    }

    #[test]
    fn minecraft_top_left_uv_origin_is_preserved_for_generated_explicit_and_rotated_faces() {
        let bake_face = |face: Value| {
            let model: ModelWire = serde_json::from_value(serde_json::json!({
                "textures": {"face": "minecraft:block/asymmetric"},
                "elements": [{
                    "from": [0, 0, 0], "to": [16, 16, 16],
                    "faces": {"north": face}
                }]
            }))
            .unwrap();
            bake_model(&ResolvedModel {
                textures: model
                    .textures
                    .into_iter()
                    .map(|(name, value)| (name, value.into_sprite()))
                    .collect(),
                elements: model.elements.unwrap(),
            })
            .unwrap()
            .remove(0)
        };

        let generated = bake_face(serde_json::json!({"texture": "#face"}));
        let explicit = bake_face(serde_json::json!({
            "texture": "#face", "uv": [2, 3, 14, 13]
        }));
        let rotated = bake_face(serde_json::json!({
            "texture": "#face", "uv": [0, 0, 16, 16], "rotation": 90
        }));

        assert_eq!(
            generated.uv,
            [[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]]
        );
        assert_eq!(explicit.uv[0], [2.0 / 16.0, 3.0 / 16.0]);
        assert_eq!(explicit.uv[1], [2.0 / 16.0, 13.0 / 16.0]);
        assert_eq!(rotated.uv, [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]]);
    }

    #[test]
    fn all_cube_faces_use_minecrafts_canonical_local_u_axis() {
        let mut rgba = Vec::with_capacity(16 * 16 * 4);
        for _y in 0_u8..16 {
            for x in 0_u8..16 {
                rgba.extend_from_slice(&[x, 0, 15 - x, 255]);
            }
        }
        let atlas = pack_atlas(BTreeMap::from([(
            "minecraft:block/asymmetric_left_right".to_owned(),
            DecodedImage {
                width: 16,
                height: 16,
                rgba,
                cutout: false,
            },
        )]))
        .unwrap();
        let region = atlas.region("minecraft:block/asymmetric_left_right");
        let sample = |uv: [f32; 2]| {
            let atlas_u = region.min[0] + (region.max[0] - region.min[0]) * uv[0];
            let atlas_v = region.min[1] + (region.max[1] - region.min[1]) * uv[1];
            let x = (atlas_u * atlas.width as f32).floor() as usize;
            let y = (atlas_v * atlas.height as f32).floor() as usize;
            let offset = (y * atlas.width as usize + x) * 4;
            <[u8; 4]>::try_from(&atlas.rgba[offset..offset + 4]).unwrap()
        };
        let cases = [
            (
                "down",
                Direction::Down,
                [
                    [0.0, 0.0, 16.0],
                    [0.0, 0.0, 0.0],
                    [16.0, 0.0, 0.0],
                    [16.0, 0.0, 16.0],
                ],
            ),
            (
                "up",
                Direction::Up,
                [
                    [0.0, 16.0, 0.0],
                    [0.0, 16.0, 16.0],
                    [16.0, 16.0, 16.0],
                    [16.0, 16.0, 0.0],
                ],
            ),
            (
                "north",
                Direction::North,
                [
                    [16.0, 16.0, 0.0],
                    [16.0, 0.0, 0.0],
                    [0.0, 0.0, 0.0],
                    [0.0, 16.0, 0.0],
                ],
            ),
            (
                "south",
                Direction::South,
                [
                    [0.0, 16.0, 16.0],
                    [0.0, 0.0, 16.0],
                    [16.0, 0.0, 16.0],
                    [16.0, 16.0, 16.0],
                ],
            ),
            (
                "west",
                Direction::West,
                [
                    [0.0, 16.0, 0.0],
                    [0.0, 0.0, 0.0],
                    [0.0, 0.0, 16.0],
                    [0.0, 16.0, 16.0],
                ],
            ),
            (
                "east",
                Direction::East,
                [
                    [16.0, 16.0, 16.0],
                    [16.0, 0.0, 16.0],
                    [16.0, 0.0, 0.0],
                    [16.0, 16.0, 0.0],
                ],
            ),
        ];
        let canonical_uv = [[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]];

        for (name, direction, expected_corners) in cases {
            assert_eq!(
                face_corners([0.0; 3], [16.0; 3], direction),
                expected_corners,
                "wrong canonical corner order for {name}"
            );

            for face_json in [
                serde_json::json!({"texture": "#face"}),
                serde_json::json!({"texture": "#face", "uv": [0, 0, 16, 16]}),
            ] {
                let mut faces = serde_json::Map::new();
                faces.insert(name.to_owned(), face_json);
                let model: ModelWire = serde_json::from_value(serde_json::json!({
                    "textures": {"face": "minecraft:block/asymmetric_left_right"},
                    "elements": [{
                        "from": [0, 0, 0], "to": [16, 16, 16],
                        "faces": faces
                    }]
                }))
                .unwrap();
                let face = bake_model(&ResolvedModel {
                    textures: model
                        .textures
                        .into_iter()
                        .map(|(name, value)| (name, value.into_sprite()))
                        .collect(),
                    elements: model.elements.unwrap(),
                })
                .unwrap()
                .remove(0);
                assert_eq!(face.uv, canonical_uv, "mirrored U on {name}");
                assert_eq!(
                    face.corners,
                    expected_corners.map(|corner| corner.map(|value| value / 16.0)),
                    "UVs attached to the wrong corners on {name}"
                );
                let left = [
                    (face.uv[0][0] + face.uv[1][0]) * 0.5 + 0.5 / 16.0,
                    (face.uv[0][1] + face.uv[1][1]) * 0.5,
                ];
                let right = [
                    (face.uv[2][0] + face.uv[3][0]) * 0.5 - 0.5 / 16.0,
                    (face.uv[2][1] + face.uv[3][1]) * 0.5,
                ];
                assert_eq!(sample(left), [0, 0, 15, 255], "wrong left edge on {name}");
                assert_eq!(sample(right), [15, 0, 0, 255], "wrong right edge on {name}");
            }

            let mut rotated_faces = serde_json::Map::new();
            rotated_faces.insert(
                name.to_owned(),
                serde_json::json!({
                    "texture": "#face", "uv": [0, 0, 16, 16], "rotation": 90
                }),
            );
            let model: ModelWire = serde_json::from_value(serde_json::json!({
                "textures": {"face": "minecraft:block/asymmetric_left_right"},
                "elements": [{
                    "from": [0, 0, 0], "to": [16, 16, 16],
                    "faces": rotated_faces
                }]
            }))
            .unwrap();
            let face = bake_model(&ResolvedModel {
                textures: model
                    .textures
                    .into_iter()
                    .map(|(name, value)| (name, value.into_sprite()))
                    .collect(),
                elements: model.elements.unwrap(),
            })
            .unwrap()
            .remove(0);
            let mut rotated_uv = canonical_uv;
            rotated_uv.rotate_left(1);
            assert_eq!(face.uv, rotated_uv, "wrong 90-degree rotation on {name}");
        }
    }

    #[test]
    fn atlas_preserves_asymmetric_top_and_bottom_rows() {
        let asymmetric = DecodedImage {
            width: 2,
            height: 2,
            rgba: [
                255, 0, 0, 255, 255, 0, 0, 255, // top row
                0, 0, 255, 255, 0, 0, 255, 255, // bottom row
            ]
            .to_vec(),
            cutout: false,
        };
        let atlas = pack_atlas(BTreeMap::from([
            ("cubic:missing".to_owned(), missing_texture()),
            ("minecraft:block/asymmetric".to_owned(), asymmetric),
        ]))
        .unwrap();
        let region = atlas.region("minecraft:block/asymmetric");
        let x = (region.min[0] * atlas.width as f32).round() as usize;
        let top = (region.min[1] * atlas.height as f32).round() as usize;
        let bottom = (region.max[1] * atlas.height as f32).round() as usize - 1;
        let pixel = |x: usize, y: usize| {
            let offset = (y * atlas.width as usize + x) * 4;
            &atlas.rgba[offset..offset + 4]
        };
        assert_eq!(pixel(x, top), [255, 0, 0, 255]);
        assert_eq!(pixel(x, bottom), [0, 0, 255, 255]);
    }

    #[test]
    fn full_domain_atlas_uvs_give_every_texel_equal_width_and_duplicate_edges() {
        let mut pixels = Vec::with_capacity(16 * 16 * 4);
        for y in 0_u8..16 {
            for x in 0_u8..16 {
                pixels.extend_from_slice(&[x, y, x ^ y, 255]);
            }
        }
        let atlas = pack_atlas(BTreeMap::from([
            ("cubic:missing".to_owned(), missing_texture()),
            (
                "minecraft:block/pixel_grid".to_owned(),
                DecodedImage {
                    width: 16,
                    height: 16,
                    rgba: pixels,
                    cutout: false,
                },
            ),
        ]))
        .unwrap();
        let region = atlas.region("minecraft:block/pixel_grid");
        let span_x = (region.max[0] - region.min[0]) * atlas.width as f32;
        let span_y = (region.max[1] - region.min[1]) * atlas.height as f32;
        assert_eq!(span_x, 16.0);
        assert_eq!(span_y, 16.0);

        let sample_nearest = |u: f32, v: f32| {
            let x = (u * atlas.width as f32).floor() as usize;
            let y = (v * atlas.height as f32).floor() as usize;
            let offset = (y * atlas.width as usize + x) * 4;
            <[u8; 4]>::try_from(&atlas.rgba[offset..offset + 4]).unwrap()
        };
        for y in 0_u8..16 {
            for x in 0_u8..16 {
                let face_u = (f32::from(x) + 0.5) / 16.0;
                let face_v = (f32::from(y) + 0.5) / 16.0;
                let atlas_u = region.min[0] + (region.max[0] - region.min[0]) * face_u;
                let atlas_v = region.min[1] + (region.max[1] - region.min[1]) * face_v;
                assert_eq!(sample_nearest(atlas_u, atlas_v), [x, y, x ^ y, 255]);
            }
        }

        let px = (region.min[0] * atlas.width as f32).round() as usize;
        let py = (region.min[1] * atlas.height as f32).round() as usize;
        let pixel = |x: usize, y: usize| {
            let offset = (y * atlas.width as usize + x) * 4;
            <[u8; 4]>::try_from(&atlas.rgba[offset..offset + 4]).unwrap()
        };
        assert_eq!(pixel(px - 1, py), pixel(px, py));
        assert_eq!(pixel(px + 16, py), pixel(px + 15, py));
        assert_eq!(pixel(px, py - 1), pixel(px, py));
        assert_eq!(pixel(px, py + 16), pixel(px, py + 15));
        assert_eq!(pixel(px - 1, py - 1), pixel(px, py));
        assert_eq!(pixel(px + 16, py + 16), pixel(px + 15, py + 15));
    }

    #[test]
    fn prepared_state_lookup_is_direct_and_full_cube_detection_is_conservative() {
        let resources = BlockResources::synthetic([RuntimeBlockStateId(0)]);
        assert!(resources.state(RuntimeBlockStateId(0)).parts.is_empty());
        assert!(std::ptr::eq(
            resources.state(RuntimeBlockStateId(7)),
            resources.state(RuntimeBlockStateId(7))
        ));
        assert!(resources.state(RuntimeBlockStateId(7)).full_opaque_cube);

        let mut partial = fallback_state();
        for part in &mut partial.parts {
            for (_, model) in &mut part.entries {
                for face in &mut model.faces {
                    for corner in &mut face.corners {
                        corner[1] *= 0.5;
                    }
                }
            }
        }
        assert!(!is_full_cube(&partial.parts[0].entries[0].1));
    }

    #[test]
    fn runtime_preparation_resolves_static_blockstate_transforms_and_materials_once() {
        let atlas = pack_atlas(BTreeMap::from([(
            "cubic:missing".to_owned(),
            missing_texture(),
        )]))
        .unwrap();
        let mut state = fallback_state();
        let model = &mut state.parts[0].entries[0].1;
        model.y_rotation = 90;
        model.uvlock = true;
        let original_corner = model.faces[0].corners[0];
        let original_uv = model.faces[0].uv;
        let original_cull = model.faces[0].cullface.unwrap();

        prepare_runtime_state(&mut state, &atlas);

        let model = &state.parts[0].entries[0].1;
        assert_eq!(
            (model.x_rotation, model.y_rotation, model.uvlock),
            (0, 0, false)
        );
        assert_eq!(
            model.faces[0].corners[0],
            rotate_blockstate_corner(original_corner, 0, 90)
        );
        assert_eq!(
            model.faces[0].cullface,
            Some(rotate_blockstate_direction(original_cull, 0, 90))
        );
        let mut expected_uv = original_uv;
        expected_uv.rotate_left(uvlock_quarter_turns(model.faces[0].direction, 0, 90));
        assert_eq!(model.faces[0].uv, expected_uv);
        assert_eq!(model.faces[0].atlas_region, atlas.region("cubic:missing"));
        assert!(state.full_opaque_cube);

        let mut x_rotated = fallback_state();
        let x_model = &mut x_rotated.parts[0].entries[0].1;
        x_model.x_rotation = 90;
        x_model.uvlock = true;
        let original_corner = x_model.faces[0].corners[0];
        let original_uv = x_model.faces[0].uv;
        let original_cull = x_model.faces[0].cullface.unwrap();
        prepare_runtime_state(&mut x_rotated, &atlas);
        let x_model = &x_rotated.parts[0].entries[0].1;
        let mut expected_uv = original_uv;
        expected_uv.rotate_left(uvlock_quarter_turns(model.faces[0].direction, 90, 0));
        assert_eq!(x_model.faces[0].uv, expected_uv);
        assert_eq!(
            x_model.faces[0].corners[0],
            rotate_blockstate_corner(original_corner, 90, 0)
        );
        assert_eq!(
            x_model.faces[0].cullface,
            Some(rotate_blockstate_direction(original_cull, 90, 0))
        );
    }

    #[test]
    fn uvlock_uses_direction_specific_face_bases_for_stair_rotations() {
        let y90 = [
            (Direction::Down, 1),
            (Direction::Up, 3),
            (Direction::North, 0),
            (Direction::South, 0),
            (Direction::West, 0),
            (Direction::East, 0),
        ];
        for (direction, expected) in y90 {
            assert_eq!(uvlock_quarter_turns(direction, 0, 90), expected);
        }

        let x180 = [
            (Direction::Down, 0),
            (Direction::Up, 0),
            (Direction::North, 2),
            (Direction::South, 2),
            (Direction::West, 2),
            (Direction::East, 2),
        ];
        for (direction, expected) in x180 {
            assert_eq!(uvlock_quarter_turns(direction, 180, 0), expected);
        }

        // Representative official 26.1.2 stairs use Y quarter-turns for all
        // horizontal facings and X=180 for top-half models. Combining them
        // remains deterministic for every inner/outer/straight model face.
        for x in [0, 180] {
            for y in [0, 90, 180, 270] {
                for direction in Direction::ALL {
                    assert!(uvlock_quarter_turns(direction, x, y) < 4);
                }
            }
        }
    }

    #[test]
    fn representative_official_stair_states_keep_their_model_transforms() {
        let cases = [
            ("minecraft:block/oak_stairs", 0, 0, false),
            ("minecraft:block/oak_stairs", 0, 270, true),
            ("minecraft:block/oak_stairs_inner", 0, 90, true),
            ("minecraft:block/oak_stairs_outer", 0, 90, true),
            ("minecraft:block/oak_stairs_inner", 180, 0, true),
            ("minecraft:block/oak_stairs_outer", 180, 0, true),
            ("minecraft:block/oak_stairs", 180, 90, true),
            ("minecraft:block/oak_stairs_inner", 180, 270, true),
        ];
        for (model, x, y, uvlock) in cases {
            let reference = parse_model_references(&serde_json::json!({
                "model": model,
                "x": x,
                "y": y,
                "uvlock": uvlock
            }))
            .unwrap()
            .remove(0);
            assert_eq!(reference.model.as_str(), model);
            assert_eq!((reference.x, reference.y, reference.uvlock), (x, y, uvlock));
        }
    }

    #[test]
    fn official_door_blockstate_rotations_keep_thin_model_inside_the_cell() {
        let base_corners = [
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, 1.0, 0.0],
            [0.0, 1.0, 1.0],
            [3.0 / 16.0, 0.0, 0.0],
            [3.0 / 16.0, 0.0, 1.0],
            [3.0 / 16.0, 1.0, 0.0],
            [3.0 / 16.0, 1.0, 1.0],
        ];
        for y in [0, 90, 180, 270] {
            let rotated = base_corners.map(|corner| rotate_blockstate_corner(corner, 0, y));
            assert!(
                rotated
                    .iter()
                    .flatten()
                    .all(|value| (0.0..=1.0).contains(value))
            );
            let min_x = rotated
                .iter()
                .map(|corner| corner[0])
                .fold(f32::INFINITY, f32::min);
            let max_x = rotated
                .iter()
                .map(|corner| corner[0])
                .fold(f32::NEG_INFINITY, f32::max);
            let min_z = rotated
                .iter()
                .map(|corner| corner[2])
                .fold(f32::INFINITY, f32::min);
            let max_z = rotated
                .iter()
                .map(|corner| corner[2])
                .fold(f32::NEG_INFINITY, f32::max);
            assert!(
                (max_x - min_x - 3.0 / 16.0).abs() < 1.0e-6
                    || (max_z - min_z - 3.0 / 16.0).abs() < 1.0e-6
            );
        }
    }

    #[test]
    fn atlas_packing_is_deterministic_guttered_and_classifies_alpha() {
        let opaque = DecodedImage {
            width: 2,
            height: 2,
            rgba: vec![255; 16],
            cutout: false,
        };
        let mut alpha = opaque.clone();
        alpha.cutout = true;
        alpha.rgba[3] = 0;
        let images = BTreeMap::from([
            ("minecraft:block/a".to_owned(), opaque),
            ("minecraft:block/b".to_owned(), alpha),
            ("cubic:missing".to_owned(), missing_texture()),
        ]);
        let first = pack_atlas(images.clone()).unwrap();
        let second = pack_atlas(images).unwrap();
        assert_eq!(first.rgba, second.rgba);
        assert_eq!(
            first.regions.keys().collect::<Vec<_>>(),
            second.regions.keys().collect::<Vec<_>>()
        );
        assert_eq!(first.region("minecraft:block/a").layer, RenderLayer::Opaque);
        assert_eq!(first.region("minecraft:block/b").layer, RenderLayer::Cutout);
        let region = first.region("minecraft:block/a");
        assert!(region.min[0] > 0.0 && region.max[0] < 1.0);
    }

    #[test]
    fn malformed_and_oversized_pngs_are_rejected() {
        assert!(decode_png(b"not png").is_err());
        let oversized = DecodedImage {
            width: MAX_ATLAS_SIDE,
            height: MAX_ATLAS_SIDE,
            rgba: vec![],
            cutout: false,
        };
        assert!(
            pack_atlas(BTreeMap::from([
                ("cubic:missing".to_owned(), missing_texture()),
                ("minecraft:block/huge".to_owned(), oversized),
            ]))
            .is_err()
        );
    }
}
