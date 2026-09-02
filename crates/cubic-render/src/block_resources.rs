use std::{
    collections::{BTreeMap, BTreeSet},
    io::Cursor,
};

use cubic_resources::{
    MAX_VANILLA_RESOURCE_BYTES, ResourceError, VanillaResourcePath, VanillaResourceSource,
};
use cubic_version::{GameData, MinecraftIdentifier};
use cubic_world::{
    BlockCollisionProfile, BlockEnvironmentProfile, CollisionShape, FluidKind, FluidState,
    RuntimeBlockStateId,
};
use png::{ColorType, Transformations};
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

const MAX_JSON_BYTES: u64 = 1024 * 1024;
const MAX_MODEL_DEPTH: usize = 32;
const MAX_ELEMENTS: usize = 256;
const MAX_FLUID_OCCLUSION_BOXES: usize = 32;
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
    #[error(
        "malformed texture metadata section `{section}` for `{texture}` at `{metadata_path}`: {reason}"
    )]
    TextureMetadata {
        texture: String,
        metadata_path: String,
        section: &'static str,
        reason: String,
    },
    #[error("texture atlas exceeds the {maximum}-pixel side limit")]
    AtlasTooLarge { maximum: u32 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RenderLayer {
    Opaque,
    Cutout,
    Translucent,
    /// Layered translucent model geometry that needs every layer blended.
    ///
    /// Vanilla's translucent chunk layer sorts individual quads before using
    /// its depth-writing terrain pipeline. Cubic deliberately defers general
    /// per-quad translucent sorting, so exact-version resources with nested
    /// translucent shells use a non-depth-writing compatibility policy rather
    /// than losing all geometry behind the first shell.
    LayeredTranslucent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TintKind {
    None,
    Grass,
    Foliage,
    DryFoliage,
    Water,
    Fixed(u32),
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
    pub tint_kind: TintKind,
    pub render_layer: RenderLayer,
    pub directional_shade: bool,
    pub shade: f32,
}

#[derive(Clone, Debug)]
pub(crate) struct ModelApplication {
    pub faces: Vec<ModelFace>,
    /// Axis-aligned solid element bounds used only to suppress contained
    /// fluid surfaces. Rotated model elements are deliberately omitted rather
    /// than approximated with an over-large box.
    pub solid_boxes: Vec<[[f32; 3]; 2]>,
    pub x_rotation: u16,
    pub y_rotation: u16,
    pub uvlock: bool,
    pub ambient_occlusion: bool,
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
    /// Exact-version projection of `BlockState.isSolid()` for fluid surface
    /// sampling. This is deliberately distinct from visual occlusion.
    pub fluid_surface_solid: bool,
    pub fluid: Option<FluidState>,
    pub emissive: bool,
    pub model_offset: ModelOffset,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ModelOffset {
    #[default]
    None,
    Xz,
    Xyz,
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
    pub(crate) animations: Vec<TextureAnimationData>,
}

/// A bounded GUI sprite decoded from the verified exact-version resource set.
///
/// Keeping the logical sprite separate from the terrain atlas lets the HUD
/// retain Minecraft's native sprite identity and leaves a clean replacement
/// point for later resource-pack selection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuiSpriteData {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Clone, Debug)]
pub(crate) struct TextureAnimationData {
    pub origin: [u32; 2],
    pub width: u32,
    pub height: u32,
    pub frames: Vec<Vec<u8>>,
    pub sequence: Vec<AnimationStep>,
    pub interpolate: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AnimationStep {
    pub frame: usize,
    pub ticks: u32,
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
    pub crosshair: GuiSpriteData,
    pub destroy_stages: Vec<GuiSpriteData>,
    pub(crate) grass_colormap: Vec<u32>,
    pub(crate) foliage_colormap: Vec<u32>,
    pub(crate) dry_foliage_colormap: Vec<u32>,
}

impl BlockResources {
    pub fn load(
        data: &GameData,
        source: &mut impl VanillaResourceSource,
    ) -> Result<Self, BlockResourceError> {
        let mut loader = Loader::new(source);
        let environment = BlockEnvironmentProfile::from_game_data(data);
        let collision = BlockCollisionProfile::from_game_data(data);
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
                let mut models = if is_air {
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
                apply_state_semantics(
                    &mut models,
                    block.identifier.as_str(),
                    &state.properties,
                    environment.state(RuntimeBlockStateId(state.state_id)),
                    collision.shape(RuntimeBlockStateId(state.state_id)),
                );
                states.insert(RuntimeBlockStateId(state.state_id), models);
            }
        }
        let atlas = loader.build_atlas(&states)?;
        let grass_colormap = loader.load_colormap("minecraft:colormap/grass")?;
        let foliage_colormap = loader.load_colormap("minecraft:colormap/foliage")?;
        let dry_foliage_colormap = loader.load_colormap("minecraft:colormap/dry_foliage")?;
        let crosshair = loader.load_gui_sprite("minecraft:gui/sprites/hud/crosshair")?;
        let destroy_stages = (0..cubic_world::DESTROY_STAGE_COUNT)
            .map(|stage| loader.load_gui_sprite(&format!("minecraft:block/destroy_stage_{stage}")))
            .collect::<Result<Vec<_>, _>>()?;
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
            crosshair,
            destroy_stages,
            grass_colormap,
            foliage_colormap,
            dry_foliage_colormap,
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
            crosshair: GuiSpriteData {
                width: 15,
                height: 15,
                rgba: vec![0; 15 * 15 * 4],
            },
            destroy_stages: (0..cubic_world::DESTROY_STAGE_COUNT)
                .map(|stage| GuiSpriteData {
                    width: 16,
                    height: 16,
                    rgba: vec![stage; 16 * 16 * 4],
                })
                .collect(),
            grass_colormap: vec![0x7fb238; 256 * 256],
            foliage_colormap: vec![0x48b518; 256 * 256],
            dry_foliage_colormap: vec![0x9e814d; 256 * 256],
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

    #[cfg(test)]
    pub(crate) fn with_synthetic_non_full(mut self, state: RuntimeBlockStateId) -> Self {
        let Ok(index) = usize::try_from(state.0) else {
            return self;
        };
        if self.states.len() <= index {
            self.states.resize_with(index + 1, || None);
        }
        let mut models = fallback_state();
        models.full_opaque_cube = false;
        for part in &mut models.parts {
            for (_, model) in &mut part.entries {
                for face in &mut model.faces {
                    face.render_layer = RenderLayer::Translucent;
                }
            }
        }
        self.states[index] = Some(models);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_synthetic_render_layer(
        mut self,
        state: RuntimeBlockStateId,
        layer: RenderLayer,
    ) -> Self {
        let Ok(index) = usize::try_from(state.0) else {
            return self;
        };
        if self.states.len() <= index {
            self.states.resize_with(index + 1, || None);
        }
        let mut models = fallback_state();
        for part in &mut models.parts {
            for (_, model) in &mut part.entries {
                for face in &mut model.faces {
                    face.render_layer = layer;
                }
            }
        }
        models.full_opaque_cube = layer == RenderLayer::Opaque;
        self.states[index] = Some(models);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_synthetic_opaque_boxes(
        mut self,
        state: RuntimeBlockStateId,
        boxes: Vec<[[f32; 3]; 2]>,
    ) -> Self {
        let Ok(index) = usize::try_from(state.0) else {
            return self;
        };
        if self.states.len() <= index {
            self.states.resize_with(index + 1, || None);
        }
        let mut models = fallback_state();
        models.full_opaque_cube = false;
        for part in &mut models.parts {
            for (_, model) in &mut part.entries {
                model.solid_boxes.clone_from(&boxes);
            }
        }
        self.states[index] = Some(models);
        self
    }

    #[cfg(test)]
    pub(crate) fn synthetic_fluid(state: RuntimeBlockStateId, fluid: FluidState) -> Self {
        Self::synthetic_fluids([(state, fluid)])
    }

    #[cfg(test)]
    pub(crate) fn synthetic_fluids(
        fluids: impl IntoIterator<Item = (RuntimeBlockStateId, FluidState)>,
    ) -> Self {
        let mut resources = Self::synthetic([RuntimeBlockStateId(0)]);
        for (state, fluid) in fluids {
            let index = usize::try_from(state.0).expect("synthetic state index");
            resources.states.resize_with(index + 1, || None);
            resources.states[index] = Some(StateModels {
                fluid: Some(fluid),
                ..StateModels::default()
            });
        }
        resources
    }
}

fn prepare_runtime_state(models: &mut StateModels, atlas: &TextureAtlasData) {
    for part in &mut models.parts {
        for (_, model) in &mut part.entries {
            for bounds in &mut model.solid_boxes {
                let corners = box_corners(*bounds).map(|corner| {
                    rotate_blockstate_corner(corner, model.x_rotation, model.y_rotation)
                });
                *bounds = bounds_of_corners(corners);
            }
            for face in &mut model.faces {
                if model.uvlock {
                    face.uv =
                        uvlock_uvs(face.uv, face.direction, model.x_rotation, model.y_rotation);
                }
                face.corners = face.corners.map(|corner| {
                    rotate_blockstate_corner(corner, model.x_rotation, model.y_rotation)
                });
                face.direction =
                    rotate_blockstate_direction(face.direction, model.x_rotation, model.y_rotation);
                recalculate_axis_aligned_winding(face);
                face.shade = if face.directional_shade {
                    direction_shade(face.direction)
                } else {
                    1.0
                };
                face.cullface = face.cullface.map(|direction| {
                    rotate_blockstate_direction(direction, model.x_rotation, model.y_rotation)
                });
                face.atlas_region = atlas.region(&face.texture);
                if face.render_layer == RenderLayer::Opaque
                    && face.atlas_region.layer == RenderLayer::Cutout
                {
                    face.render_layer = RenderLayer::Cutout;
                }
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
                    .all(|face| face.render_layer == RenderLayer::Opaque)
        });
}

fn recalculate_axis_aligned_winding(face: &mut ModelFace) {
    let mut from = [f32::INFINITY; 3];
    let mut to = [f32::NEG_INFINITY; 3];
    for corner in face.corners {
        for axis in 0..3 {
            from[axis] = from[axis].min(corner[axis]);
            to[axis] = to[axis].max(corner[axis]);
        }
    }
    let expected = face_corners(from, to, face.direction);
    let mut mapped = [None; 4];
    for (target_index, target) in expected.iter().enumerate() {
        mapped[target_index] = face
            .corners
            .iter()
            .position(|corner| (0..3).all(|axis| (corner[axis] - target[axis]).abs() <= 1.0e-6));
    }
    let Some(indices) = mapped
        .into_iter()
        .collect::<Option<Vec<_>>>()
        .and_then(|values| <[usize; 4]>::try_from(values).ok())
    else {
        // Arbitrarily rotated model elements are not axis-aligned cuboids and
        // vanilla deliberately retains their original winding.
        return;
    };
    let old_corners = face.corners;
    let old_uv = face.uv;
    face.corners = indices.map(|index| old_corners[index]);
    face.uv = indices.map(|index| old_uv[index]);
}

fn apply_state_semantics(
    models: &mut StateModels,
    identifier: &str,
    properties: &BTreeMap<String, String>,
    environment: cubic_world::BlockEnvironment,
    collision: &CollisionShape,
) {
    let path = identifier
        .split_once(':')
        .map_or(identifier, |(_, path)| path);
    let layer = render_layer_26_1_2(path);
    for part in &mut models.parts {
        for (_, model) in &mut part.entries {
            for face in &mut model.faces {
                face.render_layer = layer;
                face.tint_kind = face.tint_index.map_or(TintKind::None, |index| {
                    tint_kind_26_1_2(path, properties, index)
                });
            }
        }
    }
    models.fluid = environment.fluid;
    models.fluid_surface_solid = environment.fluid.is_none() && legacy_solid_shape(collision);
    models.emissive = environment.emissive;
    models.model_offset = model_offset_26_1_2(path);
}

fn legacy_solid_shape(shape: &CollisionShape) -> bool {
    let bounds = match shape {
        CollisionShape::Empty => return false,
        CollisionShape::FullCube => return true,
        CollisionShape::Boxes(boxes) => {
            boxes
                .iter()
                .fold(None::<cubic_world::Aabb>, |bounds, part| {
                    Some(match bounds {
                        None => *part,
                        Some(bounds) => cubic_world::Aabb::new(
                            cubic_world::Vec3d::new(
                                bounds.min.x.min(part.min.x),
                                bounds.min.y.min(part.min.y),
                                bounds.min.z.min(part.min.z),
                            ),
                            cubic_world::Vec3d::new(
                                bounds.max.x.max(part.max.x),
                                bounds.max.y.max(part.max.y),
                                bounds.max.z.max(part.max.z),
                            ),
                        ),
                    })
                })
        }
    };
    bounds.is_some_and(|bounds| {
        let x = bounds.max.x - bounds.min.x;
        let y = bounds.max.y - bounds.min.y;
        let z = bounds.max.z - bounds.min.z;
        (x + y + z) / 3.0 >= 0.729_166_666_666_666_6 || y >= 1.0
    })
}

fn model_offset_26_1_2(path: &str) -> ModelOffset {
    // Exact-version adapter: these registrations were verified against the
    // 26.1.2 Blocks bootstrap. Keep this table out of generic meshing logic.
    match path {
        "short_grass" | "fern" => ModelOffset::Xyz,
        "tall_grass" | "large_fern" => ModelOffset::Xz,
        _ => ModelOffset::None,
    }
}

fn render_layer_26_1_2(path: &str) -> RenderLayer {
    if path == "honey_block" {
        // The official model has a full outer shell around a second inset
        // translucent cube. Vanilla sorts those quads before drawing them;
        // preserve both layers until Cubic gains the deferred general sorter.
        RenderLayer::LayeredTranslucent
    } else if path == "water"
        || path == "glass"
        || path == "glass_pane"
        || path.ends_with("_stained_glass")
        || path.ends_with("_stained_glass_pane")
        || matches!(path, "ice" | "frosted_ice" | "slime_block")
    {
        RenderLayer::Translucent
    } else if path.ends_with("_leaves")
        || path.ends_with("_sapling")
        || path.ends_with("_door")
        || path.ends_with("_trapdoor")
        || path.ends_with("_tulip")
        || path.ends_with("_coral")
        || path.ends_with("_coral_fan")
        || matches!(
            path,
            "short_grass"
                | "tall_grass"
                | "fern"
                | "large_fern"
                | "dead_bush"
                | "dandelion"
                | "poppy"
                | "blue_orchid"
                | "allium"
                | "azure_bluet"
                | "oxeye_daisy"
                | "cornflower"
                | "lily_of_the_valley"
                | "wither_rose"
                | "sugar_cane"
                | "vine"
                | "ladder"
                | "fire"
                | "soul_fire"
                | "cobweb"
                | "wheat"
                | "carrots"
                | "potatoes"
                | "beetroots"
                | "nether_wart"
                | "leaf_litter"
                | "melon_stem"
                | "pumpkin_stem"
                | "attached_melon_stem"
                | "attached_pumpkin_stem"
                | "seagrass"
                | "tall_seagrass"
                | "kelp"
                | "kelp_plant"
                | "scaffolding"
        )
    {
        RenderLayer::Cutout
    } else {
        RenderLayer::Opaque
    }
}

/// Exact 26.1.2 BlockColors registration projected into renderer-neutral tint
/// semantics. Block names intentionally live only at this version/resource
/// boundary; the mesher never switches on Minecraft identifiers.
fn tint_kind_26_1_2(
    path: &str,
    properties: &BTreeMap<String, String>,
    tint_index: u32,
) -> TintKind {
    if tint_index > 1 {
        return TintKind::None;
    }
    if matches!(
        path,
        "grass_block"
            | "short_grass"
            | "tall_grass"
            | "fern"
            | "large_fern"
            | "potted_fern"
            | "bush"
            | "sugar_cane"
    ) {
        TintKind::Grass
    } else if path == "spruce_leaves" {
        TintKind::Fixed(0x619961)
    } else if path == "birch_leaves" {
        TintKind::Fixed(0x80a755)
    } else if matches!(
        path,
        "oak_leaves"
            | "jungle_leaves"
            | "acacia_leaves"
            | "dark_oak_leaves"
            | "mangrove_leaves"
            | "vine"
    ) {
        TintKind::Foliage
    } else if path == "leaf_litter" {
        TintKind::DryFoliage
    } else if matches!(path, "water" | "bubble_column" | "water_cauldron") {
        TintKind::Water
    } else if matches!(path, "attached_melon_stem" | "attached_pumpkin_stem") {
        TintKind::Fixed(0xe0c71c)
    } else if matches!(path, "melon_stem" | "pumpkin_stem") {
        let age = properties
            .get("age")
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(0)
            .min(7);
        TintKind::Fixed((age * 32) << 16 | (255 - age * 8) << 8 | (age * 4))
    } else if matches!(path, "pink_petals" | "wildflowers") && tint_index == 1 {
        TintKind::Grass
    } else if path == "lily_pad" {
        TintKind::Fixed(if tint_index == 0 { 0x71c35c } else { 0x207f38 })
    } else {
        TintKind::None
    }
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
        // Minecraft blockstate X rotations are clockwise when viewed along
        // +X. Cubic's +Y-up/+Z-south coordinates therefore use the inverse of
        // the conventional right-handed positive-X matrix.
        point = [point[0], point[2], -point[1]];
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
            Direction::Up => Direction::North,
            Direction::North => Direction::Down,
            Direction::Down => Direction::South,
            Direction::South => Direction::Up,
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

#[cfg(test)]
pub(crate) fn uvlock_quarter_turns(
    direction: Direction,
    x_rotation: u16,
    y_rotation: u16,
) -> usize {
    let [f00, f01, f10, f11] = uvlock_inverse_coefficients(direction, x_rotation, y_rotation);
    let source_zero = [-1_i8, -1_i8];
    let transformed_zero = [
        f00 * source_zero[0] + f10 * source_zero[1],
        f01 * source_zero[0] + f11 * source_zero[1],
    ];
    [[-1, -1], [-1, 1], [1, 1], [1, -1]]
        .iter()
        .position(|corner| *corner == transformed_zero)
        .unwrap_or(0)
}

pub(crate) fn uvlock_uvs(
    uvs: [[f32; 2]; 4],
    direction: Direction,
    x_rotation: u16,
    y_rotation: u16,
) -> [[f32; 2]; 4] {
    let [f00, f01, f10, f11] = uvlock_inverse_coefficients(direction, x_rotation, y_rotation);
    uvs.map(|[u, v]| {
        let centered_u = u - 0.5;
        let centered_v = v - 0.5;
        [
            f32::from(f00) * centered_u + f32::from(f10) * centered_v + 0.5,
            f32::from(f01) * centered_u + f32::from(f11) * centered_v + 0.5,
        ]
    })
}

fn uvlock_inverse_coefficients(direction: Direction, x_rotation: u16, y_rotation: u16) -> [i8; 4] {
    let target_direction = rotate_blockstate_direction(direction, x_rotation, y_rotation);
    let (source_u, source_v) = face_uv_axes(direction);
    let (target_u, target_v) = face_uv_axes(target_direction);
    let transformed_u = rotate_blockstate_vector(source_u, x_rotation, y_rotation);
    let transformed_v = rotate_blockstate_vector(source_v, x_rotation, y_rotation);

    // BlockMath.getFaceTransformation builds target-local * model *
    // source-local. FaceBakery applies its affine inverse to each UV around
    // the sprite centre. The orthogonal transform is therefore the transpose
    // below, not a permutation inferred from transformed geometry corners.
    let f00 = dot_axis(transformed_u, target_u);
    let f01 = dot_axis(transformed_v, target_u);
    let f10 = dot_axis(transformed_u, target_v);
    let f11 = dot_axis(transformed_v, target_v);
    [f00, f01, f10, f11]
}

fn face_uv_axes(direction: Direction) -> ([i8; 3], [i8; 3]) {
    match direction {
        Direction::South => ([1, 0, 0], [0, 1, 0]),
        Direction::East => ([0, 0, -1], [0, 1, 0]),
        Direction::West => ([0, 0, 1], [0, 1, 0]),
        Direction::North => ([-1, 0, 0], [0, 1, 0]),
        Direction::Up => ([1, 0, 0], [0, 0, -1]),
        Direction::Down => ([1, 0, 0], [0, 0, 1]),
    }
}

fn rotate_blockstate_vector(mut vector: [i8; 3], x_rotation: u16, y_rotation: u16) -> [i8; 3] {
    for _ in 0..(x_rotation / 90) {
        vector = [vector[0], vector[2], -vector[1]];
    }
    for _ in 0..(y_rotation / 90) {
        vector = [-vector[2], vector[1], vector[0]];
    }
    vector
}

fn dot_axis(left: [i8; 3], right: [i8; 3]) -> i8 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
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
    ambientocclusion: Option<bool>,
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
    ambient_occlusion: bool,
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
                        solid_boxes: model_solid_boxes(&model),
                        x_rotation: reference.x,
                        y_rotation: reference.y,
                        uvlock: reference.uvlock,
                        ambient_occlusion: model.ambient_occlusion,
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
            fluid: None,
            fluid_surface_solid: false,
            emissive: false,
            model_offset: ModelOffset::None,
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
                ambient_occlusion: true,
            }
        };
        if let Some(ambient_occlusion) = wire.ambientocclusion {
            resolved.ambient_occlusion = ambient_occlusion;
        }
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
            if let Some(fluid) = state.fluid {
                let prefix = match fluid.kind {
                    FluidKind::Water => "water",
                    FluidKind::Lava => "lava",
                };
                names.insert(format!("minecraft:block/{prefix}_still"));
                names.insert(format!("minecraft:block/{prefix}_flow"));
            }
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
            images.insert(name.clone(), self.load_texture(name)?);
        }
        pack_atlas(images)
    }

    fn load_texture(&mut self, name: &str) -> Result<DecodedImage, BlockResourceError> {
        let identifier = parse_identifier(name)?;
        let path = resource_path(&identifier, "textures", "png")?;
        let mut image = self
            .source
            .read_resource(&path, MAX_VANILLA_RESOURCE_BYTES)?
            .and_then(|bytes| decode_png(&bytes).ok())
            .unwrap_or_else(missing_texture);
        let metadata_path = resource_path(&identifier, "textures", "png.mcmeta")?;
        let metadata = self
            .source
            .read_resource(&metadata_path, MAX_VANILLA_RESOURCE_BYTES)?;
        image.animation = decode_texture_metadata(
            metadata.as_deref(),
            image.frames.len(),
            name,
            metadata_path.as_str(),
        )?;
        Ok(image)
    }

    fn load_colormap(&mut self, name: &str) -> Result<Vec<u32>, BlockResourceError> {
        let identifier = parse_identifier(name)?;
        let path = resource_path(&identifier, "textures", "png")?;
        let bytes = self
            .source
            .read_resource(&path, MAX_VANILLA_RESOURCE_BYTES)?
            .ok_or_else(|| malformed("colormap", name, "resource is missing"))?;
        let image = decode_png(&bytes)?;
        if image.width != 256 || image.height != 256 {
            return Err(malformed("colormap", name, "expected a 256 by 256 image"));
        }
        Ok(image
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .map(|pixel| {
                (u32::from(pixel[0]) << 16) | (u32::from(pixel[1]) << 8) | u32::from(pixel[2])
            })
            .collect())
    }

    fn load_gui_sprite(&mut self, name: &str) -> Result<GuiSpriteData, BlockResourceError> {
        let identifier = parse_identifier(name)?;
        let path = resource_path(&identifier, "textures", "png")?;
        let bytes = self
            .source
            .read_resource(&path, MAX_VANILLA_RESOURCE_BYTES)?
            .ok_or_else(|| malformed("GUI sprite", name, "resource is missing"))?;
        let image = decode_png(&bytes)?;
        if image.frames.len() != 1 {
            return Err(malformed(
                "GUI sprite",
                name,
                "animated sprites are not supported by this HUD path",
            ));
        }
        Ok(GuiSpriteData {
            width: image.width,
            height: image.height,
            rgba: image.rgba,
        })
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
                tint_kind: TintKind::None,
                render_layer: RenderLayer::Opaque,
                directional_shade: element.shade,
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

fn model_solid_boxes(model: &ResolvedModel) -> Vec<[[f32; 3]; 2]> {
    model
        .elements
        .iter()
        .filter(|element| element.rotation.is_none())
        .take(MAX_FLUID_OCCLUSION_BOXES)
        .map(|element| {
            [
                element.from.map(|value| value / 16.0),
                element.to.map(|value| value / 16.0),
            ]
        })
        .collect()
}

fn box_corners(bounds: [[f32; 3]; 2]) -> [[f32; 3]; 8] {
    let [min, max] = bounds;
    [
        [min[0], min[1], min[2]],
        [min[0], min[1], max[2]],
        [min[0], max[1], min[2]],
        [min[0], max[1], max[2]],
        [max[0], min[1], min[2]],
        [max[0], min[1], max[2]],
        [max[0], max[1], min[2]],
        [max[0], max[1], max[2]],
    ]
}

fn bounds_of_corners(corners: [[f32; 3]; 8]) -> [[f32; 3]; 2] {
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for corner in corners {
        for axis in 0..3 {
            min[axis] = min[axis].min(corner[axis]);
            max[axis] = max[axis].max(corner[axis]);
        }
    }
    [min, max]
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
        Direction::Down => [from[0], 16.0 - to[2], to[0], 16.0 - from[2]],
        Direction::Up => [from[0], from[2], to[0], to[2]],
        Direction::North => [16.0 - to[0], 16.0 - to[1], 16.0 - from[0], 16.0 - from[1]],
        Direction::South => [from[0], 16.0 - to[1], to[0], 16.0 - from[1]],
        Direction::West => [from[2], 16.0 - to[1], to[2], 16.0 - from[1]],
        Direction::East => [16.0 - to[2], 16.0 - to[1], 16.0 - from[2], 16.0 - from[1]],
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
    // Java 26.1.2 CardinalLighting defaults. These factors are deliberately
    // symmetric by axis; the previous asymmetric values made an otherwise
    // exposed north face substantially darker than its south counterpart.
    match direction {
        Direction::Up => 1.0,
        Direction::Down => 0.5,
        Direction::North | Direction::South => 0.8,
        Direction::East | Direction::West => 0.6,
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
        ambient_occlusion: true,
    };
    let faces = bake_model(&model).unwrap_or_default();
    StateModels {
        parts: vec![WeightedApplications {
            entries: vec![(
                1,
                ModelApplication {
                    faces,
                    solid_boxes: model_solid_boxes(&model),
                    x_rotation: 0,
                    y_rotation: 0,
                    uvlock: false,
                    ambient_occlusion: true,
                },
            )],
            total_weight: 1,
        }],
        full_opaque_cube: true,
        fluid_surface_solid: true,
        fluid: None,
        emissive: false,
        model_offset: ModelOffset::None,
    }
}

#[derive(Clone)]
struct DecodedImage {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    cutout: bool,
    frames: Vec<Vec<u8>>,
    animation: Option<DecodedAnimation>,
}

impl DecodedImage {
    fn static_frame(width: u32, height: u32, rgba: Vec<u8>, cutout: bool) -> Self {
        Self {
            width,
            height,
            frames: vec![rgba.clone()],
            rgba,
            cutout,
            animation: None,
        }
    }
}

#[derive(Clone, Debug)]
struct DecodedAnimation {
    sequence: Vec<AnimationStep>,
    interpolate: bool,
}

#[derive(Deserialize)]
struct AnimationWire {
    #[serde(default = "default_frame_time")]
    frametime: u32,
    #[serde(default)]
    interpolate: bool,
    frames: Option<Vec<AnimationFrameWire>>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum AnimationFrameWire {
    Index(u32),
    Detailed { index: u32, time: Option<u32> },
}

const fn default_frame_time() -> u32 {
    1
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
    let frame_height = frame.width.min(frame.height);
    if frame.height % frame_height != 0 {
        return Err(malformed(
            "texture",
            "png",
            "animated texture height is not a multiple of its frame height",
        ));
    }
    let channels = match frame.color_type {
        ColorType::Rgba => 4,
        ColorType::Rgb => 3,
        ColorType::GrayscaleAlpha => 2,
        ColorType::Grayscale => 1,
        _ => return Err(malformed("texture", "png", "unsupported color format")),
    };
    let mut decoded = Vec::with_capacity(
        usize::try_from(frame.width.saturating_mul(frame.height).saturating_mul(4)).unwrap_or(0),
    );
    for pixel in raw.chunks_exact(channels) {
        match channels {
            4 => decoded.extend_from_slice(pixel),
            3 => decoded.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]),
            2 => decoded.extend_from_slice(&[pixel[0], pixel[0], pixel[0], pixel[1]]),
            1 => decoded.extend_from_slice(&[pixel[0], pixel[0], pixel[0], 255]),
            _ => {}
        }
    }
    let frame_bytes = usize::try_from(frame.width.saturating_mul(frame_height).saturating_mul(4))
        .map_err(|_| malformed("texture", "png", "frame size overflow"))?;
    let frames = decoded
        .chunks_exact(frame_bytes)
        .map(<[u8]>::to_vec)
        .collect::<Vec<_>>();
    let rgba = frames
        .first()
        .cloned()
        .ok_or_else(|| malformed("texture", "png", "texture contains no frames"))?;
    let cutout = rgba.as_chunks::<4>().0.iter().any(|pixel| pixel[3] < 255);
    Ok(DecodedImage {
        width: frame.width,
        height: frame_height,
        rgba,
        cutout,
        frames,
        animation: None,
    })
}

fn texture_metadata_error(
    texture: &str,
    metadata_path: &str,
    section: &'static str,
    reason: impl Into<String>,
) -> BlockResourceError {
    BlockResourceError::TextureMetadata {
        texture: texture.to_owned(),
        metadata_path: metadata_path.to_owned(),
        section,
        reason: reason.into(),
    }
}

fn decode_texture_metadata(
    bytes: Option<&[u8]>,
    frame_count: usize,
    texture: &str,
    metadata_path: &str,
) -> Result<Option<DecodedAnimation>, BlockResourceError> {
    const MAX_ANIMATION_STEPS: usize = 1024;
    const MAX_FRAME_TICKS: u32 = 72_000;
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    let metadata: serde_json::Map<String, Value> =
        serde_json::from_slice(bytes).map_err(|error| {
            texture_metadata_error(texture, metadata_path, "root", error.to_string())
        })?;
    let Some(animation) = metadata.get("animation") else {
        return Ok(None);
    };
    let animation: AnimationWire = serde_json::from_value(animation.clone()).map_err(|error| {
        texture_metadata_error(texture, metadata_path, "animation", error.to_string())
    })?;
    let default_ticks = animation.frametime;
    if default_ticks == 0 || default_ticks > MAX_FRAME_TICKS {
        return Err(texture_metadata_error(
            texture,
            metadata_path,
            "animation",
            "frame time is outside the supported bound",
        ));
    }
    let sequence = if let Some(frames) = animation.frames {
        if frames.is_empty() || frames.len() > MAX_ANIMATION_STEPS {
            return Err(texture_metadata_error(
                texture,
                metadata_path,
                "animation",
                "frame sequence is empty or exceeds the supported bound",
            ));
        }
        frames
            .into_iter()
            .map(|entry| {
                let (frame, ticks) = match entry {
                    AnimationFrameWire::Index(index) => (index, default_ticks),
                    AnimationFrameWire::Detailed { index, time } => {
                        (index, time.unwrap_or(default_ticks))
                    }
                };
                let frame = usize::try_from(frame).map_err(|_| {
                    texture_metadata_error(
                        texture,
                        metadata_path,
                        "animation",
                        "frame index overflow",
                    )
                })?;
                if frame >= frame_count || ticks == 0 || ticks > MAX_FRAME_TICKS {
                    return Err(texture_metadata_error(
                        texture,
                        metadata_path,
                        "animation",
                        "frame index or duration is outside the supported bound",
                    ));
                }
                Ok(AnimationStep { frame, ticks })
            })
            .collect::<Result<Vec<_>, BlockResourceError>>()?
    } else {
        if frame_count == 0 || frame_count > MAX_ANIMATION_STEPS {
            return Err(texture_metadata_error(
                texture,
                metadata_path,
                "animation",
                "implicit frame sequence exceeds the supported bound",
            ));
        }
        (0..frame_count)
            .map(|frame| AnimationStep {
                frame,
                ticks: default_ticks,
            })
            .collect()
    };
    Ok(Some(DecodedAnimation {
        sequence,
        interpolate: animation.interpolate,
    }))
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
    DecodedImage::static_frame(16, 16, rgba, false)
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
    let mut animations = Vec::new();
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
        let layer = if image.cutout {
            RenderLayer::Cutout
        } else {
            RenderLayer::Opaque
        };
        if let Some(animation) = image.animation {
            animations.push(TextureAnimationData {
                origin: [px, py],
                width: image.width,
                height: image.height,
                frames: image.frames,
                sequence: animation.sequence,
                interpolate: animation.interpolate,
            });
        }
        regions.insert(
            name,
            AtlasRegion {
                min: [px as f32 / width as f32, py as f32 / height as f32],
                max: [
                    (px + image.width) as f32 / width as f32,
                    (py + image.height) as f32 / height as f32,
                ],
                layer,
            },
        );
    }
    Ok(TextureAtlasData {
        width,
        height,
        rgba,
        regions,
        animations,
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

        fn insert_bytes(&mut self, path: &str, value: Vec<u8>) {
            self.0.insert(path.to_owned(), value);
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

    fn rgba_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut output, width, height);
            encoder.set_color(ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("PNG header");
            writer.write_image_data(rgba).expect("PNG pixels");
        }
        output
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
    fn exact_version_gui_sprite_is_loaded_without_embedding_asset_pixels() {
        let mut source = MemorySource::default();
        let pixels = (0..15 * 15)
            .flat_map(|index| {
                let value = u8::try_from(index % 256).unwrap_or(0);
                [value, 255 - value, value / 2, 255]
            })
            .collect::<Vec<_>>();
        source.insert_bytes(
            "assets/minecraft/textures/gui/sprites/hud/crosshair.png",
            rgba_png(15, 15, &pixels),
        );
        let mut loader = Loader::new(&mut source);
        let sprite = loader
            .load_gui_sprite("minecraft:gui/sprites/hud/crosshair")
            .unwrap();
        assert_eq!((sprite.width, sprite.height), (15, 15));
        assert_eq!(sprite.rgba, pixels);
    }

    #[test]
    fn all_ten_destroy_stages_are_loaded_from_official_runtime_resource_paths() {
        let mut source = MemorySource::default();
        for stage in 0..cubic_world::DESTROY_STAGE_COUNT {
            source.insert_bytes(
                &format!("assets/minecraft/textures/block/destroy_stage_{stage}.png"),
                rgba_png(16, 16, &vec![stage; 16 * 16 * 4]),
            );
        }
        let mut loader = Loader::new(&mut source);
        let stages = (0..cubic_world::DESTROY_STAGE_COUNT)
            .map(|stage| loader.load_gui_sprite(&format!("minecraft:block/destroy_stage_{stage}")))
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(stages.len(), usize::from(cubic_world::DESTROY_STAGE_COUNT));
        assert_eq!((stages[0].width, stages[0].height), (16, 16));
        assert_eq!(stages[9].rgba[0], 9);
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
            r##"{"ambientocclusion":false,"textures":{"cross":"minecraft:block/test"},"elements":[{"from":[0.8,0,8],"to":[15.2,16,8],"shade":false,"faces":{"north":{"texture":"#cross"},"south":{"texture":"#cross"}}}]}"##,
        );
        let mut loader = Loader::new(&mut source);
        let resolved = loader
            .resolve_model(&identifier("minecraft:block/test_cross"), &mut Vec::new())
            .unwrap();
        let faces = bake_model(&resolved).unwrap();
        assert_eq!(faces.len(), 2);
        assert!(!resolved.ambient_occlusion);
        assert!(
            faces
                .iter()
                .all(|face| !face.directional_shade && face.shade == 1.0)
        );

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
            ambient_occlusion: true,
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
                ambient_occlusion: true,
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
            DecodedImage::static_frame(16, 16, rgba, false),
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
                    ambient_occlusion: true,
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
                ambient_occlusion: true,
            })
            .unwrap()
            .remove(0);
            let mut rotated_uv = canonical_uv;
            rotated_uv.rotate_left(1);
            assert_eq!(face.uv, rotated_uv, "wrong 90-degree rotation on {name}");
        }
    }

    #[test]
    fn button_style_floor_model_rotates_to_the_vanilla_north_wall_attachment() {
        let source = [
            [5.0 / 16.0, 0.0, 6.0 / 16.0],
            [11.0 / 16.0, 2.0 / 16.0, 10.0 / 16.0],
        ];
        let rotated = source.map(|corner| rotate_blockstate_corner(corner, 90, 0));
        let xs = [rotated[0][0], rotated[1][0]];
        let ys = [rotated[0][1], rotated[1][1]];
        let zs = [rotated[0][2], rotated[1][2]];
        assert_eq!(
            (xs[0].min(xs[1]), xs[0].max(xs[1])),
            (5.0 / 16.0, 11.0 / 16.0)
        );
        assert_eq!(
            (ys[0].min(ys[1]), ys[0].max(ys[1])),
            (6.0 / 16.0, 10.0 / 16.0)
        );
        assert_eq!((zs[0].min(zs[1]), zs[0].max(zs[1])), (14.0 / 16.0, 1.0));
        assert_eq!(
            rotate_blockstate_direction(Direction::Up, 90, 0),
            Direction::North
        );
    }

    #[test]
    fn atlas_preserves_asymmetric_top_and_bottom_rows() {
        let asymmetric = DecodedImage::static_frame(
            2,
            2,
            [
                255, 0, 0, 255, 255, 0, 0, 255, // top row
                0, 0, 255, 255, 0, 0, 255, 255, // bottom row
            ]
            .to_vec(),
            false,
        );
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
                DecodedImage::static_frame(16, 16, pixels, false),
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
        let original_uv = model.faces[0].uv;
        let original_direction = model.faces[0].direction;
        let original_cull = model.faces[0].cullface.unwrap();

        prepare_runtime_state(&mut state, &atlas);

        let model = &state.parts[0].entries[0].1;
        assert_eq!(
            (model.x_rotation, model.y_rotation, model.uvlock),
            (0, 0, false)
        );
        assert_eq!(
            model.faces[0].corners,
            face_corners([0.0; 3], [1.0; 3], model.faces[0].direction)
        );
        assert_eq!(
            model.faces[0].cullface,
            Some(rotate_blockstate_direction(original_cull, 0, 90))
        );
        assert_eq!(
            model.faces[0].direction,
            rotate_blockstate_direction(original_direction, 0, 90)
        );
        assert_eq!(model.faces[0].uv, original_uv);
        assert_eq!(model.faces[0].atlas_region, atlas.region("cubic:missing"));
        assert!(state.full_opaque_cube);

        let mut x_rotated = fallback_state();
        let x_model = &mut x_rotated.parts[0].entries[0].1;
        x_model.x_rotation = 90;
        x_model.uvlock = true;
        let original_uv = x_model.faces[0].uv;
        let original_direction = x_model.faces[0].direction;
        let original_cull = x_model.faces[0].cullface.unwrap();
        prepare_runtime_state(&mut x_rotated, &atlas);
        let x_model = &x_rotated.parts[0].entries[0].1;
        assert_eq!(x_model.faces[0].uv, original_uv);
        assert_eq!(
            x_model.faces[0].direction,
            rotate_blockstate_direction(original_direction, 90, 0)
        );
        assert_eq!(
            x_model.faces[0].shade,
            direction_shade(x_model.faces[0].direction)
        );
        assert_eq!(
            x_model.faces[0].corners,
            face_corners([0.0; 3], [1.0; 3], x_model.faces[0].direction)
        );
        assert_eq!(
            x_model.faces[0].cullface,
            Some(rotate_blockstate_direction(original_cull, 90, 0))
        );
    }

    #[test]
    fn vanilla_directional_shading_is_symmetric_by_horizontal_axis() {
        assert_eq!(direction_shade(Direction::Up), 1.0);
        assert_eq!(direction_shade(Direction::Down), 0.5);
        assert_eq!(direction_shade(Direction::North), 0.8);
        assert_eq!(direction_shade(Direction::South), 0.8);
        assert_eq!(direction_shade(Direction::East), 0.6);
        assert_eq!(direction_shade(Direction::West), 0.6);
    }

    #[test]
    fn exact_version_model_offsets_distinguish_short_and_double_height_grass() {
        assert_eq!(model_offset_26_1_2("short_grass"), ModelOffset::Xyz);
        assert_eq!(model_offset_26_1_2("fern"), ModelOffset::Xyz);
        assert_eq!(model_offset_26_1_2("tall_grass"), ModelOffset::Xz);
        assert_eq!(model_offset_26_1_2("large_fern"), ModelOffset::Xz);
        assert_eq!(model_offset_26_1_2("stone"), ModelOffset::None);
    }

    #[test]
    fn axis_aligned_model_elements_expose_exact_fluid_occlusion_boxes() {
        let model = ResolvedModel {
            textures: BTreeMap::new(),
            elements: vec![ElementWire {
                from: [0.0, 0.0, 0.0],
                to: [16.0, 8.0, 16.0],
                rotation: None,
                shade: true,
                faces: BTreeMap::new(),
            }],
            ambient_occlusion: true,
        };
        assert_eq!(model_solid_boxes(&model), vec![[[0.0; 3], [1.0, 0.5, 1.0]]]);
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
    fn top_half_north_south_stair_surface_uses_affine_inverse_uvlock() {
        let wire: ModelWire = serde_json::from_value(serde_json::json!({
            "textures": {"all": "cubic:missing"},
            "elements": [{
                "from": [0, 0, 0],
                "to": [16, 8, 16],
                "faces": {"down": {"texture": "#all", "uv": [1, 2, 13, 14]}}
            }]
        }))
        .unwrap();
        let resolved = ResolvedModel {
            textures: wire
                .textures
                .into_iter()
                .map(|(name, value)| (name, value.into_sprite()))
                .collect(),
            elements: wire.elements.unwrap(),
            ambient_occlusion: true,
        };
        let atlas = pack_atlas(BTreeMap::from([(
            "cubic:missing".to_owned(),
            missing_texture(),
        )]))
        .unwrap();
        for (facing, y_rotation, expected) in [
            (
                "south",
                90,
                [
                    [2.0 / 16.0, 1.0 / 16.0],
                    [2.0 / 16.0, 13.0 / 16.0],
                    [14.0 / 16.0, 13.0 / 16.0],
                    [14.0 / 16.0, 1.0 / 16.0],
                ],
            ),
            (
                "north",
                270,
                [
                    [2.0 / 16.0, 3.0 / 16.0],
                    [2.0 / 16.0, 15.0 / 16.0],
                    [14.0 / 16.0, 15.0 / 16.0],
                    [14.0 / 16.0, 3.0 / 16.0],
                ],
            ),
        ] {
            let mut state = StateModels {
                parts: vec![WeightedApplications {
                    entries: vec![(
                        1,
                        ModelApplication {
                            faces: bake_model(&resolved).unwrap(),
                            solid_boxes: model_solid_boxes(&resolved),
                            x_rotation: 180,
                            y_rotation,
                            uvlock: true,
                            ambient_occlusion: true,
                        },
                    )],
                    total_weight: 1,
                }],
                ..StateModels::default()
            };
            prepare_runtime_state(&mut state, &atlas);
            let face = &state.parts[0].entries[0].1.faces[0];
            assert_eq!(face.direction, Direction::Up, "{facing}");
            assert!(face.corners.iter().all(|corner| corner[1] == 1.0));
            assert_eq!(face.uv, expected, "{facing}");
        }
    }

    #[test]
    fn resource_backed_top_stair_uses_inverse_face_transform_for_every_facing() {
        let mut source = MemorySource::default();
        // Minimal independently authored fixture with the same relevant shape
        // as the 26.1.2 straight-stair resource: top-half states rotate a
        // source Down face through X=180 and a facing-specific Y transform.
        source.insert(
            "assets/minecraft/blockstates/test_stairs.json",
            r#"{"variants":{"facing=east,half=top,shape=straight":{"model":"block/test_stairs","x":180,"uvlock":true},"facing=south,half=top,shape=straight":{"model":"block/test_stairs","x":180,"y":90,"uvlock":true},"facing=west,half=top,shape=straight":{"model":"block/test_stairs","x":180,"y":180,"uvlock":true},"facing=north,half=top,shape=straight":{"model":"block/test_stairs","x":180,"y":270,"uvlock":true}}}"#,
        );
        source.insert(
            "assets/minecraft/models/block/test_stairs.json",
            r##"{"textures":{"top":"minecraft:block/test"},"elements":[{"from":[0,0,0],"to":[16,8,16],"faces":{"down":{"texture":"#top","uv":[1,2,13,14]}}}]}"##,
        );
        let atlas = pack_atlas(BTreeMap::from([(
            "minecraft:block/test".to_owned(),
            missing_texture(),
        )]))
        .unwrap();
        let mut loader = Loader::new(&mut source);
        let definition = loader
            .load_blockstate(&identifier("minecraft:test_stairs"))
            .unwrap();

        for (facing, expected) in [
            (
                "east",
                [
                    [1.0 / 16.0, 2.0 / 16.0],
                    [1.0 / 16.0, 14.0 / 16.0],
                    [13.0 / 16.0, 14.0 / 16.0],
                    [13.0 / 16.0, 2.0 / 16.0],
                ],
            ),
            (
                "south",
                [
                    [2.0 / 16.0, 1.0 / 16.0],
                    [2.0 / 16.0, 13.0 / 16.0],
                    [14.0 / 16.0, 13.0 / 16.0],
                    [14.0 / 16.0, 1.0 / 16.0],
                ],
            ),
            (
                "west",
                [
                    [3.0 / 16.0, 2.0 / 16.0],
                    [3.0 / 16.0, 14.0 / 16.0],
                    [15.0 / 16.0, 14.0 / 16.0],
                    [15.0 / 16.0, 2.0 / 16.0],
                ],
            ),
            (
                "north",
                [
                    [2.0 / 16.0, 3.0 / 16.0],
                    [2.0 / 16.0, 15.0 / 16.0],
                    [14.0 / 16.0, 15.0 / 16.0],
                    [14.0 / 16.0, 3.0 / 16.0],
                ],
            ),
        ] {
            let mut state = loader
                .resolve_state(
                    &definition,
                    &BTreeMap::from([
                        ("facing".to_owned(), facing.to_owned()),
                        ("half".to_owned(), "top".to_owned()),
                        ("shape".to_owned(), "straight".to_owned()),
                    ]),
                )
                .unwrap();
            prepare_runtime_state(&mut state, &atlas);
            let face = &state.parts[0].entries[0].1.faces[0];
            assert_eq!(face.direction, Direction::Up, "{facing}");
            assert_eq!(face.uv, expected, "{facing}");
        }
    }

    #[test]
    fn resource_backed_inner_stair_fluid_keeps_its_exposed_internal_sides() {
        use std::sync::Arc;

        use cubic_world::{
            Chunk, ChunkCoordinate, ChunkLightSummary, ChunkSection, DimensionGeometry,
            PalettedContainer, RuntimeBiomeId,
        };

        // Independently authored minimal resource fixture for the geometry and
        // exact property selection used by 26.1.2 runtime state 3981:
        // oak_stairs[facing=east,half=bottom,shape=inner_right,waterlogged=true].
        // The lower slab plus two upper arms leave only the upper north-west
        // quarter available to the contained source fluid.
        let mut source = MemorySource::default();
        source.insert(
            "assets/minecraft/blockstates/test_inner_stairs.json",
            r#"{"variants":{"facing=east,half=bottom,shape=inner_right,waterlogged=true":{"model":"block/test_inner_stairs"},"facing=west,half=bottom,shape=inner_right,waterlogged=true":{"model":"block/test_inner_stairs","y":180,"uvlock":true}}}"#,
        );
        source.insert(
            "assets/minecraft/models/block/test_inner_stairs.json",
            r##"{"textures":{"all":"block/test"},"elements":[{"from":[0,0,0],"to":[16,8,16],"faces":{"up":{"texture":"#all"}}},{"from":[8,8,0],"to":[16,16,16],"faces":{"west":{"texture":"#all"}}},{"from":[0,8,8],"to":[8,16,16],"faces":{"north":{"texture":"#all"}}}]}"##,
        );
        let mut loader = Loader::new(&mut source);
        let definition = loader
            .load_blockstate(&identifier("minecraft:test_inner_stairs"))
            .unwrap();
        let mut inner_stair = loader
            .resolve_state(
                &definition,
                &BTreeMap::from([
                    ("facing".to_owned(), "east".to_owned()),
                    ("half".to_owned(), "bottom".to_owned()),
                    ("shape".to_owned(), "inner_right".to_owned()),
                    ("waterlogged".to_owned(), "true".to_owned()),
                ]),
            )
            .unwrap();
        inner_stair.fluid = Some(FluidState {
            kind: FluidKind::Water,
            level: 0,
            falling: false,
        });
        let mut mirrored_stair = loader
            .resolve_state(
                &definition,
                &BTreeMap::from([
                    ("facing".to_owned(), "west".to_owned()),
                    ("half".to_owned(), "bottom".to_owned()),
                    ("shape".to_owned(), "inner_right".to_owned()),
                    ("waterlogged".to_owned(), "true".to_owned()),
                ]),
            )
            .unwrap();
        mirrored_stair.fluid = inner_stair.fluid;

        let mut resources = BlockResources::synthetic([RuntimeBlockStateId(0)]);
        prepare_runtime_state(&mut inner_stair, &resources.atlas);
        prepare_runtime_state(&mut mirrored_stair, &resources.atlas);
        let selected = &inner_stair.parts[0].entries[0].1;
        assert_eq!(
            selected.solid_boxes,
            vec![
                [[0.0, 0.0, 0.0], [1.0, 0.5, 1.0]],
                [[0.5, 0.5, 0.0], [1.0, 1.0, 1.0]],
                [[0.0, 0.5, 0.5], [0.5, 1.0, 1.0]],
            ]
        );
        resources.states.resize_with(3982, || None);
        resources.states[3981] = Some(inner_stair);
        resources.states[3980] = Some(mirrored_stair);
        resources.states[86] = Some(StateModels {
            fluid: Some(FluidState {
                kind: FluidKind::Water,
                level: 0,
                falling: false,
            }),
            ..StateModels::default()
        });

        let mut states = vec![RuntimeBlockStateId(0); 4096];
        let index = |x: usize, y: usize, z: usize| y * 256 + z * 16 + x;
        states[index(1, 1, 1)] = RuntimeBlockStateId(3981);
        // Match the live control: shared fluid suppresses the outer north and
        // west faces, while the clipped cavity still owns south/east internal
        // boundaries against the stair arms.
        states[index(1, 1, 0)] = RuntimeBlockStateId(86);
        states[index(0, 1, 1)] = RuntimeBlockStateId(86);
        states[index(4, 1, 4)] = RuntimeBlockStateId(3980);
        states[index(4, 1, 5)] = RuntimeBlockStateId(86);
        states[index(5, 1, 4)] = RuntimeBlockStateId(86);
        let coordinate = ChunkCoordinate::new(0, 0);
        let chunks = BTreeMap::from([(
            coordinate,
            Arc::new(Chunk {
                coordinate,
                sections: vec![ChunkSection {
                    non_empty_block_count: 6,
                    fluid_count: 6,
                    blocks: PalettedContainer::Direct { values: states },
                    biomes: PalettedContainer::Single {
                        value: RuntimeBiomeId(0),
                        entries: 64,
                    },
                }],
                heightmaps: Vec::new(),
                block_entities: Vec::new(),
                light: ChunkLightSummary::default(),
            }),
        )]);
        let mesh = crate::mesher::mesh_chunk(
            coordinate,
            &chunks,
            DimensionGeometry {
                min_y: 0,
                height: 16,
            },
            &resources,
        )
        .unwrap();
        let (vertex_quads, remainder) = mesh.vertices.as_chunks::<4>();
        assert!(remainder.is_empty());
        let fluid_quads = vertex_quads
            .iter()
            .filter(|quad| quad.iter().all(|vertex| vertex.layer & 0xff == 2))
            .collect::<Vec<_>>();
        let approximately = |left: f32, right: f32| (left - right).abs() < 1.0e-5;
        let south = fluid_quads.iter().find(|quad| {
            quad.iter()
                .all(|vertex| approximately(vertex.position[2], 1.5 - 0.001))
                && quad.iter().all(|vertex| vertex.position[0] <= 1.5 + 1.0e-6)
                && quad.iter().all(|vertex| vertex.position[1] >= 1.5 - 1.0e-6)
        });
        let east = fluid_quads.iter().find(|quad| {
            quad.iter()
                .all(|vertex| approximately(vertex.position[0], 1.5 - 0.001))
                && quad.iter().all(|vertex| vertex.position[2] <= 1.5 + 1.0e-6)
                && quad.iter().all(|vertex| vertex.position[1] >= 1.5 - 1.0e-6)
        });
        assert!(
            south.is_some(),
            "the retained fluid quarter needs its south wall"
        );
        assert!(
            east.is_some(),
            "the retained fluid quarter needs its east wall"
        );
        assert!(!fluid_quads.iter().any(|quad| {
            quad.iter()
                .all(|vertex| approximately(vertex.position[2], 2.0 - 0.001))
                && quad.iter().all(|vertex| vertex.position[0] <= 1.5 + 1.0e-6)
                && quad.iter().all(|vertex| vertex.position[1] >= 1.5 - 1.0e-6)
        }));
        assert!(!fluid_quads.iter().any(|quad| {
            quad.iter()
                .all(|vertex| approximately(vertex.position[0], 2.0 - 0.001))
                && quad.iter().all(|vertex| vertex.position[2] <= 1.5 + 1.0e-6)
                && quad.iter().all(|vertex| vertex.position[1] >= 1.5 - 1.0e-6)
        }));
        let mirrored_north = fluid_quads.iter().find(|quad| {
            quad.iter()
                .all(|vertex| approximately(vertex.position[2], 4.5 + 0.001))
                && quad.iter().all(|vertex| vertex.position[0] >= 4.5 - 1.0e-6)
                && quad.iter().all(|vertex| vertex.position[1] >= 1.5 - 1.0e-6)
        });
        let mirrored_west = fluid_quads.iter().find(|quad| {
            quad.iter()
                .all(|vertex| approximately(vertex.position[0], 4.5 + 0.001))
                && quad.iter().all(|vertex| vertex.position[2] >= 4.5 - 1.0e-6)
                && quad.iter().all(|vertex| vertex.position[1] >= 1.5 - 1.0e-6)
        });
        assert!(mirrored_north.is_some());
        assert!(mirrored_west.is_some());
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
        let opaque = DecodedImage::static_frame(2, 2, vec![255; 16], false);
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
        let oversized = DecodedImage::static_frame(MAX_ATLAS_SIDE, MAX_ATLAS_SIDE, vec![], false);
        assert!(
            pack_atlas(BTreeMap::from([
                ("cubic:missing".to_owned(), missing_texture()),
                ("minecraft:block/huge".to_owned(), oversized),
            ]))
            .is_err()
        );
    }

    #[test]
    fn animation_metadata_supports_default_and_explicit_bounded_sequences() {
        let default = decode_texture_metadata(
            Some(br#"{"animation":{"frametime":2}}"#),
            3,
            "minecraft:block/test",
            "assets/minecraft/textures/block/test.png.mcmeta",
        )
        .expect("default animation")
        .expect("animation section");
        assert_eq!(
            default.sequence,
            vec![
                AnimationStep { frame: 0, ticks: 2 },
                AnimationStep { frame: 1, ticks: 2 },
                AnimationStep { frame: 2, ticks: 2 },
            ]
        );
        assert!(!default.interpolate);

        let explicit = decode_texture_metadata(
            Some(br#"{"animation":{"frametime":4,"interpolate":true,"frames":[2,{"index":0,"time":7},1]}}"#),
            3,
            "minecraft:block/test",
            "assets/minecraft/textures/block/test.png.mcmeta",
        )
        .expect("explicit animation")
        .expect("animation section");
        assert_eq!(
            explicit.sequence,
            vec![
                AnimationStep { frame: 2, ticks: 4 },
                AnimationStep { frame: 0, ticks: 7 },
                AnimationStep { frame: 1, ticks: 4 },
            ]
        );
        assert!(explicit.interpolate);
    }

    #[test]
    fn texture_metadata_distinguishes_absent_unrelated_and_animation_sections() {
        let texture = "minecraft:block/test";
        let path = "assets/minecraft/textures/block/test.png.mcmeta";
        assert!(
            decode_texture_metadata(None, 2, texture, path)
                .expect("no metadata")
                .is_none()
        );
        assert!(
            decode_texture_metadata(
                Some(br#"{"texture":{"mipmap_strategy":"dark_cutout"}}"#),
                2,
                texture,
                path,
            )
            .expect("texture-only metadata")
            .is_none()
        );
        assert!(
            decode_texture_metadata(
                Some(br#"{"future_section":{"enabled":true}}"#),
                2,
                texture,
                path,
            )
            .expect("unknown unrelated metadata")
            .is_none()
        );

        let combined = decode_texture_metadata(
            Some(br#"{"animation":{"frametime":3},"texture":{"mipmap_strategy":"dark_cutout"}}"#),
            2,
            texture,
            path,
        )
        .expect("combined metadata")
        .expect("animation section");
        assert_eq!(combined.sequence[0], AnimationStep { frame: 0, ticks: 3 });
    }

    #[test]
    fn vanilla_dark_cutout_texture_metadata_is_static_not_malformed() {
        let metadata = br#"{
  "texture": {
    "mipmap_strategy": "dark_cutout"
  }
}"#;
        assert!(
            decode_texture_metadata(
                Some(metadata),
                1,
                "minecraft:block/acacia_leaves",
                "assets/minecraft/textures/block/acacia_leaves.png.mcmeta",
            )
            .expect("official texture-only metadata shape")
            .is_none()
        );
    }

    #[test]
    fn resource_texture_loading_accepts_missing_and_non_animation_metadata() {
        let texture_path = "assets/minecraft/textures/block/test.png";
        let metadata_path = "assets/minecraft/textures/block/test.png.mcmeta";
        let png = rgba_png(1, 1, &[10, 20, 30, 255]);

        let mut without_metadata = MemorySource::default();
        without_metadata.insert_bytes(texture_path, png.clone());
        let image = Loader::new(&mut without_metadata)
            .load_texture("minecraft:block/test")
            .expect("texture without metadata");
        assert!(image.animation.is_none());

        let mut texture_only = MemorySource::default();
        texture_only.insert_bytes(texture_path, png);
        texture_only.insert(
            metadata_path,
            r#"{"texture":{"mipmap_strategy":"dark_cutout"}}"#,
        );
        let image = Loader::new(&mut texture_only)
            .load_texture("minecraft:block/test")
            .expect("texture with unrelated metadata");
        assert!(image.animation.is_none());
    }

    #[test]
    fn exact_resource_adapter_classifies_opaque_cutout_and_translucent_materials() {
        assert_eq!(render_layer_26_1_2("stone"), RenderLayer::Opaque);
        assert_eq!(render_layer_26_1_2("short_grass"), RenderLayer::Cutout);
        assert_eq!(render_layer_26_1_2("glass"), RenderLayer::Translucent);
        assert_eq!(render_layer_26_1_2("water"), RenderLayer::Translucent);
        assert_eq!(
            render_layer_26_1_2("honey_block"),
            RenderLayer::LayeredTranslucent
        );
        assert_eq!(render_layer_26_1_2("scaffolding"), RenderLayer::Cutout);
        let empty = BTreeMap::new();
        assert_eq!(tint_kind_26_1_2("grass_block", &empty, 0), TintKind::Grass);
        assert_eq!(tint_kind_26_1_2("oak_leaves", &empty, 0), TintKind::Foliage);
        assert_eq!(tint_kind_26_1_2("water", &empty, 0), TintKind::Water);
        assert_eq!(
            tint_kind_26_1_2("leaf_litter", &empty, 0),
            TintKind::DryFoliage
        );
        let age = BTreeMap::from([("age".to_owned(), "7".to_owned())]);
        assert_eq!(
            tint_kind_26_1_2("melon_stem", &age, 0),
            TintKind::Fixed(0xe0c71c)
        );
        assert_eq!(
            tint_kind_26_1_2("attached_pumpkin_stem", &empty, 0),
            TintKind::Fixed(0xe0c71c)
        );
        assert_eq!(render_layer_26_1_2("seagrass"), RenderLayer::Cutout);
    }

    #[test]
    fn honey_model_faces_keep_the_exact_version_layered_translucent_policy() {
        let mut models = fallback_state();
        apply_state_semantics(
            &mut models,
            "minecraft:honey_block",
            &BTreeMap::new(),
            cubic_world::BlockEnvironment::default(),
            &CollisionShape::FullCube,
        );
        let atlas = pack_atlas(BTreeMap::from([(
            "cubic:missing".to_owned(),
            missing_texture(),
        )]))
        .unwrap();
        prepare_runtime_state(&mut models, &atlas);
        assert!(models.parts.iter().all(|part| {
            part.entries.iter().all(|(_, model)| {
                model
                    .faces
                    .iter()
                    .all(|face| face.render_layer == RenderLayer::LayeredTranslucent)
            })
        }));
        assert!(!models.full_opaque_cube);
    }

    #[test]
    fn fluid_surface_solid_projection_uses_collision_bounds_not_visual_opacity() {
        assert!(legacy_solid_shape(&CollisionShape::FullCube));
        assert!(!legacy_solid_shape(&CollisionShape::Empty));
        assert!(legacy_solid_shape(&CollisionShape::Boxes(
            std::sync::Arc::from([cubic_world::Aabb::new(
                cubic_world::Vec3d::new(0.0, 0.0, 0.0),
                cubic_world::Vec3d::new(1.0, 0.5, 1.0),
            )])
        )));
        assert!(!legacy_solid_shape(&CollisionShape::Boxes(
            std::sync::Arc::from([cubic_world::Aabb::new(
                cubic_world::Vec3d::new(0.0, 0.0, 0.0),
                cubic_world::Vec3d::new(1.0, 1.0 / 16.0, 1.0),
            )])
        )));
    }

    #[test]
    fn malformed_animation_metadata_is_rejected_before_atlas_use() {
        let texture = "minecraft:block/test";
        let path = "assets/minecraft/textures/block/test.png.mcmeta";
        for metadata in [
            br#"{"animation":{"frametime":0}}"#.as_slice(),
            br#"{"animation":{"frames":[3]}}"#.as_slice(),
            br#"{"animation":{"frames":[]}}"#.as_slice(),
            br#"{"animation":"invalid"}"#.as_slice(),
        ] {
            let error = decode_texture_metadata(Some(metadata), 2, texture, path)
                .expect_err("malformed animation must fail");
            let message = error.to_string();
            assert!(message.contains(texture));
            assert!(message.contains(path));
            assert!(message.contains("animation"));
        }
    }
}
