use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use cubic_version::{GameData, MinecraftIdentifier};
use thiserror::Error;

use super::MAX_BOOTSTRAP_FRAME_SIZE;
use crate::{
    CodecError, CodecReader, CodecWriter, StringLimits, encode_frame,
    nbt::{NbtError, NbtLimits, decode_unnamed_network_tag},
    split_raw_packet,
};

pub const MAX_CONTAINER_SLOTS: usize = 512;
pub const MAX_COMPONENTS_PER_STACK: usize = 256;
pub const MAX_COMPONENT_VALUE_BYTES: usize = 1024 * 1024;
pub const MAX_STACK_COUNT: u32 = 99;
pub const MAX_CHANGED_SLOTS: usize = 128;
pub const MAX_BANNER_PATTERN_LAYERS: usize = 20;
const BANNER_PATTERN_ASSET_LIMITS: StringLimits = StringLimits::new(256, 1_024);
const BANNER_PATTERN_TRANSLATION_LIMITS: StringLimits = StringLimits::new(256, 1_024);
const TITLE_LIMITS: StringLimits = StringLimits::new(32_767, 32_767);
pub(super) const CONTAINER_CLOSE_ID: i32 = 0x11;
pub(super) const CONTAINER_SET_CONTENT_ID: i32 = 0x12;
pub(super) const CONTAINER_SET_DATA_ID: i32 = 0x13;
pub(super) const CONTAINER_SET_SLOT_ID: i32 = 0x14;
pub(super) const OPEN_SCREEN_ID: i32 = 0x3b;
pub(super) const SET_HELD_SLOT_ID: i32 = 0x69;
pub(super) const SERVERBOUND_CONTAINER_BUTTON_CLICK_ID: i32 = 0x11;
pub(super) const SERVERBOUND_CONTAINER_CLICK_ID: i32 = 0x12;
pub(super) const SERVERBOUND_CONTAINER_CLOSE_ID: i32 = 0x13;
pub(super) const SERVERBOUND_SET_CARRIED_ITEM_ID: i32 = 0x35;
pub(super) const SERVERBOUND_SET_CREATIVE_SLOT_ID: i32 = 0x38;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ComponentCodec {
    Unit,
    VarInt,
    I32,
    F32,
    Bool,
    String,
    Nbt,
    NbtList,
    VarIntPairs,
    VarIntList,
    StringPairs,
    UseEffects,
    Food,
    Weapon,
    AttackRange,
    CustomModelData,
    TooltipDisplay,
    PotionContents,
    BannerPatterns,
    Fireworks,
    Instrument,
    PaintingVariant,
    TwoVarInts,
    Unsupported,
}

#[derive(Clone, Debug)]
pub struct ItemStackProfile {
    items: BTreeMap<u32, MinecraftIdentifier>,
    item_raw_ids: BTreeMap<MinecraftIdentifier, u32>,
    components: BTreeMap<u32, (MinecraftIdentifier, ComponentCodec)>,
    component_raw_ids: BTreeMap<MinecraftIdentifier, u32>,
    menus: BTreeMap<u32, MinecraftIdentifier>,
    banner_patterns: BTreeMap<u32, MinecraftIdentifier>,
    banner_pattern_raw_ids: BTreeMap<MinecraftIdentifier, u32>,
}

impl ItemStackProfile {
    pub fn from_game_data(data: &GameData) -> Result<Self, InventoryCodecError> {
        let items = registry_entries(data, "minecraft:item")?;
        let component_entries = registry_entries(data, "minecraft:data_component_type")?;
        let menus = registry_entries(data, "minecraft:menu")?;
        // Banner patterns are a data-driven Configuration registry. Static
        // game data may not contain it before a connection has supplied the
        // authoritative ordered entries.
        let banner_patterns = optional_registry_entries(data, "minecraft:banner_pattern")?;
        let item_raw_ids = reverse(&items);
        let component_raw_ids = reverse(&component_entries);
        let banner_pattern_raw_ids = reverse(&banner_patterns);
        let components = component_entries
            .into_iter()
            .map(|(raw_id, identifier)| {
                let codec = component_codec(&identifier);
                Ok((raw_id, (identifier, codec)))
            })
            .collect::<Result<_, InventoryCodecError>>()?;
        Ok(Self {
            items,
            item_raw_ids,
            components,
            component_raw_ids,
            menus,
            banner_patterns,
            banner_pattern_raw_ids,
        })
    }

    #[cfg(test)]
    pub(super) fn synthetic(
        items: impl IntoIterator<Item = (u32, &'static str)>,
        components: impl IntoIterator<Item = (u32, &'static str)>,
        menus: impl IntoIterator<Item = (u32, &'static str)>,
    ) -> Self {
        let items = identifiers(items);
        let component_entries = identifiers(components);
        let item_raw_ids = reverse(&items);
        let component_raw_ids = reverse(&component_entries);
        let components = component_entries
            .into_iter()
            .map(|(raw, identifier)| {
                let codec = component_codec(&identifier);
                (raw, (identifier, codec))
            })
            .collect();
        Self {
            items,
            item_raw_ids,
            components,
            component_raw_ids,
            menus: identifiers(menus),
            banner_patterns: BTreeMap::new(),
            banner_pattern_raw_ids: BTreeMap::new(),
        }
    }

    pub fn item_raw_id(&self, item: &MinecraftIdentifier) -> Option<u32> {
        self.item_raw_ids.get(item).copied()
    }

    pub fn component_raw_id(&self, component: &MinecraftIdentifier) -> Option<u32> {
        self.component_raw_ids.get(component).copied()
    }

    /// Bounded, value-free wire metadata for diagnosing a Creative Slot send.
    /// Raw component payloads (which can contain player text) are never logged.
    pub fn creative_stack_wire_summary(
        &self,
        stack: &ItemStack,
    ) -> Result<String, InventoryCodecError> {
        let item_raw = self
            .item_raw_id(&stack.item)
            .ok_or_else(|| InventoryCodecError::UnknownItem(stack.item.clone()))?;
        let mut summary = format!(
            "item={} raw_item={} count={} added={} removed={}",
            stack.item,
            item_raw,
            stack.count,
            stack.components.added.len(),
            stack.components.removed.len()
        );
        for (component, value) in &stack.components.added {
            let raw = self
                .component_raw_id(component)
                .ok_or_else(|| InventoryCodecError::UnknownComponent(component.clone()))?;
            let codec = self
                .components
                .get(&raw)
                .map(|(_, codec)| *codec)
                .ok_or_else(|| InventoryCodecError::UnknownComponent(component.clone()))?;
            let encoded = encode_added_component_value(stack, value, codec, self)?;
            let _ = write!(
                summary,
                " added_component={component}#{raw}:{}B",
                encoded.len()
            );
            if codec == ComponentCodec::BannerPatterns {
                let mut reader = CodecReader::new(&encoded);
                let patterns = decode_banner_pattern_layers(&mut reader, self)?;
                finish(&reader)?;
                for layer in patterns.layers {
                    match layer.pattern {
                        BannerPatternHolder::Reference(identifier) => {
                            let raw_id = self
                                .banner_pattern_raw_ids
                                .get(&identifier)
                                .copied()
                                .ok_or_else(|| {
                                    InventoryCodecError::UnknownBannerPattern(identifier.clone())
                                })?;
                            let _ = write!(
                                summary,
                                " layer=reference:{identifier}:raw={raw_id}:wire={}:dye={}",
                                raw_id.saturating_add(1),
                                layer.dye_raw_id
                            );
                        }
                        BannerPatternHolder::Direct(pattern) => {
                            let _ = write!(
                                summary,
                                " layer=direct:{}:{}:wire=0:dye={}",
                                pattern.asset_id, pattern.translation_key, layer.dye_raw_id
                            );
                        }
                    }
                }
            }
        }
        for component in &stack.components.removed {
            let raw = self
                .component_raw_id(component)
                .ok_or_else(|| InventoryCodecError::UnknownComponent(component.clone()))?;
            let _ = write!(summary, " removed_component={component}#{raw}");
        }
        let mut encoded_stack = CodecWriter::new();
        encode_item_stack(&mut encoded_stack, Some(stack), self)?;
        let _ = write!(summary, " stack_wire_bytes={}", encoded_stack.len());
        Ok(summary)
    }

    pub fn install_banner_patterns(
        &mut self,
        patterns: impl IntoIterator<Item = MinecraftIdentifier>,
    ) -> Result<(), InventoryCodecError> {
        let mut entries = BTreeMap::new();
        for (index, identifier) in patterns.into_iter().enumerate() {
            let raw_id = u32::try_from(index).map_err(|_| InventoryCodecError::CountTooLarge {
                context: "banner-pattern registry",
                count: index,
                max: u32::MAX as usize,
            })?;
            entries.insert(raw_id, identifier);
        }
        self.banner_pattern_raw_ids = reverse(&entries);
        self.banner_patterns = entries;
        Ok(())
    }

    /// Computes the protocol-775 CRC32C dynamic-codec fingerprint for the
    /// primitive component families Cubic semantically understands. Complex
    /// registry-aware values return `None` and use server-authoritative click
    /// reconciliation rather than a fabricated hash.
    pub fn component_hash(
        &self,
        component: &MinecraftIdentifier,
        wire_value: &[u8],
    ) -> Result<Option<i32>, InventoryCodecError> {
        let codec = self
            .component_raw_ids
            .get(component)
            .and_then(|raw| self.components.get(raw))
            .map(|(_, codec)| *codec)
            .ok_or_else(|| InventoryCodecError::UnknownComponent(component.clone()))?;
        let mut reader = CodecReader::new(wire_value);
        let payload = match codec {
            ComponentCodec::Unit => vec![2, 3],
            ComponentCodec::VarInt => {
                let value = reader.read_var_int()?;
                primitive_hash_payload(8, &value.to_le_bytes())
            }
            ComponentCodec::I32 => {
                let value = reader.read_i32()?;
                primitive_hash_payload(8, &value.to_le_bytes())
            }
            ComponentCodec::F32 => {
                let value = reader.read_f32()?;
                primitive_hash_payload(10, &value.to_bits().to_le_bytes())
            }
            ComponentCodec::Bool => {
                let value = reader.read_bool()?;
                vec![13, u8::from(value)]
            }
            ComponentCodec::String => {
                let value = reader.read_string(StringLimits::new(32_767, 98_301))?;
                let utf16 = value.encode_utf16().collect::<Vec<_>>();
                let mut payload = Vec::with_capacity(5 + utf16.len() * 2);
                payload.push(12);
                payload.extend_from_slice(
                    &i32::try_from(utf16.len())
                        .map_err(|_| InventoryCodecError::ValueOutOfRange {
                            context: "component string length",
                            value: utf16.len() as i64,
                            max: i64::from(i32::MAX),
                        })?
                        .to_le_bytes(),
                );
                for unit in utf16 {
                    payload.extend_from_slice(&unit.to_le_bytes());
                }
                payload
            }
            _ => return Ok(None),
        };
        finish(&reader)?;
        Ok(Some(crc32c(&payload) as i32))
    }
}

fn primitive_hash_payload(tag: u8, bytes: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(bytes.len() + 1);
    payload.push(tag);
    payload.extend_from_slice(bytes);
    payload
}

fn crc32c(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0x82f6_3b78 & mask);
        }
    }
    !crc
}

fn registry_entries(
    data: &GameData,
    registry: &'static str,
) -> Result<BTreeMap<u32, MinecraftIdentifier>, InventoryCodecError> {
    let identifier = MinecraftIdentifier::new(registry)
        .map_err(|_| InventoryCodecError::InvalidBuiltInProfile(registry))?;
    let table = data
        .registry(&identifier)
        .ok_or(InventoryCodecError::MissingRegistry(registry))?;
    Ok(table
        .entries
        .iter()
        .map(|entry| (entry.raw_id, entry.identifier.clone()))
        .collect())
}

fn optional_registry_entries(
    data: &GameData,
    registry: &'static str,
) -> Result<BTreeMap<u32, MinecraftIdentifier>, InventoryCodecError> {
    let identifier = MinecraftIdentifier::new(registry)
        .map_err(|_| InventoryCodecError::InvalidBuiltInProfile(registry))?;
    Ok(data
        .registry(&identifier)
        .map_or_else(BTreeMap::new, |table| {
            table
                .entries
                .iter()
                .map(|entry| (entry.raw_id, entry.identifier.clone()))
                .collect()
        }))
}

fn reverse(entries: &BTreeMap<u32, MinecraftIdentifier>) -> BTreeMap<MinecraftIdentifier, u32> {
    entries
        .iter()
        .map(|(raw, identifier)| (identifier.clone(), *raw))
        .collect()
}

#[cfg(test)]
fn identifiers(
    values: impl IntoIterator<Item = (u32, &'static str)>,
) -> BTreeMap<u32, MinecraftIdentifier> {
    values
        .into_iter()
        .map(|(raw, value)| (raw, MinecraftIdentifier::new(value).unwrap()))
        .collect()
}

fn component_codec(identifier: &MinecraftIdentifier) -> ComponentCodec {
    let value = identifier.as_str();
    match value {
        "minecraft:unbreakable"
        | "minecraft:creative_slot_lock"
        | "minecraft:intangible_projectile"
        | "minecraft:glider" => ComponentCodec::Unit,
        "minecraft:max_stack_size"
        | "minecraft:max_damage"
        | "minecraft:damage"
        | "minecraft:repair_cost"
        | "minecraft:enchantable"
        | "minecraft:map_id"
        | "minecraft:map_post_processing"
        | "minecraft:ominous_bottle_amplifier"
        | "minecraft:additional_trade_cost" => ComponentCodec::VarInt,
        "minecraft:rarity"
        | "minecraft:dye"
        | "minecraft:base_color"
        | "minecraft:villager/variant"
        | "minecraft:wolf/variant"
        | "minecraft:wolf/sound_variant"
        | "minecraft:wolf/collar"
        | "minecraft:fox/variant"
        | "minecraft:salmon/size"
        | "minecraft:parrot/variant"
        | "minecraft:tropical_fish/pattern"
        | "minecraft:tropical_fish/base_color"
        | "minecraft:tropical_fish/pattern_color"
        | "minecraft:mooshroom/variant"
        | "minecraft:rabbit/variant"
        | "minecraft:pig/variant"
        | "minecraft:pig/sound_variant"
        | "minecraft:cow/variant"
        | "minecraft:cow/sound_variant"
        | "minecraft:frog/variant"
        | "minecraft:horse/variant"
        | "minecraft:llama/variant"
        | "minecraft:axolotl/variant"
        | "minecraft:cat/variant"
        | "minecraft:cat/sound_variant"
        | "minecraft:cat/collar"
        | "minecraft:sheep/color"
        | "minecraft:shulker/color" => ComponentCodec::VarInt,
        "minecraft:dyed_color" | "minecraft:map_color" => ComponentCodec::I32,
        "minecraft:minimum_attack_charge" | "minecraft:potion_duration_scale" => {
            ComponentCodec::F32
        }
        "minecraft:enchantment_glint_override" => ComponentCodec::Bool,
        "minecraft:item_model"
        | "minecraft:damage_resistant"
        | "minecraft:note_block_sound"
        | "minecraft:tooltip_style"
        | "minecraft:provides_banner_patterns" => ComponentCodec::String,
        "minecraft:custom_data"
        | "minecraft:custom_name"
        | "minecraft:item_name"
        | "minecraft:bucket_entity_data"
        | "minecraft:container_loot"
        | "minecraft:debug_stick_state"
        | "minecraft:lock"
        | "minecraft:map_decorations"
        | "minecraft:recipes" => ComponentCodec::Nbt,
        "minecraft:lore" => ComponentCodec::NbtList,
        "minecraft:enchantments"
        | "minecraft:stored_enchantments"
        | "minecraft:suspicious_stew_effects" => ComponentCodec::VarIntPairs,
        "minecraft:pot_decorations" => ComponentCodec::VarIntList,
        "minecraft:block_state" => ComponentCodec::StringPairs,
        "minecraft:use_effects" => ComponentCodec::UseEffects,
        "minecraft:food" => ComponentCodec::Food,
        "minecraft:weapon" => ComponentCodec::Weapon,
        "minecraft:swing_animation" => ComponentCodec::TwoVarInts,
        "minecraft:attack_range" => ComponentCodec::AttackRange,
        "minecraft:custom_model_data" => ComponentCodec::CustomModelData,
        "minecraft:tooltip_display" => ComponentCodec::TooltipDisplay,
        "minecraft:potion_contents" => ComponentCodec::PotionContents,
        "minecraft:banner_patterns" => ComponentCodec::BannerPatterns,
        "minecraft:fireworks" => ComponentCodec::Fireworks,
        "minecraft:instrument" => ComponentCodec::Instrument,
        "minecraft:painting/variant" => ComponentCodec::PaintingVariant,
        _ => ComponentCodec::Unsupported,
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ComponentPatch {
    pub added: BTreeMap<MinecraftIdentifier, Vec<u8>>,
    pub removed: BTreeSet<MinecraftIdentifier>,
    pub hashes: BTreeMap<MinecraftIdentifier, i32>,
    /// Parsed registry-aware value for `minecraft:banner_patterns`. The exact
    /// component bytes remain in `added` for cache identity, while all
    /// protocol re-encoding is performed from this bounded semantic value.
    pub banner_patterns: Option<BannerPatternLayers>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectBannerPattern {
    pub asset_id: MinecraftIdentifier,
    pub translation_key: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BannerPatternHolder {
    Reference(MinecraftIdentifier),
    Direct(DirectBannerPattern),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BannerPatternLayer {
    pub pattern: BannerPatternHolder,
    pub dye_raw_id: u8,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BannerPatternLayers {
    pub layers: Vec<BannerPatternLayer>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ItemStack {
    pub item: MinecraftIdentifier,
    pub count: u32,
    pub components: ComponentPatch,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientboundContainerSetContent {
    pub container_id: u32,
    pub state_id: i32,
    pub items: Vec<Option<ItemStack>>,
    pub carried: Option<ItemStack>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientboundContainerSetSlot {
    pub container_id: u32,
    pub state_id: i32,
    pub slot: i16,
    pub item: Option<ItemStack>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientboundOpenScreen {
    pub container_id: u32,
    pub menu: MinecraftIdentifier,
    pub title_nbt: Vec<u8>,
    pub title_plain_text: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClientboundContainerClose {
    pub container_id: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClientboundSetHeldSlot {
    pub slot: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClientboundContainerSetData {
    pub container_id: u32,
    pub property: i16,
    pub value: i16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClientboundInventoryPacket {
    Close(ClientboundContainerClose),
    SetContent(ClientboundContainerSetContent),
    SetData(ClientboundContainerSetData),
    SetSlot(ClientboundContainerSetSlot),
    OpenScreen(ClientboundOpenScreen),
    SetHeldSlot(ClientboundSetHeldSlot),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerboundHashedStack {
    pub item: MinecraftIdentifier,
    pub count: u32,
    pub added_component_hashes: BTreeMap<MinecraftIdentifier, i32>,
    pub removed_components: BTreeSet<MinecraftIdentifier>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerboundContainerClick {
    pub container_id: u32,
    pub state_id: i32,
    pub slot: i16,
    pub button: i8,
    pub mode: i32,
    pub changed_slots: BTreeMap<i16, Option<ServerboundHashedStack>>,
    pub carried: Option<ServerboundHashedStack>,
}

pub fn decode_inventory_clientbound(
    frame_body: &[u8],
    profile: &ItemStackProfile,
) -> Result<Option<ClientboundInventoryPacket>, InventoryCodecError> {
    let packet = split_raw_packet(frame_body)?;
    let decoded = match packet.id {
        CONTAINER_CLOSE_ID => {
            ClientboundInventoryPacket::Close(decode_container_close(packet.payload)?)
        }
        CONTAINER_SET_CONTENT_ID => ClientboundInventoryPacket::SetContent(
            decode_container_set_content(packet.payload, profile)?,
        ),
        CONTAINER_SET_DATA_ID => {
            let mut reader = CodecReader::new(packet.payload);
            let result = ClientboundContainerSetData {
                container_id: read_nonnegative(&mut reader, "container ID")?,
                property: reader.read_i16()?,
                value: reader.read_i16()?,
            };
            finish(&reader)?;
            ClientboundInventoryPacket::SetData(result)
        }
        CONTAINER_SET_SLOT_ID => {
            ClientboundInventoryPacket::SetSlot(decode_container_set_slot(packet.payload, profile)?)
        }
        OPEN_SCREEN_ID => {
            ClientboundInventoryPacket::OpenScreen(decode_open_screen(packet.payload, profile)?)
        }
        SET_HELD_SLOT_ID => {
            ClientboundInventoryPacket::SetHeldSlot(decode_set_held_slot(packet.payload)?)
        }
        _ => return Ok(None),
    };
    Ok(Some(decoded))
}

pub fn decode_container_set_content(
    body: &[u8],
    profile: &ItemStackProfile,
) -> Result<ClientboundContainerSetContent, InventoryCodecError> {
    let mut reader = CodecReader::new(body);
    let container_id = read_nonnegative(&mut reader, "container ID")?;
    let state_id = reader.read_var_int()?;
    let count = read_bounded_count(&mut reader, "container slot count", MAX_CONTAINER_SLOTS)?;
    let mut items = Vec::with_capacity(count);
    for _ in 0..count {
        items.push(decode_item_stack(&mut reader, profile)?);
    }
    let carried = decode_item_stack(&mut reader, profile)?;
    finish(&reader)?;
    Ok(ClientboundContainerSetContent {
        container_id,
        state_id,
        items,
        carried,
    })
}

pub fn decode_container_set_slot(
    body: &[u8],
    profile: &ItemStackProfile,
) -> Result<ClientboundContainerSetSlot, InventoryCodecError> {
    let mut reader = CodecReader::new(body);
    let packet = ClientboundContainerSetSlot {
        container_id: read_nonnegative(&mut reader, "container ID")?,
        state_id: reader.read_var_int()?,
        slot: reader.read_i16()?,
        item: decode_item_stack(&mut reader, profile)?,
    };
    finish(&reader)?;
    Ok(packet)
}

pub fn decode_open_screen(
    body: &[u8],
    profile: &ItemStackProfile,
) -> Result<ClientboundOpenScreen, InventoryCodecError> {
    let mut reader = CodecReader::new(body);
    let container_id = read_nonnegative(&mut reader, "container ID")?;
    let menu_id = read_nonnegative(&mut reader, "menu type")?;
    let menu = profile
        .menus
        .get(&menu_id)
        .cloned()
        .ok_or(InventoryCodecError::UnknownMenuId(menu_id))?;
    let start = reader.position();
    let title = super::decode_text_component(&mut reader)?;
    let title_nbt = reader.consumed_since(start)?.to_vec();
    if title_nbt.len() > TITLE_LIMITS.max_encoded_bytes() {
        return Err(InventoryCodecError::PayloadTooLarge {
            context: "container title",
            size: title_nbt.len(),
            max: TITLE_LIMITS.max_encoded_bytes(),
        });
    }
    finish(&reader)?;
    Ok(ClientboundOpenScreen {
        container_id,
        menu,
        title_nbt,
        title_plain_text: title.plain_text,
    })
}

pub fn decode_container_close(
    body: &[u8],
) -> Result<ClientboundContainerClose, InventoryCodecError> {
    let mut reader = CodecReader::new(body);
    let packet = ClientboundContainerClose {
        container_id: read_nonnegative(&mut reader, "container ID")?,
    };
    finish(&reader)?;
    Ok(packet)
}

pub fn decode_set_held_slot(body: &[u8]) -> Result<ClientboundSetHeldSlot, InventoryCodecError> {
    let mut reader = CodecReader::new(body);
    let slot = read_nonnegative(&mut reader, "held slot")?;
    let slot = u8::try_from(slot).map_err(|_| InventoryCodecError::ValueOutOfRange {
        context: "held slot",
        value: i64::from(slot),
        max: 8,
    })?;
    if slot > 8 {
        return Err(InventoryCodecError::ValueOutOfRange {
            context: "held slot",
            value: i64::from(slot),
            max: 8,
        });
    }
    finish(&reader)?;
    Ok(ClientboundSetHeldSlot { slot })
}

pub(super) fn decode_item_stack(
    reader: &mut CodecReader<'_>,
    profile: &ItemStackProfile,
) -> Result<Option<ItemStack>, InventoryCodecError> {
    let count = reader.read_var_int()?;
    if count == 0 {
        return Ok(None);
    }
    let raw_item = read_nonnegative(reader, "item ID")?;
    decode_nonempty_stack(reader, profile, count, raw_item).map(Some)
}

/// `ItemStackTemplate.STREAM_CODEC` (used by item particles) puts its
/// non-optional item holder before the count; the component patch is shared.
pub(super) fn decode_item_stack_template(
    reader: &mut CodecReader<'_>,
    profile: &ItemStackProfile,
) -> Result<ItemStack, InventoryCodecError> {
    let raw_item = read_nonnegative(reader, "item ID")?;
    let count = reader.read_var_int()?;
    decode_nonempty_stack(reader, profile, count, raw_item)
}

fn decode_nonempty_stack(
    reader: &mut CodecReader<'_>,
    profile: &ItemStackProfile,
    count: i32,
    raw_item: u32,
) -> Result<ItemStack, InventoryCodecError> {
    if count < 0 || !u32::try_from(count).is_ok_and(|count| count <= MAX_STACK_COUNT) {
        return Err(InventoryCodecError::ValueOutOfRange {
            context: "item count",
            value: i64::from(count),
            max: i64::from(MAX_STACK_COUNT),
        });
    }
    let item = profile
        .items
        .get(&raw_item)
        .cloned()
        .ok_or(InventoryCodecError::UnknownItemId(raw_item))?;
    let added = read_bounded_count(reader, "added component count", MAX_COMPONENTS_PER_STACK)?;
    let removed = read_bounded_count(
        reader,
        "removed component count",
        MAX_COMPONENTS_PER_STACK.saturating_sub(added),
    )?;
    let mut patch = ComponentPatch::default();
    for _ in 0..added {
        let raw = read_nonnegative(reader, "component type")?;
        let (identifier, codec) = profile
            .components
            .get(&raw)
            .ok_or(InventoryCodecError::UnknownComponentId(raw))?;
        let start = reader.position();
        let banner_patterns = if *codec == ComponentCodec::BannerPatterns {
            Some(decode_banner_pattern_layers(reader, profile)?)
        } else {
            skip_component(reader, *codec, identifier)?;
            None
        };
        let bytes = reader.consumed_since(start)?;
        if bytes.len() > MAX_COMPONENT_VALUE_BYTES {
            return Err(InventoryCodecError::PayloadTooLarge {
                context: "item component",
                size: bytes.len(),
                max: MAX_COMPONENT_VALUE_BYTES,
            });
        }
        if patch
            .added
            .insert(identifier.clone(), bytes.to_vec())
            .is_some()
        {
            return Err(InventoryCodecError::DuplicateComponent(identifier.clone()));
        }
        if let Some(hash) = profile.component_hash(identifier, bytes)? {
            patch.hashes.insert(identifier.clone(), hash);
        }
        if let Some(patterns) = banner_patterns {
            patch.banner_patterns = Some(patterns);
        }
    }
    for _ in 0..removed {
        let raw = read_nonnegative(reader, "removed component type")?;
        let identifier = profile
            .components
            .get(&raw)
            .map(|(identifier, _)| identifier.clone())
            .ok_or(InventoryCodecError::UnknownComponentId(raw))?;
        if patch.added.contains_key(&identifier) || !patch.removed.insert(identifier.clone()) {
            return Err(InventoryCodecError::DuplicateComponent(identifier));
        }
    }
    Ok(ItemStack {
        item,
        count: u32::try_from(count).map_err(|_| InventoryCodecError::ValueOutOfRange {
            context: "item count",
            value: i64::from(count),
            max: i64::from(MAX_STACK_COUNT),
        })?,
        components: patch,
    })
}

fn skip_component(
    reader: &mut CodecReader<'_>,
    codec: ComponentCodec,
    identifier: &MinecraftIdentifier,
) -> Result<(), InventoryCodecError> {
    match codec {
        ComponentCodec::Unit => {}
        ComponentCodec::VarInt => {
            let _ = reader.read_var_int()?;
        }
        ComponentCodec::I32 => {
            let _ = reader.read_i32()?;
        }
        ComponentCodec::F32 => {
            let _ = reader.read_f32()?;
        }
        ComponentCodec::Bool => {
            let _ = reader.read_bool()?;
        }
        ComponentCodec::String => {
            let _ = reader.read_string(StringLimits::new(32_767, 98_301))?;
        }
        ComponentCodec::Nbt => {
            let _ = decode_unnamed_network_tag(reader, NbtLimits::default())?;
        }
        ComponentCodec::NbtList => {
            let count = read_bounded_count(reader, "component NBT list", 256)?;
            for _ in 0..count {
                let _ = decode_unnamed_network_tag(reader, NbtLimits::default())?;
            }
        }
        ComponentCodec::VarIntPairs => {
            let count = read_bounded_count(reader, "component pair list", 256)?;
            for _ in 0..count {
                let _ = reader.read_var_int()?;
                let _ = reader.read_var_int()?;
            }
        }
        ComponentCodec::VarIntList => {
            let count = read_bounded_count(reader, "component integer list", 256)?;
            for _ in 0..count {
                let _ = reader.read_var_int()?;
            }
        }
        ComponentCodec::StringPairs => {
            let count = read_bounded_count(reader, "component string map", 256)?;
            for _ in 0..count {
                let _ = reader.read_string(StringLimits::new(32_767, 98_301))?;
                let _ = reader.read_string(StringLimits::new(32_767, 98_301))?;
            }
        }
        ComponentCodec::UseEffects => {
            let _ = reader.read_bool()?;
            let _ = reader.read_bool()?;
            let _ = reader.read_f32()?;
        }
        ComponentCodec::Food => {
            let _ = reader.read_var_int()?;
            let _ = reader.read_f32()?;
            let _ = reader.read_bool()?;
        }
        ComponentCodec::Weapon => {
            let _ = reader.read_var_int()?;
            let _ = reader.read_f32()?;
        }
        ComponentCodec::TwoVarInts => {
            let _ = reader.read_var_int()?;
            let _ = reader.read_var_int()?;
        }
        ComponentCodec::AttackRange => {
            for _ in 0..6 {
                let _ = reader.read_f32()?;
            }
        }
        ComponentCodec::CustomModelData => {
            skip_f32_list(reader)?;
            skip_bool_list(reader)?;
            skip_string_list(reader)?;
            let count = read_bounded_count(reader, "custom model colors", 256)?;
            for _ in 0..count {
                let _ = reader.read_i32()?;
            }
        }
        ComponentCodec::TooltipDisplay => {
            let _ = reader.read_bool()?;
            let count = read_bounded_count(reader, "hidden tooltip components", 256)?;
            for _ in 0..count {
                let _ = reader.read_var_int()?;
            }
        }
        ComponentCodec::PotionContents => skip_potion_contents(reader)?,
        ComponentCodec::Fireworks => skip_fireworks(reader)?,
        ComponentCodec::Instrument => skip_registry_holder(reader, skip_instrument)?,
        ComponentCodec::PaintingVariant => skip_registry_holder(reader, skip_painting_variant)?,
        ComponentCodec::BannerPatterns => {
            return Err(InventoryCodecError::InternalCodecDispatch(
                "banner-pattern component bypassed its semantic decoder",
            ));
        }
        ComponentCodec::Unsupported => {
            return Err(InventoryCodecError::UnsupportedComponentCodec(
                identifier.clone(),
            ));
        }
    }
    Ok(())
}

fn skip_registry_holder(
    reader: &mut CodecReader<'_>,
    skip_direct: fn(&mut CodecReader<'_>) -> Result<(), InventoryCodecError>,
) -> Result<(), InventoryCodecError> {
    // ByteBufCodecs.holder uses zero for a direct value and raw registry ID +
    // one for a reference. A reference carries no inline payload.
    if read_nonnegative(reader, "registry holder")? == 0 {
        skip_direct(reader)?;
    }
    Ok(())
}

fn skip_fireworks(reader: &mut CodecReader<'_>) -> Result<(), InventoryCodecError> {
    let _ = reader.read_var_int()?;
    let explosions = read_bounded_count(reader, "firework explosions", 256)?;
    for _ in 0..explosions {
        let shape = read_nonnegative(reader, "firework shape")?;
        if shape > 4 {
            return Err(InventoryCodecError::ValueOutOfRange {
                context: "firework shape",
                value: i64::from(shape),
                max: 4,
            });
        }
        for context in ["firework colors", "firework fade colors"] {
            let colors = read_bounded_count(reader, context, 256)?;
            for _ in 0..colors {
                let _ = reader.read_i32()?;
            }
        }
        let _ = reader.read_bool()?;
        let _ = reader.read_bool()?;
    }
    Ok(())
}

fn skip_instrument(reader: &mut CodecReader<'_>) -> Result<(), InventoryCodecError> {
    skip_registry_holder(reader, skip_sound_event)?;
    let _ = reader.read_f32()?;
    let _ = reader.read_f32()?;
    let _ = decode_unnamed_network_tag(reader, NbtLimits::default())?;
    Ok(())
}

fn skip_sound_event(reader: &mut CodecReader<'_>) -> Result<(), InventoryCodecError> {
    let identifier = reader.read_string(StringLimits::new(256, 1_024))?;
    MinecraftIdentifier::new(identifier)
        .map_err(|_| InventoryCodecError::InvalidInlineIdentifier(identifier.to_owned()))?;
    if reader.read_bool()? {
        let _ = reader.read_f32()?;
    }
    Ok(())
}

fn skip_painting_variant(reader: &mut CodecReader<'_>) -> Result<(), InventoryCodecError> {
    let _ = read_nonnegative(reader, "painting width")?;
    let _ = read_nonnegative(reader, "painting height")?;
    let identifier = reader.read_string(StringLimits::new(256, 1_024))?;
    MinecraftIdentifier::new(identifier)
        .map_err(|_| InventoryCodecError::InvalidInlineIdentifier(identifier.to_owned()))?;
    for _ in 0..2 {
        if reader.read_bool()? {
            let _ = decode_unnamed_network_tag(reader, NbtLimits::default())?;
        }
    }
    Ok(())
}

fn decode_banner_pattern_layers(
    reader: &mut CodecReader<'_>,
    profile: &ItemStackProfile,
) -> Result<BannerPatternLayers, InventoryCodecError> {
    decode_banner_pattern_layers_with(reader, |raw_id| {
        profile.banner_patterns.get(&raw_id).cloned()
    })
}

/// Resolves an exact-version Creative template in its original built-in
/// registry order before the semantic holders are re-encoded for the server's
/// authoritative connection registry. No static numeric ID is sent as-is.
pub fn decode_creative_banner_pattern_layers(
    bytes: &[u8],
    source_registry: &[MinecraftIdentifier],
) -> Result<BannerPatternLayers, InventoryCodecError> {
    let mut reader = CodecReader::new(bytes);
    let layers = decode_banner_pattern_layers_with(&mut reader, |raw_id| {
        usize::try_from(raw_id)
            .ok()
            .and_then(|index| source_registry.get(index))
            .cloned()
    })?;
    finish(&reader)?;
    Ok(layers)
}

fn decode_banner_pattern_layers_with(
    reader: &mut CodecReader<'_>,
    mut reference: impl FnMut(u32) -> Option<MinecraftIdentifier>,
) -> Result<BannerPatternLayers, InventoryCodecError> {
    let count = read_bounded_count(reader, "banner-pattern layer", MAX_BANNER_PATTERN_LAYERS)?;
    let mut layers = Vec::new();
    layers
        .try_reserve_exact(count)
        .map_err(|_| CodecError::AllocationFailed {
            context: "banner-pattern layers",
            requested: count,
        })?;
    for _ in 0..count {
        // ByteBufCodecs.holder encodes direct values as zero and registry
        // references as registry raw ID + 1.
        let encoded_holder = read_nonnegative(reader, "banner-pattern holder")?;
        let pattern = if encoded_holder == 0 {
            let asset = reader.read_string(BANNER_PATTERN_ASSET_LIMITS)?;
            let asset_id = MinecraftIdentifier::new(asset).map_err(|_| {
                InventoryCodecError::InvalidBannerPatternIdentifier(asset.to_owned())
            })?;
            let translation_key = reader
                .read_string(BANNER_PATTERN_TRANSLATION_LIMITS)?
                .to_owned();
            BannerPatternHolder::Direct(DirectBannerPattern {
                asset_id,
                translation_key,
            })
        } else {
            let raw_id = encoded_holder - 1;
            let identifier =
                reference(raw_id).ok_or(InventoryCodecError::UnknownBannerPatternId(raw_id))?;
            BannerPatternHolder::Reference(identifier)
        };
        let dye = read_nonnegative(reader, "banner-pattern dye")?;
        let dye_raw_id = u8::try_from(dye)
            .ok()
            .filter(|dye| *dye < 16)
            .ok_or(InventoryCodecError::InvalidDyeColor(dye))?;
        layers.push(BannerPatternLayer {
            pattern,
            dye_raw_id,
        });
    }
    Ok(BannerPatternLayers { layers })
}

fn encode_banner_pattern_layers(
    writer: &mut CodecWriter,
    patterns: &BannerPatternLayers,
    profile: &ItemStackProfile,
) -> Result<(), InventoryCodecError> {
    if patterns.layers.len() > MAX_BANNER_PATTERN_LAYERS {
        return Err(InventoryCodecError::CountTooLarge {
            context: "banner-pattern layer",
            count: patterns.layers.len(),
            max: MAX_BANNER_PATTERN_LAYERS,
        });
    }
    write_len(writer, patterns.layers.len(), "banner-pattern layer")?;
    for layer in &patterns.layers {
        match &layer.pattern {
            BannerPatternHolder::Reference(identifier) => {
                let raw_id = profile
                    .banner_pattern_raw_ids
                    .get(identifier)
                    .copied()
                    .ok_or_else(|| InventoryCodecError::UnknownBannerPattern(identifier.clone()))?;
                let encoded =
                    raw_id
                        .checked_add(1)
                        .ok_or(InventoryCodecError::ValueOutOfRange {
                            context: "banner-pattern holder",
                            value: i64::from(raw_id),
                            max: i64::from(i32::MAX - 1),
                        })?;
                write_u32_varint(writer, encoded, "banner-pattern holder")?;
            }
            BannerPatternHolder::Direct(pattern) => {
                writer.write_var_int(0);
                writer.write_string(pattern.asset_id.as_str(), BANNER_PATTERN_ASSET_LIMITS)?;
                writer.write_string(&pattern.translation_key, BANNER_PATTERN_TRANSLATION_LIMITS)?;
            }
        }
        if layer.dye_raw_id >= 16 {
            return Err(InventoryCodecError::InvalidDyeColor(u32::from(
                layer.dye_raw_id,
            )));
        }
        writer.write_var_int(i32::from(layer.dye_raw_id));
    }
    Ok(())
}

fn skip_f32_list(reader: &mut CodecReader<'_>) -> Result<(), InventoryCodecError> {
    let count = read_bounded_count(reader, "component float list", 256)?;
    for _ in 0..count {
        let _ = reader.read_f32()?;
    }
    Ok(())
}

fn skip_bool_list(reader: &mut CodecReader<'_>) -> Result<(), InventoryCodecError> {
    let count = read_bounded_count(reader, "component boolean list", 256)?;
    for _ in 0..count {
        let _ = reader.read_bool()?;
    }
    Ok(())
}

fn skip_string_list(reader: &mut CodecReader<'_>) -> Result<(), InventoryCodecError> {
    let count = read_bounded_count(reader, "component string list", 256)?;
    for _ in 0..count {
        let _ = reader.read_string(StringLimits::new(32_767, 98_301))?;
    }
    Ok(())
}

fn skip_potion_contents(reader: &mut CodecReader<'_>) -> Result<(), InventoryCodecError> {
    if reader.read_bool()? {
        let _ = reader.read_var_int()?;
    }
    if reader.read_bool()? {
        let _ = reader.read_i32()?;
    }
    let count = read_bounded_count(reader, "custom potion effects", 256)?;
    for _ in 0..count {
        let _ = reader.read_var_int()?;
        skip_effect_detail(reader, 0)?;
    }
    if reader.read_bool()? {
        let _ = reader.read_string(StringLimits::new(32_767, 98_301))?;
    }
    Ok(())
}

fn skip_effect_detail(
    reader: &mut CodecReader<'_>,
    depth: usize,
) -> Result<(), InventoryCodecError> {
    if depth >= 16 {
        return Err(InventoryCodecError::ComponentNestingTooDeep { max: 16 });
    }
    let _ = reader.read_var_int()?;
    let _ = reader.read_var_int()?;
    let _ = reader.read_bool()?;
    let _ = reader.read_bool()?;
    let _ = reader.read_bool()?;
    if reader.read_bool()? {
        skip_effect_detail(reader, depth + 1)?;
    }
    Ok(())
}

pub fn encode_container_click(
    packet: &ServerboundContainerClick,
    profile: &ItemStackProfile,
) -> Result<Vec<u8>, InventoryCodecError> {
    if packet.changed_slots.len() > MAX_CHANGED_SLOTS {
        return Err(InventoryCodecError::TooManyChangedSlots {
            count: packet.changed_slots.len(),
            max: MAX_CHANGED_SLOTS,
        });
    }
    let mut writer = CodecWriter::new();
    write_u32_varint(&mut writer, packet.container_id, "container ID")?;
    writer.write_var_int(packet.state_id);
    writer.write_i16(packet.slot);
    writer.write_i8(packet.button);
    writer.write_var_int(packet.mode);
    write_len(
        &mut writer,
        packet.changed_slots.len(),
        "changed slot count",
    )?;
    for (slot, item) in &packet.changed_slots {
        writer.write_i16(*slot);
        encode_hashed_stack(&mut writer, item.as_ref(), profile)?;
    }
    encode_hashed_stack(&mut writer, packet.carried.as_ref(), profile)?;
    Ok(writer.into_inner())
}

pub fn encode_play_container_click(
    packet: &ServerboundContainerClick,
    profile: &ItemStackProfile,
) -> Result<Vec<u8>, InventoryCodecError> {
    let body = encode_container_click(packet, profile)?;
    framed(SERVERBOUND_CONTAINER_CLICK_ID, &body)
}

pub fn encode_play_container_close(container_id: u32) -> Result<Vec<u8>, InventoryCodecError> {
    let mut body = CodecWriter::new();
    write_u32_varint(&mut body, container_id, "container ID")?;
    framed(SERVERBOUND_CONTAINER_CLOSE_ID, body.as_slice())
}

pub fn encode_play_set_carried_item(slot: u8) -> Result<Vec<u8>, InventoryCodecError> {
    if slot > 8 {
        return Err(InventoryCodecError::ValueOutOfRange {
            context: "hotbar slot",
            value: i64::from(slot),
            max: 8,
        });
    }
    let mut body = CodecWriter::new();
    body.write_i16(i16::from(slot));
    framed(SERVERBOUND_SET_CARRIED_ITEM_ID, body.as_slice())
}

pub fn encode_play_set_creative_slot(
    slot: i16,
    stack: Option<&ItemStack>,
    profile: &ItemStackProfile,
) -> Result<Vec<u8>, InventoryCodecError> {
    if slot < -1 || usize::try_from(slot).is_ok_and(|slot| slot >= 46) {
        return Err(InventoryCodecError::ValueOutOfRange {
            context: "Creative inventory slot",
            value: i64::from(slot),
            max: 45,
        });
    }
    let mut body = CodecWriter::new();
    body.write_i16(slot);
    encode_item_stack(&mut body, stack, profile)?;
    framed(SERVERBOUND_SET_CREATIVE_SLOT_ID, body.as_slice())
}

#[must_use]
pub const fn creative_slot_packet_id() -> i32 {
    SERVERBOUND_SET_CREATIVE_SLOT_ID
}

fn encode_item_stack(
    writer: &mut CodecWriter,
    stack: Option<&ItemStack>,
    profile: &ItemStackProfile,
) -> Result<(), InventoryCodecError> {
    let Some(stack) = stack else {
        writer.write_var_int(0);
        return Ok(());
    };
    if stack.count == 0 || stack.count > MAX_STACK_COUNT {
        return Err(InventoryCodecError::ValueOutOfRange {
            context: "item count",
            value: i64::from(stack.count),
            max: i64::from(MAX_STACK_COUNT),
        });
    }
    writer.write_var_int(i32::try_from(stack.count).map_err(|_| {
        InventoryCodecError::ValueOutOfRange {
            context: "item count",
            value: i64::from(stack.count),
            max: i64::from(MAX_STACK_COUNT),
        }
    })?);
    let raw_item = profile
        .item_raw_id(&stack.item)
        .ok_or_else(|| InventoryCodecError::UnknownItem(stack.item.clone()))?;
    write_u32_varint(writer, raw_item, "item ID")?;
    write_len(
        writer,
        stack.components.added.len(),
        "added component count",
    )?;
    write_len(
        writer,
        stack.components.removed.len(),
        "removed component count",
    )?;
    for (component, value) in &stack.components.added {
        let raw = profile
            .component_raw_id(component)
            .ok_or_else(|| InventoryCodecError::UnknownComponent(component.clone()))?;
        write_u32_varint(writer, raw, "component type")?;
        let codec = profile
            .components
            .get(&raw)
            .map(|(_, codec)| *codec)
            .ok_or_else(|| InventoryCodecError::UnknownComponent(component.clone()))?;
        // Serverbound Creative Slot uses ItemStack.OPTIONAL_UNTRUSTED_STREAM_CODEC.
        // Unlike the clientbound stack codec, its DataComponentPatch delimits
        // every added component value with a VarInt byte length.
        let component_value = encode_added_component_value(stack, value, codec, profile)?;
        write_len(writer, component_value.len(), "item component length")?;
        writer.write_bytes(&component_value);
    }
    for component in &stack.components.removed {
        let raw = profile
            .component_raw_id(component)
            .ok_or_else(|| InventoryCodecError::UnknownComponent(component.clone()))?;
        write_u32_varint(writer, raw, "component type")?;
    }
    Ok(())
}

fn encode_added_component_value(
    stack: &ItemStack,
    value: &[u8],
    codec: ComponentCodec,
    profile: &ItemStackProfile,
) -> Result<Vec<u8>, InventoryCodecError> {
    let mut encoded = CodecWriter::new();
    if codec == ComponentCodec::BannerPatterns {
        let parsed;
        let patterns = if let Some(patterns) = &stack.components.banner_patterns {
            patterns
        } else {
            let mut reader = CodecReader::new(value);
            parsed = decode_banner_pattern_layers(&mut reader, profile)?;
            finish(&reader)?;
            &parsed
        };
        encode_banner_pattern_layers(&mut encoded, patterns, profile)?;
    } else {
        encoded.write_bytes(value);
    }
    if encoded.len() > MAX_COMPONENT_VALUE_BYTES {
        return Err(InventoryCodecError::PayloadTooLarge {
            context: "item component",
            size: encoded.len(),
            max: MAX_COMPONENT_VALUE_BYTES,
        });
    }
    Ok(encoded.into_inner())
}

pub fn encode_play_container_button_click(
    container_id: u32,
    button: u32,
) -> Result<Vec<u8>, InventoryCodecError> {
    let mut body = CodecWriter::new();
    write_u32_varint(&mut body, container_id, "container ID")?;
    write_u32_varint(&mut body, button, "container button")?;
    framed(SERVERBOUND_CONTAINER_BUTTON_CLICK_ID, body.as_slice())
}

fn framed(packet_id: i32, body: &[u8]) -> Result<Vec<u8>, InventoryCodecError> {
    let mut packet = CodecWriter::new();
    packet.write_var_int(packet_id);
    packet.write_bytes(body);
    Ok(encode_frame(packet.as_slice(), MAX_BOOTSTRAP_FRAME_SIZE)?)
}

fn encode_hashed_stack(
    writer: &mut CodecWriter,
    stack: Option<&ServerboundHashedStack>,
    profile: &ItemStackProfile,
) -> Result<(), InventoryCodecError> {
    let Some(stack) = stack else {
        writer.write_bool(false);
        return Ok(());
    };
    writer.write_bool(true);
    let item = profile
        .item_raw_id(&stack.item)
        .ok_or_else(|| InventoryCodecError::UnknownItem(stack.item.clone()))?;
    write_u32_varint(writer, item, "item ID")?;
    write_u32_varint(writer, stack.count, "item count")?;
    write_len(
        writer,
        stack.added_component_hashes.len(),
        "hashed component count",
    )?;
    for (component, hash) in &stack.added_component_hashes {
        let raw = profile
            .component_raw_id(component)
            .ok_or_else(|| InventoryCodecError::UnknownComponent(component.clone()))?;
        write_u32_varint(writer, raw, "component type")?;
        writer.write_i32(*hash);
    }
    write_len(
        writer,
        stack.removed_components.len(),
        "removed component count",
    )?;
    for component in &stack.removed_components {
        let raw = profile
            .component_raw_id(component)
            .ok_or_else(|| InventoryCodecError::UnknownComponent(component.clone()))?;
        write_u32_varint(writer, raw, "component type")?;
    }
    Ok(())
}

fn read_nonnegative(
    reader: &mut CodecReader<'_>,
    context: &'static str,
) -> Result<u32, InventoryCodecError> {
    let value = reader.read_var_int()?;
    u32::try_from(value).map_err(|_| InventoryCodecError::NegativeValue { context, value })
}

fn read_bounded_count(
    reader: &mut CodecReader<'_>,
    context: &'static str,
    max: usize,
) -> Result<usize, InventoryCodecError> {
    let value = reader.read_var_int()?;
    let count = usize::try_from(value)
        .map_err(|_| InventoryCodecError::NegativeValue { context, value })?;
    if count > max {
        return Err(InventoryCodecError::CountTooLarge {
            context,
            count,
            max,
        });
    }
    Ok(count)
}

fn write_u32_varint(
    writer: &mut CodecWriter,
    value: u32,
    context: &'static str,
) -> Result<(), InventoryCodecError> {
    let value = i32::try_from(value).map_err(|_| InventoryCodecError::ValueOutOfRange {
        context,
        value: i64::from(value),
        max: i64::from(i32::MAX),
    })?;
    writer.write_var_int(value);
    Ok(())
}

fn write_len(
    writer: &mut CodecWriter,
    value: usize,
    context: &'static str,
) -> Result<(), InventoryCodecError> {
    let value = i32::try_from(value).map_err(|_| InventoryCodecError::ValueOutOfRange {
        context,
        value: value as i64,
        max: i64::from(i32::MAX),
    })?;
    writer.write_var_int(value);
    Ok(())
}

fn finish(reader: &CodecReader<'_>) -> Result<(), InventoryCodecError> {
    if reader.remaining() != 0 {
        return Err(InventoryCodecError::TrailingData(reader.remaining()));
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum InventoryCodecError {
    #[error("inventory bootstrap codec failed: {0}")]
    Bootstrap(#[from] super::BootstrapProtocolError),
    #[error("inventory codec failed: {0}")]
    Codec(#[from] CodecError),
    #[error("inventory NBT failed: {0}")]
    Nbt(#[from] NbtError),
    #[error("generated game data is missing required registry {0}")]
    MissingRegistry(&'static str),
    #[error("invalid built-in inventory profile identifier {0}")]
    InvalidBuiltInProfile(&'static str),
    #[error("protocol 775 has no bounded component codec for {0}")]
    UnsupportedComponentCodec(MinecraftIdentifier),
    #[error("unknown runtime item ID {0}")]
    UnknownItemId(u32),
    #[error("unknown runtime component ID {0}")]
    UnknownComponentId(u32),
    #[error("unknown runtime menu ID {0}")]
    UnknownMenuId(u32),
    #[error("unknown item {0}")]
    UnknownItem(MinecraftIdentifier),
    #[error("unknown component {0}")]
    UnknownComponent(MinecraftIdentifier),
    #[error("unknown banner-pattern registry ID {0}")]
    UnknownBannerPatternId(u32),
    #[error("unknown banner-pattern registry entry {0}")]
    UnknownBannerPattern(MinecraftIdentifier),
    #[error("invalid direct banner-pattern asset identifier {0:?}")]
    InvalidBannerPatternIdentifier(String),
    #[error("invalid inline registry identifier {0:?}")]
    InvalidInlineIdentifier(String),
    #[error("invalid banner-pattern dye raw ID {0}")]
    InvalidDyeColor(u32),
    #[error("duplicate or conflicting item component {0}")]
    DuplicateComponent(MinecraftIdentifier),
    #[error("negative {context}: {value}")]
    NegativeValue { context: &'static str, value: i32 },
    #[error("{context} count {count} exceeds maximum {max}")]
    CountTooLarge {
        context: &'static str,
        count: usize,
        max: usize,
    },
    #[error("{context} value {value} exceeds maximum {max}")]
    ValueOutOfRange {
        context: &'static str,
        value: i64,
        max: i64,
    },
    #[error("inventory payload has {0} trailing bytes")]
    TrailingData(usize),
    #[error("{context} retains {size} bytes, maximum is {max}")]
    PayloadTooLarge {
        context: &'static str,
        size: usize,
        max: usize,
    },
    #[error("click contains {count} changed slots, maximum is {max}")]
    TooManyChangedSlots { count: usize, max: usize },
    #[error("item component nesting exceeds maximum depth {max}")]
    ComponentNestingTooDeep { max: usize },
    #[error("inventory component codec dispatch error: {0}")]
    InternalCodecDispatch(&'static str),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> ItemStackProfile {
        ItemStackProfile::synthetic(
            [(1, "minecraft:stone"), (2, "minecraft:diamond_pickaxe")],
            [
                (0, "minecraft:custom_data"),
                (1, "minecraft:max_stack_size"),
                (3, "minecraft:damage"),
                (4, "minecraft:unbreakable"),
            ],
            [(0, "minecraft:generic_9x3")],
        )
    }

    fn banner_profile() -> ItemStackProfile {
        let mut profile = ItemStackProfile::synthetic(
            [(1, "minecraft:white_banner")],
            [(5, "minecraft:banner_patterns")],
            [],
        );
        profile.banner_patterns = identifiers([(0, "minecraft:base"), (1, "minecraft:creeper")]);
        profile.banner_pattern_raw_ids = reverse(&profile.banner_patterns);
        profile
    }

    #[test]
    fn independent_empty_and_simple_stack_vector() {
        let profile = profile();
        let packet = decode_container_set_content(
            &[0x00, 0x07, 0x02, 0x00, 0x03, 0x01, 0x00, 0x00, 0x00],
            &profile,
        )
        .unwrap();
        assert_eq!(packet.container_id, 0);
        assert_eq!(packet.state_id, 7);
        assert_eq!(packet.items[0], None);
        assert_eq!(packet.items[1].as_ref().unwrap().count, 3);
        assert_eq!(
            packet.items[1].as_ref().unwrap().item.as_str(),
            "minecraft:stone"
        );
        assert_eq!(packet.carried, None);
    }

    #[test]
    fn component_patch_preserves_exact_value_bytes() {
        let profile = profile();
        let packet = decode_container_set_slot(
            &[0x00, 0x01, 0x00, 0x24, 0x01, 0x02, 0x01, 0x00, 0x03, 0x11],
            &profile,
        )
        .unwrap();
        let stack = packet.item.unwrap();
        assert_eq!(stack.item.as_str(), "minecraft:diamond_pickaxe");
        assert_eq!(
            stack.components.added[&MinecraftIdentifier::new("minecraft:damage").unwrap()],
            vec![0x11]
        );
        assert!(
            stack
                .components
                .hashes
                .contains_key(&MinecraftIdentifier::new("minecraft:damage").unwrap())
        );
    }

    #[test]
    fn banner_patterns_reference_and_direct_holders_round_trip_semantically() {
        let profile = banner_profile();
        let mut body = CodecWriter::new();
        body.write_var_int(0);
        body.write_var_int(1);
        body.write_i16(36);
        body.write_var_int(1);
        body.write_var_int(1);
        body.write_var_int(1);
        body.write_var_int(0);
        body.write_var_int(5);
        body.write_var_int(2);
        body.write_var_int(2); // reference raw ID 1 + 1
        body.write_var_int(15);
        body.write_var_int(0); // direct holder discriminator
        body.write_string("minecraft:test", BANNER_PATTERN_ASSET_LIMITS)
            .unwrap();
        body.write_string("block.minecraft.test", BANNER_PATTERN_TRANSLATION_LIMITS)
            .unwrap();
        body.write_var_int(1);
        let body = body.into_inner();

        let decoded = decode_container_set_slot(&body, &profile).unwrap();
        let stack = decoded.item.unwrap();
        let patterns = stack.components.banner_patterns.as_ref().unwrap();
        assert_eq!(patterns.layers.len(), 2);
        assert_eq!(
            patterns.layers[0],
            BannerPatternLayer {
                pattern: BannerPatternHolder::Reference(
                    MinecraftIdentifier::new("minecraft:creeper").unwrap()
                ),
                dye_raw_id: 15,
            }
        );
        assert!(matches!(
            &patterns.layers[1],
            BannerPatternLayer {
                pattern: BannerPatternHolder::Direct(DirectBannerPattern { asset_id, translation_key }),
                dye_raw_id: 1,
            } if asset_id.as_str() == "minecraft:test" && translation_key == "block.minecraft.test"
        ));

        let framed = encode_play_set_creative_slot(36, Some(&stack), &profile).unwrap();
        let mut frame = CodecReader::new(&framed);
        let frame_len = usize::try_from(frame.read_var_int().unwrap()).unwrap();
        let packet = frame.read_remaining();
        assert_eq!(packet.len(), frame_len);
        let raw = split_raw_packet(packet).unwrap();
        assert_eq!(raw.id, SERVERBOUND_SET_CREATIVE_SLOT_ID);
        let mut payload = CodecReader::new(raw.payload);
        assert_eq!(payload.read_i16().unwrap(), 36);
        let reencoded = decode_untrusted_test_stack(&mut payload, &profile);
        finish(&payload).unwrap();
        assert_eq!(reencoded.components.banner_patterns, Some(patterns.clone()));
    }

    // The Creative Slot packet uses the serverbound, per-component-delimited
    // stack codec. Rebuild its clientbound counterpart here to exercise the
    // independent decoder without teaching production code a false symmetry.
    fn decode_untrusted_test_stack(
        reader: &mut CodecReader<'_>,
        profile: &ItemStackProfile,
    ) -> ItemStack {
        let mut clientbound = CodecWriter::new();
        let count = reader.read_var_int().unwrap();
        let item = reader.read_var_int().unwrap();
        let added = reader.read_var_int().unwrap();
        let removed = reader.read_var_int().unwrap();
        for value in [count, item, added, removed] {
            clientbound.write_var_int(value);
        }
        for _ in 0..added {
            let component = reader.read_var_int().unwrap();
            let length = usize::try_from(reader.read_var_int().unwrap()).unwrap();
            let bytes = reader.read_bytes(length, "test component").unwrap();
            clientbound.write_var_int(component);
            clientbound.write_bytes(bytes);
        }
        for _ in 0..removed {
            clientbound.write_var_int(reader.read_var_int().unwrap());
        }
        let mut decoded = CodecReader::new(clientbound.as_slice());
        let stack = decode_item_stack(&mut decoded, profile).unwrap().unwrap();
        finish(&decoded).unwrap();
        stack
    }

    #[test]
    fn creative_slot_component_is_length_delimited_like_vanilla_untrusted_stack() {
        let profile = banner_profile();
        let id = MinecraftIdentifier::new("minecraft:banner_patterns").unwrap();
        let stack = ItemStack {
            item: MinecraftIdentifier::new("minecraft:white_banner").unwrap(),
            count: 1,
            components: ComponentPatch {
                added: BTreeMap::from([(id, vec![1, 2, 15])]),
                ..ComponentPatch::default()
            },
        };
        assert_eq!(
            encode_play_set_creative_slot(36, Some(&stack), &profile).unwrap(),
            [
                0x0c, 0x38, 0x00, 0x24, 0x01, 0x01, 0x01, 0x00, 0x05, 0x03, 0x01, 0x02, 0x0f
            ]
        );
    }

    #[test]
    fn creative_template_reference_rebases_to_connection_registry_id() {
        let mut profile = banner_profile();
        let source = identifiers([(0, "minecraft:base"), (1, "minecraft:creeper")]);
        let source = source.into_values().collect::<Vec<_>>();
        let layers = decode_creative_banner_pattern_layers(&[1, 2, 15], &source).unwrap();
        profile
            .install_banner_patterns([
                MinecraftIdentifier::new("minecraft:creeper").unwrap(),
                MinecraftIdentifier::new("minecraft:base").unwrap(),
            ])
            .unwrap();
        let stack = ItemStack {
            item: MinecraftIdentifier::new("minecraft:white_banner").unwrap(),
            count: 1,
            components: ComponentPatch {
                added: BTreeMap::from([(
                    MinecraftIdentifier::new("minecraft:banner_patterns").unwrap(),
                    vec![1, 2, 15],
                )]),
                banner_patterns: Some(layers),
                ..ComponentPatch::default()
            },
        };
        assert_eq!(
            encode_play_set_creative_slot(36, Some(&stack), &profile).unwrap(),
            [
                0x0c, 0x38, 0x00, 0x24, 0x01, 0x01, 0x01, 0x00, 0x05, 0x03, 0x01, 0x01, 0x0f
            ]
        );
    }

    #[test]
    fn higher_banner_registry_id_and_missing_reference_remain_strict() {
        let mut profile = banner_profile();
        let higher = MinecraftIdentifier::new("minecraft:triangles_top").unwrap();
        profile
            .install_banner_patterns((0..=41).map(|index| {
                if index == 41 {
                    higher.clone()
                } else {
                    MinecraftIdentifier::new(format!("minecraft:unused_{index}")).unwrap()
                }
            }))
            .unwrap();
        let pattern = BannerPatternLayers {
            layers: vec![BannerPatternLayer {
                pattern: BannerPatternHolder::Reference(higher.clone()),
                dye_raw_id: 4,
            }],
        };
        let mut encoded = CodecWriter::new();
        encode_banner_pattern_layers(&mut encoded, &pattern, &profile).unwrap();
        assert_eq!(encoded.as_slice(), [1, 42, 4]);
        profile.install_banner_patterns([]).unwrap();
        assert!(matches!(
            encode_banner_pattern_layers(&mut CodecWriter::new(), &pattern, &profile),
            Err(InventoryCodecError::UnknownBannerPattern(id)) if id == higher
        ));
    }

    #[test]
    fn malformed_banner_patterns_are_rejected_without_becoming_empty() {
        let profile = banner_profile();
        for value in [
            vec![21],       // over bounded layer count
            vec![1, 3, 0],  // unknown registry reference raw ID 2
            vec![1, 1, 16], // invalid dye
            vec![1, 0],     // truncated direct value
        ] {
            let mut reader = CodecReader::new(&value);
            assert!(decode_banner_pattern_layers(&mut reader, &profile).is_err());
        }
    }

    #[test]
    fn ominous_banner_hotbar_stack_round_trips_all_ordered_layers() {
        let mut profile = ItemStackProfile::synthetic(
            [(1, "minecraft:white_banner")],
            [(5, "minecraft:banner_patterns")],
            [],
        );
        profile.banner_patterns = identifiers([
            (1, "minecraft:border"),
            (3, "minecraft:circle"),
            (17, "minecraft:half_horizontal"),
            (23, "minecraft:rhombus"),
            (31, "minecraft:stripe_bottom"),
            (32, "minecraft:stripe_center"),
            (36, "minecraft:stripe_middle"),
        ]);
        profile.banner_pattern_raw_ids = reverse(&profile.banner_patterns);
        // Player inventory Set Slot: container 0, state 19, hotbar slot 36,
        // one white banner with the canonical 26.1.2 Ominous Banner layers.
        let component = [8, 24, 9, 32, 8, 33, 7, 2, 8, 37, 15, 18, 8, 4, 8, 2, 15];
        let mut body = CodecWriter::new();
        body.write_var_int(0);
        body.write_var_int(19);
        body.write_i16(36);
        body.write_var_int(1);
        body.write_var_int(1);
        body.write_var_int(1);
        body.write_var_int(0);
        body.write_var_int(5);
        body.write_bytes(&component);
        let stack = decode_container_set_slot(&body.into_inner(), &profile)
            .unwrap()
            .item
            .unwrap();
        let layers = &stack.components.banner_patterns.as_ref().unwrap().layers;
        assert_eq!(layers.len(), 8);
        assert_eq!(layers[0].dye_raw_id, 9);
        assert!(matches!(
            &layers[0].pattern,
            BannerPatternHolder::Reference(id) if id.as_str() == "minecraft:rhombus"
        ));
        assert!(matches!(
            &layers[7].pattern,
            BannerPatternHolder::Reference(id) if id.as_str() == "minecraft:border"
        ));

        let framed = encode_play_set_creative_slot(36, Some(&stack), &profile).unwrap();
        let mut frame = CodecReader::new(&framed);
        let _ = frame.read_var_int().unwrap();
        let raw = split_raw_packet(frame.read_remaining()).unwrap();
        let mut payload = CodecReader::new(raw.payload);
        assert_eq!(payload.read_i16().unwrap(), 36);
        let returned = decode_untrusted_test_stack(&mut payload, &profile);
        finish(&payload).unwrap();
        assert_eq!(
            returned.components.banner_patterns,
            stack.components.banner_patterns
        );
        assert_eq!(
            returned.components.added
                [&MinecraftIdentifier::new("minecraft:banner_patterns").unwrap()],
            component
        );
    }

    #[test]
    fn canonical_ominous_banner_matches_official_untrusted_stack_oracle() {
        // Body of vanilla 26.1.2 ServerboundSetCreativeModeSlotPacket (slot
        // 36), produced by its RegistryFriendlyByteBuf STREAM_CODEC with the
        // canonical Creative Ominous Banner. No packet framing or ID is here.
        let oracle = "002401f30904004811081809200821070208250f12080408020f1203000148092e0a0800097472616e736c617465001e626c6f636b2e6d696e6563726166742e6f6d696e6f75735f62616e6e6572000c0101";
        let oracle: Vec<u8> = oracle
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect();
        let creative = cubic_version::CreativeData::builtin_26_1_2().unwrap();
        let canonical = creative
            .tabs(false)
            .iter()
            .flat_map(|tab| &tab.items)
            .find(|item| {
                item.item.as_str() == "minecraft:white_banner"
                    && item
                        .components
                        .iter()
                        .any(|component| component.id.as_str() == "minecraft:banner_patterns")
            })
            .unwrap();
        let mut profile = ItemStackProfile::synthetic(
            [(1267, "minecraft:white_banner")],
            [
                (72, "minecraft:banner_patterns"),
                (18, "minecraft:tooltip_display"),
                (9, "minecraft:item_name"),
                (12, "minecraft:rarity"),
            ],
            [],
        );
        profile.banner_patterns = creative
            .banner_patterns
            .iter()
            .enumerate()
            .map(|(raw, id)| (u32::try_from(raw).unwrap(), id.clone()))
            .collect();
        profile.banner_pattern_raw_ids = reverse(&profile.banner_patterns);
        let stack = ItemStack {
            item: canonical.item.clone(),
            count: canonical.count,
            components: ComponentPatch {
                added: canonical
                    .components
                    .iter()
                    .map(|component| {
                        (
                            component.id.clone(),
                            component.decoded_value().unwrap().unwrap(),
                        )
                    })
                    .collect(),
                ..ComponentPatch::default()
            },
        };
        let framed = encode_play_set_creative_slot(36, Some(&stack), &profile).unwrap();
        let mut frame = CodecReader::new(&framed);
        assert_eq!(
            usize::try_from(frame.read_var_int().unwrap()).unwrap(),
            frame.remaining()
        );
        let packet = split_raw_packet(frame.read_remaining()).unwrap();
        assert_eq!(packet.id, SERVERBOUND_SET_CREATIVE_SLOT_ID);
        // The first added component is the banner pattern value. Its VarInt
        // length byte (0x11) was the first divergence in the failing packet.
        assert_eq!(&packet.payload[..26], &oracle[..26]);
        let mut vanilla = CodecReader::new(&oracle);
        assert_eq!(vanilla.read_i16().unwrap(), 36);
        let vanilla_stack = decode_untrusted_test_stack(&mut vanilla, &profile);
        finish(&vanilla).unwrap();
        let mut cubic = CodecReader::new(packet.payload);
        assert_eq!(cubic.read_i16().unwrap(), 36);
        let cubic_stack = decode_untrusted_test_stack(&mut cubic, &profile);
        finish(&cubic).unwrap();
        // Vanilla's in-memory component insertion order differs from Cubic's
        // stable identifier ordering, but the complete patch is identical.
        assert_eq!(cubic_stack, vanilla_stack);
        assert_eq!(cubic_stack.components.added, stack.components.added);
        assert_eq!(
            cubic_stack.components.banner_patterns.unwrap().layers.len(),
            8
        );
    }

    #[test]
    fn every_canonical_creative_component_has_a_bounded_reencodable_codec() {
        let creative = cubic_version::CreativeData::builtin_26_1_2().unwrap();
        let mut profile = ItemStackProfile::synthetic([], [], []);
        profile.banner_patterns = creative
            .banner_patterns
            .iter()
            .enumerate()
            .map(|(raw, identifier)| (u32::try_from(raw).unwrap(), identifier.clone()))
            .collect();
        profile.banner_pattern_raw_ids = reverse(&profile.banner_patterns);
        let mut unsupported = BTreeSet::new();
        let mut malformed = BTreeSet::new();
        for component in creative
            .tabs(true)
            .iter()
            .flat_map(|tab| std::iter::once(&tab.icon).chain(&tab.items))
            .flat_map(|stack| &stack.components)
        {
            let codec = component_codec(&component.id);
            if codec == ComponentCodec::Unsupported {
                unsupported.insert(component.id.clone());
                continue;
            }
            let Some(value) = component.decoded_value().unwrap() else {
                continue;
            };
            let mut reader = CodecReader::new(&value);
            let result = if codec == ComponentCodec::BannerPatterns {
                decode_banner_pattern_layers(&mut reader, &profile).map(|_| ())
            } else {
                skip_component(&mut reader, codec, &component.id)
            }
            .and_then(|()| finish(&reader));
            if result.is_err() {
                malformed.insert(component.id.clone());
            }
        }
        assert!(
            unsupported.is_empty(),
            "unsupported canonical components: {unsupported:?}"
        );
        assert!(
            malformed.is_empty(),
            "malformed canonical components: {malformed:?}"
        );
    }

    #[test]
    fn malformed_component_and_slot_counts_are_bounded() {
        let profile = profile();
        assert!(matches!(
            decode_container_set_content(&[0x00, 0x00, 0x81, 0x04], &profile),
            Err(InventoryCodecError::CountTooLarge { .. })
        ));
        assert!(matches!(
            decode_container_set_slot(
                &[0x00, 0x00, 0x00, 0x00, 0x01, 0x01, 0x01, 0x00, 0x05, 0x00],
                &profile
            ),
            Err(InventoryCodecError::UnknownComponentId(5))
        ));
    }

    #[test]
    fn click_vector_uses_hashed_stack_option_and_no_full_stack() {
        let profile = profile();
        let packet = ServerboundContainerClick {
            container_id: 2,
            state_id: 9,
            slot: 5,
            button: 0,
            mode: 0,
            changed_slots: BTreeMap::from([(5, None)]),
            carried: Some(ServerboundHashedStack {
                item: MinecraftIdentifier::new("minecraft:stone").unwrap(),
                count: 4,
                added_component_hashes: BTreeMap::new(),
                removed_components: BTreeSet::new(),
            }),
        };
        assert_eq!(
            encode_container_click(&packet, &profile).unwrap(),
            vec![
                0x02, 0x09, 0x00, 0x05, 0x00, 0x00, 0x01, 0x00, 0x05, 0x00, 0x01, 0x01, 0x04, 0x00,
                0x00
            ]
        );
    }

    #[test]
    fn quick_craft_start_add_and_end_use_exact_mode_five_button_headers() {
        let profile = profile();
        let vector = [
            (
                -999_i16,
                0_i8,
                vec![0x00, 0x00, 0xfc, 0x19, 0x00, 0x05, 0x00, 0x00],
            ),
            (
                9_i16,
                1_i8,
                vec![0x00, 0x00, 0x00, 0x09, 0x01, 0x05, 0x00, 0x00],
            ),
            (
                -999_i16,
                2_i8,
                vec![0x00, 0x00, 0xfc, 0x19, 0x02, 0x05, 0x00, 0x00],
            ),
        ];
        for (slot, button, expected) in vector {
            assert_eq!(
                encode_container_click(
                    &ServerboundContainerClick {
                        container_id: 0,
                        state_id: 0,
                        slot,
                        button,
                        mode: 5,
                        changed_slots: BTreeMap::new(),
                        carried: None,
                    },
                    &profile,
                )
                .unwrap(),
                expected
            );
        }
    }

    #[test]
    fn held_slot_is_strictly_bounded() {
        assert_eq!(decode_set_held_slot(&[0x08]).unwrap().slot, 8);
        assert!(matches!(
            decode_set_held_slot(&[0x09]),
            Err(InventoryCodecError::ValueOutOfRange { .. })
        ));
    }

    #[test]
    fn creative_slot_vector_uses_full_item_stack_not_click_hashes() {
        let profile = profile();
        let stack = ItemStack {
            item: MinecraftIdentifier::new("minecraft:stone").unwrap(),
            count: 64,
            components: ComponentPatch::default(),
        };
        assert_eq!(
            encode_play_set_creative_slot(36, Some(&stack), &profile).unwrap(),
            vec![0x07, 0x38, 0x00, 0x24, 0x40, 0x01, 0x00, 0x00]
        );
        assert_eq!(
            encode_play_set_creative_slot(2, None, &profile).unwrap(),
            vec![0x04, 0x38, 0x00, 0x02, 0x00]
        );
        assert!(encode_play_set_creative_slot(46, None, &profile).is_err());
    }

    #[test]
    fn crc32c_and_primitive_component_fingerprints_match_hash_ops_shape() {
        assert_eq!(crc32c(b"123456789"), 0xe306_9283);
        let profile = profile();
        let damage = MinecraftIdentifier::new("minecraft:damage").unwrap();
        assert_eq!(
            profile.component_hash(&damage, &[0x11]).unwrap(),
            Some(crc32c(&[8, 17, 0, 0, 0]) as i32)
        );
        assert!(profile.component_hash(&damage, &[]).is_err());
        let custom = MinecraftIdentifier::new("minecraft:custom_data").unwrap();
        assert_eq!(
            profile.component_hash(&custom, &[10, 0, 0, 0]).unwrap(),
            None
        );
    }

    #[test]
    fn inventory_dispatch_cannot_capture_a_valid_chunk_packet() {
        let mut section = CodecWriter::new();
        section.write_i16(0);
        section.write_i16(0);
        section.write_u8(0);
        section.write_var_int(0);
        section.write_u8(0);
        section.write_var_int(0);

        let mut packet = CodecWriter::new();
        packet.write_var_int(super::super::PLAY_LEVEL_CHUNK_WITH_LIGHT_ID);
        packet.write_i32(-2);
        packet.write_i32(3);
        packet.write_var_int(0);
        packet
            .write_byte_array(&section.into_inner(), 2 * 1024 * 1024)
            .unwrap();
        packet.write_var_int(0);
        for _ in 0..4 {
            packet.write_var_int(0);
        }
        packet.write_var_int(0);
        packet.write_var_int(0);
        let packet = packet.into_inner();

        assert_eq!(
            decode_inventory_clientbound(&packet, &profile()).unwrap(),
            None
        );
        let decoded = super::super::decode_play_clientbound(&packet).unwrap();
        let super::super::PlayClientbound::LevelChunkWithLight(chunk) = decoded else {
            panic!("expected chunk packet")
        };
        assert_eq!((chunk.x, chunk.z), (-2, 3));
    }
}
