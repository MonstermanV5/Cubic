use std::collections::{BTreeMap, BTreeSet};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};

use crate::{MinecraftIdentifier, MinecraftVersionId, VersionError};

pub const CREATIVE_DATA_SCHEMA_VERSION: u32 = 1;
pub const MAX_CREATIVE_DATA_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_CREATIVE_TABS: usize = 32;
pub const MAX_CREATIVE_STACKS_PER_TAB: usize = 4096;
pub const MAX_CREATIVE_COMPONENTS_PER_STACK: usize = 256;
pub const MAX_CREATIVE_COMPONENT_BYTES: usize = 1024 * 1024;
pub const MAX_BANNER_PATTERNS: usize = 64;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CreativeTabRow {
    Top,
    Bottom,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CreativeTabType {
    Category,
    Inventory,
    Hotbar,
    Search,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreativeComponentData {
    pub id: MinecraftIdentifier,
    pub removed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_base64: Option<String>,
}

impl CreativeComponentData {
    pub fn decoded_value(&self) -> Result<Option<Vec<u8>>, VersionError> {
        match (&self.value_base64, self.removed) {
            (None, true) => Ok(None),
            (Some(value), false) => STANDARD
                .decode(value)
                .map(Some)
                .map_err(|_| invalid_creative("Creative component value is not canonical base64")),
            _ => Err(invalid_creative(
                "Creative component removal/value representation is inconsistent",
            )),
        }
    }

    /// Extracts the translation key from the bounded unnamed-network NBT
    /// string-component shape emitted by the current Creative data generator.
    ///
    /// This is intentionally not a general NBT decoder. Unknown rich component
    /// forms remain opaque for the version-neutral inventory layer.
    pub fn text_translation_key(&self) -> Result<Option<String>, VersionError> {
        let Some(bytes) = self.decoded_value()? else {
            return Ok(None);
        };
        Ok(current_text_translation_key(&bytes).map(str::to_owned))
    }

    /// Decodes the bounded string-to-string map wire form used by the current
    /// `minecraft:block_state` data component.
    pub fn block_state_properties(&self) -> Result<Option<BTreeMap<String, String>>, VersionError> {
        if self.id.as_str() != "minecraft:block_state" {
            return Ok(None);
        }
        let Some(bytes) = self.decoded_value()? else {
            return Ok(None);
        };
        current_string_map(&bytes).map(Some)
    }
}

fn current_string_map(bytes: &[u8]) -> Result<BTreeMap<String, String>, VersionError> {
    fn varint(bytes: &[u8], offset: &mut usize) -> Option<usize> {
        let mut value = 0_u32;
        for shift in (0..35).step_by(7) {
            let byte = *bytes.get(*offset)?;
            *offset += 1;
            value |= u32::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return usize::try_from(value).ok();
            }
        }
        None
    }
    fn string(bytes: &[u8], offset: &mut usize) -> Option<String> {
        let length = varint(bytes, offset)?;
        if length > 128 {
            return None;
        }
        let end = offset.checked_add(length)?;
        let value = std::str::from_utf8(bytes.get(*offset..end)?)
            .ok()?
            .to_owned();
        *offset = end;
        Some(value)
    }

    let mut offset = 0;
    let count = varint(bytes, &mut offset)
        .filter(|count| *count <= 64)
        .ok_or_else(|| invalid_creative("Creative block-state property count is invalid"))?;
    let mut output = BTreeMap::new();
    for _ in 0..count {
        let key = string(bytes, &mut offset)
            .ok_or_else(|| invalid_creative("Creative block-state key is invalid"))?;
        let value = string(bytes, &mut offset)
            .ok_or_else(|| invalid_creative("Creative block-state value is invalid"))?;
        if output.insert(key, value).is_some() {
            return Err(invalid_creative(
                "Creative block-state properties contain a duplicate key",
            ));
        }
    }
    if offset != bytes.len() {
        return Err(invalid_creative(
            "Creative block-state properties contain trailing bytes",
        ));
    }
    Ok(output)
}

fn current_text_translation_key(bytes: &[u8]) -> Option<&str> {
    // TAG_Compound { TAG_String "translate": <key>, TAG_End }
    if bytes.first().copied()? != 10 {
        return None;
    }
    let mut offset = 1_usize;
    if *bytes.get(offset)? != 8 {
        return None;
    }
    offset += 1;
    let name_length = usize::from(u16::from_be_bytes([
        *bytes.get(offset)?,
        *bytes.get(offset + 1)?,
    ]));
    offset += 2;
    let name_end = offset.checked_add(name_length)?;
    if bytes.get(offset..name_end)? != b"translate" {
        return None;
    }
    offset = name_end;
    let value_length = usize::from(u16::from_be_bytes([
        *bytes.get(offset)?,
        *bytes.get(offset + 1)?,
    ]));
    if value_length == 0 || value_length > 128 {
        return None;
    }
    offset += 2;
    let value_end = offset.checked_add(value_length)?;
    if bytes.get(value_end).copied()? != 0 || value_end + 1 != bytes.len() {
        return None;
    }
    std::str::from_utf8(bytes.get(offset..value_end)?).ok()
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreativeStackData {
    pub item: MinecraftIdentifier,
    pub count: u32,
    pub search_visible: bool,
    pub effective_item_model: MinecraftIdentifier,
    pub required_features: Vec<MinecraftIdentifier>,
    pub components: Vec<CreativeComponentData>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreativeTabData {
    pub id: MinecraftIdentifier,
    pub title_key: String,
    pub row: CreativeTabRow,
    pub column: u8,
    #[serde(rename = "type")]
    pub tab_type: CreativeTabType,
    pub background: MinecraftIdentifier,
    pub aligned_right: bool,
    pub show_title: bool,
    pub can_scroll: bool,
    pub should_display: bool,
    pub icon: CreativeStackData,
    pub items: Vec<CreativeStackData>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreativeData {
    pub schema_version: u32,
    pub minecraft_version: MinecraftVersionId,
    /// Exact built-in registry order used to encode canonical Creative stacks.
    /// Server-provided stacks must instead resolve against their Configuration
    /// registry snapshot.
    #[serde(default)]
    pub banner_patterns: Vec<MinecraftIdentifier>,
    pub without_permissions: Vec<CreativeTabData>,
    pub with_permissions: Vec<CreativeTabData>,
}

impl CreativeData {
    pub fn builtin_26_1_2() -> Result<Self, VersionError> {
        parse_creative_data(include_bytes!("../data/26.1.2/creative-data.json"))
    }

    #[must_use]
    pub fn tabs(&self, has_permissions: bool) -> &[CreativeTabData] {
        if has_permissions {
            &self.with_permissions
        } else {
            &self.without_permissions
        }
    }
}

pub fn parse_creative_data(bytes: &[u8]) -> Result<CreativeData, VersionError> {
    if bytes.len() > MAX_CREATIVE_DATA_BYTES {
        return Err(invalid_creative("Creative data exceeds its byte limit"));
    }
    let data: CreativeData =
        serde_json::from_slice(bytes).map_err(|error| VersionError::InvalidCreativeData {
            reason: format!(
                "malformed JSON at line {}, column {}",
                error.line(),
                error.column()
            ),
        })?;
    validate(&data)?;
    Ok(data)
}

fn validate(data: &CreativeData) -> Result<(), VersionError> {
    if data.schema_version != CREATIVE_DATA_SCHEMA_VERSION {
        return Err(VersionError::UnsupportedCreativeDataFormat {
            found: data.schema_version,
            supported: CREATIVE_DATA_SCHEMA_VERSION,
        });
    }
    if data.banner_patterns.len() > MAX_BANNER_PATTERNS
        || data.banner_patterns.iter().collect::<BTreeSet<_>>().len() != data.banner_patterns.len()
    {
        return Err(invalid_creative(
            "Creative banner-pattern registry is oversized or contains duplicates",
        ));
    }
    for tabs in [&data.without_permissions, &data.with_permissions] {
        if tabs.len() > MAX_CREATIVE_TABS {
            return Err(invalid_creative("too many Creative tabs"));
        }
        let mut ids = BTreeSet::new();
        let mut positions = BTreeSet::new();
        for tab in tabs {
            if !ids.insert(tab.id.as_str()) {
                return Err(invalid_creative("duplicate Creative tab identifier"));
            }
            if !positions.insert((tab.row, tab.column)) || tab.column > 6 {
                return Err(invalid_creative(
                    "invalid or duplicate Creative tab position",
                ));
            }
            if tab.title_key.is_empty() || tab.title_key.len() > 128 {
                return Err(invalid_creative("invalid Creative tab title key"));
            }
            validate_stack(&tab.icon)?;
            if tab.items.len() > MAX_CREATIVE_STACKS_PER_TAB {
                return Err(invalid_creative("too many Creative stacks in one tab"));
            }
            for stack in &tab.items {
                validate_stack(stack)?;
            }
        }
        if !tabs
            .iter()
            .any(|tab| tab.tab_type == CreativeTabType::Search)
        {
            return Err(invalid_creative("Creative Search tab is missing"));
        }
    }
    Ok(())
}

fn validate_stack(stack: &CreativeStackData) -> Result<(), VersionError> {
    if stack.count == 0 || stack.count > 99 {
        return Err(invalid_creative("invalid Creative stack count"));
    }
    if stack.components.len() > MAX_CREATIVE_COMPONENTS_PER_STACK {
        return Err(invalid_creative("too many Creative stack components"));
    }
    let mut ids = BTreeSet::new();
    let mut retained = 0_usize;
    for component in &stack.components {
        if !ids.insert(component.id.as_str()) {
            return Err(invalid_creative("duplicate Creative stack component"));
        }
        if let Some(value) = component.decoded_value()? {
            retained = retained.saturating_add(value.len());
        }
    }
    if retained > MAX_CREATIVE_COMPONENT_BYTES {
        return Err(invalid_creative(
            "Creative stack component data exceeds its limit",
        ));
    }
    Ok(())
}

fn invalid_creative(reason: impl Into<String>) -> VersionError {
    VersionError::InvalidCreativeData {
        reason: reason.into(),
    }
}

/// Stable FNV-1a digest over the exact ordered stack identities and component bytes.
#[must_use]
pub fn creative_order_hash(stacks: &[CreativeStackData]) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    let mut add = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    };
    for stack in stacks {
        add(stack.item.as_str().as_bytes());
        add(&stack.count.to_be_bytes());
        add(&[u8::from(stack.search_visible)]);
        add(stack.effective_item_model.as_str().as_bytes());
        for component in &stack.components {
            add(component.id.as_str().as_bytes());
            add(&[u8::from(component.removed)]);
            if let Some(value) = &component.value_base64 {
                add(value.as_bytes());
            }
        }
        add(&[0xff]);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_metadata_and_order_are_exact_and_stable() {
        let data = CreativeData::builtin_26_1_2().unwrap();
        assert_eq!(data.minecraft_version.as_str(), "26.1.2");
        let tabs = data.tabs(false);
        assert_eq!(tabs.len(), 14);
        let building = &tabs[0];
        assert_eq!(building.id.as_str(), "minecraft:building_blocks");
        assert_eq!(building.row, CreativeTabRow::Top);
        assert_eq!(building.column, 0);
        assert_eq!(building.title_key, "itemGroup.buildingBlocks");
        assert_eq!(building.icon.item.as_str(), "minecraft:bricks");
        assert_eq!(building.items.len(), 436);
        assert_eq!(
            building
                .items
                .iter()
                .take(13)
                .map(|stack| stack.item.as_str())
                .collect::<Vec<_>>(),
            [
                "minecraft:oak_log",
                "minecraft:oak_wood",
                "minecraft:stripped_oak_log",
                "minecraft:stripped_oak_wood",
                "minecraft:oak_planks",
                "minecraft:oak_stairs",
                "minecraft:oak_slab",
                "minecraft:oak_fence",
                "minecraft:oak_fence_gate",
                "minecraft:oak_door",
                "minecraft:oak_trapdoor",
                "minecraft:oak_pressure_plate",
                "minecraft:oak_button",
            ]
        );
        assert!(
            !tabs
                .iter()
                .find(|tab| tab.id.as_str() == "minecraft:op_blocks")
                .unwrap()
                .should_display
        );
        assert!(
            data.tabs(true)
                .iter()
                .find(|tab| tab.id.as_str() == "minecraft:op_blocks")
                .unwrap()
                .should_display
        );
        let expected = [
            (
                "minecraft:building_blocks",
                436,
                "eb0c788fc049a869",
                "af7ec9dd3cc60f9b",
                "c0842b395c3cfffd",
            ),
            (
                "minecraft:colored_blocks",
                198,
                "69010778052bac3b",
                "07bfbc53510aa623",
                "3818877ed3d035bb",
            ),
            (
                "minecraft:natural_blocks",
                243,
                "1d1a9b621a0d3f90",
                "d3768d18bc351e3b",
                "5254c2b43f0e5fab",
            ),
            (
                "minecraft:functional_blocks",
                280,
                "7a6d38c969bb8377",
                "edc82f9a9cc13edb",
                "044caa39051bda4b",
            ),
            (
                "minecraft:redstone_blocks",
                70,
                "8727a6b0813383a9",
                "39d6645c4495d9f5",
                "1d9acee807aeeb01",
            ),
            (
                "minecraft:hotbar",
                0,
                "cbf29ce484222325",
                "cbf29ce484222325",
                "cbf29ce484222325",
            ),
            (
                "minecraft:search",
                1864,
                "2c0062deb574069e",
                "af7ec9dd3cc60f9b",
                "f9b98d2685d552d5",
            ),
            (
                "minecraft:tools_and_utilities",
                154,
                "66b02b98afc094d4",
                "31ff03da1499da91",
                "bfb93d760f4f2ad1",
            ),
            (
                "minecraft:combat",
                126,
                "977d9d471684bc1e",
                "e020669244969459",
                "a19cab971bebaf7b",
            ),
            (
                "minecraft:food_and_drinks",
                195,
                "e3024cca554deb1e",
                "e89ba19205d5e631",
                "2e2ed8048ea3eacf",
            ),
            (
                "minecraft:ingredients",
                194,
                "6f570f481fb0bb2a",
                "beccdf19002f264b",
                "8b5faaee935a6637",
            ),
            (
                "minecraft:spawn_eggs",
                88,
                "17fefc489981c2b7",
                "6a029044bbb70807",
                "f9b98d2685d552d5",
            ),
            (
                "minecraft:op_blocks",
                0,
                "cbf29ce484222325",
                "cbf29ce484222325",
                "cbf29ce484222325",
            ),
            (
                "minecraft:inventory",
                0,
                "cbf29ce484222325",
                "cbf29ce484222325",
                "cbf29ce484222325",
            ),
        ];
        for (tab, (id, count, all, first, last)) in tabs.iter().zip(expected) {
            let first_items = tab.items.iter().take(20).cloned().collect::<Vec<_>>();
            let last_items = tab
                .items
                .iter()
                .rev()
                .take(20)
                .cloned()
                .rev()
                .collect::<Vec<_>>();
            assert_eq!(tab.id.as_str(), id);
            assert_eq!(tab.items.len(), count);
            assert_eq!(creative_order_hash(&tab.items), all);
            assert_eq!(creative_order_hash(&first_items), first);
            assert_eq!(creative_order_hash(&last_items), last);
        }
    }

    #[test]
    fn ominous_banner_item_name_projects_from_the_exact_component_bytes() {
        let data = CreativeData::builtin_26_1_2().unwrap();
        let banner = data
            .tabs(false)
            .iter()
            .flat_map(|tab| &tab.items)
            .find(|stack| {
                stack.item.as_str() == "minecraft:white_banner"
                    && stack
                        .components
                        .iter()
                        .any(|component| component.id.as_str() == "minecraft:item_name")
            })
            .unwrap();
        let item_name = banner
            .components
            .iter()
            .find(|component| component.id.as_str() == "minecraft:item_name")
            .unwrap();
        assert_eq!(
            item_name.text_translation_key().unwrap().as_deref(),
            Some("block.minecraft.ominous_banner")
        );
    }

    #[test]
    fn operator_variants_preserve_all_light_levels_and_test_block_modes() {
        let data = CreativeData::builtin_26_1_2().unwrap();
        let stacks = data
            .tabs(true)
            .iter()
            .flat_map(|tab| &tab.items)
            .collect::<Vec<_>>();
        let properties = |stack: &CreativeStackData| {
            stack
                .components
                .iter()
                .find(|component| component.id.as_str() == "minecraft:block_state")
                .and_then(|component| component.block_state_properties().ok().flatten())
        };
        let light_levels = stacks
            .iter()
            .filter(|stack| stack.item.as_str() == "minecraft:light")
            .map(|stack| {
                properties(stack)
                    .and_then(|values| values.get("level").cloned())
                    // The item prototype's effective default component is
                    // level 15; the generated patch is empty for that one
                    // canonical Creative stack.
                    .unwrap_or_else(|| "15".to_owned())
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            light_levels,
            (0..16).map(|level| level.to_string()).collect()
        );
        let test_modes = stacks
            .iter()
            .filter(|stack| stack.item.as_str() == "minecraft:test_block")
            .map(|stack| {
                properties(stack)
                    .and_then(|values| values.get("mode").cloned())
                    // The generated patch is empty when it matches the item
                    // prototype's effective default `start` block state.
                    .unwrap_or_else(|| "start".to_owned())
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            test_modes,
            ["accept", "fail", "log", "start"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
    }

    #[test]
    fn malformed_and_unsupported_data_are_rejected() {
        let mut value: serde_json::Value =
            serde_json::from_slice(include_bytes!("../data/26.1.2/creative-data.json")).unwrap();
        value["schema_version"] = 99.into();
        assert!(matches!(
            parse_creative_data(&serde_json::to_vec(&value).unwrap()),
            Err(VersionError::UnsupportedCreativeDataFormat { .. })
        ));
        assert!(parse_creative_data(br#"{}"#).is_err());
    }

    #[test]
    fn every_vanilla_tab_metadata_field_is_pinned() {
        let data = CreativeData::builtin_26_1_2().unwrap();
        let expected = [
            (
                "minecraft:building_blocks",
                "itemGroup.buildingBlocks",
                CreativeTabRow::Top,
                0,
                CreativeTabType::Category,
                "minecraft:bricks",
                false,
                true,
                true,
                true,
            ),
            (
                "minecraft:colored_blocks",
                "itemGroup.coloredBlocks",
                CreativeTabRow::Top,
                1,
                CreativeTabType::Category,
                "minecraft:cyan_wool",
                false,
                true,
                true,
                true,
            ),
            (
                "minecraft:natural_blocks",
                "itemGroup.natural",
                CreativeTabRow::Top,
                2,
                CreativeTabType::Category,
                "minecraft:grass_block",
                false,
                true,
                true,
                true,
            ),
            (
                "minecraft:functional_blocks",
                "itemGroup.functional",
                CreativeTabRow::Top,
                3,
                CreativeTabType::Category,
                "minecraft:oak_sign",
                false,
                true,
                true,
                true,
            ),
            (
                "minecraft:redstone_blocks",
                "itemGroup.redstone",
                CreativeTabRow::Top,
                4,
                CreativeTabType::Category,
                "minecraft:redstone",
                false,
                true,
                true,
                true,
            ),
            (
                "minecraft:hotbar",
                "itemGroup.hotbar",
                CreativeTabRow::Top,
                5,
                CreativeTabType::Hotbar,
                "minecraft:bookshelf",
                true,
                true,
                true,
                true,
            ),
            (
                "minecraft:search",
                "itemGroup.search",
                CreativeTabRow::Top,
                6,
                CreativeTabType::Search,
                "minecraft:compass",
                true,
                true,
                true,
                true,
            ),
            (
                "minecraft:tools_and_utilities",
                "itemGroup.tools",
                CreativeTabRow::Bottom,
                0,
                CreativeTabType::Category,
                "minecraft:diamond_pickaxe",
                false,
                true,
                true,
                true,
            ),
            (
                "minecraft:combat",
                "itemGroup.combat",
                CreativeTabRow::Bottom,
                1,
                CreativeTabType::Category,
                "minecraft:netherite_sword",
                false,
                true,
                true,
                true,
            ),
            (
                "minecraft:food_and_drinks",
                "itemGroup.foodAndDrink",
                CreativeTabRow::Bottom,
                2,
                CreativeTabType::Category,
                "minecraft:golden_apple",
                false,
                true,
                true,
                true,
            ),
            (
                "minecraft:ingredients",
                "itemGroup.ingredients",
                CreativeTabRow::Bottom,
                3,
                CreativeTabType::Category,
                "minecraft:iron_ingot",
                false,
                true,
                true,
                true,
            ),
            (
                "minecraft:spawn_eggs",
                "itemGroup.spawnEggs",
                CreativeTabRow::Bottom,
                4,
                CreativeTabType::Category,
                "minecraft:creeper_spawn_egg",
                false,
                true,
                true,
                true,
            ),
            (
                "minecraft:op_blocks",
                "itemGroup.op",
                CreativeTabRow::Bottom,
                5,
                CreativeTabType::Category,
                "minecraft:command_block",
                true,
                true,
                true,
                false,
            ),
            (
                "minecraft:inventory",
                "itemGroup.inventory",
                CreativeTabRow::Bottom,
                6,
                CreativeTabType::Inventory,
                "minecraft:chest",
                true,
                false,
                false,
                true,
            ),
        ];
        for (tab, expected) in data.tabs(false).iter().zip(expected) {
            assert_eq!(
                (
                    tab.id.as_str(),
                    tab.title_key.as_str(),
                    tab.row,
                    tab.column,
                    tab.tab_type,
                    tab.icon.item.as_str(),
                    tab.aligned_right,
                    tab.show_title,
                    tab.can_scroll,
                    tab.should_display
                ),
                expected
            );
            let expected_background = match tab.tab_type {
                CreativeTabType::Search => {
                    "minecraft:textures/gui/container/creative_inventory/tab_item_search.png"
                }
                CreativeTabType::Inventory => {
                    "minecraft:textures/gui/container/creative_inventory/tab_inventory.png"
                }
                CreativeTabType::Category | CreativeTabType::Hotbar => {
                    "minecraft:textures/gui/container/creative_inventory/tab_items.png"
                }
            };
            assert_eq!(tab.background.as_str(), expected_background);
        }
        let component_bearing = data
            .tabs(false)
            .iter()
            .flat_map(|tab| &tab.items)
            .filter(|stack| !stack.components.is_empty())
            .count();
        assert_eq!(component_bearing, 683);
        let permitted_op = data
            .tabs(true)
            .iter()
            .find(|tab| tab.id.as_str() == "minecraft:op_blocks")
            .unwrap();
        assert!(permitted_op.should_display);
        assert_eq!(permitted_op.items.len(), 34);
        assert_eq!(
            creative_order_hash(&data.tabs(true)[6].items),
            "626a4ec8aa0ae3af"
        );
        assert_eq!(creative_order_hash(&permitted_op.items), "6221890dffe1bc94");
    }
}
