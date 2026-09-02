//! Exact-version vanilla collision-rule adapter.
//!
//! Mojang's generated `reports/blocks.json` exposes runtime IDs and state
//! properties, but not physical voxel shapes. This module is the deliberately
//! isolated behavior adapter that maps those generated facts to the verified
//! Java 26.1.2 collision families. The movement solver remains version- and
//! block-name independent.

use std::{collections::BTreeMap, sync::Arc};

use cubic_version::MinecraftVersionId;

use crate::{Aabb, CollisionShape, Vec3d};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CollisionRuleSet {
    Java26_1_2,
    Conservative,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum CollisionOffset {
    #[default]
    None,
    DeterministicHorizontal {
        max: u8,
    },
}

impl CollisionOffset {
    pub(crate) fn at(self, x: i32, z: i32) -> Vec3d {
        let Self::DeterministicHorizontal { max } = self else {
            return Vec3d::default();
        };
        // Java 26.1.2 BlockBehaviour.Properties OffsetType.XZ. Arithmetic is
        // deliberately wrapping to reproduce Java int/long overflow.
        let mixed = i64::from(x.wrapping_mul(3_129_871)) ^ i64::from(z).wrapping_mul(116_129_781);
        let seed = mixed
            .wrapping_mul(mixed)
            .wrapping_mul(42_317_861)
            .wrapping_add(mixed.wrapping_mul(11))
            >> 16;
        let maximum = f64::from(max) / 16.0;
        let component = |shift: u32| (((seed >> shift) & 15) as f64 / 15.0 - 0.5) * 0.5;
        Vec3d::new(
            component(0).clamp(-maximum, maximum),
            0.0,
            component(8).clamp(-maximum, maximum),
        )
    }

    pub(crate) fn maximum_horizontal(self) -> f64 {
        match self {
            Self::None => 0.0,
            Self::DeterministicHorizontal { max } => f64::from(max) / 16.0,
        }
    }
}

impl CollisionRuleSet {
    pub(crate) fn for_version(version: &MinecraftVersionId) -> Self {
        if version.as_str() == "26.1.2" {
            Self::Java26_1_2
        } else {
            Self::Conservative
        }
    }

    pub(crate) fn shape(self, path: &str, properties: &BTreeMap<String, String>) -> CollisionShape {
        match self {
            Self::Java26_1_2 => classify_shape(path, properties),
            Self::Conservative => {
                if matches!(path, "air" | "cave_air" | "void_air") {
                    CollisionShape::Empty
                } else {
                    CollisionShape::FullCube
                }
            }
        }
    }

    pub(crate) fn outline_shape(
        self,
        path: &str,
        properties: &BTreeMap<String, String>,
    ) -> CollisionShape {
        match self {
            Self::Java26_1_2 => classify_outline_shape(path, properties),
            Self::Conservative => self.shape(path, properties),
        }
    }

    /// Empty-hand destroy progress per 20 Hz client tick. Phase 20 can replace
    /// the baseline speed/tool-correctness inputs without changing the break
    /// state machine.
    pub(crate) fn bare_hand_destroy_progress(self, path: &str) -> f32 {
        let Some((hardness, requires_correct_tool)) = destroy_properties(path) else {
            return 0.0;
        };
        if hardness < 0.0 {
            return 0.0;
        }
        let divisor = if requires_correct_tool { 100.0 } else { 30.0 };
        1.0 / hardness / divisor
    }

    pub(crate) fn offset(self, path: &str) -> CollisionOffset {
        match self {
            Self::Java26_1_2 if matches!(path, "bamboo" | "bamboo_sapling") => {
                CollisionOffset::DeterministicHorizontal { max: 4 }
            }
            _ => CollisionOffset::None,
        }
    }

    pub(crate) fn has_verified_shape(self, path: &str) -> bool {
        matches!(self, Self::Java26_1_2) && has_verified_shape(path)
    }
}

pub(crate) fn has_verified_shape(path: &str) -> bool {
    plant_collision_family(path).is_some()
        || path.ends_with("_slab")
        || path.ends_with("_stairs")
        || path.ends_with("_door")
        || path.ends_with("_trapdoor")
        || path.ends_with("_fence")
        || path.ends_with("_fence_gate")
        || path.ends_with("_wall")
        || is_pane(path)
        || matches!(path, "ladder" | "honey_block" | "scaffolding" | "lever")
        || path.ends_with("_button")
        || is_verified_block_entity_shape(path)
        || matches!(
            path,
            "carpet"
                | "moss_carpet"
                | "pale_moss_carpet"
                | "snow"
                | "farmland"
                | "dirt_path"
                | "soul_sand"
                | "lily_pad"
                | "cactus"
        )
        || path.ends_with("_carpet")
}

pub(crate) fn classify_shape(path: &str, properties: &BTreeMap<String, String>) -> CollisionShape {
    // Both blocks are registered with noCollision in 26.1.2. Their non-empty
    // state-dependent interaction outline is intentionally handled separately.
    if path == "lever" || path.ends_with("_button") {
        return CollisionShape::Empty;
    }
    if let Some(family) = plant_collision_family(path) {
        return plant_collision_shape(family, properties);
    }
    if path.ends_with("_slab") {
        return match property(properties, "type") {
            Some("bottom") => cuboid(0.0, 0.0, 0.0, 1.0, 0.5, 1.0),
            Some("top") => cuboid(0.0, 0.5, 0.0, 1.0, 1.0, 1.0),
            _ => CollisionShape::FullCube,
        };
    }
    if path == "ladder" {
        // LadderBlock's 26.1.2 shape is a facing-dependent 3/16-thick plane.
        return match property(properties, "facing") {
            Some("north") => cuboid(0.0, 0.0, 13.0 / 16.0, 1.0, 1.0, 1.0),
            Some("south") => cuboid(0.0, 0.0, 0.0, 1.0, 1.0, 3.0 / 16.0),
            Some("west") => cuboid(13.0 / 16.0, 0.0, 0.0, 1.0, 1.0, 1.0),
            Some("east") => cuboid(0.0, 0.0, 0.0, 3.0 / 16.0, 1.0, 1.0),
            _ => CollisionShape::FullCube,
        };
    }
    if path == "honey_block" {
        // HoneyBlock.column(14, 0, 15).
        return cuboid(
            1.0 / 16.0,
            0.0,
            1.0 / 16.0,
            15.0 / 16.0,
            15.0 / 16.0,
            15.0 / 16.0,
        );
    }
    if path == "scaffolding" {
        // Its collision is supplied by the movement collision context: a
        // stable top while approached from above, or empty while descending.
        return CollisionShape::Empty;
    }
    if path == "pale_moss_carpet" && property(properties, "bottom") == Some("false") {
        return CollisionShape::Empty;
    }
    if path.ends_with("_carpet") || path == "moss_carpet" {
        return cuboid(0.0, 0.0, 0.0, 1.0, 1.0 / 16.0, 1.0);
    }
    if path == "snow" {
        let layers = properties
            .get("layers")
            .and_then(|value| value.parse::<u8>().ok())
            .unwrap_or(1)
            .clamp(1, 8);
        // 26.1.2 SnowLayerBlock indexes the collision array at layers - 1.
        // One visual layer therefore has no physical collision height.
        let height = f64::from(layers - 1) / 8.0;
        return if height == 0.0 {
            CollisionShape::Empty
        } else {
            cuboid(0.0, 0.0, 0.0, 1.0, height, 1.0)
        };
    }
    if path.ends_with("_stairs") {
        return stair_shape(properties);
    }
    if path.ends_with("_trapdoor") {
        return trapdoor_shape(properties);
    }
    if path.ends_with("_door") {
        return door_shape(properties);
    }
    if path.ends_with("_fence_gate") {
        return fence_gate_shape(properties);
    }
    if path.ends_with("_fence") {
        return cross_shape(properties, 4.0 / 16.0, 24.0 / 16.0);
    }
    if path.ends_with("_wall") {
        return wall_shape(properties);
    }
    if is_pane(path) {
        return cross_shape(properties, 2.0 / 16.0, 1.0);
    }
    if matches!(path, "chest" | "trapped_chest") {
        return chest_shape(properties);
    }
    if path == "ender_chest" {
        return column(14.0 / 16.0, 0.0, 14.0 / 16.0);
    }
    if path.ends_with("_bed") {
        return bed_shape(properties);
    }
    if path == "hopper" {
        return hopper_shape(properties);
    }
    if path == "bell" {
        return bell_shape(properties);
    }
    match path {
        "brewing_stand" => boxes(vec![
            centered_column(2.0 / 16.0, 2.0 / 16.0, 14.0 / 16.0),
            centered_column(14.0 / 16.0, 0.0, 2.0 / 16.0),
        ]),
        "lectern" => boxes(vec![
            centered_column(1.0, 0.0, 2.0 / 16.0),
            centered_column(8.0 / 16.0, 2.0 / 16.0, 14.0 / 16.0),
        ]),
        "decorated_pot" => column(14.0 / 16.0, 0.0, 1.0),
        "enchanting_table" => column(1.0, 0.0, 12.0 / 16.0),
        "campfire" | "soul_campfire" => column(1.0, 0.0, 7.0 / 16.0),
        "farmland" | "dirt_path" => cuboid(0.0, 0.0, 0.0, 1.0, 15.0 / 16.0, 1.0),
        "soul_sand" => cuboid(0.0, 0.0, 0.0, 1.0, 14.0 / 16.0, 1.0),
        "lily_pad" => cuboid(0.0, 0.0, 0.0, 1.0, 1.0 / 16.0, 1.0),
        "cactus" => cuboid(1.0 / 16.0, 0.0, 1.0 / 16.0, 15.0 / 16.0, 1.0, 15.0 / 16.0),
        _ => CollisionShape::FullCube,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PlantCollisionFamily {
    Empty,
    Azalea,
    Bamboo,
    BambooSapling,
    BigDripleaf,
    ChorusPlant,
    ChorusFlower,
    Cocoa,
    FlowerPot,
    PitcherCrop,
    SeaPickle,
    SnifferEgg,
    TurtleEgg,
}

fn plant_collision_family(path: &str) -> Option<PlantCollisionFamily> {
    use PlantCollisionFamily as Family;

    let family = match path {
        "azalea" | "flowering_azalea" => Family::Azalea,
        "bamboo" => Family::Bamboo,
        "bamboo_sapling" => Family::BambooSapling,
        "big_dripleaf" => Family::BigDripleaf,
        "chorus_plant" => Family::ChorusPlant,
        "chorus_flower" => Family::ChorusFlower,
        "cocoa" => Family::Cocoa,
        "flower_pot" => Family::FlowerPot,
        "pitcher_crop" => Family::PitcherCrop,
        "sea_pickle" => Family::SeaPickle,
        "sniffer_egg" => Family::SnifferEgg,
        "turtle_egg" => Family::TurtleEgg,
        _ if path.starts_with("potted_") => Family::FlowerPot,
        _ if is_empty_plant_collision(path) => Family::Empty,
        _ => return None,
    };
    Some(family)
}

fn plant_collision_shape(
    family: PlantCollisionFamily,
    properties: &BTreeMap<String, String>,
) -> CollisionShape {
    use PlantCollisionFamily as Family;

    match family {
        Family::Empty => CollisionShape::Empty,
        Family::Azalea => boxes(vec![
            centered_column(1.0, 8.0 / 16.0, 1.0),
            centered_column(4.0 / 16.0, 0.0, 8.0 / 16.0),
        ]),
        Family::Bamboo => column(3.0 / 16.0, 0.0, 1.0),
        Family::BambooSapling => column(8.0 / 16.0, 0.0, 12.0 / 16.0),
        Family::BigDripleaf => match property(properties, "tilt") {
            Some("full") => CollisionShape::Empty,
            Some("partial") => cuboid(0.0, 11.0 / 16.0, 0.0, 1.0, 13.0 / 16.0, 1.0),
            Some("none" | "unstable") => cuboid(0.0, 11.0 / 16.0, 0.0, 1.0, 15.0 / 16.0, 1.0),
            _ => CollisionShape::FullCube,
        },
        Family::ChorusPlant => chorus_plant_shape(properties),
        Family::ChorusFlower => CollisionShape::FullCube,
        Family::Cocoa => cocoa_shape(properties),
        Family::FlowerPot => column(6.0 / 16.0, 0.0, 6.0 / 16.0),
        Family::PitcherCrop => pitcher_crop_shape(properties),
        Family::SeaPickle => {
            let pickles = property(properties, "pickles")
                .and_then(|value| value.parse::<u8>().ok())
                .unwrap_or(1);
            let (width, height) = match pickles {
                1 => (4.0, 6.0),
                2 => (10.0, 6.0),
                3 => (12.0, 6.0),
                4 => (12.0, 7.0),
                _ => return CollisionShape::FullCube,
            };
            column(width / 16.0, 0.0, height / 16.0)
        }
        Family::SnifferEgg => cuboid(1.0 / 16.0, 0.0, 2.0 / 16.0, 15.0 / 16.0, 1.0, 14.0 / 16.0),
        Family::TurtleEgg => {
            if property(properties, "eggs") == Some("1") {
                cuboid(
                    3.0 / 16.0,
                    0.0,
                    3.0 / 16.0,
                    12.0 / 16.0,
                    7.0 / 16.0,
                    12.0 / 16.0,
                )
            } else {
                column(14.0 / 16.0, 0.0, 7.0 / 16.0)
            }
        }
    }
}

fn cocoa_shape(properties: &BTreeMap<String, String>) -> CollisionShape {
    let Some(age) = property(properties, "age").and_then(|value| value.parse::<u8>().ok()) else {
        return CollisionShape::FullCube;
    };
    if age > 2 {
        return CollisionShape::FullCube;
    }
    let width = f64::from(4 + age * 2) / 16.0;
    let min_y = f64::from(7 - age * 2) / 16.0;
    let max_y = 12.0 / 16.0;
    let side_min = (1.0 - width) / 2.0;
    let side_max = 1.0 - side_min;
    let outward_min = 1.0 / 16.0;
    let outward_max = f64::from(5 + age * 2) / 16.0;
    match property(properties, "facing") {
        Some("north") => cuboid(side_min, min_y, outward_min, side_max, max_y, outward_max),
        Some("south") => cuboid(
            side_min,
            min_y,
            1.0 - outward_max,
            side_max,
            max_y,
            1.0 - outward_min,
        ),
        Some("west") => cuboid(outward_min, min_y, side_min, outward_max, max_y, side_max),
        Some("east") => cuboid(
            1.0 - outward_max,
            min_y,
            side_min,
            1.0 - outward_min,
            max_y,
            side_max,
        ),
        _ => CollisionShape::FullCube,
    }
}

fn pitcher_crop_shape(properties: &BTreeMap<String, String>) -> CollisionShape {
    if property(properties, "half") == Some("upper") {
        return CollisionShape::Empty;
    }
    if property(properties, "half") != Some("lower") {
        return CollisionShape::FullCube;
    }
    match property(properties, "age") {
        Some("0") => column(6.0 / 16.0, -1.0 / 16.0, 3.0 / 16.0),
        Some("1" | "2" | "3" | "4") => column(10.0 / 16.0, -1.0 / 16.0, 5.0 / 16.0),
        _ => CollisionShape::FullCube,
    }
}

fn chorus_plant_shape(properties: &BTreeMap<String, String>) -> CollisionShape {
    let min = 3.0 / 16.0;
    let max = 13.0 / 16.0;
    let mut values = vec![Aabb::new(
        Vec3d::new(min, min, min),
        Vec3d::new(max, max, max),
    )];
    if enabled(properties, "north") {
        values.push(Aabb::new(
            Vec3d::new(min, min, 0.0),
            Vec3d::new(max, max, min),
        ));
    }
    if enabled(properties, "south") {
        values.push(Aabb::new(
            Vec3d::new(min, min, max),
            Vec3d::new(max, max, 1.0),
        ));
    }
    if enabled(properties, "west") {
        values.push(Aabb::new(
            Vec3d::new(0.0, min, min),
            Vec3d::new(min, max, max),
        ));
    }
    if enabled(properties, "east") {
        values.push(Aabb::new(
            Vec3d::new(max, min, min),
            Vec3d::new(1.0, max, max),
        ));
    }
    if enabled(properties, "down") {
        values.push(Aabb::new(
            Vec3d::new(min, 0.0, min),
            Vec3d::new(max, min, max),
        ));
    }
    if enabled(properties, "up") {
        values.push(Aabb::new(
            Vec3d::new(min, max, min),
            Vec3d::new(max, 1.0, max),
        ));
    }
    boxes(values)
}

fn is_verified_block_entity_shape(path: &str) -> bool {
    matches!(
        path,
        "chest"
            | "trapped_chest"
            | "ender_chest"
            | "hopper"
            | "bell"
            | "brewing_stand"
            | "lectern"
            | "decorated_pot"
            | "enchanting_table"
            | "campfire"
            | "soul_campfire"
    ) || path.ends_with("_bed")
        || is_empty_block_entity_shape(path)
}

fn is_empty_block_entity_shape(path: &str) -> bool {
    path.ends_with("_sign")
        || path.ends_with("_hanging_sign")
        || path.ends_with("_banner")
        || path.ends_with("_head")
        || path.ends_with("_skull")
}

fn property<'a>(properties: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    properties.get(name).map(String::as_str)
}

fn enabled(properties: &BTreeMap<String, String>, name: &str) -> bool {
    property(properties, name).is_some_and(|value| value != "false" && value != "none")
}

fn is_pane(path: &str) -> bool {
    matches!(path, "iron_bars" | "glass_pane") || path.ends_with("_stained_glass_pane")
}

fn cuboid(
    min_x: f64,
    min_y: f64,
    min_z: f64,
    max_x: f64,
    max_y: f64,
    max_z: f64,
) -> CollisionShape {
    boxes(vec![Aabb::new(
        Vec3d::new(min_x, min_y, min_z),
        Vec3d::new(max_x, max_y, max_z),
    )])
}

fn centered_column(width: f64, min_y: f64, max_y: f64) -> Aabb {
    let inset = (1.0 - width) / 2.0;
    Aabb::new(
        Vec3d::new(inset, min_y, inset),
        Vec3d::new(1.0 - inset, max_y, 1.0 - inset),
    )
}

fn column(width: f64, min_y: f64, max_y: f64) -> CollisionShape {
    boxes(vec![centered_column(width, min_y, max_y)])
}

fn chest_shape(properties: &BTreeMap<String, String>) -> CollisionShape {
    let mut bounds = centered_column(14.0 / 16.0, 0.0, 14.0 / 16.0);
    let connected = match (property(properties, "type"), property(properties, "facing")) {
        (Some("left"), Some("north")) | (Some("right"), Some("south")) => Some("east"),
        (Some("left"), Some("east")) | (Some("right"), Some("west")) => Some("south"),
        (Some("left"), Some("south")) | (Some("right"), Some("north")) => Some("west"),
        (Some("left"), Some("west")) | (Some("right"), Some("east")) => Some("north"),
        (Some("single") | None, _) => None,
        _ => return CollisionShape::FullCube,
    };
    match connected {
        Some("north") => bounds.min.z = 0.0,
        Some("south") => bounds.max.z = 1.0,
        Some("west") => bounds.min.x = 0.0,
        Some("east") => bounds.max.x = 1.0,
        None => {}
        Some(_) => return CollisionShape::FullCube,
    }
    boxes(vec![bounds])
}

fn bed_shape(properties: &BTreeMap<String, String>) -> CollisionShape {
    let facing = property(properties, "facing").unwrap_or("north");
    let head = property(properties, "part") == Some("head");
    let outer = if head {
        facing
    } else {
        match facing {
            "north" => "south",
            "south" => "north",
            "west" => "east",
            _ => "west",
        }
    };
    let mut values = vec![Aabb::new(
        Vec3d::new(0.0, 3.0 / 16.0, 0.0),
        Vec3d::new(1.0, 9.0 / 16.0, 1.0),
    )];
    let (first, second) = match outer {
        "north" => ((0.0, 0.0), (13.0 / 16.0, 0.0)),
        "south" => ((0.0, 13.0 / 16.0), (13.0 / 16.0, 13.0 / 16.0)),
        "west" => ((0.0, 0.0), (0.0, 13.0 / 16.0)),
        "east" => ((13.0 / 16.0, 0.0), (13.0 / 16.0, 13.0 / 16.0)),
        _ => return CollisionShape::FullCube,
    };
    for (x, z) in [first, second] {
        values.push(Aabb::new(
            Vec3d::new(x, 0.0, z),
            Vec3d::new(x + 3.0 / 16.0, 3.0 / 16.0, z + 3.0 / 16.0),
        ));
    }
    boxes(values)
}

fn hopper_shape(properties: &BTreeMap<String, String>) -> CollisionShape {
    let mut values = vec![
        // The 26.1.2 bowl is a full 1/16-thick floor with four 2/16 walls.
        Aabb::new(
            Vec3d::new(0.0, 10.0 / 16.0, 0.0),
            Vec3d::new(1.0, 11.0 / 16.0, 1.0),
        ),
        Aabb::new(
            Vec3d::new(0.0, 11.0 / 16.0, 0.0),
            Vec3d::new(2.0 / 16.0, 1.0, 1.0),
        ),
        Aabb::new(
            Vec3d::new(14.0 / 16.0, 11.0 / 16.0, 0.0),
            Vec3d::new(1.0, 1.0, 1.0),
        ),
        Aabb::new(
            Vec3d::new(2.0 / 16.0, 11.0 / 16.0, 0.0),
            Vec3d::new(14.0 / 16.0, 1.0, 2.0 / 16.0),
        ),
        Aabb::new(
            Vec3d::new(2.0 / 16.0, 11.0 / 16.0, 14.0 / 16.0),
            Vec3d::new(14.0 / 16.0, 1.0, 1.0),
        ),
        centered_column(8.0 / 16.0, 4.0 / 16.0, 10.0 / 16.0),
    ];
    let spout = match property(properties, "facing").unwrap_or("down") {
        "north" => Aabb::new(
            Vec3d::new(6.0 / 16.0, 4.0 / 16.0, 0.0),
            Vec3d::new(10.0 / 16.0, 8.0 / 16.0, 8.0 / 16.0),
        ),
        "south" => Aabb::new(
            Vec3d::new(6.0 / 16.0, 4.0 / 16.0, 8.0 / 16.0),
            Vec3d::new(10.0 / 16.0, 8.0 / 16.0, 1.0),
        ),
        "west" => Aabb::new(
            Vec3d::new(0.0, 4.0 / 16.0, 6.0 / 16.0),
            Vec3d::new(8.0 / 16.0, 8.0 / 16.0, 10.0 / 16.0),
        ),
        "east" => Aabb::new(
            Vec3d::new(8.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0),
            Vec3d::new(1.0, 8.0 / 16.0, 10.0 / 16.0),
        ),
        "down" => Aabb::new(
            Vec3d::new(6.0 / 16.0, 0.0, 6.0 / 16.0),
            Vec3d::new(10.0 / 16.0, 8.0 / 16.0, 10.0 / 16.0),
        ),
        _ => return CollisionShape::FullCube,
    };
    values.push(spout);
    boxes(values)
}

fn bell_shape(properties: &BTreeMap<String, String>) -> CollisionShape {
    let bell = [
        centered_column(6.0 / 16.0, 6.0 / 16.0, 13.0 / 16.0),
        centered_column(8.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0),
    ];
    match property(properties, "attachment") {
        Some("floor") => match property(properties, "facing") {
            Some("north" | "south") => cuboid(0.0, 0.0, 4.0 / 16.0, 1.0, 1.0, 12.0 / 16.0),
            Some("east" | "west") => cuboid(4.0 / 16.0, 0.0, 0.0, 12.0 / 16.0, 1.0, 1.0),
            _ => CollisionShape::FullCube,
        },
        Some("ceiling") => boxes(vec![
            bell[0],
            bell[1],
            centered_column(2.0 / 16.0, 13.0 / 16.0, 1.0),
        ]),
        Some("double_wall") => {
            let support = match property(properties, "facing") {
                Some("north" | "south") => Aabb::new(
                    Vec3d::new(7.0 / 16.0, 13.0 / 16.0, 0.0),
                    Vec3d::new(9.0 / 16.0, 15.0 / 16.0, 1.0),
                ),
                Some("east" | "west") => Aabb::new(
                    Vec3d::new(0.0, 13.0 / 16.0, 7.0 / 16.0),
                    Vec3d::new(1.0, 15.0 / 16.0, 9.0 / 16.0),
                ),
                _ => return CollisionShape::FullCube,
            };
            boxes(vec![bell[0], bell[1], support])
        }
        Some("single_wall") => {
            let support = match property(properties, "facing") {
                Some("north") => Aabb::new(
                    Vec3d::new(7.0 / 16.0, 13.0 / 16.0, 0.0),
                    Vec3d::new(9.0 / 16.0, 15.0 / 16.0, 13.0 / 16.0),
                ),
                Some("east") => Aabb::new(
                    Vec3d::new(3.0 / 16.0, 13.0 / 16.0, 7.0 / 16.0),
                    Vec3d::new(1.0, 15.0 / 16.0, 9.0 / 16.0),
                ),
                Some("south") => Aabb::new(
                    Vec3d::new(7.0 / 16.0, 13.0 / 16.0, 3.0 / 16.0),
                    Vec3d::new(9.0 / 16.0, 15.0 / 16.0, 1.0),
                ),
                Some("west") => Aabb::new(
                    Vec3d::new(0.0, 13.0 / 16.0, 7.0 / 16.0),
                    Vec3d::new(13.0 / 16.0, 15.0 / 16.0, 9.0 / 16.0),
                ),
                _ => return CollisionShape::FullCube,
            };
            boxes(vec![bell[0], bell[1], support])
        }
        _ => CollisionShape::FullCube,
    }
}

fn boxes(values: Vec<Aabb>) -> CollisionShape {
    CollisionShape::Boxes(Arc::from(values))
}

fn stair_shape(properties: &BTreeMap<String, String>) -> CollisionShape {
    let top = property(properties, "half") == Some("top");
    let (base_min, base_max, step_min, step_max) = if top {
        (0.5, 1.0, 0.0, 0.5)
    } else {
        (0.0, 0.5, 0.5, 1.0)
    };
    let facing = property(properties, "facing").unwrap_or("east");
    let shape = property(properties, "shape").unwrap_or("straight");
    let mut values = vec![Aabb::new(
        Vec3d::new(0.0, base_min, 0.0),
        Vec3d::new(1.0, base_max, 1.0),
    )];
    for (x, z) in stair_quadrants(facing, shape) {
        values.push(Aabb::new(
            Vec3d::new(f64::from(x) * 0.5, step_min, f64::from(z) * 0.5),
            Vec3d::new(f64::from(x + 1) * 0.5, step_max, f64::from(z + 1) * 0.5),
        ));
    }
    boxes(values)
}

fn stair_quadrants(facing: &str, shape: &str) -> Vec<(u8, u8)> {
    let forward = match facing {
        "north" => [(0, 0), (1, 0)],
        "south" => [(0, 1), (1, 1)],
        "west" => [(0, 0), (0, 1)],
        _ => [(1, 0), (1, 1)],
    };
    let left = match facing {
        "north" => (0, 1),
        "south" => (1, 0),
        "west" => (1, 1),
        _ => (0, 0),
    };
    let right = match facing {
        "north" => (1, 1),
        "south" => (0, 0),
        "west" => (1, 0),
        _ => (0, 1),
    };
    let (forward_left, forward_right) = match facing {
        "north" => ((0, 0), (1, 0)),
        "south" => ((1, 1), (0, 1)),
        "west" => ((0, 1), (0, 0)),
        _ => ((1, 0), (1, 1)),
    };
    match shape {
        "outer_left" => vec![forward_left],
        "outer_right" => vec![forward_right],
        "inner_left" => vec![forward[0], forward[1], left],
        "inner_right" => vec![forward[0], forward[1], right],
        _ => forward.to_vec(),
    }
}

fn door_shape(properties: &BTreeMap<String, String>) -> CollisionShape {
    const T: f64 = 3.0 / 16.0;
    let facing = property(properties, "facing");
    let open = property(properties, "open") == Some("true");
    let right_hinge = property(properties, "hinge") == Some("right");
    let plane = if !open {
        match facing {
            Some("east") => 0,
            Some("south") => 1,
            Some("west") => 2,
            Some("north") => 3,
            _ => return CollisionShape::FullCube,
        }
    } else {
        match (facing, right_hinge) {
            (Some("east"), false) | (Some("west"), true) => 1,
            (Some("east"), true) | (Some("west"), false) => 3,
            (Some("north"), false) | (Some("south"), true) => 0,
            (Some("north"), true) | (Some("south"), false) => 2,
            _ => return CollisionShape::FullCube,
        }
    };
    boundary_plane(plane, T, 1.0)
}

fn trapdoor_shape(properties: &BTreeMap<String, String>) -> CollisionShape {
    const T: f64 = 3.0 / 16.0;
    if property(properties, "open") == Some("true") {
        return match property(properties, "facing") {
            Some("north") => boundary_plane(3, T, 1.0),
            Some("south") => boundary_plane(1, T, 1.0),
            Some("west") => boundary_plane(2, T, 1.0),
            Some("east") => boundary_plane(0, T, 1.0),
            _ => CollisionShape::FullCube,
        };
    }
    if property(properties, "half") == Some("top") {
        cuboid(0.0, 1.0 - T, 0.0, 1.0, 1.0, 1.0)
    } else {
        cuboid(0.0, 0.0, 0.0, 1.0, T, 1.0)
    }
}

fn classify_outline_shape(path: &str, properties: &BTreeMap<String, String>) -> CollisionShape {
    if path == "short_grass" {
        return cuboid(
            2.0 / 16.0,
            0.0,
            2.0 / 16.0,
            14.0 / 16.0,
            13.0 / 16.0,
            14.0 / 16.0,
        );
    }
    if path.ends_with("_fence") {
        return cross_shape(properties, 4.0 / 16.0, 1.0);
    }
    if path.ends_with("_button") {
        return attached_control_shape(properties, true);
    }
    if path == "lever" {
        return attached_control_shape(properties, false);
    }
    if path == "scaffolding" {
        return scaffolding_outline(properties);
    }
    match classify_shape(path, properties) {
        CollisionShape::Empty => CollisionShape::FullCube,
        shape => shape,
    }
}

fn attached_control_shape(properties: &BTreeMap<String, String>, button: bool) -> CollisionShape {
    let pressed = property(properties, "powered") == Some("true");
    let base = if button {
        // ButtonBlock starts with boxZ(6, 4, 8, 16), then subtracts a
        // centered 12/16 (unpowered) or 14/16 (powered) cube. The remaining
        // protruding part is therefore 6x4x2 or 6x4x1 sixteenths.
        let depth = if pressed { 1.0 / 16.0 } else { 2.0 / 16.0 };
        Aabb::new(
            Vec3d::new(5.0 / 16.0, 6.0 / 16.0, 1.0 - depth),
            Vec3d::new(11.0 / 16.0, 10.0 / 16.0, 1.0),
        )
    } else {
        // LeverBlock.boxZ(6, 8, 10, 16), before rotateAttachFace.
        Aabb::new(
            Vec3d::new(5.0 / 16.0, 4.0 / 16.0, 10.0 / 16.0),
            Vec3d::new(11.0 / 16.0, 12.0 / 16.0, 1.0),
        )
    };
    let x_turns = match property(properties, "face") {
        Some("wall") => 0,
        Some("floor") => 1,
        Some("ceiling") => 3,
        _ => return CollisionShape::Empty,
    };
    let y_turns = match property(properties, "facing") {
        Some("north") => 0,
        Some("east") => 1,
        Some("south") => 2,
        Some("west") => 3,
        _ => return CollisionShape::Empty,
    };
    CollisionShape::Boxes([rotate_attachment_box(base, x_turns, y_turns)].into())
}

fn rotate_attachment_box(bounds: Aabb, x_turns: u8, y_turns: u8) -> Aabb {
    let mut min = Vec3d::new(f64::INFINITY, f64::INFINITY, f64::INFINITY);
    let mut max = Vec3d::new(f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for mut point in [
        Vec3d::new(bounds.min.x, bounds.min.y, bounds.min.z),
        Vec3d::new(bounds.min.x, bounds.min.y, bounds.max.z),
        Vec3d::new(bounds.min.x, bounds.max.y, bounds.min.z),
        Vec3d::new(bounds.min.x, bounds.max.y, bounds.max.z),
        Vec3d::new(bounds.max.x, bounds.min.y, bounds.min.z),
        Vec3d::new(bounds.max.x, bounds.min.y, bounds.max.z),
        Vec3d::new(bounds.max.x, bounds.max.y, bounds.min.z),
        Vec3d::new(bounds.max.x, bounds.max.y, bounds.max.z),
    ] {
        point = Vec3d::new(point.x - 0.5, point.y - 0.5, point.z - 0.5);
        for _ in 0..x_turns {
            point = Vec3d::new(point.x, -point.z, point.y);
        }
        for _ in 0..y_turns {
            point = Vec3d::new(-point.z, point.y, point.x);
        }
        point = Vec3d::new(point.x + 0.5, point.y + 0.5, point.z + 0.5);
        min = Vec3d::new(min.x.min(point.x), min.y.min(point.y), min.z.min(point.z));
        max = Vec3d::new(max.x.max(point.x), max.y.max(point.y), max.z.max(point.z));
    }
    Aabb::new(min, max)
}

fn scaffolding_outline(properties: &BTreeMap<String, String>) -> CollisionShape {
    let t = 2.0 / 16.0;
    let mut parts = vec![
        Aabb::new(Vec3d::new(0.0, 14.0 / 16.0, 0.0), Vec3d::new(1.0, 1.0, 1.0)),
        Aabb::new(Vec3d::new(0.0, 0.0, 0.0), Vec3d::new(t, 1.0, t)),
        Aabb::new(Vec3d::new(1.0 - t, 0.0, 0.0), Vec3d::new(1.0, 1.0, t)),
        Aabb::new(Vec3d::new(0.0, 0.0, 1.0 - t), Vec3d::new(t, 1.0, 1.0)),
        Aabb::new(Vec3d::new(1.0 - t, 0.0, 1.0 - t), Vec3d::new(1.0, 1.0, 1.0)),
    ];
    if property(properties, "bottom") == Some("true") {
        parts.push(Aabb::new(
            Vec3d::new(0.0, 0.0, 0.0),
            Vec3d::new(1.0, t, 1.0),
        ));
    }
    boxes(parts)
}

fn destroy_properties(path: &str) -> Option<(f32, bool)> {
    if matches!(
        path,
        "bedrock" | "barrier" | "end_portal" | "end_portal_frame"
    ) {
        return Some((-1.0, true));
    }
    let value = if matches!(path, "dirt" | "sand" | "clay" | "gravel") {
        (0.5, false)
    } else if matches!(path, "grass_block" | "podzol" | "mycelium") {
        (0.6, false)
    } else if path.ends_with("_leaves") {
        (0.2, false)
    } else if path.contains("glass") {
        (0.3, false)
    } else if path.ends_with("_planks") || path.ends_with("_log") || path.ends_with("_wood") {
        (2.0, false)
    } else if matches!(path, "stone" | "cobblestone") {
        (1.5, true)
    } else if path.ends_with("_button") || path == "lever" {
        (0.5, false)
    } else {
        return None;
    };
    Some(value)
}

fn fence_gate_shape(properties: &BTreeMap<String, String>) -> CollisionShape {
    if property(properties, "open") == Some("true") {
        return CollisionShape::Empty;
    }
    const T: f64 = 4.0 / 16.0;
    match property(properties, "facing") {
        Some("north" | "south") => cuboid(0.0, 0.0, 0.5 - T / 2.0, 1.0, 1.5, 0.5 + T / 2.0),
        Some("east" | "west") => cuboid(0.5 - T / 2.0, 0.0, 0.0, 0.5 + T / 2.0, 1.5, 1.0),
        _ => CollisionShape::FullCube,
    }
}

fn boundary_plane(plane: u8, thickness: f64, height: f64) -> CollisionShape {
    match plane {
        0 => cuboid(0.0, 0.0, 0.0, thickness, height, 1.0),
        1 => cuboid(0.0, 0.0, 0.0, 1.0, height, thickness),
        2 => cuboid(1.0 - thickness, 0.0, 0.0, 1.0, height, 1.0),
        _ => cuboid(0.0, 0.0, 1.0 - thickness, 1.0, height, 1.0),
    }
}

fn cross_shape(properties: &BTreeMap<String, String>, width: f64, height: f64) -> CollisionShape {
    let min = 0.5 - width / 2.0;
    let max = 0.5 + width / 2.0;
    let mut values = vec![Aabb::new(
        Vec3d::new(min, 0.0, min),
        Vec3d::new(max, height, max),
    )];
    if enabled(properties, "north") {
        values.push(Aabb::new(
            Vec3d::new(min, 0.0, 0.0),
            Vec3d::new(max, height, min),
        ));
    }
    if enabled(properties, "south") {
        values.push(Aabb::new(
            Vec3d::new(min, 0.0, max),
            Vec3d::new(max, height, 1.0),
        ));
    }
    if enabled(properties, "west") {
        values.push(Aabb::new(
            Vec3d::new(0.0, 0.0, min),
            Vec3d::new(min, height, max),
        ));
    }
    if enabled(properties, "east") {
        values.push(Aabb::new(
            Vec3d::new(max, 0.0, min),
            Vec3d::new(1.0, height, max),
        ));
    }
    boxes(values)
}

fn wall_shape(properties: &BTreeMap<String, String>) -> CollisionShape {
    let mut values = Vec::with_capacity(5);
    if enabled(properties, "up") {
        values.push(Aabb::new(
            Vec3d::new(4.0 / 16.0, 0.0, 4.0 / 16.0),
            Vec3d::new(12.0 / 16.0, 1.5, 12.0 / 16.0),
        ));
    }
    let min = 5.0 / 16.0;
    let max = 11.0 / 16.0;
    if enabled(properties, "north") {
        values.push(Aabb::new(
            Vec3d::new(min, 0.0, 0.0),
            Vec3d::new(max, 1.5, 0.5),
        ));
    }
    if enabled(properties, "south") {
        values.push(Aabb::new(
            Vec3d::new(min, 0.0, 0.5),
            Vec3d::new(max, 1.5, 1.0),
        ));
    }
    if enabled(properties, "west") {
        values.push(Aabb::new(
            Vec3d::new(0.0, 0.0, min),
            Vec3d::new(0.5, 1.5, max),
        ));
    }
    if enabled(properties, "east") {
        values.push(Aabb::new(
            Vec3d::new(0.5, 0.0, min),
            Vec3d::new(1.0, 1.5, max),
        ));
    }
    if values.is_empty() {
        CollisionShape::Empty
    } else {
        boxes(values)
    }
}

fn is_empty_plant_collision(path: &str) -> bool {
    if is_empty_block_entity_shape(path) {
        return true;
    }
    matches!(
        path,
        "air"
            | "cave_air"
            | "void_air"
            | "water"
            | "lava"
            | "wheat"
            | "carrots"
            | "potatoes"
            | "beetroots"
            | "nether_wart"
            | "pumpkin_stem"
            | "melon_stem"
            | "attached_pumpkin_stem"
            | "attached_melon_stem"
            | "torchflower_crop"
            | "pitcher_plant"
            | "sweet_berry_bush"
            | "sugar_cane"
            | "brown_mushroom"
            | "red_mushroom"
            | "crimson_fungus"
            | "warped_fungus"
            | "crimson_roots"
            | "warped_roots"
            | "nether_sprouts"
            | "hanging_roots"
            | "pale_hanging_moss"
            | "mangrove_propagule"
            | "small_dripleaf"
            | "big_dripleaf_stem"
            | "cave_vines"
            | "cave_vines_plant"
            | "twisting_vines"
            | "twisting_vines_plant"
            | "weeping_vines"
            | "weeping_vines_plant"
            | "cobweb"
            | "frogspawn"
            | "short_grass"
            | "tall_grass"
            | "short_dry_grass"
            | "tall_dry_grass"
            | "fern"
            | "large_fern"
            | "dead_bush"
            | "leaf_litter"
            | "wildflowers"
            | "pink_petals"
            | "sunflower"
            | "torchflower"
            | "spore_blossom"
            | "dandelion"
            | "poppy"
            | "blue_orchid"
            | "allium"
            | "azure_bluet"
            | "oxeye_daisy"
            | "cornflower"
            | "lily_of_the_valley"
            | "wither_rose"
            | "closed_eyeblossom"
            | "open_eyeblossom"
            | "bush"
            | "firefly_bush"
            | "lilac"
            | "peony"
            | "rose_bush"
            | "vine"
            | "glow_lichen"
            | "sculk_vein"
            | "resin_clump"
            | "torch"
            | "wall_torch"
            | "redstone_torch"
            | "redstone_wall_torch"
            | "redstone_wire"
            | "tripwire"
            | "fire"
            | "soul_fire"
            | "seagrass"
            | "tall_seagrass"
            | "kelp"
            | "kelp_plant"
    ) || path.ends_with("_sapling")
        || path.ends_with("_flower")
        || path.ends_with("_coral")
        || path.ends_with("_coral_fan")
        || path.ends_with("_wall_coral_fan")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BlockCollisionProfile, RuntimeBlockStateId};
    use cubic_version::{
        GameData, GameDataProvenance, MinecraftVersionId, generate_game_data_from_reports,
    };

    fn props(values: &[(&str, &str)]) -> BTreeMap<String, String> {
        values
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn shape_boxes(shape: CollisionShape) -> Arc<[Aabb]> {
        match shape {
            CollisionShape::Boxes(values) => values,
            other => panic!("expected boxes, got {other:?}"),
        }
    }

    #[test]
    fn verified_empty_and_partial_families_match_26_1_2() {
        for path in ["wheat", "leaf_litter", "dandelion", "air"] {
            assert_eq!(
                classify_shape(path, &BTreeMap::new()),
                CollisionShape::Empty
            );
        }
        assert_eq!(
            classify_shape("snow", &props(&[("layers", "1")])),
            CollisionShape::Empty
        );
        let snow = shape_boxes(classify_shape("snow", &props(&[("layers", "8")])));
        assert_eq!(snow[0].max.y, 7.0 / 8.0);
        let path = shape_boxes(classify_shape("dirt_path", &BTreeMap::new()));
        assert_eq!(path[0].max.y, 15.0 / 16.0);
        assert_eq!(
            classify_shape("pale_moss_carpet", &props(&[("bottom", "false")])),
            CollisionShape::Empty
        );
    }

    #[test]
    fn ladders_honey_and_scaffolding_use_verified_non_full_cube_shapes() {
        let cases = [
            (
                "north",
                Aabb::new(Vec3d::new(0.0, 0.0, 13.0 / 16.0), Vec3d::new(1.0, 1.0, 1.0)),
            ),
            (
                "south",
                Aabb::new(Vec3d::new(0.0, 0.0, 0.0), Vec3d::new(1.0, 1.0, 3.0 / 16.0)),
            ),
            (
                "west",
                Aabb::new(Vec3d::new(13.0 / 16.0, 0.0, 0.0), Vec3d::new(1.0, 1.0, 1.0)),
            ),
            (
                "east",
                Aabb::new(Vec3d::new(0.0, 0.0, 0.0), Vec3d::new(3.0 / 16.0, 1.0, 1.0)),
            ),
        ];
        for (facing, expected) in cases {
            assert_eq!(
                shape_boxes(classify_shape("ladder", &props(&[("facing", facing)]))).as_ref(),
                &[expected]
            );
        }
        let honey = shape_boxes(classify_shape("honey_block", &BTreeMap::new()));
        assert_eq!(honey[0].min, Vec3d::new(1.0 / 16.0, 0.0, 1.0 / 16.0));
        assert_eq!(
            honey[0].max,
            Vec3d::new(15.0 / 16.0, 15.0 / 16.0, 15.0 / 16.0)
        );
        assert_eq!(
            classify_shape("scaffolding", &BTreeMap::new()),
            CollisionShape::Empty
        );
    }

    #[test]
    fn crop_and_vegetation_families_match_verified_26_1_2_collision_categories() {
        let empty = [
            "wheat",
            "carrots",
            "potatoes",
            "beetroots",
            "nether_wart",
            "sweet_berry_bush",
            "torchflower_crop",
            "pitcher_plant",
            "sugar_cane",
            "small_dripleaf",
            "big_dripleaf_stem",
            "oak_sapling",
            "mangrove_propagule",
            "brown_mushroom",
            "red_mushroom",
            "dandelion",
            "sunflower",
            "rose_bush",
            "short_grass",
            "tall_grass",
            "fern",
            "large_fern",
            "dead_bush",
            "vine",
            "glow_lichen",
            "sculk_vein",
            "resin_clump",
            "hanging_roots",
            "pale_hanging_moss",
            "crimson_roots",
            "warped_roots",
            "nether_sprouts",
            "cave_vines",
            "cave_vines_plant",
            "twisting_vines",
            "twisting_vines_plant",
            "weeping_vines",
            "weeping_vines_plant",
            "leaf_litter",
            "pink_petals",
            "wildflowers",
            "seagrass",
            "tall_seagrass",
            "kelp",
            "kelp_plant",
            "cobweb",
            "frogspawn",
        ];
        for path in empty {
            assert_eq!(
                classify_shape(path, &BTreeMap::new()),
                CollisionShape::Empty,
                "{path}"
            );
            assert!(has_verified_shape(path), "{path}");
        }

        for path in ["pumpkin_stem", "melon_stem"] {
            for age in 0..=7 {
                assert_eq!(
                    classify_shape(path, &props(&[("age", &age.to_string())])),
                    CollisionShape::Empty,
                    "{path} age={age}"
                );
            }
        }
        for path in ["attached_pumpkin_stem", "attached_melon_stem"] {
            for facing in ["north", "east", "south", "west"] {
                assert_eq!(
                    classify_shape(path, &props(&[("facing", facing)])),
                    CollisionShape::Empty,
                    "{path} facing={facing}"
                );
            }
        }

        for path in [
            "bamboo",
            "bamboo_sapling",
            "azalea",
            "flowering_azalea",
            "big_dripleaf",
            "chorus_plant",
            "chorus_flower",
            "cocoa",
            "flower_pot",
            "potted_oak_sapling",
            "pitcher_crop",
            "sea_pickle",
            "sniffer_egg",
            "turtle_egg",
        ] {
            assert!(has_verified_shape(path), "{path}");
        }
    }

    #[test]
    fn collidable_plant_families_preserve_state_dependent_26_1_2_bounds() {
        let bamboo = shape_boxes(classify_shape("bamboo", &BTreeMap::new()));
        assert_eq!(
            (bamboo[0].min.x, bamboo[0].max.x),
            (13.0 / 32.0, 19.0 / 32.0)
        );
        let sapling = shape_boxes(classify_shape("bamboo_sapling", &BTreeMap::new()));
        assert_eq!(
            (sapling[0].max.y, sapling[0].max.x),
            (12.0 / 16.0, 12.0 / 16.0)
        );
        let azalea = shape_boxes(classify_shape("azalea", &BTreeMap::new()));
        assert_eq!(azalea.len(), 2);
        assert_eq!((azalea[0].min.y, azalea[0].max.y), (0.5, 1.0));
        assert_eq!(
            (azalea[1].min.x, azalea[1].max.x),
            (6.0 / 16.0, 10.0 / 16.0)
        );

        for age in 0..=2 {
            for facing in ["north", "east", "south", "west"] {
                let cocoa = shape_boxes(classify_shape(
                    "cocoa",
                    &props(&[("age", &age.to_string()), ("facing", facing)]),
                ));
                assert_eq!(cocoa[0].max.y, 12.0 / 16.0);
                assert_eq!(cocoa[0].min.y, f64::from(7 - age * 2) / 16.0);
            }
        }

        let young_pitcher = shape_boxes(classify_shape(
            "pitcher_crop",
            &props(&[("age", "0"), ("half", "lower")]),
        ));
        assert_eq!(
            (young_pitcher[0].min.y, young_pitcher[0].max.y),
            (-1.0 / 16.0, 3.0 / 16.0)
        );
        let grown_pitcher = shape_boxes(classify_shape(
            "pitcher_crop",
            &props(&[("age", "4"), ("half", "lower")]),
        ));
        assert_eq!(grown_pitcher[0].max.y, 5.0 / 16.0);
        assert_eq!(
            classify_shape("pitcher_crop", &props(&[("age", "4"), ("half", "upper")])),
            CollisionShape::Empty
        );

        for (tilt, expected_height) in [
            ("none", Some(15.0 / 16.0)),
            ("unstable", Some(15.0 / 16.0)),
            ("partial", Some(13.0 / 16.0)),
            ("full", None),
        ] {
            let shape = classify_shape("big_dripleaf", &props(&[("tilt", tilt)]));
            match expected_height {
                Some(height) => assert_eq!(shape_boxes(shape)[0].max.y, height),
                None => assert_eq!(shape, CollisionShape::Empty),
            }
        }

        let chorus = shape_boxes(classify_shape(
            "chorus_plant",
            &props(&[
                ("north", "true"),
                ("east", "false"),
                ("south", "true"),
                ("west", "false"),
                ("up", "true"),
                ("down", "false"),
            ]),
        ));
        assert_eq!(chorus.len(), 4);
        assert_eq!(chorus[1].min.z, 0.0);
        assert_eq!(chorus[3].max.y, 1.0);
        assert_eq!(
            classify_shape("chorus_flower", &props(&[("age", "5")])),
            CollisionShape::FullCube
        );

        for (pickles, width, height) in [
            (1, 4.0, 6.0),
            (2, 10.0, 6.0),
            (3, 12.0, 6.0),
            (4, 12.0, 7.0),
        ] {
            let shape = shape_boxes(classify_shape(
                "sea_pickle",
                &props(&[("pickles", &pickles.to_string())]),
            ));
            assert_eq!(shape[0].max.x - shape[0].min.x, width / 16.0);
            assert_eq!(shape[0].max.y, height / 16.0);
        }

        let single_egg = shape_boxes(classify_shape("turtle_egg", &props(&[("eggs", "1")])));
        let multiple_eggs = shape_boxes(classify_shape("turtle_egg", &props(&[("eggs", "4")])));
        assert_eq!(single_egg[0].max.x, 12.0 / 16.0);
        assert_eq!(multiple_eggs[0].max.x, 15.0 / 16.0);
        let sniffer = shape_boxes(classify_shape("sniffer_egg", &BTreeMap::new()));
        assert_eq!(
            (sniffer[0].min.z, sniffer[0].max.z),
            (2.0 / 16.0, 14.0 / 16.0)
        );
        let pot = shape_boxes(classify_shape("potted_dandelion", &BTreeMap::new()));
        assert_eq!((pot[0].max.x, pot[0].max.y), (11.0 / 16.0, 6.0 / 16.0));
    }

    #[test]
    fn bamboo_collision_offset_matches_26_1_2_position_seed() {
        let rules = CollisionRuleSet::Java26_1_2;
        let offset = rules.offset("bamboo");
        assert_eq!(offset.maximum_horizontal(), 0.25);
        assert_eq!(offset.at(0, 0), Vec3d::new(-0.25, 0.0, -0.25));
        let positive = offset.at(1, 1);
        assert!((positive.x + 1.0 / 12.0).abs() < 1.0e-12);
        assert!((positive.z + 1.0 / 12.0).abs() < 1.0e-12);
        let negative = offset.at(-17, 31);
        assert!((negative.x - 1.0 / 12.0).abs() < 1.0e-12);
        assert!((negative.z + 0.15).abs() < 1.0e-12);
        assert_eq!(rules.offset("wheat"), CollisionOffset::None);
    }

    #[test]
    fn slabs_trapdoors_and_gates_follow_state_properties() {
        let lower = shape_boxes(classify_shape("stone_slab", &props(&[("type", "bottom")])));
        let upper = shape_boxes(classify_shape("stone_slab", &props(&[("type", "top")])));
        assert_eq!((lower[0].min.y, lower[0].max.y), (0.0, 0.5));
        assert_eq!((upper[0].min.y, upper[0].max.y), (0.5, 1.0));
        assert_eq!(
            classify_shape("stone_slab", &props(&[("type", "double")])),
            CollisionShape::FullCube
        );

        let closed = shape_boxes(classify_shape(
            "oak_trapdoor",
            &props(&[("open", "false"), ("half", "top"), ("facing", "north")]),
        ));
        assert_eq!(closed[0].min.y, 13.0 / 16.0);
        let open = shape_boxes(classify_shape(
            "oak_trapdoor",
            &props(&[("open", "true"), ("half", "bottom"), ("facing", "north")]),
        ));
        assert_eq!((open[0].min.z, open[0].max.z), (13.0 / 16.0, 1.0));
        assert_eq!(
            classify_shape(
                "oak_fence_gate",
                &props(&[("open", "true"), ("facing", "north")])
            ),
            CollisionShape::Empty
        );
    }

    #[test]
    fn stairs_are_multipart_for_all_verified_shape_variants() {
        let expected = [
            ("straight", 3),
            ("outer_left", 2),
            ("outer_right", 2),
            ("inner_left", 4),
            ("inner_right", 4),
        ];
        for facing in ["north", "east", "south", "west"] {
            for (shape, count) in expected {
                let values = shape_boxes(classify_shape(
                    "oak_stairs",
                    &props(&[("facing", facing), ("half", "bottom"), ("shape", shape)]),
                ));
                assert_eq!(values.len(), count, "{facing} {shape}");
            }
        }
        assert_eq!(stair_quadrants("north", "outer_left"), vec![(0, 0)]);
        assert_eq!(stair_quadrants("south", "outer_left"), vec![(1, 1)]);
        assert_eq!(stair_quadrants("west", "outer_right"), vec![(0, 0)]);
        assert_eq!(stair_quadrants("east", "outer_right"), vec![(1, 1)]);
        assert_eq!(
            stair_quadrants("north", "inner_left"),
            vec![(0, 0), (1, 0), (0, 1)]
        );
    }

    #[test]
    fn encoded_connections_select_fence_wall_and_pane_parts() {
        let connected = props(&[
            ("north", "true"),
            ("east", "false"),
            ("south", "true"),
            ("west", "false"),
        ]);
        let fence = shape_boxes(classify_shape("oak_fence", &connected));
        assert_eq!(fence.len(), 3);
        assert!(fence.iter().all(|bounds| bounds.max.y == 1.5));
        let pane = shape_boxes(classify_shape("glass_pane", &connected));
        assert_eq!(pane.len(), 3);
        assert!(pane.iter().all(|bounds| bounds.max.y == 1.0));

        let wall = shape_boxes(classify_shape(
            "cobblestone_wall",
            &props(&[("up", "true"), ("north", "low"), ("east", "none")]),
        ));
        assert_eq!(wall.len(), 2);
    }

    #[test]
    fn block_entity_bearing_states_use_physical_block_shapes_only() {
        let single = shape_boxes(classify_shape(
            "chest",
            &props(&[("type", "single"), ("facing", "north")]),
        ));
        assert_eq!(
            single.as_ref(),
            &[Aabb::new(
                Vec3d::new(1.0 / 16.0, 0.0, 1.0 / 16.0),
                Vec3d::new(15.0 / 16.0, 14.0 / 16.0, 15.0 / 16.0),
            )]
        );
        let double = shape_boxes(classify_shape(
            "trapped_chest",
            &props(&[("type", "left"), ("facing", "north")]),
        ));
        assert_eq!((double[0].min.x, double[0].max.x), (1.0 / 16.0, 1.0));

        let bed = shape_boxes(classify_shape(
            "red_bed",
            &props(&[("part", "head"), ("facing", "north")]),
        ));
        assert_eq!(bed.len(), 3);
        assert_eq!((bed[0].min.y, bed[0].max.y), (3.0 / 16.0, 9.0 / 16.0));

        let brewing_stand = shape_boxes(classify_shape("brewing_stand", &BTreeMap::new()));
        assert_eq!(brewing_stand.len(), 2);
        assert_eq!(brewing_stand[0].max.y, 14.0 / 16.0);
        let lectern = shape_boxes(classify_shape("lectern", &BTreeMap::new()));
        assert_eq!(lectern.len(), 2);
        assert_eq!(lectern[1].max.y, 14.0 / 16.0);
        let hopper = shape_boxes(classify_shape("hopper", &props(&[("facing", "east")])));
        assert_eq!(hopper.len(), 7);
        assert_eq!(hopper[6].max.x, 1.0);
        let pot = shape_boxes(classify_shape("decorated_pot", &BTreeMap::new()));
        assert_eq!((pot[0].min.x, pot[0].max.x), (1.0 / 16.0, 15.0 / 16.0));
        let bell = shape_boxes(classify_shape(
            "bell",
            &props(&[("attachment", "ceiling"), ("facing", "north")]),
        ));
        assert_eq!(bell.len(), 3);
        assert_eq!(bell[2].max.y, 1.0);

        for path in [
            "oak_sign",
            "oak_hanging_sign",
            "white_banner",
            "player_head",
        ] {
            assert_eq!(
                classify_shape(path, &BTreeMap::new()),
                CollisionShape::Empty
            );
            assert!(has_verified_shape(path));
        }
        // No block-entity payload is accepted by this API: exact runtime-state
        // identity and properties are the sole collision inputs.
        assert!(has_verified_shape("chest"));
        assert!(has_verified_shape("red_bed"));
    }

    #[test]
    fn fence_and_gate_shapes_preserve_the_verified_one_and_a_half_block_height() {
        for connected in [
            props(&[
                ("north", "false"),
                ("east", "false"),
                ("south", "false"),
                ("west", "false"),
            ]),
            props(&[
                ("north", "true"),
                ("east", "false"),
                ("south", "true"),
                ("west", "false"),
            ]),
            props(&[
                ("north", "true"),
                ("east", "true"),
                ("south", "false"),
                ("west", "false"),
            ]),
        ] {
            let fence = shape_boxes(classify_shape("oak_fence", &connected));
            assert!(
                fence
                    .iter()
                    .all(|bounds| bounds.min.y == 0.0 && bounds.max.y == 1.5)
            );
        }
        for facing in ["north", "east", "south", "west"] {
            for in_wall in ["false", "true"] {
                let closed = shape_boxes(classify_shape(
                    "oak_fence_gate",
                    &props(&[("facing", facing), ("open", "false"), ("in_wall", in_wall)]),
                ));
                assert_eq!((closed[0].min.y, closed[0].max.y), (0.0, 1.5));
                assert_eq!(
                    classify_shape(
                        "oak_fence_gate",
                        &props(&[("facing", facing), ("open", "true"), ("in_wall", in_wall)])
                    ),
                    CollisionShape::Empty
                );
            }
        }
    }

    #[test]
    fn generated_runtime_states_flow_through_the_collision_profile() {
        let registries = br#"{
            "minecraft:block":{"protocol_id":0,"entries":{
                "minecraft:air":{"protocol_id":0},
                "minecraft:wheat":{"protocol_id":1},
                "minecraft:stone_slab":{"protocol_id":2},
                "minecraft:oak_fence":{"protocol_id":3},
                "minecraft:chest":{"protocol_id":4},
                "minecraft:pumpkin_stem":{"protocol_id":5},
                "minecraft:attached_melon_stem":{"protocol_id":6}
            }}
        }"#;
        let blocks = br#"{
            "minecraft:air":{"states":[{"id":0,"default":true}]},
            "minecraft:wheat":{"properties":{"age":["0","1"]},"states":[
                {"id":1,"default":true,"properties":{"age":"0"}},
                {"id":2,"properties":{"age":"1"}}
            ]},
            "minecraft:stone_slab":{"properties":{"type":["bottom","top","double"]},"states":[
                {"id":3,"default":true,"properties":{"type":"bottom"}},
                {"id":4,"properties":{"type":"top"}},
                {"id":5,"properties":{"type":"double"}}
            ]},
            "minecraft:oak_fence":{"properties":{"north":["false","true"],"east":["false","true"],"south":["false","true"],"west":["false","true"]},"states":[
                {"id":6,"default":true,"properties":{"north":"true","east":"false","south":"true","west":"false"}}
            ]},
            "minecraft:chest":{"properties":{"type":["single"],"facing":["north"]},"states":[
                {"id":7,"default":true,"properties":{"type":"single","facing":"north"}}
            ]},
            "minecraft:pumpkin_stem":{"properties":{"age":["0","7"]},"states":[
                {"id":8,"default":true,"properties":{"age":"0"}},
                {"id":9,"properties":{"age":"7"}}
            ]},
            "minecraft:attached_melon_stem":{"properties":{"facing":["north","east"]},"states":[
                {"id":10,"default":true,"properties":{"facing":"north"}},
                {"id":11,"properties":{"facing":"east"}}
            ]}
        }"#;
        let hash = "0123456789abcdef0123456789abcdef01234567".parse().unwrap();
        let artifact = generate_game_data_from_reports(
            MinecraftVersionId::new("26.1.2").unwrap(),
            GameDataProvenance::mojang_data_generator(hash, hash, hash),
            registries,
            blocks,
        )
        .unwrap();
        let profile = BlockCollisionProfile::from_game_data(&GameData::new(artifact).unwrap());
        assert_eq!(
            profile.shape(RuntimeBlockStateId(1)),
            &CollisionShape::Empty
        );
        let slab = profile.shape(RuntimeBlockStateId(3));
        assert!(matches!(slab, CollisionShape::Boxes(values) if values[0].max.y == 0.5));
        assert_eq!(
            profile.shape(RuntimeBlockStateId(5)),
            &CollisionShape::FullCube
        );
        assert!(
            matches!(profile.shape(RuntimeBlockStateId(6)), CollisionShape::Boxes(values) if values.len() == 3)
        );
        assert!(
            matches!(profile.shape(RuntimeBlockStateId(7)), CollisionShape::Boxes(values) if values[0].max.y == 14.0 / 16.0)
        );
        assert!(!profile.is_approximate(RuntimeBlockStateId(1)));
        assert!(!profile.is_approximate(RuntimeBlockStateId(7)));
        for state in 8..=11 {
            assert_eq!(
                profile.shape(RuntimeBlockStateId(state)),
                &CollisionShape::Empty
            );
            assert!(!profile.is_approximate(RuntimeBlockStateId(state)));
        }
    }

    #[test]
    fn exact_version_selects_rules_and_unknown_versions_remain_conservative() {
        let current = CollisionRuleSet::for_version(&MinecraftVersionId::new("26.1.2").unwrap());
        let future =
            CollisionRuleSet::for_version(&MinecraftVersionId::new("future-test").unwrap());
        assert_eq!(
            current.shape("wheat", &BTreeMap::new()),
            CollisionShape::Empty
        );
        assert_eq!(
            future.shape("wheat", &BTreeMap::new()),
            CollisionShape::FullCube
        );
        assert_eq!(future.shape("air", &BTreeMap::new()), CollisionShape::Empty);
        assert!(!future.has_verified_shape("oak_stairs"));
    }

    #[test]
    fn trapdoor_open_planes_match_the_rendered_facing_for_every_direction() {
        let expected = [("north", 3_u8), ("south", 1), ("west", 2), ("east", 0)];
        for (facing, plane) in expected {
            let properties = props(&[("facing", facing), ("open", "true"), ("half", "bottom")]);
            assert_eq!(
                trapdoor_shape(&properties),
                boundary_plane(plane, 3.0 / 16.0, 1.0)
            );
        }
    }

    #[test]
    fn fence_outline_is_connection_aware_but_collision_keeps_extra_height() {
        let connected = props(&[
            ("north", "true"),
            ("east", "true"),
            ("south", "false"),
            ("west", "false"),
        ]);
        let outline = shape_boxes(classify_outline_shape("oak_fence", &connected));
        let collision = shape_boxes(classify_shape("oak_fence", &connected));
        assert_eq!(outline.len(), 3);
        assert!(outline.iter().all(|bounds| bounds.max.y == 1.0));
        assert_eq!(collision.len(), 3);
        assert!(collision.iter().all(|bounds| bounds.max.y == 1.5));
    }

    #[test]
    fn buttons_and_levers_are_non_colliding_with_thin_state_dependent_outlines() {
        for path in ["stone_button", "oak_button", "lever"] {
            for face in ["wall", "floor", "ceiling"] {
                for facing in ["north", "east", "south", "west"] {
                    for powered in ["false", "true"] {
                        let properties =
                            props(&[("face", face), ("facing", facing), ("powered", powered)]);
                        assert_eq!(classify_shape(path, &properties), CollisionShape::Empty);
                        let boxes = shape_boxes(classify_outline_shape(path, &properties));
                        assert_eq!(boxes.len(), 1);
                        let bounds = boxes[0];
                        assert!(bounds.max.x - bounds.min.x < 1.0);
                        assert!(bounds.max.y - bounds.min.y < 1.0);
                        assert!(bounds.max.z - bounds.min.z < 1.0);
                    }
                }
            }
        }
        let north = shape_boxes(classify_outline_shape(
            "oak_button",
            &props(&[("face", "wall"), ("facing", "north"), ("powered", "false")]),
        ));
        assert_eq!((north[0].min.z, north[0].max.z), (14.0 / 16.0, 1.0));
        assert_eq!((north[0].min.y, north[0].max.y), (6.0 / 16.0, 10.0 / 16.0));

        for face in ["wall", "floor", "ceiling"] {
            for facing in ["north", "east", "south", "west"] {
                for (powered, depth) in [("false", 2.0 / 16.0), ("true", 1.0 / 16.0)] {
                    let outline = shape_boxes(classify_outline_shape(
                        "stone_button",
                        &props(&[("face", face), ("facing", facing), ("powered", powered)]),
                    ));
                    assert_eq!(outline.len(), 1);
                    let bounds = outline[0];
                    let mut dimensions = [
                        bounds.max.x - bounds.min.x,
                        bounds.max.y - bounds.min.y,
                        bounds.max.z - bounds.min.z,
                    ];
                    dimensions.sort_by(f64::total_cmp);
                    assert_eq!(dimensions, [depth, 4.0 / 16.0, 6.0 / 16.0]);
                }
            }
        }
        for (powered, depth) in [("false", 2.0 / 16.0), ("true", 1.0 / 16.0)] {
            for (face, expected) in [
                (
                    "wall",
                    Aabb::new(
                        Vec3d::new(5.0 / 16.0, 6.0 / 16.0, 1.0 - depth),
                        Vec3d::new(11.0 / 16.0, 10.0 / 16.0, 1.0),
                    ),
                ),
                (
                    "floor",
                    Aabb::new(
                        Vec3d::new(5.0 / 16.0, 0.0, 6.0 / 16.0),
                        Vec3d::new(11.0 / 16.0, depth, 10.0 / 16.0),
                    ),
                ),
                (
                    "ceiling",
                    Aabb::new(
                        Vec3d::new(5.0 / 16.0, 1.0 - depth, 6.0 / 16.0),
                        Vec3d::new(11.0 / 16.0, 1.0, 10.0 / 16.0),
                    ),
                ),
            ] {
                assert_eq!(
                    shape_boxes(classify_outline_shape(
                        "stone_button",
                        &props(&[("face", face), ("facing", "north"), ("powered", powered),]),
                    )),
                    [expected].into()
                );
            }
        }

        // Vanilla 26.1.2's LeverBlock.boxZ(6, 8, 10, 16) expands to a
        // 6x8x6-sixteenth wall-mounted outline. POWERED is deliberately not
        // part of the shape cache, so both states have identical bounds.
        for powered in ["false", "true"] {
            let wall = shape_boxes(classify_outline_shape(
                "lever",
                &props(&[("face", "wall"), ("facing", "north"), ("powered", powered)]),
            ));
            assert_eq!(
                wall,
                [Aabb::new(
                    Vec3d::new(5.0 / 16.0, 4.0 / 16.0, 10.0 / 16.0),
                    Vec3d::new(11.0 / 16.0, 12.0 / 16.0, 1.0),
                )]
                .into()
            );
        }
        for (face, facing) in [
            ("wall", "east"),
            ("wall", "south"),
            ("wall", "west"),
            ("floor", "north"),
            ("floor", "east"),
            ("floor", "south"),
            ("floor", "west"),
            ("ceiling", "north"),
            ("ceiling", "east"),
            ("ceiling", "south"),
            ("ceiling", "west"),
        ] {
            let unpowered = shape_boxes(classify_outline_shape(
                "lever",
                &props(&[("face", face), ("facing", facing), ("powered", "false")]),
            ));
            let powered = shape_boxes(classify_outline_shape(
                "lever",
                &props(&[("face", face), ("facing", facing), ("powered", "true")]),
            ));
            assert_eq!(powered, unpowered);
            let bounds = unpowered[0];
            let mut dimensions = [
                bounds.max.x - bounds.min.x,
                bounds.max.y - bounds.min.y,
                bounds.max.z - bounds.min.z,
            ];
            dimensions.sort_by(f64::total_cmp);
            assert_eq!(dimensions, [6.0 / 16.0, 6.0 / 16.0, 8.0 / 16.0]);
        }
    }

    #[test]
    fn short_grass_has_exact_outline_and_no_collision() {
        let properties = BTreeMap::new();
        assert_eq!(
            classify_shape("short_grass", &properties),
            CollisionShape::Empty
        );
        assert_eq!(
            shape_boxes(classify_outline_shape("short_grass", &properties)),
            [Aabb::new(
                Vec3d::new(2.0 / 16.0, 0.0, 2.0 / 16.0),
                Vec3d::new(14.0 / 16.0, 13.0 / 16.0, 14.0 / 16.0),
            )]
            .into()
        );
        assert_eq!(
            classify_outline_shape("tall_grass", &properties),
            CollisionShape::FullCube,
            "the short-grass override must not alter DoublePlantBlock"
        );
    }

    #[test]
    fn empty_hand_destroy_progress_uses_hardness_and_correct_tool_divisor() {
        let rules = CollisionRuleSet::Java26_1_2;
        assert!((rules.bare_hand_destroy_progress("dirt") - 1.0 / 15.0).abs() < f32::EPSILON);
        assert!((rules.bare_hand_destroy_progress("stone") - 1.0 / 150.0).abs() < f32::EPSILON);
        assert_eq!(rules.bare_hand_destroy_progress("bedrock"), 0.0);
        assert_eq!(
            rules.bare_hand_destroy_progress("unknown_future_block"),
            0.0
        );
    }
}
