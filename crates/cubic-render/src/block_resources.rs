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

use crate::model_part::{
    CubeDefinition, HALF_PI, ModelPartDefinition, ModelPartQuad, PartDirection, PartPose,
};

const MAX_JSON_BYTES: u64 = 1024 * 1024;
const MAX_MODEL_DEPTH: usize = 32;
const MAX_ELEMENTS: usize = 256;
const MAX_FLUID_OCCLUSION_BOXES: usize = 32;
const MAX_ATLAS_SIDE: u32 = 4096;
const ATLAS_GUTTER: u32 = 1;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeprecatedTranslations {
    #[serde(default)]
    removed: Vec<String>,
    #[serde(default)]
    renamed: BTreeMap<String, String>,
}

fn apply_deprecated_translations(
    translations: &mut BTreeMap<String, String>,
    deprecated: DeprecatedTranslations,
) {
    for key in deprecated.removed {
        translations.remove(&key);
    }
    for (from, to) in deprecated.renamed {
        if let Some(value) = translations.remove(&from) {
            translations.insert(to, value);
        } else {
            // This matches DeprecatedTranslationsInfo: a stale destination
            // must not survive when its authoritative legacy source is absent.
            translations.remove(&to);
        }
    }
}

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
    pub material: TextureMaterial,
    pub cullface: Option<Direction>,
    pub tint_index: Option<u32>,
    pub tint_kind: TintKind,
    pub render_layer: RenderLayer,
    pub directional_shade: bool,
    pub shade: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TextureMaterial {
    Terrain,
    BannerPattern,
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
    pub(crate) fn exact_region(&self, texture: &str) -> Option<AtlasRegion> {
        self.regions.get(texture).copied()
    }

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
    pub(crate) entity_dimensions: Option<cubic_version::EntityData>,
    states: Vec<Option<StateModels>>,
    fallback: StateModels,
    pub atlas: TextureAtlasData,
    /// Separate material source matching `minecraft:textures/atlas/banner_patterns.png`.
    /// World banner quads use this atlas, never a terrain missing-texture fallback.
    pub(crate) banner_atlas: TextureAtlasData,
    pub blockstate_count: usize,
    pub model_count: usize,
    pub texture_count: usize,
    pub fallback_count: usize,
    pub crosshair: GuiSpriteData,
    pub destroy_stages: Vec<GuiSpriteData>,
    /// Exact-version inventory/HUD images loaded from the verified runtime
    /// resource source. Keys are stable resource paths consumed by the
    /// platform/UI boundary; pixels are never embedded in Cubic.
    pub inventory_sprites: BTreeMap<String, GuiSpriteData>,
    /// Bounded English GUI strings from the selected runtime resource set.
    pub inventory_translations: BTreeMap<String, String>,
    /// Bounded item previews loaded from the verified exact-version resource
    /// set. These are presentation data only; stable item identity remains in
    /// `cubic-world` and numeric IDs remain in the protocol profile.
    /// Physical-resolution GUI icons grouped by Minecraft GUI scale. Each
    /// sprite still occupies 16 logical GUI units; no 16-pixel intermediate
    /// is enlarged by the presentation layer.
    pub item_icons_by_scale: BTreeMap<u32, BTreeMap<String, GuiSpriteData>>,
    /// Read-only exact-version coverage result for the inventory item-model
    /// graph. This makes resource regressions visible without retaining any
    /// Mojang source asset bytes beyond the already prepared sprites.
    pub item_icon_audit: ItemIconAudit,
    /// Bounded component payloads whose registry-aware holder references could
    /// not yet be resolved. Static model/resource loading remains usable; a
    /// later registry-aware rebuild can resolve these without losing bytes.
    deferred_banner_patterns: BTreeMap<String, DeferredBannerPatterns>,
    pub(crate) grass_colormap: Vec<u32>,
    pub(crate) foliage_colormap: Vec<u32>,
    pub(crate) dry_foliage_colormap: Vec<u32>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ItemIconAudit {
    pub registered: usize,
    pub resolved: usize,
    /// Items that must be represented by the UI's identifier-text fallback.
    pub fallback: usize,
    /// Resolved non-empty icons whose dynamic state is intentionally reduced
    /// to an idle/default resource-backed state.
    pub static_simplification: usize,
    pub missing_resource: usize,
    pub unsupported_model: usize,
    /// Exact selected renderer class counts (`baked`, `generated`, or a
    /// `special:minecraft:*` family) for the canonical idle GUI state.
    pub renderer_classes: BTreeMap<String, usize>,
}

impl BlockResources {
    #[must_use]
    pub fn has_deferred_banner_patterns(&self) -> bool {
        !self.deferred_banner_patterns.is_empty()
    }

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
                if let Some(special_parts) =
                    special_world_parts_26_1_2(block.identifier.as_str(), &state.properties)?
                {
                    // Vanilla omits these dynamic/block-entity models from
                    // the ordinary terrain model. Replacing only the visual
                    // parts prevents a fallback cube from being emitted while
                    // retaining exact-version fluid/emissive semantics.
                    models.parts = special_parts;
                    models.full_opaque_cube = false;
                }
                states.insert(RuntimeBlockStateId(state.state_id), models);
            }
        }
        let atlas = loader.build_atlas(&states, &BTreeSet::new())?;
        let banner_pattern_assets = data
            .registry(&parse_identifier("minecraft:banner_pattern")?)
            .map(|registry| {
                registry
                    .entries
                    .iter()
                    .map(|entry| entry.identifier.clone())
                    .collect::<Vec<_>>()
            })
            .or_else(|| {
                (data.artifact().minecraft_version.as_str() == "26.1.2")
                    .then(|| cubic_version::CreativeData::builtin_26_1_2().ok())
                    .flatten()
                    .map(|creative| creative.banner_patterns)
            })
            .unwrap_or_default();
        let banner_atlas = loader.build_banner_atlas(&banner_pattern_assets)?;
        let grass_colormap = loader.load_colormap("minecraft:colormap/grass")?;
        let foliage_colormap = loader.load_colormap("minecraft:colormap/foliage")?;
        let dry_foliage_colormap = loader.load_colormap("minecraft:colormap/dry_foliage")?;
        let crosshair = loader.load_gui_sprite("minecraft:gui/sprites/hud/crosshair")?;
        let destroy_stages = (0..cubic_world::DESTROY_STAGE_COUNT)
            .map(|stage| loader.load_gui_sprite(&format!("minecraft:block/destroy_stage_{stage}")))
            .collect::<Result<Vec<_>, _>>()?;
        let inventory_sprites = [
            "gui/container/inventory",
            "gui/container/generic_54",
            "gui/container/furnace",
            "gui/container/crafting_table",
            "gui/sprites/hud/hotbar",
            "gui/sprites/hud/hotbar_selection",
            "gui/sprites/container/furnace/burn_progress",
            "gui/sprites/container/furnace/lit_progress",
            "gui/sprites/container/slot/helmet",
            "gui/sprites/container/slot/chestplate",
            "gui/sprites/container/slot/leggings",
            "gui/sprites/container/slot/boots",
            "gui/sprites/container/slot/shield",
            "gui/sprites/container/slot_highlight_back",
            "gui/sprites/container/slot_highlight_front",
            "gui/sprites/tooltip/background",
            "gui/sprites/tooltip/frame",
            "gui/container/creative_inventory/tab_items",
            "gui/container/creative_inventory/tab_item_search",
            "gui/container/creative_inventory/tab_inventory",
            "gui/sprites/container/creative_inventory/scroller",
            "gui/sprites/container/creative_inventory/scroller_disabled",
            "gui/sprites/container/creative_inventory/tab_top_selected_1",
            "gui/sprites/container/creative_inventory/tab_top_selected_2",
            "gui/sprites/container/creative_inventory/tab_top_selected_3",
            "gui/sprites/container/creative_inventory/tab_top_selected_4",
            "gui/sprites/container/creative_inventory/tab_top_selected_5",
            "gui/sprites/container/creative_inventory/tab_top_selected_6",
            "gui/sprites/container/creative_inventory/tab_top_selected_7",
            "gui/sprites/container/creative_inventory/tab_top_unselected_1",
            "gui/sprites/container/creative_inventory/tab_top_unselected_2",
            "gui/sprites/container/creative_inventory/tab_top_unselected_3",
            "gui/sprites/container/creative_inventory/tab_top_unselected_4",
            "gui/sprites/container/creative_inventory/tab_top_unselected_5",
            "gui/sprites/container/creative_inventory/tab_top_unselected_6",
            "gui/sprites/container/creative_inventory/tab_top_unselected_7",
            "gui/sprites/container/creative_inventory/tab_bottom_selected_1",
            "gui/sprites/container/creative_inventory/tab_bottom_selected_2",
            "gui/sprites/container/creative_inventory/tab_bottom_selected_3",
            "gui/sprites/container/creative_inventory/tab_bottom_selected_4",
            "gui/sprites/container/creative_inventory/tab_bottom_selected_5",
            "gui/sprites/container/creative_inventory/tab_bottom_selected_6",
            "gui/sprites/container/creative_inventory/tab_bottom_selected_7",
            "gui/sprites/container/creative_inventory/tab_bottom_unselected_1",
            "gui/sprites/container/creative_inventory/tab_bottom_unselected_2",
            "gui/sprites/container/creative_inventory/tab_bottom_unselected_3",
            "gui/sprites/container/creative_inventory/tab_bottom_unselected_4",
            "gui/sprites/container/creative_inventory/tab_bottom_unselected_5",
            "gui/sprites/container/creative_inventory/tab_bottom_unselected_6",
            "gui/sprites/container/creative_inventory/tab_bottom_unselected_7",
            "font/ascii",
        ]
        .into_iter()
        .map(|name| {
            loader
                .load_gui_sprite(&format!("minecraft:{name}"))
                .map(|sprite| (name.to_owned(), sprite))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
        let inventory_translations = loader.load_inventory_translations()?;
        let mut item_icons_by_scale = (1_u32..=4)
            .map(|scale| (scale, BTreeMap::new()))
            .collect::<BTreeMap<_, _>>();
        let mut item_icon_audit = ItemIconAudit::default();
        if let Some(items) = data.registry(&parse_identifier("minecraft:item")?) {
            item_icon_audit.registered = items.entries.len();
            for entry in &items.entries {
                let fallback_events_before = loader.failures.values().sum::<usize>();
                match loader.load_item_icon(&entry.identifier, 1) {
                    Ok(icon)
                        if icon
                            .rgba
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .any(|pixel| pixel[3] != 0) =>
                    {
                        item_icon_audit.resolved += 1;
                        if loader.failures.values().sum::<usize>() > fallback_events_before {
                            item_icon_audit.static_simplification += 1;
                        }
                        item_icons_by_scale
                            .entry(1)
                            .or_default()
                            .insert(entry.identifier.to_string(), icon);
                        for scale in 2_u32..=4 {
                            let scaled = loader.load_item_icon(&entry.identifier, scale)?;
                            item_icons_by_scale
                                .entry(scale)
                                .or_default()
                                .insert(entry.identifier.to_string(), scaled);
                        }
                    }
                    Ok(_) => {
                        item_icon_audit.fallback += 1;
                        item_icon_audit.unsupported_model += 1;
                        loader.record_fallback("item model produced a fully transparent GUI icon");
                    }
                    Err(error) => {
                        item_icon_audit.fallback += 1;
                        if error.to_string().contains("resource is missing") {
                            item_icon_audit.missing_resource += 1;
                        } else {
                            item_icon_audit.unsupported_model += 1;
                        }
                        loader.record_failure(&error);
                    }
                }
            }
        }
        // Current Creative data contains component-distinct stacks whose item
        // model graph selects from `minecraft:block_state`. Pre-bake those
        // finite canonical variants under the same semantic key consumed by
        // the UI; ordinary stacks continue sharing their registered icon.
        let mut deferred_banner_patterns = BTreeMap::new();
        if data.artifact().minecraft_version.as_str() == "26.1.2"
            && let Ok(creative) = cubic_version::CreativeData::builtin_26_1_2()
        {
            let creative_stacks = creative
                .tabs(true)
                .iter()
                .flat_map(|tab| std::iter::once(&tab.icon).chain(&tab.items))
                .collect::<Vec<_>>();
            let banner_patterns = data
                .registry(&parse_identifier("minecraft:banner_pattern")?)
                .map(BannerPatternRegistry::GameData)
                .or_else(|| {
                    (!creative.banner_patterns.is_empty())
                        .then_some(BannerPatternRegistry::Ordered(&creative.banner_patterns))
                });
            let mut variants = BTreeMap::<
                String,
                (
                    MinecraftIdentifier,
                    BTreeMap<String, String>,
                    Vec<BannerPatternLayer>,
                ),
            >::new();
            for stack in creative_stacks {
                let block_component = stack
                    .components
                    .iter()
                    .find(|component| component.id.as_str() == "minecraft:block_state");
                let banner_component = stack
                    .components
                    .iter()
                    .find(|component| component.id.as_str() == "minecraft:banner_patterns");
                if block_component.is_none() && banner_component.is_none() {
                    continue;
                }
                let explicit = block_component
                    .and_then(|component| component.block_state_properties().ok().flatten());
                let properties = explicit.clone().unwrap_or_else(|| {
                    data.artifact()
                        .blocks
                        .iter()
                        .find(|block| block.identifier == stack.item)
                        .and_then(|block| block.state(block.default_state_id))
                        .map(|state| state.properties.clone())
                        .unwrap_or_default()
                });
                let mut key = explicit.as_ref().map_or_else(
                    || stack.effective_item_model.to_string(),
                    |_| gui_item_render_key(&stack.effective_item_model, &properties),
                );
                let layers = banner_component
                    .and_then(|component| component.decoded_value().ok().flatten())
                    .map(|bytes| {
                        append_component_key(&mut key, "banner_patterns", &bytes);
                        decode_banner_pattern_layers(&bytes, banner_patterns)
                    })
                    .transpose()?
                    .map_or_else(Vec::new, |state| match state {
                        BannerPatternLayers::Resolved(layers) => layers,
                        BannerPatternLayers::Deferred(component) => {
                            deferred_banner_patterns.insert(key.clone(), component);
                            Vec::new()
                        }
                    });
                variants
                    .entry(key)
                    .or_insert_with(|| (stack.effective_item_model.clone(), properties, layers));
            }
            for (key, (model, properties, banner_layers)) in variants {
                for scale in 1_u32..=4 {
                    let icon = loader.load_item_icon_with_context(
                        &model,
                        scale,
                        &properties,
                        &banner_layers,
                    )?;
                    item_icons_by_scale
                        .entry(scale)
                        .or_default()
                        .insert(key.clone(), icon);
                }
            }
        }
        item_icon_audit.renderer_classes = loader.item_renderer_classes.clone();
        for models in states.values_mut() {
            prepare_runtime_state(models, &atlas);
            bind_banner_base_material(models, &banner_atlas)?;
        }
        for (reason, count) in loader.failures.iter().take(16) {
            tracing::warn!(count, %reason, "vanilla runtime resource used bounded fallback");
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
            entity_dimensions: match cubic_version::entity_data_for(
                &data.artifact().minecraft_version,
            ) {
                Ok(dimensions) => dimensions,
                Err(error) => {
                    tracing::warn!(%error, "exact-version entity dimensions unavailable; debug boxes use bounded fallback");
                    None
                }
            },
            states: indexed_states,
            fallback,
            atlas,
            banner_atlas,
            blockstate_count: loader.blockstates_loaded,
            model_count: loader.models.len(),
            texture_count,
            fallback_count,
            crosshair,
            destroy_stages,
            inventory_sprites,
            inventory_translations,
            item_icons_by_scale,
            item_icon_audit,
            deferred_banner_patterns,
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

    pub(crate) fn banner_pattern_faces(
        &self,
        model: &ModelApplication,
        layers: &[cubic_world::BannerPatternLayer],
    ) -> Result<Vec<ModelFace>, String> {
        let semantic = layers
            .iter()
            .take(16)
            .filter_map(|layer| {
                dye_color(u32::from(layer.dye_raw_id)).map(|tint| BannerPatternLayer {
                    texture: banner_pattern_texture(&layer.pattern.asset_id),
                    tint,
                })
            })
            .collect::<Vec<_>>();
        composed_banner_pattern_faces(&model.faces, &semantic, 1)
            .into_iter()
            .map(|(mut face, tint)| {
                face.atlas_region = self
                    .banner_atlas
                    .exact_region(&face.texture)
                    .ok_or_else(|| face.texture.clone())?;
                face.material = TextureMaterial::BannerPattern;
                face.tint_kind = TintKind::Fixed(tint);
                face.tint_index = None;
                Ok(face)
            })
            .collect()
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
        let banner_atlas = atlas.clone();
        Self {
            entity_dimensions: None,
            states,
            fallback: fallback_state(),
            atlas,
            banner_atlas,
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
            inventory_sprites: BTreeMap::new(),
            inventory_translations: BTreeMap::new(),
            item_icons_by_scale: BTreeMap::new(),
            item_icon_audit: ItemIconAudit::default(),
            deferred_banner_patterns: BTreeMap::new(),
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

fn bind_banner_base_material(
    models: &mut StateModels,
    banner_atlas: &TextureAtlasData,
) -> Result<(), BlockResourceError> {
    for part in &mut models.parts {
        for (_, model) in &mut part.entries {
            for face in &mut model.faces {
                if face.texture == "minecraft:entity/banner/banner_base"
                    && face.tint_index == Some(0)
                {
                    face.atlas_region =
                        banner_atlas.exact_region(&face.texture).ok_or_else(|| {
                            malformed(
                                "banner pattern atlas",
                                &face.texture,
                                "base sprite is missing",
                            )
                        })?;
                    face.material = TextureMaterial::BannerPattern;
                }
            }
        }
    }
    Ok(())
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
    #[serde(default)]
    display: BTreeMap<String, DisplayTransformWire>,
    gui_light: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
struct DisplayTransformWire {
    #[serde(default)]
    rotation: [f32; 3],
    #[serde(default)]
    translation: [f32; 3],
    #[serde(default = "unit_scale")]
    scale: [f32; 3],
}

const fn unit_scale() -> [f32; 3] {
    [1.0; 3]
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
    gui_transform: Option<DisplayTransformWire>,
    gui_light_side: bool,
}

struct Loader<'a, S> {
    source: &'a mut S,
    models: BTreeMap<MinecraftIdentifier, ResolvedModel>,
    blockstates_loaded: usize,
    failures: BTreeMap<String, usize>,
    item_renderer_classes: BTreeMap<String, usize>,
}

impl<'a, S: VanillaResourceSource> Loader<'a, S> {
    fn new(source: &'a mut S) -> Self {
        Self {
            source,
            models: BTreeMap::new(),
            blockstates_loaded: 0,
            failures: BTreeMap::new(),
            item_renderer_classes: BTreeMap::new(),
        }
    }

    fn record_failure(&mut self, error: &BlockResourceError) {
        *self.failures.entry(error.to_string()).or_default() += 1;
    }

    fn record_fallback(&mut self, reason: &str) {
        *self.failures.entry(reason.to_owned()).or_default() += 1;
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
        // These are model-bakery roots, not JSON resources. Vanilla's
        // item/generated model inherits from builtin/generated, so treating
        // the identifier as a missing file makes almost every flat inventory
        // item fail before its inherited layer textures can be rendered.
        if matches!(
            identifier.as_str(),
            "minecraft:builtin/generated" | "minecraft:builtin/entity"
        ) {
            let model = ResolvedModel {
                textures: BTreeMap::new(),
                elements: Vec::new(),
                ambient_occlusion: true,
                gui_transform: None,
                gui_light_side: identifier.as_str() == "minecraft:builtin/entity",
            };
            self.models.insert(identifier.clone(), model.clone());
            return Ok(model);
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
                gui_transform: None,
                gui_light_side: true,
            }
        };
        if let Some(ambient_occlusion) = wire.ambientocclusion {
            resolved.ambient_occlusion = ambient_occlusion;
        }
        if let Some(transform) = wire.display.get("gui") {
            resolved.gui_transform = Some(*transform);
        }
        if let Some(light) = wire.gui_light.as_deref() {
            resolved.gui_light_side = light != "front";
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
        additional_textures: &BTreeSet<String>,
    ) -> Result<TextureAtlasData, BlockResourceError> {
        let mut names = BTreeSet::from(["cubic:missing".to_owned()]);
        names.extend(additional_textures.iter().cloned());
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

    fn build_banner_atlas(
        &mut self,
        assets: &[MinecraftIdentifier],
    ) -> Result<TextureAtlasData, BlockResourceError> {
        if assets.is_empty() {
            return pack_atlas(BTreeMap::from([(
                "cubic:missing".to_owned(),
                missing_texture(),
            )]));
        }
        let mut names = BTreeSet::from(["minecraft:entity/banner/banner_base".to_owned()]);
        names.extend(assets.iter().map(banner_pattern_texture));
        let mut images = BTreeMap::new();
        for name in names {
            let identifier = parse_identifier(&name)?;
            let path = resource_path(&identifier, "textures", "png")?;
            let bytes = self
                .source
                .read_resource(&path, MAX_VANILLA_RESOURCE_BYTES)?
                .ok_or_else(|| malformed("banner pattern sprite", &name, "resource is missing"))?;
            images.insert(name.clone(), decode_png(&bytes)?);
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
        decode_gui_png(&bytes)
    }

    fn load_inventory_translations(
        &mut self,
    ) -> Result<BTreeMap<String, String>, BlockResourceError> {
        let path = VanillaResourcePath::new("assets/minecraft/lang/en_us.json".to_owned())?;
        let bytes = self
            .source
            .read_resource(&path, MAX_JSON_BYTES)?
            .ok_or_else(|| malformed("language", "minecraft:en_us", "resource is missing"))?;
        let mut all: BTreeMap<String, String> = serde_json::from_slice(&bytes)
            .map_err(|error| malformed("language", "minecraft:en_us", error.to_string()))?;
        let deprecated_path =
            VanillaResourcePath::new("assets/minecraft/lang/deprecated.json".to_owned())?;
        if let Some(bytes) = self
            .source
            .read_resource(&deprecated_path, MAX_JSON_BYTES)?
        {
            let deprecated: DeprecatedTranslations =
                serde_json::from_slice(&bytes).map_err(|error| {
                    malformed("language", "minecraft:deprecated", error.to_string())
                })?;
            apply_deprecated_translations(&mut all, deprecated);
        }
        let wanted = [
            "container.inventory",
            "container.crafting",
            "container.chest",
            "container.chestDouble",
            "container.furnace",
        ];
        let mut selected = wanted
            .into_iter()
            .filter_map(|key| {
                all.get(key)
                    .filter(|value| value.len() <= 256)
                    .map(|value| (key.to_owned(), value.clone()))
            })
            .collect::<BTreeMap<_, _>>();
        selected.extend(all.into_iter().filter(|(key, value)| {
            (key.starts_with("itemGroup.") || key.starts_with("item.") || key.starts_with("block."))
                && value.len() <= 256
        }));
        Ok(selected)
    }

    fn load_item_icon(
        &mut self,
        item: &MinecraftIdentifier,
        gui_scale: u32,
    ) -> Result<GuiSpriteData, BlockResourceError> {
        self.load_item_icon_with_block_state(item, gui_scale, &BTreeMap::new())
    }

    fn load_item_icon_with_block_state(
        &mut self,
        item: &MinecraftIdentifier,
        gui_scale: u32,
        block_state: &BTreeMap<String, String>,
    ) -> Result<GuiSpriteData, BlockResourceError> {
        self.load_item_icon_with_context(item, gui_scale, block_state, &[])
    }

    fn load_item_icon_with_context(
        &mut self,
        item: &MinecraftIdentifier,
        gui_scale: u32,
        block_state: &BTreeMap<String, String>,
        banner_layers: &[BannerPatternLayer],
    ) -> Result<GuiSpriteData, BlockResourceError> {
        let path = resource_path(item, "items", "json")?;
        let bytes = self
            .source
            .read_resource(&path, MAX_JSON_BYTES)?
            .ok_or_else(|| malformed("item definition", item.as_str(), "resource is missing"))?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| malformed("item definition", item.as_str(), error.to_string()))?;
        let root = value
            .get("model")
            .ok_or_else(|| malformed("item definition", item.as_str(), "model is missing"))?;
        self.validate_item_model_graph(root, 0)?;
        let mut models = Vec::new();
        collect_gui_item_models_with_block_state(root, 0, block_state, &mut models)?;
        if models.is_empty() {
            return Err(malformed(
                "item definition",
                item.as_str(),
                "GUI model graph produced no renderable model",
            ));
        }
        let icon_size = 16_u32
            .checked_mul(gui_scale)
            .filter(|size| (16..=64).contains(size))
            .ok_or_else(|| malformed("item icon", item.as_str(), "GUI scale is out of bounds"))?;
        let mut rgba = vec![0_u8; icon_size as usize * icon_size as usize * 4];
        let mut item_classes = BTreeSet::new();
        for model in models {
            if let Some(kind) = model.fallback_kind {
                self.record_fallback(kind);
            }
            let model_id = parse_identifier(&model.model)?;
            let resolved = self.resolve_model(&model_id, &mut Vec::new())?;
            if gui_scale == 1 {
                let class = model.special.as_ref().map_or_else(
                    || {
                        if resolved.elements.is_empty() {
                            "generated".to_owned()
                        } else {
                            "baked".to_owned()
                        }
                    },
                    |special| format!("special:{}", special.family),
                );
                item_classes.insert(class);
            }
            tracing::trace!(
                item = item.as_str(),
                item_model = model.model,
                baked_model = model_id.as_str(),
                gui_transform = ?resolved.gui_transform,
                gui_light_side = resolved.gui_light_side,
                quad_count = resolved.elements.iter().map(|element| element.faces.len()).sum::<usize>(),
                textures = ?resolved.textures.values().collect::<BTreeSet<_>>(),
                "resolved canonical GUI item model"
            );
            let layer = if let Some(special) = &model.special {
                self.render_special_item_model(
                    &resolved,
                    special,
                    model.local_transform,
                    icon_size,
                    banner_layers,
                )?
            } else {
                self.render_item_model(&resolved, &model.tints, model.local_transform, icon_size)?
            };
            composite_rgba(&mut rgba, &layer.rgba);
        }
        for class in item_classes {
            *self.item_renderer_classes.entry(class).or_default() += 1;
        }
        Ok(GuiSpriteData {
            width: icon_size,
            height: icon_size,
            rgba,
        })
    }

    fn validate_item_model_graph(
        &mut self,
        value: &Value,
        depth: usize,
    ) -> Result<(), BlockResourceError> {
        if depth >= MAX_MODEL_DEPTH {
            return Err(malformed(
                "item model",
                "graph",
                "model graph depth exceeded",
            ));
        }
        let object = value
            .as_object()
            .ok_or_else(|| malformed("item model", "graph", "node is not an object"))?;
        let kind = object
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| malformed("item model", "graph", "node type is missing"))?;
        let mut validate_child = |child: &Value| self.validate_item_model_graph(child, depth + 1);
        match kind {
            "minecraft:model" => {
                let model = object
                    .get("model")
                    .and_then(Value::as_str)
                    .ok_or_else(|| malformed("item model", kind, "model identity is missing"))?;
                self.resolve_model(&parse_identifier(model)?, &mut Vec::new())?;
            }
            "minecraft:composite" => {
                let children = object
                    .get("models")
                    .and_then(Value::as_array)
                    .ok_or_else(|| malformed("item model", kind, "models are missing"))?;
                for child in children {
                    validate_child(child)?;
                }
            }
            "minecraft:condition" => {
                validate_child(
                    object
                        .get("on_true")
                        .ok_or_else(|| malformed("item model", kind, "true branch is missing"))?,
                )?;
                validate_child(
                    object
                        .get("on_false")
                        .ok_or_else(|| malformed("item model", kind, "false branch is missing"))?,
                )?;
            }
            "minecraft:select" => {
                let cases = object
                    .get("cases")
                    .and_then(Value::as_array)
                    .ok_or_else(|| malformed("item model", kind, "cases are missing"))?;
                for case in cases {
                    validate_child(
                        case.get("model").ok_or_else(|| {
                            malformed("item model", kind, "case model is missing")
                        })?,
                    )?;
                }
                if let Some(fallback) = object.get("fallback") {
                    validate_child(fallback)?;
                }
            }
            "minecraft:range_dispatch" => {
                let entries = object
                    .get("entries")
                    .and_then(Value::as_array)
                    .ok_or_else(|| malformed("item model", kind, "entries are missing"))?;
                for entry in entries {
                    validate_child(
                        entry.get("model").ok_or_else(|| {
                            malformed("item model", kind, "entry model is missing")
                        })?,
                    )?;
                }
                if let Some(fallback) = object.get("fallback") {
                    validate_child(fallback)?;
                }
            }
            "minecraft:special" => {
                let base = object
                    .get("base")
                    .and_then(Value::as_str)
                    .ok_or_else(|| malformed("item model", kind, "special base is missing"))?;
                self.resolve_model(&parse_identifier(base)?, &mut Vec::new())?;
                let renderer = object
                    .get("model")
                    .and_then(|model| model.get("type"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| malformed("item model", kind, "special renderer is missing"))?;
                if !matches!(
                    renderer,
                    "minecraft:banner"
                        | "minecraft:bed"
                        | "minecraft:chest"
                        | "minecraft:conduit"
                        | "minecraft:copper_golem_statue"
                        | "minecraft:decorated_pot"
                        | "minecraft:head"
                        | "minecraft:player_head"
                        | "minecraft:shield"
                        | "minecraft:shulker_box"
                        | "minecraft:trident"
                ) {
                    return Err(malformed(
                        "item model",
                        renderer,
                        "unsupported special renderer type",
                    ));
                }
            }
            "minecraft:empty" | "minecraft:bundle/selected_item" => {}
            _ => {
                return Err(malformed("item model", kind, "unsupported GUI model form"));
            }
        }
        Ok(())
    }

    fn render_item_model(
        &mut self,
        model: &ResolvedModel,
        tints: &[u32],
        local_transform: ItemModelTransform,
        icon_size: u32,
    ) -> Result<GuiSpriteData, BlockResourceError> {
        let mut layers = model
            .textures
            .iter()
            .filter_map(|(name, value)| {
                name.strip_prefix("layer")
                    .and_then(|suffix| suffix.parse::<usize>().ok())
                    .map(|index| (index, value))
            })
            .collect::<Vec<_>>();
        layers.sort_by_key(|(index, _)| *index);
        if model.elements.is_empty() && !layers.is_empty() {
            let mut rgba = vec![0_u8; icon_size as usize * icon_size as usize * 4];
            for (index, texture) in layers {
                let texture = resolve_texture(texture, &model.textures)?;
                let image = self.load_texture(&texture)?;
                let frame = image
                    .frames
                    .first()
                    .ok_or_else(|| malformed("item texture", &texture, "texture has no frame"))?;
                let sampled = sample_icon(
                    frame,
                    image.width,
                    image.height,
                    icon_size,
                    tints.get(index).copied(),
                );
                composite_rgba(&mut rgba, &sampled);
            }
            return Ok(GuiSpriteData {
                width: icon_size,
                height: icon_size,
                rgba,
            });
        }
        // Special item models are rendered by vanilla's typed special-model
        // renderers. Until their dynamic geometry is needed, preserve a
        // bounded, resource-derived icon from the base model's particle
        // texture instead of falling through to identifier text.
        if model.elements.is_empty()
            && let Some(texture) = model.textures.get("particle")
        {
            let texture = resolve_texture(texture, &model.textures)?;
            let image = self.load_texture(&texture)?;
            let frame = image
                .frames
                .first()
                .ok_or_else(|| malformed("item texture", &texture, "texture has no frame"))?;
            return Ok(GuiSpriteData {
                width: icon_size,
                height: icon_size,
                rgba: sample_icon(frame, image.width, image.height, icon_size, None),
            });
        }
        rasterize_gui_model_transformed(self, model, tints, local_transform, icon_size)
    }

    fn render_special_item_model(
        &mut self,
        base: &ResolvedModel,
        special: &SpecialItemModel,
        local_transform: ItemModelTransform,
        icon_size: u32,
        banner_layers: &[BannerPatternLayer],
    ) -> Result<GuiSpriteData, BlockResourceError> {
        let mut faces = special.model_faces()?;
        let mut tints = special.tint().into_iter().collect::<Vec<_>>();
        if special.family == "minecraft:banner" {
            let composed = composed_banner_pattern_faces(&faces, banner_layers, tints.len());
            tints.extend(banner_layers.iter().take(16).map(|layer| layer.tint));
            faces.extend(composed.into_iter().map(|(face, _)| face));
        }
        rasterize_gui_faces(
            self,
            &faces,
            &tints,
            base.gui_light_side,
            local_transform,
            base.gui_transform.unwrap_or(DisplayTransformWire {
                rotation: [0.0; 3],
                translation: [0.0; 3],
                scale: [1.0; 3],
            }),
            icon_size,
        )
    }
}

#[derive(Debug)]
struct StaticItemModel {
    model: String,
    tints: Vec<u32>,
    fallback_kind: Option<&'static str>,
    special: Option<SpecialItemModel>,
    local_transform: ItemModelTransform,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ItemModelTransform([[f32; 4]; 4]);

impl ItemModelTransform {
    const IDENTITY: Self = Self([
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]);

    fn from_value(value: Option<&Value>) -> Result<Self, BlockResourceError> {
        let Some(object) = value.and_then(Value::as_object) else {
            return Ok(Self::IDENTITY);
        };
        let vec3 = |name: &str, default: [f32; 3]| -> Result<[f32; 3], BlockResourceError> {
            let Some(values) = object.get(name) else {
                return Ok(default);
            };
            parse_f32_array::<3>(values).ok_or_else(|| {
                malformed("item transformation", name, "expected three finite numbers")
            })
        };
        let quat = |name: &str| -> Result<[f32; 4], BlockResourceError> {
            let Some(values) = object.get(name) else {
                return Ok([0.0, 0.0, 0.0, 1.0]);
            };
            parse_f32_array::<4>(values).ok_or_else(|| {
                malformed("item transformation", name, "expected four finite numbers")
            })
        };
        let translation = vec3("translation", [0.0; 3])?;
        let scale = vec3("scale", [1.0; 3])?;
        let left = quaternion_matrix(quat("left_rotation")?)?;
        let right = quaternion_matrix(quat("right_rotation")?)?;
        let translation = ItemModelTransform([
            [1.0, 0.0, 0.0, translation[0]],
            [0.0, 1.0, 0.0, translation[1]],
            [0.0, 0.0, 1.0, translation[2]],
            [0.0, 0.0, 0.0, 1.0],
        ]);
        let scale = ItemModelTransform([
            [scale[0], 0.0, 0.0, 0.0],
            [0.0, scale[1], 0.0, 0.0],
            [0.0, 0.0, scale[2], 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ]);
        Ok(translation.multiply(left).multiply(scale).multiply(right))
    }

    fn multiply(self, right: Self) -> Self {
        Self(std::array::from_fn(|row| {
            std::array::from_fn(|column| {
                (0..4)
                    .map(|index| self.0[row][index] * right.0[index][column])
                    .sum()
            })
        }))
    }

    fn point(self, point: [f32; 3]) -> [f32; 3] {
        std::array::from_fn(|row| {
            self.0[row][0] * point[0]
                + self.0[row][1] * point[1]
                + self.0[row][2] * point[2]
                + self.0[row][3]
        })
    }

    fn determinant3(self) -> f32 {
        let matrix = self.0;
        matrix[0][0] * (matrix[1][1] * matrix[2][2] - matrix[1][2] * matrix[2][1])
            - matrix[0][1] * (matrix[1][0] * matrix[2][2] - matrix[1][2] * matrix[2][0])
            + matrix[0][2] * (matrix[1][0] * matrix[2][1] - matrix[1][1] * matrix[2][0])
    }
}

fn quaternion_matrix(value: [f32; 4]) -> Result<ItemModelTransform, BlockResourceError> {
    let length = value.iter().map(|axis| axis * axis).sum::<f32>().sqrt();
    if !length.is_finite() || length <= f32::EPSILON {
        return Err(malformed(
            "item transformation",
            "rotation",
            "quaternion is not finite and non-zero",
        ));
    }
    let [x, y, z, w] = value.map(|axis| axis / length);
    Ok(ItemModelTransform([
        [
            1.0 - 2.0 * (y * y + z * z),
            2.0 * (x * y - z * w),
            2.0 * (x * z + y * w),
            0.0,
        ],
        [
            2.0 * (x * y + z * w),
            1.0 - 2.0 * (x * x + z * z),
            2.0 * (y * z - x * w),
            0.0,
        ],
        [
            2.0 * (x * z - y * w),
            2.0 * (y * z + x * w),
            1.0 - 2.0 * (x * x + y * y),
            0.0,
        ],
        [0.0, 0.0, 0.0, 1.0],
    ]))
}

fn parse_f32_array<const N: usize>(value: &Value) -> Option<[f32; N]> {
    let values = value.as_array()?;
    if values.len() != N {
        return None;
    }
    let mut output = [0.0; N];
    for (target, value) in output.iter_mut().zip(values) {
        *target = value.as_f64()? as f32;
        if !target.is_finite() {
            return None;
        }
    }
    Some(output)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SpecialItemModel {
    family: String,
    texture: Option<String>,
    variant: Option<String>,
    part: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct BannerPatternLayer {
    texture: String,
    tint: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DeferredBannerPatternLayer {
    pattern: DeferredBannerPattern,
    tint: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum DeferredBannerPattern {
    Reference(u32),
    Direct(MinecraftIdentifier),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DeferredBannerPatterns {
    layers: Vec<DeferredBannerPatternLayer>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum BannerPatternLayers {
    Deferred(DeferredBannerPatterns),
    Resolved(Vec<BannerPatternLayer>),
}

#[derive(Clone, Copy)]
enum BannerPatternRegistry<'a> {
    GameData(&'a cubic_version::RegistryTable),
    Ordered(&'a [MinecraftIdentifier]),
}

impl<'a> BannerPatternRegistry<'a> {
    fn identifier(self, raw_id: u32) -> Option<&'a MinecraftIdentifier> {
        match self {
            Self::GameData(registry) => registry.by_raw_id(raw_id).map(|entry| &entry.identifier),
            Self::Ordered(entries) => usize::try_from(raw_id)
                .ok()
                .and_then(|index| entries.get(index)),
        }
    }
}

impl DeferredBannerPatterns {
    fn resolve(
        &self,
        registry: BannerPatternRegistry<'_>,
    ) -> Result<Vec<BannerPatternLayer>, BlockResourceError> {
        self.layers
            .iter()
            .map(|layer| {
                let pattern = match &layer.pattern {
                    DeferredBannerPattern::Reference(raw_id) => {
                        registry.identifier(*raw_id).ok_or_else(|| {
                            malformed(
                                "banner patterns",
                                "component",
                                "pattern registry ID is invalid",
                            )
                        })?
                    }
                    DeferredBannerPattern::Direct(identifier) => identifier,
                };
                Ok(BannerPatternLayer {
                    texture: format!(
                        "minecraft:entity/banner/{}",
                        identifier_path(pattern.as_str())
                    ),
                    tint: layer.tint,
                })
            })
            .collect()
    }
}

impl SpecialItemModel {
    fn from_value(value: &Value) -> Result<Self, BlockResourceError> {
        let object = value
            .as_object()
            .ok_or_else(|| malformed("special item model", "model", "renderer is not an object"))?;
        let family = object
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| malformed("special item model", "model", "type is missing"))?
            .to_owned();
        let texture = object
            .get("texture")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let variant = object
            .get("color")
            .or_else(|| object.get("kind"))
            .or_else(|| object.get("pose"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let part = object
            .get("part")
            .and_then(Value::as_str)
            .map(str::to_owned);
        Ok(Self {
            family,
            texture,
            variant,
            part,
        })
    }

    fn texture_identifier(&self) -> Result<String, BlockResourceError> {
        let raw = match self.family.as_str() {
            "minecraft:chest" => format!(
                "minecraft:entity/chest/{}",
                identifier_path(self.texture.as_deref().unwrap_or("minecraft:normal"))
            ),
            "minecraft:bed" => format!(
                "minecraft:entity/bed/{}",
                identifier_path(self.texture.as_deref().unwrap_or("minecraft:red"))
            ),
            "minecraft:banner" => "minecraft:entity/banner/banner_base".to_owned(),
            "minecraft:shulker_box" => format!(
                "minecraft:entity/shulker/{}",
                identifier_path(self.texture.as_deref().unwrap_or("minecraft:shulker"))
            ),
            "minecraft:shield" => "minecraft:entity/shield/shield_base_nopattern".to_owned(),
            "minecraft:decorated_pot" => {
                "minecraft:entity/decorated_pot/decorated_pot_base".to_owned()
            }
            "minecraft:copper_golem_statue" => normalize_special_texture(
                self.texture
                    .as_deref()
                    .unwrap_or("minecraft:textures/entity/copper_golem/copper_golem.png"),
            ),
            "minecraft:head" => match self.variant.as_deref().unwrap_or("skeleton") {
                "creeper" => "minecraft:entity/creeper/creeper".to_owned(),
                "dragon" => "minecraft:entity/enderdragon/dragon".to_owned(),
                "piglin" => "minecraft:entity/piglin/piglin".to_owned(),
                "wither_skeleton" => "minecraft:entity/skeleton/wither_skeleton".to_owned(),
                "zombie" => "minecraft:entity/zombie/zombie".to_owned(),
                _ => "minecraft:entity/skeleton/skeleton".to_owned(),
            },
            // A static Steve skin is the bounded offline/default appearance;
            // per-stack profile skins remain a future dynamic-atlas concern.
            "minecraft:player_head" => "minecraft:entity/player/wide/steve".to_owned(),
            "minecraft:conduit" => "minecraft:entity/conduit/base".to_owned(),
            family => {
                return Err(malformed(
                    "special item model",
                    family,
                    "renderer has no static GUI texture",
                ));
            }
        };
        Ok(raw)
    }

    fn model_parts(&self) -> ModelPartDefinition {
        let cube =
            |origin, size, uv, texture_size| CubeDefinition::new(origin, size, uv, texture_size);
        let part = |name, pose, cubes| ModelPartDefinition::part(name, pose, cubes, vec![]);
        match self.family.as_str() {
            "minecraft:chest" => {
                let (origin_x, width, lock_x, hidden_face) = match self.part.as_deref() {
                    Some("left") => (0.0, 15.0, 0.0, Some(PartDirection::West)),
                    Some("right") => (1.0, 15.0, 15.0, Some(PartDirection::East)),
                    _ => (1.0, 14.0, 7.0, None),
                };
                let visible_faces = hidden_face.map_or(crate::model_part::ALL_FACES, |face| {
                    crate::model_part::ALL_FACES & !(1 << face as u8)
                });
                ModelPartDefinition::root(vec![
                    part(
                        "bottom",
                        PartPose::IDENTITY,
                        vec![
                            cube(
                                [origin_x, 0.0, 1.0],
                                [width, 10.0, 14.0],
                                [0.0, 19.0],
                                [64.0; 2],
                            )
                            .faces(visible_faces),
                        ],
                    ),
                    part(
                        "lid",
                        PartPose::offset(0.0, 9.0, 1.0),
                        vec![
                            cube(
                                [origin_x, 0.0, 0.0],
                                [width, 5.0, 14.0],
                                [0.0, 0.0],
                                [64.0; 2],
                            )
                            .faces(visible_faces),
                        ],
                    ),
                    part(
                        "lock",
                        PartPose::offset(0.0, 9.0, 1.0),
                        vec![cube(
                            [lock_x, -2.0, 14.0],
                            [if hidden_face.is_some() { 1.0 } else { 2.0 }, 4.0, 1.0],
                            [0.0, 0.0],
                            [64.0; 2],
                        )],
                    ),
                ])
            }
            "minecraft:bed" if self.part.as_deref() == Some("head") => {
                ModelPartDefinition::root(vec![
                    part(
                        "main",
                        PartPose::IDENTITY,
                        vec![cube([0.0; 3], [16.0, 16.0, 6.0], [0.0, 0.0], [64.0; 2])],
                    ),
                    part(
                        "left_leg",
                        PartPose::rotation(HALF_PI, 0.0, HALF_PI),
                        vec![cube([0.0, 6.0, 0.0], [3.0; 3], [50.0, 6.0], [64.0; 2])],
                    ),
                    part(
                        "right_leg",
                        PartPose::rotation(HALF_PI, 0.0, std::f32::consts::PI),
                        vec![cube([-16.0, 6.0, 0.0], [3.0; 3], [50.0, 18.0], [64.0; 2])],
                    ),
                ])
            }
            "minecraft:bed" => ModelPartDefinition::root(vec![
                part(
                    "main",
                    PartPose::IDENTITY,
                    vec![cube([0.0; 3], [16.0, 16.0, 6.0], [0.0, 22.0], [64.0; 2])],
                ),
                part(
                    "left_leg",
                    PartPose::rotation(HALF_PI, 0.0, 0.0),
                    vec![cube([0.0, 6.0, -16.0], [3.0; 3], [50.0, 0.0], [64.0; 2])],
                ),
                part(
                    "right_leg",
                    PartPose::rotation(HALF_PI, 0.0, 3.0 * HALF_PI),
                    vec![cube([-16.0, 6.0, -16.0], [3.0; 3], [50.0, 12.0], [64.0; 2])],
                ),
            ]),
            "minecraft:banner" => {
                let standing = self.part.as_deref() != Some("wall");
                let mut parts = Vec::new();
                if standing {
                    parts.push(part(
                        "pole",
                        PartPose::IDENTITY,
                        vec![cube(
                            [-1.0, -42.0, -1.0],
                            [2.0, 42.0, 2.0],
                            [44.0, 0.0],
                            [64.0; 2],
                        )],
                    ));
                }
                let (bar_y, bar_z, flag_z) = if standing {
                    (-44.0, -1.0, 0.0)
                } else {
                    (-20.5, 9.5, 10.5)
                };
                parts.push(part(
                    "bar",
                    PartPose::IDENTITY,
                    vec![cube(
                        [-10.0, bar_y, bar_z],
                        [20.0, 2.0, 2.0],
                        [0.0, 42.0],
                        [64.0; 2],
                    )],
                ));
                parts.push(part(
                    "flag",
                    PartPose::offset(0.0, bar_y, flag_z),
                    vec![cube(
                        [-10.0, 0.0, -2.0],
                        [20.0, 40.0, 1.0],
                        [0.0, 0.0],
                        [64.0; 2],
                    )],
                ));
                ModelPartDefinition::root(parts)
            }
            "minecraft:shulker_box" => ModelPartDefinition::root(vec![
                part(
                    "base",
                    PartPose::offset(0.0, 24.0, 0.0),
                    vec![cube(
                        [-8.0, -8.0, -8.0],
                        [16.0, 8.0, 16.0],
                        [0.0, 28.0],
                        [64.0; 2],
                    )],
                ),
                part(
                    "lid",
                    PartPose::offset(0.0, 24.0, 0.0),
                    vec![cube(
                        [-8.0, -16.0, -8.0],
                        [16.0, 12.0, 16.0],
                        [0.0, 0.0],
                        [64.0; 2],
                    )],
                ),
            ]),
            "minecraft:decorated_pot" => decorated_pot_parts(),
            "minecraft:copper_golem_statue" => copper_golem_parts(self.variant.as_deref()),
            "minecraft:player_head" => ModelPartDefinition::root(vec![part(
                "head",
                PartPose::IDENTITY,
                vec![
                    cube([-4.0, -8.0, -4.0], [8.0; 3], [0.0, 0.0], [64.0; 2]),
                    CubeDefinition {
                        deformation: [0.25; 3],
                        ..cube([-4.0, -8.0, -4.0], [8.0; 3], [32.0, 0.0], [64.0; 2])
                    },
                ],
            )]),
            "minecraft:head" if self.variant.as_deref() == Some("dragon") => dragon_head_parts(),
            "minecraft:head" if self.variant.as_deref() == Some("piglin") => piglin_head_parts(),
            "minecraft:head" if self.variant.as_deref() == Some("zombie") => zombie_head_parts(),
            "minecraft:head" => ModelPartDefinition::root(vec![part(
                "head",
                PartPose::IDENTITY,
                vec![cube([-4.0, -8.0, -4.0], [8.0; 3], [0.0; 2], [64.0, 32.0])],
            )]),
            "minecraft:conduit" => ModelPartDefinition::root(vec![part(
                "eye",
                PartPose::IDENTITY,
                vec![cube([-3.0; 3], [6.0; 3], [0.0; 2], [32.0, 16.0])],
            )]),
            // Shield remains on the existing bounded special path; it is not
            // a placed block-entity family in this pass.
            "minecraft:shield" => ModelPartDefinition::root(vec![
                part(
                    "plate",
                    PartPose::IDENTITY,
                    vec![cube(
                        [-6.0, -11.0, -2.0],
                        [12.0, 22.0, 1.0],
                        [0.0; 2],
                        [64.0; 2],
                    )],
                ),
                part(
                    "handle",
                    PartPose::IDENTITY,
                    vec![cube(
                        [-1.0, -3.0, -1.0],
                        [2.0, 6.0, 6.0],
                        [26.0, 0.0],
                        [64.0; 2],
                    )],
                ),
            ]),
            _ => ModelPartDefinition::root(vec![]),
        }
    }

    fn model_faces(&self) -> Result<Vec<ModelFace>, BlockResourceError> {
        self.model_faces_from_parts(self.model_parts())
    }

    fn model_faces_from_parts(
        &self,
        parts: ModelPartDefinition,
    ) -> Result<Vec<ModelFace>, BlockResourceError> {
        let default_texture = self.texture_identifier()?;
        Ok(parts
            .bake()
            .into_iter()
            .map(|quad| {
                let texture = if self.family == "minecraft:decorated_pot"
                    && matches!(quad.part, "front" | "back" | "left" | "right")
                {
                    "minecraft:entity/decorated_pot/decorated_pot_side".to_owned()
                } else {
                    default_texture.clone()
                };
                let tint_index =
                    (self.family == "minecraft:banner" && quad.part == "flag").then_some(0);
                model_part_face(quad, texture, tint_index, &self.family)
            })
            .collect())
    }

    #[cfg(test)]
    fn geometry(&self) -> Vec<ElementWire> {
        let specs: Vec<SpecialCuboid> = match self.family.as_str() {
            "minecraft:chest" => vec![
                SpecialCuboid::new(
                    [[1.0, 0.0, 1.0], [15.0, 10.0, 15.0]],
                    [0.0, 19.0],
                    [64.0; 2],
                ),
                SpecialCuboid::new([[1.0, 9.0, 1.0], [15.0, 14.0, 15.0]], [0.0, 0.0], [64.0; 2]),
                SpecialCuboid::new([[7.0, 7.0, 15.0], [9.0, 11.0, 16.0]], [0.0, 0.0], [64.0; 2]),
            ],
            "minecraft:bed" if self.part.as_deref() == Some("head") => vec![
                SpecialCuboid::new([[0.0, 0.0, 0.0], [16.0, 16.0, 6.0]], [0.0, 0.0], [64.0; 2]),
                // BedRenderer's two leg cubes are children with X=90 degrees
                // and distinct Z quarter-turns. Apply those ModelPart poses
                // here; the special-node transform remains separate below.
                SpecialCuboid::new([[0.0, 0.0, 6.0], [3.0, 3.0, 9.0]], [50.0, 6.0], [64.0; 2]),
                SpecialCuboid::new(
                    [[13.0, 0.0, 6.0], [16.0, 3.0, 9.0]],
                    [50.0, 18.0],
                    [64.0; 2],
                ),
            ],
            "minecraft:bed" => vec![
                SpecialCuboid::new([[0.0, 0.0, 0.0], [16.0, 16.0, 6.0]], [0.0, 22.0], [64.0; 2]),
                SpecialCuboid::new([[0.0, 13.0, 6.0], [3.0, 16.0, 9.0]], [50.0, 0.0], [64.0; 2]),
                SpecialCuboid::new(
                    [[13.0, 13.0, 6.0], [16.0, 16.0, 9.0]],
                    [50.0, 12.0],
                    [64.0; 2],
                ),
            ],
            "minecraft:banner" => vec![
                SpecialCuboid::new(
                    [[-10.0, -40.0, 0.0], [10.0, 0.0, 0.0]],
                    [0.0, 0.0],
                    [64.0; 2],
                ),
                SpecialCuboid::new(
                    [[-1.0, -42.0, -1.0], [1.0, 0.0, 1.0]],
                    [44.0, 0.0],
                    [64.0; 2],
                ),
                SpecialCuboid::new(
                    [[-10.0, -44.0, -1.0], [10.0, -42.0, 1.0]],
                    [0.0, 42.0],
                    [64.0; 2],
                ),
            ],
            "minecraft:shulker_box" => vec![
                SpecialCuboid::new([[-8.0, 8.0, -8.0], [8.0, 20.0, 8.0]], [0.0, 0.0], [64.0; 2]),
                SpecialCuboid::new(
                    [[-8.0, 16.0, -8.0], [8.0, 24.0, 8.0]],
                    [0.0, 28.0],
                    [64.0; 2],
                ),
            ],
            "minecraft:shield" => vec![
                SpecialCuboid::new(
                    [[-6.0, -11.0, -2.0], [6.0, 11.0, -1.0]],
                    [0.0, 0.0],
                    [64.0; 2],
                ),
                SpecialCuboid::new(
                    [[-1.0, -3.0, -1.0], [1.0, 3.0, 5.0]],
                    [26.0, 0.0],
                    [64.0; 2],
                ),
            ],
            "minecraft:decorated_pot" => vec![
                // DecoratedPotRenderer.createBaseLayer: the two neck cubes
                // include the exact -0.1/+0.2 cube deformations and the
                // model-part's (0, 37, 16), X=pi transform.
                SpecialCuboid::new(
                    [[4.1, 17.1, 4.1], [11.9, 19.9, 11.9]],
                    [0.0, 0.0],
                    [32.0; 2],
                ),
                SpecialCuboid::new(
                    [[4.8, 15.8, 4.8], [11.2, 17.2, 11.2]],
                    [0.0, 5.0],
                    [32.0; 2],
                ),
                // Base top/bottom plus the four separately transformed side
                // planes from createSidesLayer. Zero-thickness model parts
                // intentionally remain planes rather than invented boxes.
                SpecialCuboid::new(
                    [[1.0, 16.0, 1.0], [15.0, 16.0, 15.0]],
                    [-14.0, 13.0],
                    [32.0; 2],
                ),
                SpecialCuboid::new(
                    [[1.0, 0.0, 1.0], [15.0, 0.0, 15.0]],
                    [-14.0, 13.0],
                    [32.0; 2],
                ),
                SpecialCuboid::new([[1.0, 0.0, 1.0], [15.0, 16.0, 1.0]], [1.0, 0.0], [16.0; 2]),
                SpecialCuboid::new(
                    [[1.0, 0.0, 15.0], [15.0, 16.0, 15.0]],
                    [1.0, 0.0],
                    [16.0; 2],
                ),
                SpecialCuboid::new([[1.0, 0.0, 1.0], [1.0, 16.0, 15.0]], [1.0, 0.0], [16.0; 2]),
                SpecialCuboid::new(
                    [[15.0, 0.0, 1.0], [15.0, 16.0, 15.0]],
                    [1.0, 0.0],
                    [16.0; 2],
                ),
            ],
            "minecraft:copper_golem_statue" => vec![
                // CopperGolemModel.createBodyLayer after its root/body/head
                // model-part translations. These are the actual standing
                // statue cuboids and texture offsets, including antenna.
                SpecialCuboid::new(
                    [[-4.0, 13.0, -3.0], [4.0, 19.0, 3.0]],
                    [0.0, 15.0],
                    [64.0; 2],
                ),
                SpecialCuboid::new(
                    [[-4.015, 7.985, -5.015], [4.015, 13.015, 5.015]],
                    [0.0, 0.0],
                    [64.0; 2],
                ),
                SpecialCuboid::new(
                    [[-1.0, 11.0, -6.0], [1.0, 14.0, -4.0]],
                    [56.0, 0.0],
                    [64.0; 2],
                ),
                SpecialCuboid::new(
                    [[-0.985, 4.015, -0.985], [0.985, 7.985, 0.985]],
                    [37.0, 8.0],
                    [64.0; 2],
                ),
                SpecialCuboid::new(
                    [[-1.985, 0.015, -1.985], [1.985, 3.985, 1.985]],
                    [37.0, 0.0],
                    [64.0; 2],
                ),
                SpecialCuboid::new(
                    [[-7.0, 12.0, -2.0], [-4.0, 22.0, 2.0]],
                    [36.0, 16.0],
                    [64.0; 2],
                ),
                SpecialCuboid::new(
                    [[4.0, 12.0, -2.0], [7.0, 22.0, 2.0]],
                    [50.0, 16.0],
                    [64.0; 2],
                ),
                SpecialCuboid::new(
                    [[-4.0, 19.0, -2.0], [0.0, 24.0, 2.0]],
                    [0.0, 27.0],
                    [64.0; 2],
                ),
                SpecialCuboid::new(
                    [[0.0, 19.0, -2.0], [4.0, 24.0, 2.0]],
                    [16.0, 27.0],
                    [64.0; 2],
                ),
            ],
            "minecraft:player_head" => vec![
                SpecialCuboid::new([[-4.0, -8.0, -4.0], [4.0, 0.0, 4.0]], [0.0, 0.0], [64.0; 2]),
                SpecialCuboid::new(
                    [[-4.25, -8.25, -4.25], [4.25, 0.25, 4.25]],
                    [32.0, 0.0],
                    [64.0; 2],
                ),
            ],
            "minecraft:head" if self.variant.as_deref() == Some("dragon") => vec![
                // DragonHeadModel.createHeadLayer after the head part's
                // 0.75 scale and -7.986666 Y pivot. The idle jaw uses its
                // verified child offset; animation remains intentionally
                // static in an inventory icon.
                SpecialCuboid::new(
                    [[-4.5, -8.736_666, -18.0], [4.5, -4.986_666, -6.0]],
                    [176.0, 44.0],
                    [256.0; 2],
                ),
                SpecialCuboid::new(
                    [[-6.0, -13.986_666, -7.5], [6.0, -1.986_666, 4.5]],
                    [112.0, 30.0],
                    [256.0; 2],
                ),
                SpecialCuboid::new(
                    [[-3.75, -16.986_666, -3.0], [-2.25, -13.986_666, 1.5]],
                    [0.0, 0.0],
                    [256.0; 2],
                ),
                SpecialCuboid::new(
                    [[2.25, -16.986_666, -3.0], [3.75, -13.986_666, 1.5]],
                    [0.0, 0.0],
                    [256.0; 2],
                ),
                SpecialCuboid::new(
                    [[-3.75, -10.236_666, -16.5], [-2.25, -8.736_666, -13.5]],
                    [112.0, 0.0],
                    [256.0; 2],
                ),
                SpecialCuboid::new(
                    [[2.25, -10.236_666, -16.5], [3.75, -8.736_666, -13.5]],
                    [112.0, 0.0],
                    [256.0; 2],
                ),
                SpecialCuboid::new(
                    [[-4.5, -4.986_666, -18.0], [4.5, -1.986_666, -6.0]],
                    [176.0, 65.0],
                    [256.0; 2],
                ),
            ],
            "minecraft:head" if self.variant.as_deref() == Some("piglin") => vec![
                SpecialCuboid::new([[-5.0, -8.0, -4.0], [5.0, 0.0, 4.0]], [0.0, 0.0], [64.0; 2]),
                SpecialCuboid::new(
                    [[-2.0, -4.0, -5.0], [2.0, 0.0, -4.0]],
                    [31.0, 1.0],
                    [64.0; 2],
                ),
                SpecialCuboid::new([[2.0, -2.0, -5.0], [3.0, 0.0, -4.0]], [2.0, 4.0], [64.0; 2]),
                SpecialCuboid::new(
                    [[-3.0, -2.0, -5.0], [-2.0, 0.0, -4.0]],
                    [2.0, 0.0],
                    [64.0; 2],
                ),
                SpecialCuboid::new(
                    [[4.5, -6.0, -2.0], [5.5, -1.0, 2.0]],
                    [51.0, 6.0],
                    [64.0; 2],
                ),
                SpecialCuboid::new(
                    [[-5.5, -6.0, -2.0], [-4.5, -1.0, 2.0]],
                    [39.0, 6.0],
                    [64.0; 2],
                ),
            ],
            "minecraft:head" => vec![SpecialCuboid::new(
                [[-4.0, -8.0, -4.0], [4.0, 0.0, 4.0]],
                [0.0, 0.0],
                [64.0, 32.0],
            )],
            "minecraft:conduit" => vec![SpecialCuboid::new(
                [[-3.0, -3.0, -3.0], [3.0, 3.0, 3.0]],
                [0.0, 0.0],
                [32.0, 16.0],
            )],
            _ => Vec::new(),
        };
        specs.into_iter().map(special_box).collect()
    }

    fn tint(&self) -> Option<u32> {
        (self.family == "minecraft:banner").then_some(match self.variant.as_deref() {
            Some("white") => 0xf9_ff_fe,
            Some("orange") => 0xf9_80_1d,
            Some("magenta") => 0xc7_4e_bd,
            Some("light_blue") => 0x3a_b3_da,
            Some("yellow") => 0xfe_d8_3d,
            Some("lime") => 0x80_c7_1f,
            Some("pink") => 0xf3_8b_aa,
            Some("gray") => 0x47_4f_52,
            Some("light_gray") => 0x9d_9d_97,
            Some("cyan") => 0x16_9c_9c,
            Some("purple") => 0x89_32_b8,
            Some("blue") => 0x3c_44_aa,
            Some("brown") => 0x835432,
            Some("green") => 0x5e7c16,
            Some("red") => 0xb0_2e_26,
            Some("black") => 0x1d_1d_21,
            _ => 0xff_ff_ff,
        })
    }
}

fn model_part_face(
    quad: ModelPartQuad,
    texture: String,
    tint_index: Option<u32>,
    family: &str,
) -> ModelFace {
    let corners = quad.positions.map(|point| point.map(|axis| axis / 16.0));
    // Entity-style ModelPart cubes carry an authored Polygon normal. Using a
    // second block-face cross product here loses Cube.mirror's X-normal rule
    // and can reassign the cube-net rectangle to the opposite rendered side.
    let normal = quad.normal;
    let (axis, _) = normal
        .into_iter()
        .enumerate()
        .max_by(|(_, left), (_, right)| {
            left.abs()
                .partial_cmp(&right.abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap_or((1, 1.0));
    let direction = match (axis, normal[axis].is_sign_negative()) {
        (0, true) => Direction::West,
        (0, false) => Direction::East,
        (1, true) => Direction::Down,
        (1, false) => Direction::Up,
        (2, true) => Direction::North,
        _ => Direction::South,
    };
    let source_direction = match quad.direction {
        PartDirection::Down => Direction::Down,
        PartDirection::Up => Direction::Up,
        PartDirection::North => Direction::North,
        PartDirection::South => Direction::South,
        PartDirection::West => Direction::West,
        PartDirection::East => Direction::East,
    };
    ModelFace {
        direction,
        corners,
        uv: quad.uv,
        texture,
        atlas_region: AtlasRegion {
            min: [0.0; 2],
            max: [1.0; 2],
            layer: RenderLayer::Opaque,
        },
        material: TextureMaterial::Terrain,
        cullface: None,
        tint_index,
        tint_kind: TintKind::None,
        render_layer: if matches!(family, "minecraft:banner" | "minecraft:decorated_pot") {
            RenderLayer::Cutout
        } else {
            RenderLayer::Opaque
        },
        directional_shade: family != "minecraft:banner",
        shade: direction_shade(if normal == [0.0; 3] {
            source_direction
        } else {
            direction
        }),
    }
}

fn decorated_pot_parts() -> ModelPartDefinition {
    let single = |name, pose, cube| ModelPartDefinition::part(name, pose, vec![cube], vec![]);
    let plane = CubeDefinition::new([0.0; 3], [14.0, 16.0, 0.0], [1.0, 0.0], [16.0; 2])
        .faces(1 << PartDirection::North as u8);
    ModelPartDefinition::root(vec![
        ModelPartDefinition::part(
            "neck",
            PartPose::offset_and_rotation(0.0, 37.0, 16.0, std::f32::consts::PI, 0.0, 0.0),
            vec![
                CubeDefinition {
                    deformation: [-0.1; 3],
                    ..CubeDefinition::new([4.0, 17.0, 4.0], [8.0, 3.0, 8.0], [0.0, 0.0], [32.0; 2])
                },
                CubeDefinition {
                    deformation: [0.2; 3],
                    ..CubeDefinition::new([5.0, 20.0, 5.0], [6.0, 1.0, 6.0], [0.0, 5.0], [32.0; 2])
                },
            ],
            vec![],
        ),
        single(
            "top",
            PartPose::offset(1.0, 16.0, 1.0),
            CubeDefinition::new([0.0; 3], [14.0, 0.0, 14.0], [-14.0, 13.0], [32.0; 2]),
        ),
        single(
            "bottom",
            PartPose::offset(1.0, 0.0, 1.0),
            CubeDefinition::new([0.0; 3], [14.0, 0.0, 14.0], [-14.0, 13.0], [32.0; 2]),
        ),
        single(
            "back",
            PartPose::offset_and_rotation(15.0, 16.0, 1.0, 0.0, 0.0, std::f32::consts::PI),
            plane,
        ),
        single(
            "left",
            PartPose::offset_and_rotation(1.0, 16.0, 1.0, 0.0, -HALF_PI, std::f32::consts::PI),
            plane,
        ),
        single(
            "right",
            PartPose::offset_and_rotation(15.0, 16.0, 15.0, 0.0, HALF_PI, std::f32::consts::PI),
            plane,
        ),
        single(
            "front",
            PartPose::offset_and_rotation(1.0, 16.0, 15.0, std::f32::consts::PI, 0.0, 0.0),
            plane,
        ),
    ])
}

fn copper_golem_parts(pose: Option<&str>) -> ModelPartDefinition {
    let cube = |origin, size, uv| CubeDefinition::new(origin, size, uv, [64.0; 2]);
    let mut left_arm = PartPose::offset(4.0, -6.0, 0.0);
    let mut right_arm = PartPose::offset(-4.0, -6.0, 0.0);
    let mut left_leg = PartPose::offset(2.0, -5.0, 0.0);
    let mut right_leg = PartPose::offset(-2.0, -5.0, 0.0);
    match pose.unwrap_or("standing") {
        "sitting" => {
            left_leg.rotation[0] = -HALF_PI;
            right_leg.rotation[0] = -HALF_PI;
        }
        "star" => {
            left_arm.rotation[2] = -2.2;
            right_arm.rotation[2] = 2.2;
            left_leg.rotation[2] = 0.45;
            right_leg.rotation[2] = -0.45;
        }
        "running" => {
            left_arm.rotation[0] = -0.9;
            right_arm.rotation[0] = 0.9;
            left_leg.rotation[0] = 0.9;
            right_leg.rotation[0] = -0.9;
        }
        _ => {}
    }
    let limb = |name, part_pose, origin, size, uv| {
        ModelPartDefinition::part(name, part_pose, vec![cube(origin, size, uv)], vec![])
    };
    let head = ModelPartDefinition::part(
        "head",
        PartPose::offset(0.0, -6.0, 0.0),
        vec![
            CubeDefinition {
                deformation: [0.015; 3],
                ..cube([-4.0, -5.0, -5.0], [8.0, 5.0, 10.0], [0.0, 0.0])
            },
            cube([-1.0, -2.0, -6.0], [2.0, 3.0, 2.0], [56.0, 0.0]),
            CubeDefinition {
                deformation: [-0.015; 3],
                ..cube([-1.0, -9.0, -1.0], [2.0, 4.0, 2.0], [37.0, 8.0])
            },
            CubeDefinition {
                deformation: [-0.015; 3],
                ..cube([-2.0, -13.0, -2.0], [4.0; 3], [37.0, 0.0])
            },
        ],
        vec![],
    );
    let body = ModelPartDefinition::part(
        "body",
        PartPose::offset(0.0, -5.0, 0.0),
        vec![cube([-4.0, -6.0, -3.0], [8.0, 6.0, 6.0], [0.0, 15.0])],
        vec![
            head,
            limb(
                "right_arm",
                right_arm,
                [-3.0, -1.0, -2.0],
                [3.0, 10.0, 4.0],
                [36.0, 16.0],
            ),
            limb(
                "left_arm",
                left_arm,
                [0.0, -1.0, -2.0],
                [3.0, 10.0, 4.0],
                [50.0, 16.0],
            ),
        ],
    );
    // CopperGolemStatueModel.setupAnim replaces the baked root Y=24 with
    // Y=0 and rotates that root by PI around Z. Keeping the original +24 in
    // flattened cubes applied the setup pose twice and put statues below the
    // block. The special item node's (1,-1,-1) transform remains separate.
    ModelPartDefinition::part(
        "root",
        PartPose::rotation(0.0, 0.0, std::f32::consts::PI),
        vec![],
        vec![
            body,
            limb(
                "right_leg",
                right_leg,
                [-2.0, 0.0, -2.0],
                [4.0, 5.0, 4.0],
                [0.0, 27.0],
            ),
            limb(
                "left_leg",
                left_leg,
                [-2.0, 0.0, -2.0],
                [4.0, 5.0, 4.0],
                [16.0, 27.0],
            ),
        ],
    )
}

// Exact pose-selected world ModelLayers used by the placed-statue renderer.
// Inventory deliberately retains its accepted special-item model path.
fn copper_golem_world_parts(pose: Option<&str>) -> ModelPartDefinition {
    let cube = |origin, size, uv| CubeDefinition::new(origin, size, uv, [64.0; 2]);
    let child =
        |name, pose, definition| ModelPartDefinition::part(name, pose, vec![definition], vec![]);
    let root = |children| {
        ModelPartDefinition::part(
            "root",
            PartPose::rotation(0.0, 0.0, std::f32::consts::PI),
            vec![],
            children,
        )
    };
    let ordinary_head = || {
        ModelPartDefinition::part(
            "head",
            PartPose::offset(0.0, -6.0, 0.0),
            vec![
                CubeDefinition {
                    deformation: [0.015; 3],
                    ..cube([-4.0, -5.0, -5.0], [8.0, 5.0, 10.0], [0.0, 0.0])
                },
                cube([-1.0, -2.0, -6.0], [2.0, 3.0, 2.0], [56.0, 0.0]),
                CubeDefinition {
                    deformation: [-0.015; 3],
                    ..cube([-1.0, -9.0, -1.0], [2.0, 4.0, 2.0], [37.0, 8.0])
                },
                CubeDefinition {
                    deformation: [-0.015; 3],
                    ..cube([-2.0, -13.0, -2.0], [4.0; 3], [37.0, 0.0])
                },
            ],
            vec![],
        )
    };
    let ordinary_body = |children| {
        ModelPartDefinition::part(
            "body",
            PartPose::offset(0.0, -5.0, 0.0),
            vec![cube([-4.0, -6.0, -3.0], [8.0, 6.0, 6.0], [0.0, 15.0])],
            children,
        )
    };
    let ordinary_arm = |name, x, origin, uv| {
        child(
            name,
            PartPose::offset(x, -6.0, 0.0),
            cube(origin, [3.0, 10.0, 4.0], uv),
        )
    };

    match pose.unwrap_or("standing") {
        "running" => {
            let left_leg = ModelPartDefinition::part(
                "left_leg",
                PartPose::offset(0.936, -5.0, 0.0),
                vec![],
                vec![child(
                    "left_leg_r1",
                    PartPose::offset_and_rotation(
                        1.0,
                        0.0,
                        0.0,
                        std::f32::consts::FRAC_PI_4,
                        0.0,
                        0.0,
                    ),
                    cube([-2.088, -0.1, -2.0], [4.0, 5.0, 4.0], [16.0, 27.0]),
                )],
            );
            let right_leg = ModelPartDefinition::part(
                "right_leg",
                PartPose::offset(-3.064, -5.0, 0.0),
                vec![],
                vec![child(
                    "right_leg_r1",
                    PartPose::offset_and_rotation(1.048, 0.0, -0.9, -0.8727, 0.0, 0.0),
                    cube([-1.856, -0.1, -1.09], [4.0, 5.0, 4.0], [0.0, 27.0]),
                )],
            );
            let head = ModelPartDefinition::part(
                "head",
                PartPose::offset(0.7, -5.6, -1.8),
                vec![
                    cube([-4.0, -5.1, -5.0], [8.0, 5.0, 10.0], [0.0, 0.0]),
                    cube([-1.02, -2.1, -6.0], [2.0, 3.0, 2.0], [56.0, 0.0]),
                    cube([-1.02, -9.1, -1.0], [2.0, 4.0, 2.0], [37.0, 8.0]),
                    cube([-2.0, -13.1, -2.0], [4.0; 3], [37.0, 0.0]),
                ],
                vec![],
            );
            let right_arm = ModelPartDefinition::part(
                "right_arm",
                PartPose::offset(-4.0, -6.0, 0.0),
                vec![],
                vec![child(
                    "right_arm_r1",
                    PartPose::offset_and_rotation(0.7, -0.248, -1.62, 1.0036, 0.0, 0.0),
                    cube([-3.052, -1.11, -2.036], [3.0, 10.0, 4.0], [36.0, 16.0]),
                )],
            );
            let left_arm = ModelPartDefinition::part(
                "left_arm",
                PartPose::offset(4.0, -6.0, 0.0),
                vec![],
                vec![child(
                    "left_arm_r1",
                    PartPose::offset_and_rotation(0.732, 0.0, 0.0, -0.8715, -0.0535, -0.0449),
                    cube([0.032, -1.1, -2.0], [3.0, 10.0, 4.0], [50.0, 16.0]),
                )],
            );
            let body = ModelPartDefinition::part(
                "body",
                PartPose::offset(-1.064, -5.0, 0.0),
                vec![],
                vec![
                    head,
                    right_arm,
                    left_arm,
                    child(
                        "body_r1",
                        PartPose::offset_and_rotation(1.1, 0.1, 0.7, 0.1204, -0.0064, -0.0779),
                        cube([-4.02, -6.116, -3.5], [8.0, 6.0, 6.0], [0.0, 15.0]),
                    ),
                ],
            );
            root(vec![left_leg, right_leg, body])
        }
        "sitting" => {
            let leg = |name, inner_name, outer, inner, uv| {
                ModelPartDefinition::part(
                    name,
                    outer,
                    vec![],
                    vec![child(
                        inner_name,
                        inner,
                        cube([-2.0, 0.975, 0.0], [4.0, 5.0, 4.0], uv),
                    )],
                )
            };
            let head = ModelPartDefinition::part(
                "head",
                PartPose::offset(0.0, -6.0, -0.2),
                vec![
                    cube([-1.0, -7.0, -3.3], [2.0, 4.0, 2.0], [37.0, 8.0]),
                    cube([-2.0, -11.0, -4.3], [4.0; 3], [37.0, 0.0]),
                    cube([-4.0, -3.0, -7.325], [8.0, 5.0, 10.0], [0.0, 0.0]),
                    cube([-1.0, 0.0, -8.325], [2.0, 3.0, 2.0], [56.0, 0.0]),
                ],
                vec![],
            );
            let sitting_arm = |name, inner_name, outer, inner, origin, uv| {
                ModelPartDefinition::part(
                    name,
                    outer,
                    vec![],
                    vec![child(inner_name, inner, cube(origin, [3.0, 10.0, 4.0], uv))],
                )
            };
            let body = ModelPartDefinition::part(
                "body",
                PartPose::offset(0.0, -3.0, 2.325),
                vec![
                    cube([-3.0, -4.0, -4.525], [6.0, 1.0, 6.0], [3.0, 19.0]),
                    cube([-4.0, -3.0, -3.525], [8.0, 6.0, 6.0], [0.0, 15.0]),
                ],
                vec![
                    head,
                    sitting_arm(
                        "right_arm",
                        "right_arm_r1",
                        PartPose::offset_and_rotation(-4.0, -5.6, -1.8, 0.4363, 0.0, 0.0),
                        PartPose::offset_and_rotation(
                            0.0,
                            0.0893,
                            0.1198,
                            -std::f32::consts::FRAC_PI_3,
                            0.0,
                            0.0,
                        ),
                        [-3.075, -0.9733, -1.9966],
                        [36.0, 16.0],
                    ),
                    sitting_arm(
                        "left_arm",
                        "left_arm_r1",
                        PartPose::offset_and_rotation(4.0, -5.6, -1.7, 0.4363, 0.0, 0.0),
                        PartPose::offset_and_rotation(
                            0.0,
                            -0.0015,
                            -0.0808,
                            -std::f32::consts::FRAC_PI_3,
                            0.0,
                            0.0,
                        ),
                        [0.075, -1.0443, -1.8997],
                        [50.0, 16.0],
                    ),
                    child(
                        "body_r1",
                        PartPose::offset_and_rotation(
                            0.0,
                            -1.0,
                            -4.325,
                            0.0,
                            0.0,
                            -std::f32::consts::PI,
                        ),
                        cube([-4.0, -3.0, -2.2], [8.0, 6.0, 3.0], [0.0, 15.0]),
                    ),
                ],
            );
            root(vec![
                leg(
                    "left_leg",
                    "left_leg_r1",
                    PartPose::offset(2.0, -2.0, -2.075),
                    PartPose::offset_and_rotation(0.05, -2.0, 1.075, -HALF_PI, 0.0, 0.0),
                    [16.0, 27.0],
                ),
                leg(
                    "right_leg",
                    "right_leg_r1",
                    PartPose::offset(-2.1, -2.1, -2.075),
                    PartPose::offset_and_rotation(0.05, -1.9, 1.075, -HALF_PI, 0.0, 0.0),
                    [0.0, 27.0],
                ),
                body,
            ])
        }
        "star" => {
            let body = ordinary_body(vec![
                ordinary_head(),
                ModelPartDefinition::part(
                    "right_arm",
                    PartPose::offset(-4.0, -6.0, 0.0),
                    vec![],
                    vec![child(
                        "right_arm_r1",
                        PartPose::offset_and_rotation(1.0, 1.0, 0.0, 0.0, 0.0, 1.9199),
                        cube([-1.5, -5.0, -2.0], [3.0, 10.0, 4.0], [36.0, 16.0]),
                    )],
                ),
                ModelPartDefinition::part(
                    "left_arm",
                    PartPose::offset(4.0, -6.0, 0.0),
                    vec![],
                    vec![child(
                        "left_arm_r1",
                        PartPose::offset_and_rotation(-1.0, 1.0, 0.0, 0.0, 0.0, -1.9199),
                        cube([-1.5, -5.0, -2.0], [3.0, 10.0, 4.0], [50.0, 16.0]),
                    )],
                ),
            ]);
            root(vec![
                ModelPartDefinition::part(
                    "left_leg",
                    PartPose::offset(1.0, -5.0, 0.0),
                    vec![],
                    vec![child(
                        "left_leg_r1",
                        PartPose::offset_and_rotation(1.65, 2.0, 0.0, 0.0, 0.0, -0.2618),
                        cube([-2.0, -2.5, -2.0], [4.0, 5.0, 4.0], [16.0, 27.0]),
                    )],
                ),
                ModelPartDefinition::part(
                    "right_leg",
                    PartPose::offset(-3.0, -5.0, 0.0),
                    vec![],
                    vec![child(
                        "right_leg_r1",
                        PartPose::offset_and_rotation(0.35, 2.0, 0.01, 0.0, 0.0, 0.2618),
                        cube([-2.0, -2.5, -2.0], [4.0, 5.0, 4.0], [0.0, 27.0]),
                    )],
                ),
                body,
            ])
        }
        _ => root(vec![
            child(
                "left_leg",
                PartPose::offset(0.0, -5.0, 0.0),
                cube([0.0, 0.0, -2.0], [4.0, 5.0, 4.0], [16.0, 27.0]),
            ),
            child(
                "right_leg",
                PartPose::offset(0.0, -5.0, 0.0),
                cube([-4.0, 0.0, -2.0], [4.0, 5.0, 4.0], [0.0, 27.0]),
            ),
            ordinary_body(vec![
                ordinary_head(),
                ordinary_arm("right_arm", -4.0, [-3.0, -1.0, -2.0], [36.0, 16.0]),
                ordinary_arm("left_arm", 4.0, [0.0, -1.0, -2.0], [50.0, 16.0]),
            ]),
        ]),
    }
}

fn dragon_head_parts() -> ModelPartDefinition {
    let cube = |origin, size, uv| CubeDefinition::new(origin, size, uv, [256.0; 2]);
    ModelPartDefinition::root(vec![ModelPartDefinition::part(
        "head",
        PartPose::offset(0.0, -7.986_666, 0.0).scaled(0.75),
        vec![
            cube([-6.0, -1.0, -24.0], [12.0, 5.0, 16.0], [176.0, 44.0]),
            cube([-8.0, -8.0, -10.0], [16.0; 3], [112.0, 30.0]),
            cube([-5.0, -12.0, -4.0], [2.0, 4.0, 6.0], [0.0, 0.0]),
            cube([3.0, -12.0, -4.0], [2.0, 4.0, 6.0], [0.0, 0.0]),
            cube([-5.0, -3.0, -22.0], [2.0, 2.0, 4.0], [112.0, 0.0]),
            cube([3.0, -3.0, -22.0], [2.0, 2.0, 4.0], [112.0, 0.0]),
        ],
        vec![ModelPartDefinition::part(
            "jaw",
            // SkullSpecialRenderer's static animation position is zero, for
            // which DragonHeadModel sets the jaw X rotation to 0.2 radians.
            PartPose::offset_and_rotation(0.0, 4.0, -8.0, 0.2, 0.0, 0.0),
            vec![cube([-6.0, 0.0, -16.0], [12.0, 4.0, 16.0], [176.0, 65.0])],
            vec![],
        )],
    )])
}

fn piglin_head_parts() -> ModelPartDefinition {
    let cube = |origin, size, uv| CubeDefinition::new(origin, size, uv, [64.0; 2]);
    ModelPartDefinition::root(vec![ModelPartDefinition::part(
        "head",
        PartPose::IDENTITY,
        vec![
            cube([-5.0, -8.0, -4.0], [10.0, 8.0, 8.0], [0.0, 0.0]),
            cube([-2.0, -4.0, -5.0], [4.0, 4.0, 1.0], [31.0, 1.0]),
            cube([2.0, -2.0, -5.0], [1.0, 2.0, 1.0], [2.0, 4.0]),
            cube([-3.0, -2.0, -5.0], [1.0, 2.0, 1.0], [2.0, 0.0]),
        ],
        vec![
            ModelPartDefinition::part(
                "left_ear",
                // PiglinHeadModel at static animation position zero:
                // -(cos(0) + 2.5) * 0.2 and its mirrored counterpart.
                PartPose::offset_and_rotation(4.5, -6.0, 0.0, 0.0, 0.0, -0.7),
                vec![cube([0.0, 0.0, -2.0], [1.0, 5.0, 4.0], [51.0, 6.0])],
                vec![],
            ),
            ModelPartDefinition::part(
                "right_ear",
                PartPose::offset_and_rotation(-4.5, -6.0, 0.0, 0.0, 0.0, 0.7),
                vec![cube([-1.0, 0.0, -2.0], [1.0, 5.0, 4.0], [39.0, 6.0])],
                vec![],
            ),
        ],
    )])
}

fn zombie_head_parts() -> ModelPartDefinition {
    let cube = |origin, size, uv| CubeDefinition::new(origin, size, uv, [64.0; 2]);
    ModelPartDefinition::root(vec![ModelPartDefinition::part(
        "head",
        PartPose::IDENTITY,
        vec![cube([-4.0, -8.0, -4.0], [8.0; 3], [0.0; 2])],
        vec![ModelPartDefinition::part(
            "hat",
            PartPose::IDENTITY,
            vec![CubeDefinition {
                deformation: [0.25; 3],
                ..cube([-4.0, -8.0, -4.0], [8.0; 3], [32.0, 0.0])
            }],
            vec![],
        )],
    )])
}

fn special_world_parts_26_1_2(
    identifier: &str,
    properties: &BTreeMap<String, String>,
) -> Result<Option<Vec<WeightedApplications>>, BlockResourceError> {
    let path = identifier_path(identifier);
    let color_prefix = |suffix: &str| path.strip_suffix(suffix).unwrap_or("white");
    let special = if path.ends_with("_bed") {
        Some(SpecialItemModel {
            family: "minecraft:bed".to_owned(),
            texture: Some(format!("minecraft:{}", color_prefix("_bed"))),
            variant: None,
            part: properties.get("part").cloned(),
        })
    } else if path.ends_with("_wall_banner") || path.ends_with("_banner") {
        let suffix = if path.ends_with("_wall_banner") {
            "_wall_banner"
        } else {
            "_banner"
        };
        Some(SpecialItemModel {
            family: "minecraft:banner".to_owned(),
            texture: None,
            variant: Some(color_prefix(suffix).to_owned()),
            part: path.ends_with("_wall_banner").then(|| "wall".to_owned()),
        })
    } else if matches!(path, "chest" | "trapped_chest" | "ender_chest")
        || path.ends_with("copper_chest")
    {
        let mut texture = if path == "trapped_chest" {
            "trapped".to_owned()
        } else if path == "ender_chest" {
            "ender".to_owned()
        } else if path.ends_with("copper_chest") {
            if path.contains("oxidized") {
                "copper_oxidized".to_owned()
            } else if path.contains("weathered") {
                "copper_weathered".to_owned()
            } else if path.contains("exposed") {
                "copper_exposed".to_owned()
            } else {
                "copper".to_owned()
            }
        } else {
            "normal".to_owned()
        };
        if let Some(kind @ ("left" | "right")) = properties.get("type").map(String::as_str) {
            texture.push('_');
            texture.push_str(kind);
        }
        Some(SpecialItemModel {
            family: "minecraft:chest".to_owned(),
            texture: Some(format!("minecraft:{texture}")),
            variant: None,
            part: properties.get("type").cloned(),
        })
    } else if path == "shulker_box" || path.ends_with("_shulker_box") {
        let texture = path
            .strip_suffix("_shulker_box")
            .filter(|color| !color.is_empty())
            .map_or_else(|| "shulker".to_owned(), |color| format!("shulker_{color}"));
        Some(SpecialItemModel {
            family: "minecraft:shulker_box".to_owned(),
            texture: Some(format!("minecraft:{texture}")),
            variant: None,
            part: None,
        })
    } else if path == "decorated_pot" {
        Some(SpecialItemModel {
            family: "minecraft:decorated_pot".to_owned(),
            texture: None,
            variant: None,
            part: None,
        })
    } else if path.contains("copper_golem_statue") {
        let oxidation = if path.contains("oxidized") {
            "copper_golem_oxidized"
        } else if path.contains("weathered") {
            "copper_golem_weathered"
        } else if path.contains("exposed") {
            "copper_golem_exposed"
        } else {
            "copper_golem"
        };
        Some(SpecialItemModel {
            family: "minecraft:copper_golem_statue".to_owned(),
            texture: Some(format!(
                "minecraft:textures/entity/copper_golem/{oxidation}.png"
            )),
            variant: properties.get("copper_golem_pose").cloned(),
            part: None,
        })
    } else if path == "conduit" {
        Some(SpecialItemModel {
            family: "minecraft:conduit".to_owned(),
            texture: None,
            variant: None,
            part: None,
        })
    } else if matches!(
        path,
        "skeleton_skull"
            | "skeleton_wall_skull"
            | "wither_skeleton_skull"
            | "wither_skeleton_wall_skull"
            | "zombie_head"
            | "zombie_wall_head"
            | "creeper_head"
            | "creeper_wall_head"
            | "piglin_head"
            | "piglin_wall_head"
            | "dragon_head"
            | "dragon_wall_head"
            | "player_head"
            | "player_wall_head"
    ) {
        let kind = if path.contains("wither_skeleton") {
            "wither_skeleton"
        } else if path.contains("skeleton") {
            "skeleton"
        } else if path.contains("zombie") {
            "zombie"
        } else if path.contains("creeper") {
            "creeper"
        } else if path.contains("piglin") {
            "piglin"
        } else if path.contains("dragon") {
            "dragon"
        } else {
            "player"
        };
        Some(SpecialItemModel {
            family: if kind == "player" {
                "minecraft:player_head".to_owned()
            } else {
                "minecraft:head".to_owned()
            },
            texture: None,
            variant: Some(kind.to_owned()),
            part: None,
        })
    } else {
        None
    };
    let Some(special) = special else {
        return Ok(None);
    };
    let mut faces = if special.family == "minecraft:copper_golem_statue" {
        special.model_faces_from_parts(copper_golem_world_parts(special.variant.as_deref()))?
    } else {
        special.model_faces()?
    };
    let transform = special_world_transform(path, properties, &special);
    for face in &mut faces {
        face.corners = face.corners.map(|corner| transform.point(corner));
        let normal = face_normal(face.corners);
        face.direction = direction_from_normal(normal);
        face.shade = if face.directional_shade {
            direction_shade(face.direction)
        } else {
            1.0
        };
        if let Some(tint) = special.tint().filter(|_| face.tint_index.is_some()) {
            face.tint_kind = TintKind::Fixed(tint);
        }
    }
    Ok(Some(vec![WeightedApplications {
        entries: vec![(
            1,
            ModelApplication {
                faces,
                solid_boxes: Vec::new(),
                x_rotation: 0,
                y_rotation: 0,
                uvlock: false,
                ambient_occlusion: false,
            },
        )],
        total_weight: 1,
    }]))
}

fn special_world_transform(
    path: &str,
    properties: &BTreeMap<String, String>,
    special: &SpecialItemModel,
) -> ItemModelTransform {
    if special.family == "minecraft:bed" {
        // BedRenderer: translation(0, 9/16, 0), X +90 degrees, then a
        // direction-dependent Z rotation about the block centre. Inventory's
        // two-node composite owns a different transform and is not reused.
        let z_rotation = properties
            .get("facing")
            .map(String::as_str)
            .map_or(180.0, |facing| match facing {
                "south" => 180.0,
                "west" => 270.0,
                "north" => 360.0,
                "east" => 450.0,
                _ => 180.0,
            });
        return transform_translation([0.0, 0.5625, 0.0])
            .multiply(transform_rotation_x(90.0))
            .multiply(transform_around_block_z(z_rotation));
    }
    let facing = properties.get("facing").map_or("north", String::as_str);
    let to_y_rot = direction_to_y_rotation(facing);
    match special.family.as_str() {
        // ChestRenderer rotates the already block-local model about the block
        // centre. It does not apply the entity/item Y/Z reflection.
        "minecraft:chest" => rotate_around_block_y(-to_y_rot),
        // BannerRenderer's placed model owns this 2/3,-2/3,-2/3 transform;
        // the separate special-item node supplies the same convention only
        // for inventory rendering.
        "minecraft:banner" => {
            let degrees = if path.ends_with("_wall_banner") {
                to_y_rot
            } else {
                rotation_segment_degrees(properties)
            };
            transform_translation([0.5, 0.0, 0.5])
                .multiply(transform_rotation_y(-degrees))
                .multiply(transform_scale([2.0 / 3.0, -2.0 / 3.0, -2.0 / 3.0]))
        }
        // ShulkerBoxRenderer.create exact direction transform. Its normal item
        // path instead gets the 180-X/.9995 transform from item JSON once.
        "minecraft:shulker_box" => shulker_world_transform(facing),
        "minecraft:decorated_pot" => rotate_around_block_y(180.0 - to_y_rot),
        "minecraft:head" | "minecraft:player_head" => {
            let reflection = transform_scale([-1.0, -1.0, 1.0]);
            if path.contains("_wall_") {
                let (step_x, step_z) = direction_steps(facing);
                transform_translation([0.5 - step_x * 0.25, 0.25, 0.5 - step_z * 0.25])
                    .multiply(transform_rotation_y(-direction_to_y_rotation(
                        opposite_direction(facing),
                    )))
                    .multiply(reflection)
            } else {
                transform_translation([0.5, 0.0, 0.5])
                    .multiply(transform_rotation_y(-rotation_segment_degrees(properties)))
                    .multiply(reflection)
            }
        }
        // The model's root already carries CopperGolemStatueModel's PI Z
        // pose. The placed renderer only translates and turns it by facing.
        "minecraft:copper_golem_statue" => transform_translation([0.5, 0.0, 0.5]).multiply(
            transform_rotation_y(-direction_to_y_rotation(opposite_direction(facing))),
        ),
        // The accepted inactive conduit shell is centred by its renderer.
        "minecraft:conduit" => transform_translation([0.5; 3]),
        _ => ItemModelTransform::IDENTITY,
    }
}

fn direction_to_y_rotation(facing: &str) -> f32 {
    match facing {
        "south" => 0.0,
        "west" => 90.0,
        "north" => 180.0,
        "east" => 270.0,
        _ => 0.0,
    }
}

fn opposite_direction(facing: &str) -> &str {
    match facing {
        "north" => "south",
        "south" => "north",
        "east" => "west",
        "west" => "east",
        "up" => "down",
        "down" => "up",
        _ => facing,
    }
}

fn direction_steps(facing: &str) -> (f32, f32) {
    match facing {
        "north" => (0.0, -1.0),
        "south" => (0.0, 1.0),
        "east" => (1.0, 0.0),
        "west" => (-1.0, 0.0),
        _ => (0.0, 0.0),
    }
}

fn rotation_segment_degrees(properties: &BTreeMap<String, String>) -> f32 {
    properties
        .get("rotation")
        .and_then(|value| value.parse::<u8>().ok())
        .map_or(0.0, |segment| f32::from(segment % 16) * 22.5)
}

fn shulker_world_transform(facing: &str) -> ItemModelTransform {
    let rotation = match facing {
        "down" => transform_rotation_x(180.0),
        "north" => transform_rotation_x(90.0).multiply(transform_rotation_z(180.0)),
        "south" => transform_rotation_x(90.0),
        "west" => transform_rotation_x(90.0).multiply(transform_rotation_z(90.0)),
        "east" => transform_rotation_x(90.0).multiply(transform_rotation_z(-90.0)),
        _ => ItemModelTransform::IDENTITY,
    };
    transform_translation([0.5; 3])
        .multiply(transform_scale([0.9995; 3]))
        .multiply(rotation)
        .multiply(transform_scale([1.0, -1.0, -1.0]))
        .multiply(transform_translation([0.0, -1.0, 0.0]))
}

fn transform_translation(value: [f32; 3]) -> ItemModelTransform {
    ItemModelTransform([
        [1.0, 0.0, 0.0, value[0]],
        [0.0, 1.0, 0.0, value[1]],
        [0.0, 0.0, 1.0, value[2]],
        [0.0, 0.0, 0.0, 1.0],
    ])
}

fn transform_rotation_x(degrees: f32) -> ItemModelTransform {
    let (sin, cos) = degrees.to_radians().sin_cos();
    ItemModelTransform([
        [1.0, 0.0, 0.0, 0.0],
        [0.0, cos, -sin, 0.0],
        [0.0, sin, cos, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ])
}

fn transform_rotation_y(degrees: f32) -> ItemModelTransform {
    let (sin, cos) = degrees.to_radians().sin_cos();
    ItemModelTransform([
        [cos, 0.0, sin, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [-sin, 0.0, cos, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ])
}

fn transform_rotation_z(degrees: f32) -> ItemModelTransform {
    let (sin, cos) = degrees.to_radians().sin_cos();
    ItemModelTransform([
        [cos, -sin, 0.0, 0.0],
        [sin, cos, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ])
}

fn transform_scale(value: [f32; 3]) -> ItemModelTransform {
    ItemModelTransform([
        [value[0], 0.0, 0.0, 0.0],
        [0.0, value[1], 0.0, 0.0],
        [0.0, 0.0, value[2], 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ])
}

fn transform_around_block_z(degrees: f32) -> ItemModelTransform {
    let (sin, cos) = degrees.to_radians().sin_cos();
    ItemModelTransform([
        [cos, -sin, 0.0, 0.5 - 0.5 * cos + 0.5 * sin],
        [sin, cos, 0.0, 0.5 - 0.5 * sin - 0.5 * cos],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ])
}

fn rotate_around_block_y(degrees: f32) -> ItemModelTransform {
    let (sin, cos) = degrees.to_radians().sin_cos();
    ItemModelTransform([
        [cos, 0.0, sin, 0.5 - 0.5 * cos - 0.5 * sin],
        [0.0, 1.0, 0.0, 0.0],
        [-sin, 0.0, cos, 0.5 + 0.5 * sin - 0.5 * cos],
        [0.0, 0.0, 0.0, 1.0],
    ])
}

fn direction_from_normal(normal: [f32; 3]) -> Direction {
    let (axis, value) = normal
        .into_iter()
        .enumerate()
        .max_by(|(_, left), (_, right)| {
            left.abs()
                .partial_cmp(&right.abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap_or((1, 1.0));
    match (axis, value.is_sign_negative()) {
        (0, true) => Direction::West,
        (0, false) => Direction::East,
        (1, true) => Direction::Down,
        (1, false) => Direction::Up,
        (2, true) => Direction::North,
        _ => Direction::South,
    }
}

fn identifier_path(value: &str) -> &str {
    value.split_once(':').map_or(value, |(_, path)| path)
}

fn gui_item_render_key(
    model: &MinecraftIdentifier,
    block_state: &BTreeMap<String, String>,
) -> String {
    let mut key = model.to_string();
    if !block_state.is_empty() {
        key.push_str("|block_state");
        for (name, value) in block_state {
            key.push('|');
            key.push_str(name);
            key.push('=');
            key.push_str(value);
        }
    }
    key
}

fn append_component_key(key: &mut String, name: &str, bytes: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    key.push('|');
    key.push_str(name);
    key.push('=');
    for byte in bytes {
        key.push(char::from(HEX[usize::from(byte >> 4)]));
        key.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
}

fn banner_pattern_texture(identifier: &MinecraftIdentifier) -> String {
    let (namespace, path) = identifier
        .as_str()
        .split_once(':')
        .unwrap_or(("minecraft", identifier.as_str()));
    format!("{namespace}:entity/banner/{path}")
}

/// Shared static banner compositor used by both GUI items and placed world
/// banners. It clones only the base flag geometry, preserving its exact UVs,
/// winding, and transforms, then swaps the runtime pattern sprite and tint.
fn composed_banner_pattern_faces(
    base_faces: &[ModelFace],
    layers: &[BannerPatternLayer],
    first_tint_index: usize,
) -> Vec<(ModelFace, u32)> {
    let flag = base_faces
        .iter()
        .filter(|face| {
            face.tint_index == Some(0) && face.texture == "minecraft:entity/banner/banner_base"
        })
        .collect::<Vec<_>>();
    layers
        .iter()
        .take(16)
        .enumerate()
        .flat_map(|(layer_index, layer)| {
            flag.iter().map(move |face| {
                let mut face = (*face).clone();
                face.texture = layer.texture.clone();
                face.material = TextureMaterial::BannerPattern;
                face.tint_index = u32::try_from(first_tint_index + layer_index).ok();
                face.render_layer = RenderLayer::Cutout;
                (face, layer.tint)
            })
        })
        .collect()
}

fn decode_banner_pattern_layers(
    bytes: &[u8],
    registry: Option<BannerPatternRegistry<'_>>,
) -> Result<BannerPatternLayers, BlockResourceError> {
    fn varint(bytes: &[u8], offset: &mut usize) -> Option<u32> {
        let mut value = 0_u32;
        for shift in (0..35).step_by(7) {
            let byte = *bytes.get(*offset)?;
            *offset += 1;
            value |= u32::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Some(value);
            }
        }
        None
    }
    fn string<'a>(bytes: &'a [u8], offset: &mut usize, max: usize) -> Option<&'a str> {
        let length = usize::try_from(varint(bytes, offset)?).ok()?;
        if length > max {
            return None;
        }
        let end = offset.checked_add(length)?;
        let value = std::str::from_utf8(bytes.get(*offset..end)?).ok()?;
        *offset = end;
        Some(value)
    }
    if bytes.len() > 128 {
        return Err(malformed(
            "banner patterns",
            "component",
            "component exceeds its bounded wire size",
        ));
    }
    let mut offset = 0_usize;
    let count = varint(bytes, &mut offset)
        .and_then(|count| usize::try_from(count).ok())
        .filter(|count| *count <= 20)
        .ok_or_else(|| malformed("banner patterns", "component", "pattern count is invalid"))?;
    let mut layers = Vec::with_capacity(count);
    for _ in 0..count {
        let encoded = varint(bytes, &mut offset).ok_or_else(|| {
            malformed(
                "banner patterns",
                "component",
                "pattern registry holder ID is truncated",
            )
        })?;
        let pattern = if encoded == 0 {
            let asset = string(bytes, &mut offset, 1_024).ok_or_else(|| {
                malformed(
                    "banner patterns",
                    "component",
                    "direct pattern asset identifier is malformed",
                )
            })?;
            let asset = MinecraftIdentifier::new(asset).map_err(|_| {
                malformed(
                    "banner patterns",
                    "component",
                    "direct pattern asset identifier is invalid",
                )
            })?;
            let _translation_key = string(bytes, &mut offset, 1_024).ok_or_else(|| {
                malformed(
                    "banner patterns",
                    "component",
                    "direct pattern translation key is malformed",
                )
            })?;
            DeferredBannerPattern::Direct(asset)
        } else {
            DeferredBannerPattern::Reference(encoded - 1)
        };
        let dye = varint(bytes, &mut offset)
            .and_then(dye_color)
            .ok_or_else(|| malformed("banner patterns", "component", "dye ID is invalid"))?;
        layers.push(DeferredBannerPatternLayer { pattern, tint: dye });
    }
    if offset != bytes.len() {
        return Err(malformed(
            "banner patterns",
            "component",
            "component contains trailing bytes",
        ));
    }
    let deferred = DeferredBannerPatterns { layers };
    match registry {
        Some(registry) => deferred
            .resolve(registry)
            .map(BannerPatternLayers::Resolved),
        None => Ok(BannerPatternLayers::Deferred(deferred)),
    }
}

const fn dye_color(raw_id: u32) -> Option<u32> {
    Some(match raw_id {
        0 => 0xf9_ff_fe,
        1 => 0xf9_80_1d,
        2 => 0xc7_4e_bd,
        3 => 0x3a_b3_da,
        4 => 0xfe_d8_3d,
        5 => 0x80_c7_1f,
        6 => 0xf3_8b_aa,
        7 => 0x47_4f_52,
        8 => 0x9d_9d_97,
        9 => 0x16_9c_9c,
        10 => 0x89_32_b8,
        11 => 0x3c_44_aa,
        12 => 0x835432,
        13 => 0x5e7c16,
        14 => 0xb0_2e_26,
        15 => 0x1d_1d_21,
        _ => return None,
    })
}

fn normalize_special_texture(value: &str) -> String {
    let path = identifier_path(value)
        .strip_prefix("textures/")
        .unwrap_or(identifier_path(value))
        .strip_suffix(".png")
        .unwrap_or_else(|| {
            identifier_path(value)
                .strip_prefix("textures/")
                .unwrap_or(identifier_path(value))
        });
    format!("minecraft:{path}")
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
struct SpecialCuboid {
    bounds: [[f32; 3]; 2],
    texture_offset: [f32; 2],
    texture_size: [f32; 2],
}

#[cfg(test)]
impl SpecialCuboid {
    const fn new(bounds: [[f32; 3]; 2], texture_offset: [f32; 2], texture_size: [f32; 2]) -> Self {
        Self {
            bounds,
            texture_offset,
            texture_size,
        }
    }
}

#[cfg(test)]
fn special_box(spec: SpecialCuboid) -> ElementWire {
    let bounds = spec.bounds;
    let size = [
        (bounds[1][0] - bounds[0][0]).abs(),
        (bounds[1][1] - bounds[0][1]).abs(),
        (bounds[1][2] - bounds[0][2]).abs(),
    ];
    let [u, v] = spec.texture_offset;
    let [dx, dy, dz] = size;
    let u0 = u;
    let u1 = u0 + dz;
    let u2 = u1 + dx;
    let u3 = u2 + dx;
    let u4 = u2 + dz;
    let u5 = u4 + dx;
    let v0 = v;
    let v1 = v0 + dz;
    let v2 = v1 + dy;
    let uv = |value: [f32; 4]| {
        Some([
            value[0] * 16.0 / spec.texture_size[0],
            value[1] * 16.0 / spec.texture_size[1],
            value[2] * 16.0 / spec.texture_size[0],
            value[3] * 16.0 / spec.texture_size[1],
        ])
    };
    let rectangles = [
        ("down", [u1, v0, u2, v1]),
        ("up", [u2, v1, u3, v0]),
        ("west", [u0, v1, u1, v2]),
        ("north", [u1, v1, u2, v2]),
        ("east", [u2, v1, u4, v2]),
        ("south", [u4, v1, u5, v2]),
    ];
    ElementWire {
        from: bounds[0],
        to: bounds[1],
        rotation: None,
        shade: true,
        faces: rectangles
            .into_iter()
            .map(|(direction, rectangle)| {
                (
                    direction.to_owned(),
                    FaceWire {
                        uv: uv(rectangle),
                        texture: "#special".to_owned(),
                        cullface: None,
                        rotation: 0,
                        tintindex: None,
                    },
                )
            })
            .collect(),
    }
}

#[cfg(test)]
fn collect_gui_item_models(
    value: &Value,
    depth: usize,
    output: &mut Vec<StaticItemModel>,
) -> Result<(), BlockResourceError> {
    collect_gui_item_models_with_block_state(value, depth, &BTreeMap::new(), output)
}

fn collect_gui_item_models_with_block_state(
    value: &Value,
    depth: usize,
    block_state: &BTreeMap<String, String>,
    output: &mut Vec<StaticItemModel>,
) -> Result<(), BlockResourceError> {
    collect_gui_item_models_transformed(
        value,
        depth,
        block_state,
        ItemModelTransform::IDENTITY,
        output,
    )
}

fn collect_gui_item_models_transformed(
    value: &Value,
    depth: usize,
    block_state: &BTreeMap<String, String>,
    inherited_transform: ItemModelTransform,
    output: &mut Vec<StaticItemModel>,
) -> Result<(), BlockResourceError> {
    if depth >= MAX_MODEL_DEPTH || output.len() >= 32 {
        return Err(malformed(
            "item model",
            "graph",
            "model graph limit exceeded",
        ));
    }
    let object = value
        .as_object()
        .ok_or_else(|| malformed("item model", "graph", "node is not an object"))?;
    let kind = object
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| malformed("item model", "graph", "node type is missing"))?;
    let local_transform = inherited_transform.multiply(ItemModelTransform::from_value(
        object.get("transformation"),
    )?);
    match kind {
        "minecraft:model" => {
            let model = object
                .get("model")
                .and_then(Value::as_str)
                .ok_or_else(|| malformed("item model", kind, "model identity is missing"))?;
            let tints = object
                .get("tints")
                .and_then(Value::as_array)
                .map(|values| values.iter().map(default_item_tint).collect())
                .unwrap_or_default();
            output.push(StaticItemModel {
                model: model.to_owned(),
                tints,
                fallback_kind: None,
                special: None,
                local_transform,
            });
        }
        "minecraft:composite" => {
            let children = object
                .get("models")
                .and_then(Value::as_array)
                .ok_or_else(|| malformed("item model", kind, "composite models are missing"))?;
            for child in children {
                collect_gui_item_models_transformed(
                    child,
                    depth + 1,
                    block_state,
                    local_transform,
                    output,
                )?;
            }
        }
        "minecraft:condition" => {
            let start = output.len();
            let child = object
                .get("on_false")
                .ok_or_else(|| malformed("item model", kind, "false branch is missing"))?;
            collect_gui_item_models_transformed(
                child,
                depth + 1,
                block_state,
                local_transform,
                output,
            )?;
            for model in &mut output[start..] {
                model.fallback_kind = Some("item condition used its bounded inactive fallback");
            }
        }
        "minecraft:select" => {
            let selected_value = object
                .get("property")
                .and_then(Value::as_str)
                .filter(|property| *property == "minecraft:block_state")
                .and_then(|_| object.get("block_state_property"))
                .and_then(Value::as_str)
                .and_then(|property| block_state.get(property));
            let gui_case = object
                .get("cases")
                .and_then(Value::as_array)
                .and_then(|cases| {
                    cases.iter().find(|case| {
                        case.get("when").is_some_and(|when| match when {
                            Value::String(value) => {
                                selected_value.is_some_and(|selected| value == selected)
                                    || (selected_value.is_none() && value == "gui")
                            }
                            Value::Array(values) => values.iter().any(|value| {
                                selected_value.is_some_and(|selected| value == selected)
                                    || (selected_value.is_none() && value == "gui")
                            }),
                            _ => false,
                        })
                    })
                })
                .and_then(|case| case.get("model"));
            let used_fallback = gui_case.is_none();
            let child = gui_case.or_else(|| object.get("fallback")).ok_or_else(|| {
                malformed("item model", kind, "select has no GUI case or fallback")
            })?;
            let start = output.len();
            collect_gui_item_models_transformed(
                child,
                depth + 1,
                block_state,
                local_transform,
                output,
            )?;
            if used_fallback {
                for model in &mut output[start..] {
                    model.fallback_kind = Some("item select used its bounded fallback");
                }
            }
        }
        "minecraft:range_dispatch" => {
            // The resource loader prepares the ordinary, idle default stack
            // state. Every current numeric property is zero in that state;
            // use the same greatest-threshold-not-above-value selection as
            // the item-model dispatcher before consulting its fallback.
            let child = object
                .get("entries")
                .and_then(Value::as_array)
                .and_then(|entries| {
                    entries
                        .iter()
                        .filter_map(|entry| {
                            let threshold = entry.get("threshold")?.as_f64()?;
                            (threshold <= 0.0).then_some((threshold, entry.get("model")?))
                        })
                        .max_by(|left, right| left.0.total_cmp(&right.0))
                        .map(|(_, model)| model)
                })
                .or_else(|| object.get("fallback"))
                .ok_or_else(|| {
                    malformed(
                        "item model",
                        kind,
                        "range dispatch has no value-zero entry or fallback",
                    )
                })?;
            collect_gui_item_models_transformed(
                child,
                depth + 1,
                block_state,
                local_transform,
                output,
            )?;
        }
        "minecraft:special" => {
            let base = object
                .get("base")
                .and_then(Value::as_str)
                .ok_or_else(|| malformed("item model", kind, "special model base is missing"))?;
            output.push(StaticItemModel {
                model: base.to_owned(),
                tints: Vec::new(),
                fallback_kind: None,
                special: Some(SpecialItemModel::from_value(
                    object.get("model").ok_or_else(|| {
                        malformed("item model", kind, "special renderer is missing")
                    })?,
                )?),
                local_transform,
            });
        }
        "minecraft:empty" => {}
        _ => {
            return Err(malformed(
                "item model",
                kind,
                "unsupported dynamic GUI model form",
            ));
        }
    }
    Ok(())
}

fn default_item_tint(value: &Value) -> u32 {
    let object = value.as_object();
    let kind = object
        .and_then(|value| value.get("type"))
        .and_then(Value::as_str);
    let raw = object
        .and_then(|value| value.get("value").or_else(|| value.get("default")))
        .and_then(Value::as_i64)
        .unwrap_or(-1);
    match kind {
        Some(
            "minecraft:constant" | "minecraft:potion" | "minecraft:map_color" | "minecraft:dye",
        ) => raw as u32 & 0x00ff_ffff,
        Some("minecraft:grass") => 0x7cbd6b,
        _ => 0xff_ff_ff,
    }
}

fn sample_icon(
    source: &[u8],
    width: u32,
    height: u32,
    icon_size: u32,
    tint: Option<u32>,
) -> Vec<u8> {
    let icon_size = icon_size as usize;
    let mut output = vec![0_u8; icon_size * icon_size * 4];
    let width = width.max(1) as usize;
    let height = height.max(1) as usize;
    for y in 0..icon_size {
        for x in 0..icon_size {
            let sx = x * width / icon_size;
            let sy = y * height / icon_size;
            let source_index = (sy * width + sx) * 4;
            let target = (y * icon_size + x) * 4;
            if let Some(pixel) = source.get(source_index..source_index + 4) {
                output[target..target + 4].copy_from_slice(pixel);
                if let Some(tint) = tint {
                    output[target] =
                        ((u16::from(output[target]) * ((tint >> 16) & 255) as u16) / 255) as u8;
                    output[target + 1] =
                        ((u16::from(output[target + 1]) * ((tint >> 8) & 255) as u16) / 255) as u8;
                    output[target + 2] =
                        ((u16::from(output[target + 2]) * (tint & 255) as u16) / 255) as u8;
                }
            }
        }
    }
    output
}

fn composite_rgba(target: &mut [u8], source: &[u8]) {
    let (target_pixels, _) = target.as_chunks_mut::<4>();
    let (source_pixels, _) = source.as_chunks::<4>();
    for (target, source) in target_pixels.iter_mut().zip(source_pixels) {
        let source_alpha = u32::from(source[3]);
        let target_alpha = u32::from(target[3]);
        let inverse = 255 - source_alpha;
        let output_alpha = source_alpha + target_alpha * inverse / 255;
        for channel in 0..3 {
            let premultiplied = u32::from(source[channel]) * source_alpha
                + u32::from(target[channel]) * target_alpha * inverse / 255;
            target[channel] = premultiplied.checked_div(output_alpha).unwrap_or(0) as u8;
        }
        target[3] = output_alpha as u8;
    }
}

fn rasterize_gui_model_transformed<S: VanillaResourceSource>(
    loader: &mut Loader<'_, S>,
    model: &ResolvedModel,
    tints: &[u32],
    local_transform: ItemModelTransform,
    icon_size: u32,
) -> Result<GuiSpriteData, BlockResourceError> {
    let transform = model.gui_transform.unwrap_or(DisplayTransformWire {
        rotation: [0.0; 3],
        translation: [0.0; 3],
        scale: [1.0; 3],
    });
    let faces = bake_model(model)?;
    rasterize_gui_faces(
        loader,
        &faces,
        tints,
        model.gui_light_side,
        local_transform,
        transform,
        icon_size,
    )
}

#[allow(clippy::too_many_arguments)]
fn rasterize_gui_faces<S: VanillaResourceSource>(
    loader: &mut Loader<'_, S>,
    faces: &[ModelFace],
    tints: &[u32],
    gui_light_side: bool,
    local_transform: ItemModelTransform,
    transform: DisplayTransformWire,
    icon_size: u32,
) -> Result<GuiSpriteData, BlockResourceError> {
    let physical_size = icon_size as usize;
    let mut rgba = vec![0_u8; physical_size * physical_size * 4];
    let mut depth = vec![f32::NEG_INFINITY; physical_size * physical_size];
    for face in faces {
        let image = loader.load_texture(&face.texture)?;
        let Some(frame) = image.frames.first() else {
            continue;
        };
        let transformed = face
            .corners
            .map(|corner| transform_item_point(local_transform.point(corner), transform));
        let points = transformed.map(|point| project_transformed_item_point(point, icon_size));
        let brightness = if gui_light_side {
            items_3d_brightness(gui_item_atlas_normal(
                face_normal(transformed),
                local_transform,
                transform,
            ))
        } else {
            1.0
        };
        let tint = face
            .tint_index
            .and_then(|index| tints.get(index as usize))
            .copied();
        for triangle in [[0, 1, 2], [0, 2, 3]] {
            raster_item_triangle(
                &mut rgba,
                &mut depth,
                &points,
                &face.uv,
                triangle,
                frame,
                image.width,
                image.height,
                physical_size,
                tint,
                brightness,
            );
        }
    }
    Ok(GuiSpriteData {
        width: icon_size,
        height: icon_size,
        rgba,
    })
}

#[cfg(test)]
fn project_item_point(
    point: [f32; 3],
    transform: DisplayTransformWire,
    icon_size: u32,
) -> [f32; 3] {
    // ItemTransform.apply builds T * rotationXYZ * S * T(-0.5). JOML's
    // rotationXYZ quaternion is qx*qy*qz, so a column vector observes Z, Y,
    // then X. The JSON translation is parsed in model units (/16).
    project_transformed_item_point(transform_item_point(point, transform), icon_size)
}

fn transform_item_point(point: [f32; 3], transform: DisplayTransformWire) -> [f32; 3] {
    let mut point = [
        (point[0] - 0.5) * transform.scale[0],
        (point[1] - 0.5) * transform.scale[1],
        (point[2] - 0.5) * transform.scale[2],
    ];
    for axis in [2, 1, 0] {
        let (sin, cos) = transform.rotation[axis].to_radians().sin_cos();
        point = match axis {
            0 => [
                point[0],
                point[1] * cos - point[2] * sin,
                point[1] * sin + point[2] * cos,
            ],
            1 => [
                point[0] * cos + point[2] * sin,
                point[1],
                -point[0] * sin + point[2] * cos,
            ],
            _ => [
                point[0] * cos - point[1] * sin,
                point[0] * sin + point[1] * cos,
                point[2],
            ],
        };
    }
    point = [
        point[0] + transform.translation[0] / 16.0,
        point[1] + transform.translation[1] / 16.0,
        point[2] + transform.translation[2] / 16.0,
    ];
    point
}

fn project_transformed_item_point(point: [f32; 3], icon_size: u32) -> [f32; 3] {
    let size = icon_size as f32;
    [size * (0.5 + point[0]), size * (0.5 - point[1]), point[2]]
}

fn face_normal(points: [[f32; 3]; 4]) -> [f32; 3] {
    let a = vector_sub(points[1], points[0]);
    let b = vector_sub(points[2], points[0]);
    normalize_vector([
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ])
}

fn gui_item_atlas_normal(
    geometric_normal: [f32; 3],
    local_transform: ItemModelTransform,
    display_transform: DisplayTransformWire,
) -> [f32; 3] {
    // A cross product of already transformed vertices acquires the sign of a
    // reflecting transform, while PoseStack's normal matrix does not. Remove
    // that determinant sign before applying GuiItemAtlas' exact outer
    // `(slot_size, -slot_size, slot_size)` normal transform. Lighting::ITEMS_3D
    // already constructs its fixed lights in this same atlas coordinate space.
    let determinant =
        local_transform.determinant3() * display_transform.scale.iter().product::<f32>();
    let orientation = if determinant < 0.0 { -1.0 } else { 1.0 };
    normalize_vector([
        geometric_normal[0] * orientation,
        -geometric_normal[1] * orientation,
        geometric_normal[2] * orientation,
    ])
}

fn vector_sub(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
}

fn normalize_vector(value: [f32; 3]) -> [f32; 3] {
    let length = value.iter().map(|axis| axis * axis).sum::<f32>().sqrt();
    if length <= f32::EPSILON {
        [0.0, 1.0, 0.0]
    } else {
        value.map(|axis| axis / length)
    }
}

fn matrix_multiply(left: [[f32; 3]; 3], right: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    std::array::from_fn(|row| {
        std::array::from_fn(|column| {
            (0..3)
                .map(|index| left[row][index] * right[index][column])
                .sum()
        })
    })
}

fn rotation_yxz(y: f32, x: f32, z: f32) -> [[f32; 3]; 3] {
    let (sy, cy) = y.sin_cos();
    let (sx, cx) = x.sin_cos();
    let (sz, cz) = z.sin_cos();
    let ry = [[cy, 0.0, sy], [0.0, 1.0, 0.0], [-sy, 0.0, cy]];
    let rx = [[1.0, 0.0, 0.0], [0.0, cx, -sx], [0.0, sx, cx]];
    let rz = [[cz, -sz, 0.0], [sz, cz, 0.0], [0.0, 0.0, 1.0]];
    matrix_multiply(matrix_multiply(ry, rx), rz)
}

fn transform_direction(matrix: [[f32; 3]; 3], value: [f32; 3]) -> [f32; 3] {
    normalize_vector(std::array::from_fn(|row| {
        (0..3)
            .map(|column| matrix[row][column] * value[column])
            .sum()
    }))
}

fn items_3d_light_directions() -> [[f32; 3]; 2] {
    let mut matrix = [[1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, 1.0]];
    matrix = matrix_multiply(matrix, rotation_yxz(1.082_104_1, 3.237_585_8, 0.0));
    matrix = matrix_multiply(
        matrix,
        rotation_yxz(
            -std::f32::consts::FRAC_PI_8,
            3.0 * std::f32::consts::FRAC_PI_4,
            0.0,
        ),
    );
    [
        transform_direction(matrix, normalize_vector([0.2, 1.0, -0.7])),
        transform_direction(matrix, normalize_vector([-0.2, 1.0, 0.7])),
    ]
}

fn items_3d_brightness(normal: [f32; 3]) -> f32 {
    let positive = items_3d_light_directions()
        .iter()
        .map(|light| (light[0] * normal[0] + light[1] * normal[1] + light[2] * normal[2]).max(0.0))
        .sum::<f32>();
    (positive * 0.6 + 0.4).min(1.0)
}

#[allow(clippy::too_many_arguments)]
fn raster_item_triangle(
    target: &mut [u8],
    depth_buffer: &mut [f32],
    points: &[[f32; 3]; 4],
    uvs: &[[f32; 2]; 4],
    indices: [usize; 3],
    texture: &[u8],
    width: u32,
    height: u32,
    physical_size: usize,
    tint: Option<u32>,
    shade: f32,
) {
    let [a, b, c] = indices.map(|index| points[index]);
    let area = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
    if area.abs() < 1.0e-5 {
        return;
    }
    let maximum = physical_size.saturating_sub(1) as f32;
    let min_x = a[0].min(b[0]).min(c[0]).floor().clamp(0.0, maximum) as usize;
    let max_x = a[0].max(b[0]).max(c[0]).ceil().clamp(0.0, maximum) as usize;
    let min_y = a[1].min(b[1]).min(c[1]).floor().clamp(0.0, maximum) as usize;
    let max_y = a[1].max(b[1]).max(c[1]).ceil().clamp(0.0, maximum) as usize;
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let p = [x as f32 + 0.5, y as f32 + 0.5];
            let w0 = ((b[0] - p[0]) * (c[1] - p[1]) - (b[1] - p[1]) * (c[0] - p[0])) / area;
            let w1 = ((c[0] - p[0]) * (a[1] - p[1]) - (c[1] - p[1]) * (a[0] - p[0])) / area;
            let w2 = 1.0 - w0 - w1;
            if w0 < -0.001 || w1 < -0.001 || w2 < -0.001 {
                continue;
            }
            let depth = a[2] * w0 + b[2] * w1 + c[2] * w2;
            let depth_index = y * physical_size + x;
            if depth < depth_buffer[depth_index] {
                continue;
            }
            let uv = [
                uvs[indices[0]][0] * w0 + uvs[indices[1]][0] * w1 + uvs[indices[2]][0] * w2,
                uvs[indices[0]][1] * w0 + uvs[indices[1]][1] * w1 + uvs[indices[2]][1] * w2,
            ];
            let sx = (uv[0].clamp(0.0, 0.999_999) * width as f32) as usize;
            let sy = (uv[1].clamp(0.0, 0.999_999) * height as f32) as usize;
            let source = (sy * width as usize + sx) * 4;
            if source + 4 > texture.len() || texture[source + 3] == 0 {
                continue;
            }
            let mut pixel = [
                texture[source],
                texture[source + 1],
                texture[source + 2],
                texture[source + 3],
            ];
            if let Some(tint) = tint {
                pixel[0] = ((u16::from(pixel[0]) * ((tint >> 16) & 255) as u16) / 255) as u8;
                pixel[1] = ((u16::from(pixel[1]) * ((tint >> 8) & 255) as u16) / 255) as u8;
                pixel[2] = ((u16::from(pixel[2]) * (tint & 255) as u16) / 255) as u8;
            }
            for channel in pixel.iter_mut().take(3) {
                *channel = (f32::from(*channel) * shade).round().clamp(0.0, 255.0) as u8;
            }
            let offset = (y * physical_size + x) * 4;
            composite_rgba(&mut target[offset..offset + 4], &pixel);
            depth_buffer[depth_index] = depth;
        }
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
                material: TextureMaterial::Terrain,
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
        gui_transform: None,
        gui_light_side: true,
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

fn decode_gui_png(bytes: &[u8]) -> Result<GuiSpriteData, BlockResourceError> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(Transformations::EXPAND | Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .map_err(|error| malformed("GUI sprite", "png", error.to_string()))?;
    let info = reader.info();
    if info.width == 0 || info.height == 0 || info.width > 4096 || info.height > 4096 {
        return Err(malformed("GUI sprite", "png", "dimensions exceed limits"));
    }
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| malformed("GUI sprite", "png", "decoded size overflow"))?;
    if size > 64 * 1024 * 1024 {
        return Err(malformed(
            "GUI sprite",
            "png",
            "decoded image exceeds limit",
        ));
    }
    let mut output = vec![0; size];
    let frame = reader
        .next_frame(&mut output)
        .map_err(|error| malformed("GUI sprite", "png", error.to_string()))?;
    let raw = &output[..frame.buffer_size()];
    let channels = match frame.color_type {
        ColorType::Rgba => 4,
        ColorType::Rgb => 3,
        ColorType::GrayscaleAlpha => 2,
        ColorType::Grayscale => 1,
        _ => return Err(malformed("GUI sprite", "png", "unsupported color format")),
    };
    let capacity = usize::try_from(frame.width.saturating_mul(frame.height).saturating_mul(4))
        .map_err(|_| malformed("GUI sprite", "png", "decoded size overflow"))?;
    let mut rgba = Vec::with_capacity(capacity);
    for pixel in raw.chunks_exact(channels) {
        match channels {
            4 => rgba.extend_from_slice(pixel),
            3 => rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]),
            2 => rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], pixel[1]]),
            1 => rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], 255]),
            _ => {}
        }
    }
    if rgba.len() != capacity {
        return Err(malformed(
            "GUI sprite",
            "png",
            "decoded pixel count mismatch",
        ));
    }
    Ok(GuiSpriteData {
        width: frame.width,
        height: frame.height,
        rgba,
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
    fn inventory_gui_resources_use_exact_runtime_paths_and_dimensions() {
        let fixtures = [
            ("gui/container/inventory", 256, 256),
            ("gui/container/generic_54", 256, 256),
            ("gui/container/furnace", 256, 256),
            ("gui/container/crafting_table", 256, 256),
            ("gui/sprites/hud/hotbar", 182, 22),
            ("gui/sprites/hud/hotbar_selection", 24, 23),
            ("gui/sprites/container/furnace/burn_progress", 24, 16),
            ("gui/sprites/container/furnace/lit_progress", 14, 14),
            ("gui/sprites/container/slot/helmet", 16, 16),
            ("gui/sprites/container/slot/chestplate", 16, 16),
            ("gui/sprites/container/slot/leggings", 16, 16),
            ("gui/sprites/container/slot/boots", 16, 16),
            ("gui/sprites/container/slot/shield", 16, 16),
            ("gui/sprites/container/slot_highlight_back", 18, 18),
            ("gui/sprites/container/slot_highlight_front", 18, 18),
            ("font/ascii", 128, 128),
        ];
        let mut source = MemorySource::default();
        for (name, width, height) in fixtures {
            source.insert_bytes(
                &format!("assets/minecraft/textures/{name}.png"),
                rgba_png(
                    width,
                    height,
                    &vec![0x7f; width as usize * height as usize * 4],
                ),
            );
        }
        let mut loader = Loader::new(&mut source);
        for (name, width, height) in fixtures {
            let sprite = loader
                .load_gui_sprite(&format!("minecraft:{name}"))
                .unwrap();
            assert_eq!((sprite.width, sprite.height), (width, height));
        }
    }

    #[test]
    fn item_preview_is_derived_from_exact_version_definition_model_and_texture() {
        let mut source = MemorySource::default();
        source.insert(
            "assets/minecraft/items/diamond_sword.json",
            r#"{"model":{"type":"minecraft:model","model":"minecraft:item/diamond_sword"}}"#,
        );
        source.insert(
            "assets/minecraft/models/item/diamond_sword.json",
            r##"{"textures":{"layer0":"minecraft:item/diamond_sword"}}"##,
        );
        let pixels = vec![0x7f; 16 * 16 * 4];
        source.insert_bytes(
            "assets/minecraft/textures/item/diamond_sword.png",
            rgba_png(16, 16, &pixels),
        );
        let mut loader = Loader::new(&mut source);
        let icon = loader
            .load_item_icon(&identifier("minecraft:diamond_sword"), 1)
            .unwrap();
        assert_eq!((icon.width, icon.height), (16, 16));
        assert_eq!(icon.rgba, pixels);
    }

    #[test]
    fn generated_item_parent_is_a_model_bakery_root_not_a_missing_json_resource() {
        let mut source = MemorySource::default();
        source.insert(
            "assets/minecraft/items/diamond_sword.json",
            r#"{"model":{"type":"minecraft:model","model":"minecraft:item/diamond_sword"}}"#,
        );
        source.insert(
            "assets/minecraft/models/item/diamond_sword.json",
            r##"{"parent":"minecraft:item/generated","textures":{"layer0":"minecraft:item/diamond_sword"}}"##,
        );
        source.insert(
            "assets/minecraft/models/item/generated.json",
            r#"{"parent":"builtin/generated","gui_light":"front"}"#,
        );
        let pixels = vec![0xa5; 16 * 16 * 4];
        source.insert_bytes(
            "assets/minecraft/textures/item/diamond_sword.png",
            rgba_png(16, 16, &pixels),
        );
        let mut loader = Loader::new(&mut source);
        let icon = loader
            .load_item_icon(&identifier("minecraft:diamond_sword"), 1)
            .unwrap();
        assert_eq!(icon.rgba, pixels);
        assert!(
            loader
                .models
                .contains_key(&identifier("minecraft:builtin/generated"))
        );
    }

    #[test]
    fn current_item_graph_selects_gui_branches_and_bounded_static_fallbacks() {
        let graph = serde_json::json!({
            "type": "minecraft:composite",
            "models": [
                {"type": "minecraft:model", "model": "minecraft:item/plain"},
                {"type": "minecraft:condition", "property": "minecraft:using_item",
                 "on_true": {"type": "minecraft:model", "model": "minecraft:item/using"},
                 "on_false": {"type": "minecraft:model", "model": "minecraft:item/idle"}},
                {"type": "minecraft:select", "property": "minecraft:display_context",
                 "cases": [{"when": "gui", "model": {"type": "minecraft:model", "model": "minecraft:item/gui"}}],
                 "fallback": {"type": "minecraft:model", "model": "minecraft:item/fallback"}},
                {"type": "minecraft:range_dispatch", "property": "minecraft:damage",
                 "fallback": {"type": "minecraft:model", "model": "minecraft:item/range_fallback"}},
                {"type": "minecraft:special", "base": "minecraft:item/shield",
                 "model": {"type": "minecraft:shield"}}
            ]
        });
        let mut models = Vec::new();
        collect_gui_item_models(&graph, 0, &mut models).unwrap();
        assert_eq!(
            models
                .iter()
                .map(|model| model.model.as_str())
                .collect::<Vec<_>>(),
            [
                "minecraft:item/plain",
                "minecraft:item/idle",
                "minecraft:item/gui",
                "minecraft:item/range_fallback",
                "minecraft:item/shield",
            ]
        );
        assert_eq!(
            default_item_tint(&serde_json::json!({
                "type": "minecraft:map_color",
                "default": 0x12_34_56
            })),
            0x12_34_56
        );
        assert_eq!(
            default_item_tint(&serde_json::json!({
                "type": "minecraft:dye",
                "default": 0xab_cd_ef
            })),
            0xab_cd_ef
        );
        assert_eq!(
            models[4]
                .special
                .as_ref()
                .map(|model| model.family.as_str()),
            Some("minecraft:shield")
        );
    }

    #[test]
    fn special_item_families_route_to_distinct_runtime_textures_and_geometry() {
        let cases = [
            (
                serde_json::json!({"type":"minecraft:chest","texture":"minecraft:copper"}),
                "minecraft:entity/chest/copper",
                3,
            ),
            (
                serde_json::json!({"type":"minecraft:bed","part":"head","texture":"minecraft:red"}),
                "minecraft:entity/bed/red",
                3,
            ),
            (
                serde_json::json!({"type":"minecraft:banner","color":"black"}),
                "minecraft:entity/banner/banner_base",
                3,
            ),
            (
                serde_json::json!({"type":"minecraft:shulker_box","texture":"minecraft:shulker_white"}),
                "minecraft:entity/shulker/shulker_white",
                2,
            ),
            (
                serde_json::json!({"type":"minecraft:head","kind":"creeper"}),
                "minecraft:entity/creeper/creeper",
                1,
            ),
            (
                serde_json::json!({"type":"minecraft:player_head"}),
                "minecraft:entity/player/wide/steve",
                2,
            ),
            (
                serde_json::json!({"type":"minecraft:shield"}),
                "minecraft:entity/shield/shield_base_nopattern",
                2,
            ),
            (
                serde_json::json!({"type":"minecraft:decorated_pot"}),
                "minecraft:entity/decorated_pot/decorated_pot_base",
                8,
            ),
            (
                serde_json::json!({"type":"minecraft:copper_golem_statue","texture":"minecraft:textures/entity/copper_golem/copper_golem.png"}),
                "minecraft:entity/copper_golem/copper_golem",
                9,
            ),
        ];
        for (wire, texture, boxes) in cases {
            let special = SpecialItemModel::from_value(&wire).unwrap();
            assert_eq!(special.texture_identifier().unwrap(), texture);
            assert_eq!(special.geometry().len(), boxes);
            assert!(
                special
                    .geometry()
                    .iter()
                    .all(|element| element.faces.len() == 6)
            );
        }
    }

    #[test]
    fn special_model_parts_and_item_graph_transforms_match_verified_layers() {
        let chest = SpecialItemModel::from_value(&serde_json::json!({
            "type":"minecraft:chest", "texture":"minecraft:normal"
        }))
        .unwrap();
        let chest_parts = chest.geometry();
        assert_eq!(chest_parts[0].from, [1.0, 0.0, 1.0]);
        assert_eq!(chest_parts[0].to, [15.0, 10.0, 15.0]);
        assert_eq!(chest_parts[1].from, [1.0, 9.0, 1.0]);
        assert_eq!(chest_parts[2].from, [7.0, 7.0, 15.0]);

        let bed_head = SpecialItemModel::from_value(&serde_json::json!({
            "type":"minecraft:bed", "part":"head", "texture":"minecraft:red"
        }))
        .unwrap()
        .geometry();
        assert_eq!(bed_head[0].from, [0.0, 0.0, 0.0]);
        assert_eq!(bed_head[0].to, [16.0, 16.0, 6.0]);
        assert_eq!(
            (bed_head[1].from, bed_head[1].to),
            ([0.0, 0.0, 6.0], [3.0, 3.0, 9.0])
        );
        assert_eq!(
            (bed_head[2].from, bed_head[2].to),
            ([13.0, 0.0, 6.0], [16.0, 3.0, 9.0])
        );
        let bed_foot = SpecialItemModel::from_value(&serde_json::json!({
            "type":"minecraft:bed", "part":"foot", "texture":"minecraft:red"
        }))
        .unwrap()
        .geometry();
        assert_eq!(
            (bed_foot[1].from, bed_foot[1].to),
            ([0.0, 13.0, 6.0], [3.0, 16.0, 9.0])
        );
        assert_eq!(
            (bed_foot[2].from, bed_foot[2].to),
            ([13.0, 13.0, 6.0], [16.0, 16.0, 9.0])
        );

        let shulker = SpecialItemModel::from_value(&serde_json::json!({
            "type":"minecraft:shulker_box", "texture":"minecraft:shulker"
        }))
        .unwrap()
        .geometry();
        assert_eq!(
            (shulker[0].from, shulker[0].to),
            ([-8.0, 8.0, -8.0], [8.0, 20.0, 8.0])
        );
        assert_eq!(
            (shulker[1].from, shulker[1].to),
            ([-8.0, 16.0, -8.0], [8.0, 24.0, 8.0])
        );

        let pot = SpecialItemModel::from_value(&serde_json::json!({
            "type":"minecraft:decorated_pot"
        }))
        .unwrap()
        .geometry();
        assert_eq!(pot.len(), 8);
        assert_eq!(
            pot.iter()
                .filter(|part| {
                    let size = [
                        part.to[0] - part.from[0],
                        part.to[1] - part.from[1],
                        part.to[2] - part.from[2],
                    ];
                    size.into_iter().any(|axis| axis.abs() <= f32::EPSILON)
                })
                .count(),
            6
        );

        let copper = SpecialItemModel::from_value(&serde_json::json!({
            "type":"minecraft:copper_golem_statue",
            "texture":"minecraft:textures/entity/copper_golem/copper_golem.png",
            "pose":"star"
        }))
        .unwrap();
        assert_eq!(copper.variant.as_deref(), Some("star"));
        assert_eq!(copper.geometry().len(), 9);

        let dragon = SpecialItemModel::from_value(&serde_json::json!({
            "type":"minecraft:head", "kind":"dragon"
        }))
        .unwrap();
        assert_eq!(dragon.geometry().len(), 7);
        let piglin = SpecialItemModel::from_value(&serde_json::json!({
            "type":"minecraft:head", "kind":"piglin"
        }))
        .unwrap();
        assert_eq!(piglin.geometry().len(), 6);

        let graph = serde_json::json!({
            "type":"minecraft:composite",
            "models":[{
                "type":"minecraft:special",
                "base":"minecraft:item/red_bed",
                "model":{"type":"minecraft:bed","texture":"minecraft:red","part":"head"},
                "transformation":{
                    "translation":[1.0,0.5625,1.0],
                    "left_rotation":[0.0,-0.7,0.7,0.0]
                }
            },{
                "type":"minecraft:special",
                "base":"minecraft:item/red_bed",
                "model":{"type":"minecraft:bed","texture":"minecraft:red","part":"foot"},
                "transformation":{"translation":[1.0,0.5625,0.0]}
            }]
        });
        let mut models = Vec::new();
        collect_gui_item_models(&graph, 0, &mut models).unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(
            models[0].special.as_ref().unwrap().part.as_deref(),
            Some("head")
        );
        assert_eq!(
            models[1].special.as_ref().unwrap().part.as_deref(),
            Some("foot")
        );
        assert_eq!(
            models[0].local_transform.point([0.0; 3]),
            [1.0, 0.5625, 1.0]
        );
        assert_eq!(
            models[1].local_transform.point([0.0; 3]),
            [1.0, 0.5625, 0.0]
        );
    }

    #[test]
    fn shared_model_parts_drive_inventory_and_world_special_geometry() {
        let chest = SpecialItemModel::from_value(&serde_json::json!({
            "type":"minecraft:chest", "texture":"minecraft:normal"
        }))
        .unwrap();
        let names = chest
            .model_parts()
            .bake()
            .into_iter()
            .map(|quad| quad.part)
            .collect::<BTreeSet<_>>();
        assert_eq!(names, BTreeSet::from(["bottom", "lid", "lock"]));

        let world = special_world_parts_26_1_2(
            "minecraft:chest",
            &BTreeMap::from([
                ("facing".to_owned(), "east".to_owned()),
                ("type".to_owned(), "single".to_owned()),
            ]),
        )
        .unwrap()
        .unwrap();
        assert_eq!(world.len(), 1);
        assert_eq!(world[0].entries[0].1.faces.len(), 18);
        assert!(
            world[0].entries[0]
                .1
                .faces
                .iter()
                .all(|face| face.texture == "minecraft:entity/chest/normal")
        );
        assert!(world[0].entries[0].1.faces.iter().all(|face| {
            face.corners
                .iter()
                .flatten()
                .all(|axis| (-0.001..=1.001).contains(axis))
        }));
    }

    #[test]
    fn placed_special_models_use_their_verified_renderer_transforms() {
        let properties = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect::<BTreeMap<_, _>>()
        };
        let special = |family: &str| SpecialItemModel {
            family: family.to_owned(),
            texture: None,
            variant: None,
            part: None,
        };
        let close = |left: [f32; 3], right: [f32; 3]| {
            left.into_iter()
                .zip(right)
                .all(|(left, right)| (left - right).abs() < 1.0e-5)
        };

        // ChestRenderer's SOUTH transform is identity; this specifically
        // prevents the old world-path Y/Z reflection from returning.
        assert!(close(
            special_world_transform(
                "chest",
                &properties(&[("facing", "south")]),
                &special("minecraft:chest"),
            )
            .point([0.125, 0.25, 0.75]),
            [0.125, 0.25, 0.75],
        ));

        // A standing banner's authored -42-pixel pole endpoint becomes the
        // exact 1.75-block top after the renderer's -2/3 Y scale.
        assert!(close(
            special_world_transform(
                "white_banner",
                &properties(&[("rotation", "0")]),
                &special("minecraft:banner"),
            )
            .point([0.0, -42.0 / 16.0, 0.0]),
            [0.5, 1.75, 0.5],
        ));

        // Every Direction.getRotation branch keeps the shulker model's pivot
        // at the centre while changing only its opening direction.
        for facing in ["down", "up", "north", "south", "west", "east"] {
            assert!(close(
                shulker_world_transform(facing).point([0.0, 1.0, 0.0]),
                [0.5; 3],
            ));
        }

        let ground_skull = special_world_transform(
            "skeleton_skull",
            &properties(&[("rotation", "0")]),
            &special("minecraft:head"),
        );
        assert!(close(ground_skull.point([0.0; 3]), [0.5, 0.0, 0.5]));
        let wall_skull = special_world_transform(
            "skeleton_wall_skull",
            &properties(&[("facing", "north")]),
            &special("minecraft:head"),
        );
        assert!(close(wall_skull.point([0.0; 3]), [0.5, 0.25, 0.75]));

        let statue = special_world_transform(
            "copper_golem_statue",
            &properties(&[("facing", "north")]),
            &special("minecraft:copper_golem_statue"),
        );
        assert!(close(statue.point([0.0; 3]), [0.5, 0.0, 0.5]));
    }

    #[test]
    fn shared_special_layers_preserve_hierarchy_planes_and_pose_selection() {
        let banner = SpecialItemModel::from_value(&serde_json::json!({
            "type":"minecraft:banner", "color":"white"
        }))
        .unwrap();
        let banner_parts = banner.model_parts().bake();
        assert!(banner_parts.iter().any(|quad| quad.part == "pole"));
        assert!(banner_parts.iter().any(|quad| quad.part == "bar"));
        assert!(banner_parts.iter().any(|quad| quad.part == "flag"));

        let pot_definition = decorated_pot_parts();
        let top = find_part(&pot_definition, "top").unwrap();
        let bottom = find_part(&pot_definition, "bottom").unwrap();
        let front = find_part(&pot_definition, "front").unwrap();
        assert_eq!(top.pose.translation, [1.0, 16.0, 1.0]);
        assert_eq!(top.pose.rotation, [0.0; 3]);
        assert_eq!(bottom.pose.translation, [1.0, 0.0, 1.0]);
        assert_eq!(top.cubes[0].visible_faces, crate::model_part::ALL_FACES);
        assert_eq!(bottom.cubes[0].visible_faces, crate::model_part::ALL_FACES);
        assert_eq!(front.pose.translation, [1.0, 16.0, 15.0]);
        assert_eq!(front.pose.rotation, [std::f32::consts::PI, 0.0, 0.0]);

        let pot = pot_definition.bake();
        for side in ["front", "back", "left", "right"] {
            assert_eq!(pot.iter().filter(|quad| quad.part == side).count(), 1);
        }
        assert_eq!(pot.iter().filter(|quad| quad.part == "top").count(), 6);
        assert_eq!(pot.iter().filter(|quad| quad.part == "bottom").count(), 6);
        assert!(pot.iter().any(|quad| quad.part == "neck"));

        let standing = copper_golem_parts(Some("standing")).bake();
        let running = copper_golem_parts(Some("running")).bake();
        assert_ne!(standing, running);
        assert!(running.iter().any(|quad| quad.part == "left_leg"));

        let dragon = dragon_head_parts().bake();
        assert!(
            dragon
                .iter()
                .all(|quad| matches!(quad.part, "head" | "jaw"))
        );
        assert!(dragon.iter().any(|quad| quad.part == "jaw"));
    }

    #[test]
    fn special_chest_icon_uses_family_geometry_at_physical_gui_resolution() {
        let mut source = MemorySource::default();
        source.insert(
            "assets/minecraft/items/chest.json",
            r#"{"model":{"type":"minecraft:special","base":"minecraft:item/chest","model":{"type":"minecraft:chest","texture":"minecraft:normal"}}}"#,
        );
        source.insert(
            "assets/minecraft/models/item/chest.json",
            r#"{"gui_light":"side","display":{"gui":{"rotation":[30,225,0],"scale":[0.625,0.625,0.625]}}}"#,
        );
        source.insert_bytes(
            "assets/minecraft/textures/entity/chest/normal.png",
            rgba_png(64, 64, &vec![255; 64 * 64 * 4]),
        );
        let mut loader = Loader::new(&mut source);
        let icon = loader
            .load_item_icon(&identifier("minecraft:chest"), 3)
            .unwrap();
        assert_eq!((icon.width, icon.height), (48, 48));
        assert!(
            icon.rgba
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[3] != 0)
        );
        assert!(loader.failures.is_empty());
    }

    #[test]
    fn range_dispatch_uses_the_default_stack_value_before_requiring_a_fallback() {
        let graph = serde_json::json!({
            "type": "minecraft:range_dispatch",
            "property": "minecraft:time",
            "entries": [
                {"threshold": 0.0, "model": {
                    "type": "minecraft:model", "model": "minecraft:item/clock_00"
                }},
                {"threshold": 0.5, "model": {
                    "type": "minecraft:model", "model": "minecraft:item/clock_01"
                }}
            ]
        });
        let mut models = Vec::new();
        collect_gui_item_models(&graph, 0, &mut models).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].model, "minecraft:item/clock_00");
        assert_eq!(models[0].fallback_kind, None);
    }

    #[test]
    fn block_state_select_resolves_every_light_level_and_test_block_mode() {
        let light_cases = (0..16)
            .map(|level| {
                serde_json::json!({
                    "when": level.to_string(),
                    "model": {"type":"minecraft:model", "model":format!("minecraft:item/light_{level:02}")}
                })
            })
            .collect::<Vec<_>>();
        let light = serde_json::json!({
            "type":"minecraft:select",
            "property":"minecraft:block_state",
            "block_state_property":"level",
            "cases":light_cases,
            "fallback":{"type":"minecraft:model","model":"minecraft:item/light"}
        });
        for level in 0..16 {
            let properties = BTreeMap::from([("level".to_owned(), level.to_string())]);
            let mut models = Vec::new();
            collect_gui_item_models_with_block_state(&light, 0, &properties, &mut models).unwrap();
            assert_eq!(models.len(), 1);
            assert_eq!(models[0].model, format!("minecraft:item/light_{level:02}"));
        }

        let test_block = serde_json::json!({
            "type":"minecraft:select",
            "property":"minecraft:block_state",
            "block_state_property":"mode",
            "cases":[
                {"when":"log","model":{"type":"minecraft:model","model":"minecraft:block/test_block_log"}},
                {"when":"fail","model":{"type":"minecraft:model","model":"minecraft:block/test_block_fail"}},
                {"when":"accept","model":{"type":"minecraft:model","model":"minecraft:block/test_block_accept"}}
            ],
            "fallback":{"type":"minecraft:model","model":"minecraft:block/test_block_start"}
        });
        for (mode, expected) in [
            ("start", "minecraft:block/test_block_start"),
            ("log", "minecraft:block/test_block_log"),
            ("fail", "minecraft:block/test_block_fail"),
            ("accept", "minecraft:block/test_block_accept"),
        ] {
            let properties = BTreeMap::from([("mode".to_owned(), mode.to_owned())]);
            let mut models = Vec::new();
            collect_gui_item_models_with_block_state(&test_block, 0, &properties, &mut models)
                .unwrap();
            assert_eq!(models.len(), 1);
            assert_eq!(models[0].model, expected);
        }
    }

    #[test]
    fn component_aware_gui_keys_are_stable_and_distinct() {
        let model = identifier("minecraft:test_block");
        let start = gui_item_render_key(
            &model,
            &BTreeMap::from([("mode".to_owned(), "start".to_owned())]),
        );
        let log = gui_item_render_key(
            &model,
            &BTreeMap::from([("mode".to_owned(), "log".to_owned())]),
        );
        assert_ne!(start, log);
        assert_eq!(
            start,
            gui_item_render_key(
                &model,
                &BTreeMap::from([("mode".to_owned(), "start".to_owned())]),
            )
        );
    }

    #[test]
    fn banner_pattern_component_decodes_bounded_ordered_runtime_layers() {
        let registry = cubic_version::RegistryTable {
            identifier: identifier("minecraft:banner_pattern"),
            entries: vec![
                cubic_version::RegistryEntry {
                    identifier: identifier("minecraft:stripe_bottom"),
                    raw_id: 2,
                },
                cubic_version::RegistryEntry {
                    identifier: identifier("minecraft:rhombus"),
                    raw_id: 24,
                },
            ],
        };
        let layers = decode_banner_pattern_layers(
            &[2, 25, 9, 3, 15],
            Some(BannerPatternRegistry::GameData(&registry)),
        )
        .unwrap();
        assert_eq!(
            layers,
            BannerPatternLayers::Resolved(vec![
                BannerPatternLayer {
                    texture: "minecraft:entity/banner/rhombus".to_owned(),
                    tint: 0x16_9c_9c,
                },
                BannerPatternLayer {
                    texture: "minecraft:entity/banner/stripe_bottom".to_owned(),
                    tint: 0x1d_1d_21,
                },
            ])
        );
        assert!(
            decode_banner_pattern_layers(&[21], Some(BannerPatternRegistry::GameData(&registry)))
                .is_err()
        );
        assert!(
            decode_banner_pattern_layers(
                &[1, 127, 0],
                Some(BannerPatternRegistry::GameData(&registry))
            )
            .is_err()
        );
    }

    #[test]
    fn banner_patterns_defer_without_registry_and_resolve_strictly_later() {
        let encoded = [2, 25, 9, 3, 15];
        let BannerPatternLayers::Deferred(deferred) =
            decode_banner_pattern_layers(&encoded, None).unwrap()
        else {
            panic!("missing registry must retain a deferred component");
        };
        assert_eq!(deferred.layers.len(), 2);

        let ordered = (0..=24)
            .map(|raw| identifier(&format!("minecraft:pattern_{raw}")))
            .collect::<Vec<_>>();
        let resolved = deferred
            .resolve(BannerPatternRegistry::Ordered(&ordered))
            .unwrap();
        assert_eq!(resolved[0].texture, "minecraft:entity/banner/pattern_24");
        assert_eq!(resolved[0].tint, 0x16_9c_9c);
        assert_eq!(resolved[1].texture, "minecraft:entity/banner/pattern_2");
        assert_eq!(resolved[1].tint, 0x1d_1d_21);

        let too_short = &ordered[..2];
        assert!(
            deferred
                .resolve(BannerPatternRegistry::Ordered(too_short))
                .is_err(),
            "an available registry must reject an invalid holder ID"
        );
    }

    #[test]
    fn ominous_banner_component_matches_the_verified_runtime_layer_sequence() {
        let registry = cubic_version::CreativeData::builtin_26_1_2().unwrap();
        let encoded = [8, 24, 9, 32, 8, 33, 7, 2, 8, 37, 15, 18, 8, 4, 8, 2, 15];
        let BannerPatternLayers::Resolved(layers) = decode_banner_pattern_layers(
            &encoded,
            Some(BannerPatternRegistry::Ordered(&registry.banner_patterns)),
        )
        .unwrap() else {
            panic!("exact-version registry must resolve canonical component");
        };
        let actual = layers
            .iter()
            .map(|layer| (identifier_path(&layer.texture), layer.tint))
            .collect::<Vec<_>>();
        assert_eq!(
            actual,
            [
                ("entity/banner/rhombus", 0x16_9c_9c),
                ("entity/banner/stripe_bottom", 0x9d_9d_97),
                ("entity/banner/stripe_center", 0x47_4f_52),
                ("entity/banner/border", 0x9d_9d_97),
                ("entity/banner/stripe_middle", 0x1d_1d_21),
                ("entity/banner/half_horizontal", 0x9d_9d_97),
                ("entity/banner/circle", 0x9d_9d_97),
                ("entity/banner/border", 0x1d_1d_21),
            ]
        );
    }

    #[test]
    fn placed_banner_and_item_banner_share_static_pattern_compositor() {
        let base = ModelFace {
            direction: Direction::North,
            corners: [[0.0, 0.0, 0.0]; 4],
            uv: [[0.0, 0.0]; 4],
            texture: "minecraft:entity/banner/banner_base".to_owned(),
            atlas_region: AtlasRegion {
                min: [0.0, 0.0],
                max: [1.0, 1.0],
                layer: RenderLayer::Cutout,
            },
            material: TextureMaterial::Terrain,
            cullface: None,
            tint_index: Some(0),
            tint_kind: TintKind::None,
            render_layer: RenderLayer::Cutout,
            directional_shade: false,
            shade: 1.0,
        };
        let ignored_pole = ModelFace {
            tint_index: None,
            ..base.clone()
        };
        let layers = vec![
            BannerPatternLayer {
                texture: "minecraft:entity/banner/creeper".to_owned(),
                tint: 0xb0_2e_26,
            },
            BannerPatternLayer {
                texture: "minecraft:entity/banner/border".to_owned(),
                tint: 0x1d_1d_21,
            },
        ];
        let composed = composed_banner_pattern_faces(&[base.clone(), ignored_pole], &layers, 1);
        assert_eq!(composed.len(), 2);
        assert_eq!(composed[0].0.corners, base.corners);
        assert_eq!(composed[0].0.uv, base.uv);
        assert_eq!(composed[0].0.texture, "minecraft:entity/banner/creeper");
        assert_eq!(composed[0].0.tint_index, Some(1));
        assert_eq!(composed[0].1, 0xb0_2e_26);
        assert_eq!(composed[1].0.texture, "minecraft:entity/banner/border");
        assert_eq!(composed[1].0.tint_index, Some(2));
        assert_eq!(composed[1].1, 0x1d_1d_21);
    }

    #[test]
    fn ominous_world_banner_uses_eight_real_banner_material_regions_after_remesh() {
        let creative = cubic_version::CreativeData::builtin_26_1_2().unwrap();
        let mut source = MemorySource::default();
        let pixel = rgba_png(2, 2, &[19, 47, 83, 255].repeat(4));
        source.insert_bytes(
            "assets/minecraft/textures/entity/banner/banner_base.png",
            pixel.clone(),
        );
        for asset in &creative.banner_patterns {
            let sprite = banner_pattern_texture(asset);
            let path = resource_path(&identifier(&sprite), "textures", "png").unwrap();
            source.insert_bytes(path.as_str(), pixel.clone());
        }
        let mut loader = Loader::new(&mut source);
        let banner_atlas = loader
            .build_banner_atlas(&creative.banner_patterns)
            .unwrap();
        let mut resources = BlockResources::synthetic([]);
        resources.atlas = pack_atlas(BTreeMap::from([(
            "minecraft:entity/banner/banner_base".to_owned(),
            decode_png(&pixel).unwrap(),
        )]))
        .unwrap();
        resources.banner_atlas = banner_atlas;
        let base = ModelFace {
            direction: Direction::North,
            corners: [[0.0, 0.0, 0.0]; 4],
            uv: [[0.0, 0.0]; 4],
            texture: "minecraft:entity/banner/banner_base".to_owned(),
            atlas_region: resources
                .atlas
                .exact_region("minecraft:entity/banner/banner_base")
                .unwrap(),
            material: TextureMaterial::Terrain,
            cullface: None,
            tint_index: Some(0),
            tint_kind: TintKind::None,
            render_layer: RenderLayer::Cutout,
            directional_shade: false,
            shade: 1.0,
        };
        let model = ModelApplication {
            faces: vec![base.clone()],
            solid_boxes: Vec::new(),
            x_rotation: 0,
            y_rotation: 0,
            uvlock: false,
            ambient_occlusion: false,
        };
        let mut state = StateModels {
            parts: vec![WeightedApplications {
                entries: vec![(1, model)],
                total_weight: 1,
            }],
            ..StateModels::default()
        };
        bind_banner_base_material(&mut state, &resources.banner_atlas).unwrap();
        let model = &state.parts[0].entries[0].1;
        assert_eq!(model.faces[0].material, TextureMaterial::BannerPattern);
        assert_eq!(
            Some(model.faces[0].atlas_region),
            resources.banner_atlas.exact_region(&base.texture),
        );
        assert!(
            resources
                .banner_pattern_faces(model, &[])
                .unwrap()
                .is_empty()
        );

        let assets = [
            "rhombus",
            "stripe_bottom",
            "stripe_center",
            "border",
            "stripe_middle",
            "half_horizontal",
            "circle",
            "border",
        ];
        let layers = assets
            .iter()
            .enumerate()
            .map(|(index, asset)| cubic_world::BannerPatternLayer {
                pattern: cubic_world::BannerPattern {
                    asset_id: identifier(&format!("minecraft:{asset}")),
                    translation_key: None,
                },
                dye_raw_id: [9, 8, 7, 8, 15, 8, 8, 15][index],
            })
            .collect::<Vec<_>>();
        let rebuilt = resources.banner_pattern_faces(model, &layers).unwrap();
        assert_eq!(rebuilt.len(), 8);
        for (face, asset) in rebuilt.iter().zip(assets) {
            let sprite = format!("minecraft:entity/banner/{asset}");
            assert_eq!(face.texture, sprite);
            assert_eq!(face.material, TextureMaterial::BannerPattern);
            assert_eq!(
                Some(face.atlas_region),
                resources.banner_atlas.exact_region(&sprite),
            );
            assert_ne!(face.texture, "cubic:missing");
            assert!(face.atlas_region.max[0] > face.atlas_region.min[0]);
            assert!(face.atlas_region.max[1] > face.atlas_region.min[1]);
            let x = (face.atlas_region.min[0] * resources.banner_atlas.width as f32) as usize;
            let y = (face.atlas_region.min[1] * resources.banner_atlas.height as f32) as usize;
            let offset = (y * resources.banner_atlas.width as usize + x) * 4;
            assert_eq!(
                &resources.banner_atlas.rgba[offset..offset + 4],
                &[19, 47, 83, 255]
            );
        }
        let unknown = cubic_world::BannerPatternLayer {
            pattern: cubic_world::BannerPattern {
                asset_id: identifier("minecraft:not_in_banner_atlas"),
                translation_key: None,
            },
            dye_raw_id: 0,
        };
        assert!(resources.banner_pattern_faces(model, &[unknown]).is_err());
    }

    #[test]
    fn deprecated_language_remaps_are_applied_before_inventory_filtering() {
        let mut source = MemorySource::default();
        source.insert(
            "assets/minecraft/lang/en_us.json",
            r#"{
                "item.minecraft.old_removed":"Removed",
                "item.minecraft.old_name":"Runtime Name",
                "item.minecraft.new_name":"Stale Name",
                "item.minecraft.missing_destination":"Must Disappear",
                "item.minecraft.direct":"Direct Name"
            }"#,
        );
        source.insert(
            "assets/minecraft/lang/deprecated.json",
            r#"{
                "removed":["item.minecraft.old_removed"],
                "renamed":{
                    "item.minecraft.old_name":"item.minecraft.new_name",
                    "item.minecraft.absent":"item.minecraft.missing_destination"
                }
            }"#,
        );
        let translations = Loader::new(&mut source)
            .load_inventory_translations()
            .unwrap();
        assert_eq!(
            translations
                .get("item.minecraft.new_name")
                .map(String::as_str),
            Some("Runtime Name")
        );
        assert_eq!(
            translations
                .get("item.minecraft.direct")
                .map(String::as_str),
            Some("Direct Name")
        );
        assert!(!translations.contains_key("item.minecraft.old_name"));
        assert!(!translations.contains_key("item.minecraft.old_removed"));
        assert!(!translations.contains_key("item.minecraft.missing_destination"));
    }

    #[test]
    fn banner_pattern_names_match_the_final_26_1_2_runtime_table() {
        let renamed = [
            ("flower", "Flower Charge Banner Pattern"),
            ("creeper", "Creeper Charge Banner Pattern"),
            ("skull", "Skull Charge Banner Pattern"),
            ("mojang", "Thing Banner Pattern"),
            ("globe", "Globe Banner Pattern"),
            ("piglin", "Snout Banner Pattern"),
            ("flow", "Flow Banner Pattern"),
            ("guster", "Guster Banner Pattern"),
        ];
        let mut language = serde_json::Map::new();
        let mut remaps = serde_json::Map::new();
        for (name, value) in renamed {
            let old = format!("item.minecraft.{name}_banner_pattern.new");
            let current = format!("item.minecraft.{name}_banner_pattern");
            language.insert(current.clone(), Value::String("Banner Pattern".to_owned()));
            language.insert(old.clone(), Value::String(value.to_owned()));
            remaps.insert(old, Value::String(current));
        }
        language.insert(
            "item.minecraft.field_masoned_banner_pattern".to_owned(),
            Value::String("Field Masoned Banner Pattern".to_owned()),
        );
        language.insert(
            "item.minecraft.bordure_indented_banner_pattern".to_owned(),
            Value::String("Bordure Indented Banner Pattern".to_owned()),
        );
        let mut source = MemorySource::default();
        source.insert(
            "assets/minecraft/lang/en_us.json",
            &Value::Object(language).to_string(),
        );
        source.insert(
            "assets/minecraft/lang/deprecated.json",
            &serde_json::json!({"removed":[],"renamed":remaps}).to_string(),
        );
        let translations = Loader::new(&mut source)
            .load_inventory_translations()
            .unwrap();
        for (name, value) in renamed {
            assert_eq!(
                translations
                    .get(&format!("item.minecraft.{name}_banner_pattern"))
                    .map(String::as_str),
                Some(value)
            );
        }
        assert_eq!(
            translations
                .get("item.minecraft.field_masoned_banner_pattern")
                .map(String::as_str),
            Some("Field Masoned Banner Pattern")
        );
        assert_eq!(
            translations
                .get("item.minecraft.bordure_indented_banner_pattern")
                .map(String::as_str),
            Some("Bordure Indented Banner Pattern")
        );
    }

    #[test]
    fn zombie_and_piglin_skulls_use_head_only_child_hierarchies() {
        let zombie = zombie_head_parts();
        let zombie_names = collect_part_names(&zombie);
        assert_eq!(zombie_names, ["root", "head", "hat"]);
        for forbidden in ["body", "left_arm", "right_arm", "left_leg", "right_leg"] {
            assert!(!zombie_names.contains(&forbidden));
        }

        let piglin = piglin_head_parts();
        let head = &piglin.children[0];
        let left = head
            .children
            .iter()
            .find(|part| part.name == "left_ear")
            .unwrap();
        let right = head
            .children
            .iter()
            .find(|part| part.name == "right_ear")
            .unwrap();
        assert_eq!(left.pose.translation, [4.5, -6.0, 0.0]);
        assert_eq!(right.pose.translation, [-4.5, -6.0, 0.0]);
        assert_eq!(left.cubes[0].origin, [0.0, 0.0, -2.0]);
        assert_eq!(right.cubes[0].origin, [-1.0, 0.0, -2.0]);
        assert!((left.pose.rotation[2] + 0.7).abs() < 1.0e-6);
        assert!((right.pose.rotation[2] - 0.7).abs() < 1.0e-6);
    }

    #[test]
    fn placed_copper_golem_uses_exact_pose_selected_limb_pivots() {
        let cases = [
            (
                "standing",
                [0.0, -5.0, 0.0],
                [0.0, -5.0, 0.0],
                [0.0, -5.0, 0.0],
                [4.0, -6.0, 0.0],
                [-4.0, -6.0, 0.0],
                [0.0, -6.0, 0.0],
            ),
            (
                "running",
                [-1.064, -5.0, 0.0],
                [0.936, -5.0, 0.0],
                [-3.064, -5.0, 0.0],
                [4.0, -6.0, 0.0],
                [-4.0, -6.0, 0.0],
                [0.7, -5.6, -1.8],
            ),
            (
                "sitting",
                [0.0, -3.0, 2.325],
                [2.0, -2.0, -2.075],
                [-2.1, -2.1, -2.075],
                [4.0, -5.6, -1.7],
                [-4.0, -5.6, -1.8],
                [0.0, -6.0, -0.2],
            ),
            (
                "star",
                [0.0, -5.0, 0.0],
                [1.0, -5.0, 0.0],
                [-3.0, -5.0, 0.0],
                [4.0, -6.0, 0.0],
                [-4.0, -6.0, 0.0],
                [0.0, -6.0, 0.0],
            ),
        ];
        for (pose, body, left_leg, right_leg, left_arm, right_arm, head) in cases {
            let model = copper_golem_world_parts(Some(pose));
            assert!((model.pose.rotation[2] - std::f32::consts::PI).abs() < 1.0e-6);
            assert_eq!(find_part(&model, "body").unwrap().pose.translation, body);
            assert_eq!(
                find_part(&model, "left_leg").unwrap().pose.translation,
                left_leg
            );
            assert_eq!(
                find_part(&model, "right_leg").unwrap().pose.translation,
                right_leg
            );
            assert_eq!(
                find_part(&model, "left_arm").unwrap().pose.translation,
                left_arm
            );
            assert_eq!(
                find_part(&model, "right_arm").unwrap().pose.translation,
                right_arm
            );
            assert_eq!(find_part(&model, "head").unwrap().pose.translation, head);
        }
    }

    fn collect_part_names(part: &ModelPartDefinition) -> Vec<&'static str> {
        fn visit(part: &ModelPartDefinition, output: &mut Vec<&'static str>) {
            output.push(part.name);
            for child in &part.children {
                visit(child, output);
            }
        }
        let mut output = Vec::new();
        visit(part, &mut output);
        output
    }

    fn find_part<'a>(part: &'a ModelPartDefinition, name: &str) -> Option<&'a ModelPartDefinition> {
        if part.name == name {
            return Some(part);
        }
        part.children
            .iter()
            .find_map(|child| find_part(child, name))
    }

    #[test]
    fn canonical_creative_banner_components_resolve_without_network_registry_state() {
        let creative = cubic_version::CreativeData::builtin_26_1_2().unwrap();
        assert!(!creative.banner_patterns.is_empty());
        let registry = BannerPatternRegistry::Ordered(&creative.banner_patterns);
        let mut patterned = 0_usize;
        for stack in creative
            .tabs(true)
            .iter()
            .flat_map(|tab| std::iter::once(&tab.icon).chain(&tab.items))
        {
            for component in &stack.components {
                if component.id.as_str() != "minecraft:banner_patterns" {
                    continue;
                }
                let bytes = component.decoded_value().unwrap().unwrap();
                assert!(matches!(
                    decode_banner_pattern_layers(&bytes, Some(registry)).unwrap(),
                    BannerPatternLayers::Resolved(_)
                ));
                patterned += 1;
            }
        }
        assert!(patterned > 0, "fixture must exercise Ominous Banner data");
    }

    #[test]
    fn items_3d_lighting_uses_verified_two_light_shader_formula() {
        let directions = items_3d_light_directions();
        for direction in directions {
            let length = direction.iter().map(|axis| axis * axis).sum::<f32>().sqrt();
            assert!((length - 1.0).abs() < 1.0e-5);
        }
        let values = [
            items_3d_brightness([1.0, 0.0, 0.0]),
            items_3d_brightness([-1.0, 0.0, 0.0]),
            items_3d_brightness([0.0, 1.0, 0.0]),
            items_3d_brightness([0.0, 0.0, 1.0]),
        ];
        assert!(values.iter().all(|value| (0.4..=1.0).contains(value)));
        assert!(
            values
                .windows(2)
                .any(|pair| (pair[0] - pair[1]).abs() > 0.05)
        );
    }

    #[test]
    fn canonical_block_gui_normals_match_vanilla_items_3d_coordinate_space() {
        let transform = DisplayTransformWire {
            rotation: [30.0, 225.0, 0.0],
            translation: [0.0; 3],
            scale: [0.625; 3],
        };
        let lights = items_3d_light_directions();
        let expected = [
            (
                Direction::Up,
                [0.0, -0.866_025_4, 0.5],
                [0.105_350_2, 0.939_989_57],
                1.0,
            ),
            (
                Direction::Down,
                [0.0, 0.866_025_4, -0.5],
                [-0.105_350_2, -0.939_989_57],
                0.4,
            ),
            (
                Direction::North,
                [0.707_106_77, 0.353_553_38, 0.612_372_46],
                [-0.902_520_54, -0.303_119_27],
                0.4,
            ),
            (
                Direction::South,
                [-0.707_106_77, -0.353_553_38, -0.612_372_46],
                [0.902_520_54, 0.303_119_27],
                1.0,
            ),
            (
                Direction::East,
                [-0.707_106_77, 0.353_553_38, 0.612_372_46],
                [0.417_561_95, -0.156_647_24],
                0.650_537_2,
            ),
            (
                Direction::West,
                [0.707_106_77, -0.353_553_38, -0.612_372_46],
                [-0.417_561_95, 0.156_647_24],
                0.493_988_34,
            ),
        ];
        for (direction, expected_normal, expected_dots, expected_brightness) in expected {
            let transformed = face_corners([0.0; 3], [1.0; 3], direction)
                .map(|point| transform_item_point(point, transform));
            let normal = gui_item_atlas_normal(
                face_normal(transformed),
                ItemModelTransform::IDENTITY,
                transform,
            );
            let dots = lights
                .map(|light| light[0] * normal[0] + light[1] * normal[1] + light[2] * normal[2]);
            for (actual, expected) in normal.into_iter().zip(expected_normal) {
                assert!((actual - expected).abs() < 1.0e-5, "{direction:?} normal");
            }
            for (actual, expected) in dots.into_iter().zip(expected_dots) {
                assert!((actual - expected).abs() < 1.0e-5, "{direction:?} dot");
            }
            assert!(
                (items_3d_brightness(normal) - expected_brightness).abs() < 1.0e-5,
                "{direction:?} brightness"
            );
        }
    }

    #[test]
    fn item_graph_audit_validates_unselected_branches_and_known_special_types() {
        let graph = serde_json::json!({
            "type": "minecraft:condition",
            "property": "minecraft:using_item",
            "on_false": {"type": "minecraft:model", "model": "minecraft:item/idle"},
            "on_true": {"type": "minecraft:model", "model": "minecraft:item/active"}
        });
        let mut source = MemorySource::default();
        source.insert("assets/minecraft/models/item/idle.json", r#"{}"#);
        let mut loader = Loader::new(&mut source);
        assert!(loader.validate_item_model_graph(&graph, 0).is_err());

        loader
            .source
            .insert("assets/minecraft/models/item/active.json", r#"{}"#);
        assert!(loader.validate_item_model_graph(&graph, 0).is_ok());
        assert!(
            loader
                .validate_item_model_graph(
                    &serde_json::json!({"type": "minecraft:bundle/selected_item"}),
                    0,
                )
                .is_ok()
        );
    }

    #[test]
    fn block_item_uses_gui_transformed_model_geometry_instead_of_flat_texture() {
        let mut source = MemorySource::default();
        source.insert(
            "assets/minecraft/items/stone.json",
            r#"{"model":{"type":"minecraft:model","model":"minecraft:block/stone"}}"#,
        );
        source.insert(
            "assets/minecraft/models/block/stone.json",
            r##"{
                "gui_light":"side",
                "display":{"gui":{"rotation":[30,225,0],"scale":[0.625,0.625,0.625]}},
                "textures":{"all":"minecraft:block/stone"},
                "elements":[{"from":[0,0,0],"to":[16,16,16],"faces":{
                    "down":{"texture":"#all"},"up":{"texture":"#all"},
                    "north":{"texture":"#all"},"south":{"texture":"#all"},
                    "west":{"texture":"#all"},"east":{"texture":"#all"}
                }}]
            }"##,
        );
        source.insert_bytes(
            "assets/minecraft/textures/block/stone.png",
            rgba_png(16, 16, &vec![255; 16 * 16 * 4]),
        );
        let mut loader = Loader::new(&mut source);
        let icon = loader
            .load_item_icon(&identifier("minecraft:stone"), 1)
            .unwrap();
        let (pixels, _) = icon.rgba.as_chunks::<4>();
        assert!(pixels.iter().any(|pixel| pixel[3] == 0));
        assert!(pixels.iter().any(|pixel| pixel[3] == 255));
        assert!(pixels.iter().any(|pixel| pixel[0] < 255 && pixel[3] != 0));
    }

    #[test]
    fn gui_item_quads_use_per_fragment_depth_instead_of_face_average_order() {
        fn draw(target: &mut [u8], depth: &mut [f32; 16 * 16], z: f32, color: [u8; 4]) {
            let points = [
                [2.0, 2.0, z],
                [14.0, 2.0, z],
                [14.0, 14.0, z],
                [2.0, 14.0, z],
            ];
            let uvs = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
            for triangle in [[0, 1, 2], [0, 2, 3]] {
                raster_item_triangle(
                    target,
                    &mut *depth,
                    &points,
                    &uvs,
                    triangle,
                    &color,
                    1,
                    1,
                    16,
                    None,
                    1.0,
                );
            }
        }

        for near_first in [false, true] {
            let mut rgba = vec![0_u8; 16 * 16 * 4];
            let mut depth = [f32::NEG_INFINITY; 16 * 16];
            if near_first {
                draw(&mut rgba, &mut depth, 1.0, [255, 0, 0, 255]);
                draw(&mut rgba, &mut depth, 0.0, [0, 0, 255, 255]);
            } else {
                draw(&mut rgba, &mut depth, 0.0, [0, 0, 255, 255]);
                draw(&mut rgba, &mut depth, 1.0, [255, 0, 0, 255]);
            }
            assert_eq!(
                &rgba[(8 * 16 + 8) * 4..(8 * 16 + 8) * 4 + 4],
                &[255, 0, 0, 255]
            );
        }
    }

    #[test]
    fn gui_item_transform_matches_vanilla_translation_rotation_scale_and_pivot_order() {
        let block = DisplayTransformWire {
            rotation: [30.0, 225.0, 0.0],
            translation: [0.0; 3],
            scale: [0.625; 3],
        };
        let projected = project_item_point([1.0, 0.5, 0.5], block, 16);
        assert!((projected[0] - 4.464_466).abs() < 1.0e-5);
        assert!((projected[1] - 9.767_767).abs() < 1.0e-5);
        assert!((projected[2] - 0.191_366_36).abs() < 1.0e-5);

        let translated = DisplayTransformWire {
            rotation: [0.0; 3],
            translation: [16.0, -8.0, 4.0],
            scale: [1.0; 3],
        };
        assert_eq!(
            project_item_point([0.5, 0.5, 0.5], translated, 16),
            [24.0, 16.0, 0.25]
        );
    }

    #[test]
    fn gui_item_raster_dimensions_follow_physical_gui_scale_without_upscaling() {
        let mut source = MemorySource::default();
        source.insert(
            "assets/minecraft/items/test.json",
            r#"{"model":{"type":"minecraft:model","model":"minecraft:item/test"}}"#,
        );
        source.insert(
            "assets/minecraft/models/item/test.json",
            r##"{"parent":"builtin/generated","textures":{"layer0":"minecraft:item/test"}}"##,
        );
        let mut pixels = vec![0_u8; 16 * 16 * 4];
        for y in 0..16_usize {
            for x in 0..16_usize {
                let offset = (y * 16 + x) * 4;
                pixels[offset..offset + 4].copy_from_slice(&[x as u8, y as u8, 0, 255]);
            }
        }
        source.insert_bytes(
            "assets/minecraft/textures/item/test.png",
            rgba_png(16, 16, &pixels),
        );
        let mut loader = Loader::new(&mut source);
        for scale in 1_u32..=4 {
            let icon = loader
                .load_item_icon(&identifier("minecraft:test"), scale)
                .unwrap();
            let size = 16 * scale;
            assert_eq!((icon.width, icon.height), (size, size));
            assert_eq!(icon.rgba.len(), (size * size * 4) as usize);
            for texel in 0..16_u32 {
                let sample_x = texel * scale + scale / 2;
                let offset = (sample_x * 4) as usize;
                assert_eq!(icon.rgba[offset], texel as u8);
            }
        }
    }

    #[test]
    fn child_gui_transform_overrides_parent_while_absence_inherits_it() {
        let mut source = MemorySource::default();
        source.insert(
            "assets/minecraft/models/block/parent.json",
            r#"{"display":{"gui":{"rotation":[30,225,0],"translation":[1,2,3],"scale":[0.625,0.625,0.625]}}}"#,
        );
        source.insert(
            "assets/minecraft/models/block/inherited.json",
            r#"{"parent":"minecraft:block/parent"}"#,
        );
        source.insert(
            "assets/minecraft/models/block/override.json",
            r#"{"parent":"minecraft:block/parent","display":{"gui":{"rotation":[0,90,0],"translation":[4,5,6],"scale":[1,1,1]}}}"#,
        );
        let mut loader = Loader::new(&mut source);
        let inherited = loader
            .resolve_model(&identifier("minecraft:block/inherited"), &mut Vec::new())
            .unwrap()
            .gui_transform
            .unwrap();
        let overridden = loader
            .resolve_model(&identifier("minecraft:block/override"), &mut Vec::new())
            .unwrap()
            .gui_transform
            .unwrap();
        assert_eq!(inherited.rotation, [30.0, 225.0, 0.0]);
        assert_eq!(inherited.translation, [1.0, 2.0, 3.0]);
        assert_eq!(overridden.rotation, [0.0, 90.0, 0.0]);
        assert_eq!(overridden.translation, [4.0, 5.0, 6.0]);
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
            gui_transform: None,
            gui_light_side: true,
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
                gui_transform: None,
                gui_light_side: true,
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
                    gui_transform: None,
                    gui_light_side: true,
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
                gui_transform: None,
                gui_light_side: true,
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
            gui_transform: None,
            gui_light_side: true,
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
            gui_transform: None,
            gui_light_side: true,
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
