//! Protocol-independent presentation and state for Cubic Chat Mode.

use std::collections::{BTreeSet, VecDeque};

pub use cubic_core::SessionPresentationMode;
use cubic_core::{ChatConnectionState, ChatEvent, ChatMessageKind};
use cubic_world::{
    ComponentPatch, ComponentValue, ContainerClick, ContainerClickKind, ContainerId,
    ContainerState, GameMode, InventoryState, ItemStack, MenuIdentity, SlotIndex,
};

pub const MAX_HISTORY_MESSAGES: usize = 500;
pub const MAX_HISTORY_TEXT_BYTES: usize = 256 * 1024;
pub const MAX_INPUT_UTF16_UNITS: usize = 256;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InventoryAction {
    SelectHotbar(u8),
    Click(ContainerClick),
    Close(ContainerId),
    CreativeSlot {
        slot: SlotIndex,
        stack: Option<ItemStack>,
    },
    /// Presentation-owned Creative cursor state mirrored into the network
    /// session so later authoritative slot snapshots cannot erase it.
    CreativeCarried(Option<ItemStack>),
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct MinecraftGuiMetrics {
    points_per_gui: f32,
    gui_width: u16,
    gui_height: u16,
    gui_scale: u32,
}

impl MinecraftGuiMetrics {
    fn from_context(context: &egui::Context) -> Self {
        let screen = context.viewport_rect();
        let pixels_per_point = context.pixels_per_point();
        let width = (screen.width() * pixels_per_point).round().max(1.0) as u32;
        let height = (screen.height() * pixels_per_point).round().max(1.0) as u32;
        Self::new(width, height, pixels_per_point)
    }

    fn new(width: u32, height: u32, pixels_per_point: f32) -> Self {
        let scale = automatic_gui_scale(width, height);
        Self {
            points_per_gui: scale as f32 / pixels_per_point.max(0.25),
            gui_width: u16::try_from(width.div_ceil(scale)).unwrap_or(u16::MAX),
            gui_height: u16::try_from(height.div_ceil(scale)).unwrap_or(u16::MAX),
            gui_scale: scale,
        }
    }

    fn size(self, width: u16, height: u16) -> egui::Vec2 {
        egui::vec2(
            f32::from(width) * self.points_per_gui,
            f32::from(height) * self.points_per_gui,
        )
    }

    fn offset(self, x: u16, y: u16) -> egui::Vec2 {
        egui::vec2(
            f32::from(x) * self.points_per_gui,
            f32::from(y) * self.points_per_gui,
        )
    }

    fn logical_origin(self, width: u16, _height: u16, x: u16, y: u16) -> egui::Pos2 {
        let centered_x = self.gui_width.saturating_sub(width) / 2;
        egui::pos2(
            f32::from(centered_x.saturating_add(x)) * self.points_per_gui,
            f32::from(y) * self.points_per_gui,
        )
    }
}

const fn automatic_gui_scale(width: u32, height: u32) -> u32 {
    let mut scale = 1;
    while scale < 4 && width / (scale + 1) >= 320 && height / (scale + 1) >= 240 {
        scale += 1;
    }
    scale
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MenuKind {
    Player,
    Chest,
    Furnace,
    Crafting,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SlotPlacement {
    slot: SlotIndex,
    x: u16,
    y: u16,
}

struct SlotPresentation<'a> {
    slot: SlotIndex,
    rect: egui::Rect,
    stack: Option<&'a ItemStack>,
    displayed_stack: Option<&'a ItemStack>,
    quick_craft_preview: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct MenuLayout {
    kind: MenuKind,
    texture: &'static str,
    width: u16,
    height: u16,
    title: [u16; 2],
    inventory_label: Option<[u16; 2]>,
    slots: Vec<SlotPlacement>,
}

impl MenuLayout {
    fn for_container(container: &ContainerState) -> Self {
        match &container.menu {
            MenuIdentity::PlayerInventory => player_inventory_layout(),
            MenuIdentity::Menu(identifier) => match identifier.as_str() {
                "minecraft:furnace" => furnace_layout(container.slots.len()),
                "minecraft:crafting" => crafting_layout(container.slots.len()),
                // ShulkerBoxScreen is a GenericContainerScreen backed by the
                // ordinary three-row generic container layout.
                "minecraft:shulker_box" => chest_layout(3, container.slots.len()),
                value if generic_chest_rows(value).is_some() => chest_layout(
                    generic_chest_rows(value).unwrap_or(1),
                    container.slots.len(),
                ),
                _ => unknown_layout(container.slots.len()),
            },
        }
    }
}

fn player_inventory_layout() -> MenuLayout {
    let mut slots = Vec::with_capacity(46);
    slots.push(SlotPlacement {
        slot: SlotIndex(0),
        x: 154,
        y: 28,
    });
    for row in 0_u16..2 {
        for column in 0_u16..2 {
            slots.push(SlotPlacement {
                slot: SlotIndex(i16::try_from(1 + row * 2 + column).unwrap_or(i16::MAX)),
                x: 98 + column * 18,
                y: 18 + row * 18,
            });
        }
    }
    for index in 0_u16..4 {
        slots.push(SlotPlacement {
            slot: SlotIndex(i16::try_from(5 + index).unwrap_or(i16::MAX)),
            x: 8,
            y: 8 + index * 18,
        });
    }
    append_player_inventory_slots(&mut slots, 9, 84, 142);
    slots.push(SlotPlacement {
        slot: SlotIndex(45),
        x: 77,
        y: 62,
    });
    MenuLayout {
        kind: MenuKind::Player,
        texture: "gui/container/inventory",
        width: 176,
        height: 166,
        title: [97, 8],
        inventory_label: None,
        slots,
    }
}

fn chest_layout(rows: u16, slot_count: usize) -> MenuLayout {
    let rows = rows.clamp(1, 6);
    let mut slots = Vec::with_capacity(usize::from(rows) * 9 + 36);
    for row in 0..rows {
        for column in 0_u16..9 {
            push_slot(
                &mut slots,
                row * 9 + column,
                8 + column * 18,
                18 + row * 18,
                slot_count,
            );
        }
    }
    append_open_player_slots(
        &mut slots,
        usize::from(rows) * 9,
        31 + rows * 18,
        89 + rows * 18,
        slot_count,
    );
    MenuLayout {
        kind: MenuKind::Chest,
        texture: "gui/container/generic_54",
        width: 176,
        height: 114 + rows * 18,
        title: [8, 6],
        inventory_label: Some([8, 20 + rows * 18]),
        slots,
    }
}

fn furnace_layout(slot_count: usize) -> MenuLayout {
    let mut slots = vec![
        SlotPlacement {
            slot: SlotIndex(0),
            x: 56,
            y: 17,
        },
        SlotPlacement {
            slot: SlotIndex(1),
            x: 56,
            y: 53,
        },
        SlotPlacement {
            slot: SlotIndex(2),
            x: 116,
            y: 35,
        },
    ];
    slots.retain(|slot| usize::try_from(slot.slot.0).is_ok_and(|index| index < slot_count));
    append_open_player_slots(&mut slots, 3, 84, 142, slot_count);
    MenuLayout {
        kind: MenuKind::Furnace,
        texture: "gui/container/furnace",
        width: 176,
        height: 166,
        title: [8, 6],
        inventory_label: Some([8, 72]),
        slots,
    }
}

fn crafting_layout(slot_count: usize) -> MenuLayout {
    let mut slots = Vec::with_capacity(46);
    push_slot(&mut slots, 0, 124, 35, slot_count);
    for row in 0_u16..3 {
        for column in 0_u16..3 {
            push_slot(
                &mut slots,
                1 + row * 3 + column,
                30 + column * 18,
                17 + row * 18,
                slot_count,
            );
        }
    }
    append_open_player_slots(&mut slots, 10, 84, 142, slot_count);
    MenuLayout {
        kind: MenuKind::Crafting,
        texture: "gui/container/crafting_table",
        width: 176,
        height: 166,
        title: [8, 6],
        inventory_label: Some([8, 72]),
        slots,
    }
}

fn unknown_layout(slot_count: usize) -> MenuLayout {
    let shown = slot_count.min(54);
    let rows = shown.div_ceil(9).max(1);
    let mut slots = Vec::with_capacity(shown);
    for index in 0..shown {
        let Ok(index_u16) = u16::try_from(index) else {
            break;
        };
        push_slot(
            &mut slots,
            index_u16,
            8 + index_u16 % 9 * 18,
            18 + index_u16 / 9 * 18,
            slot_count,
        );
    }
    MenuLayout {
        kind: MenuKind::Unknown,
        texture: "cubic/unsupported-container",
        width: 176,
        height: u16::try_from(34 + rows * 18).unwrap_or(166),
        title: [8, 6],
        inventory_label: None,
        slots,
    }
}

fn append_player_inventory_slots(
    slots: &mut Vec<SlotPlacement>,
    start: u16,
    main_y: u16,
    hotbar_y: u16,
) {
    append_slot_grid(slots, start, 3, 8, main_y);
    append_slot_grid(slots, start + 27, 1, 8, hotbar_y);
}

fn append_open_player_slots(
    slots: &mut Vec<SlotPlacement>,
    start: usize,
    main_y: u16,
    hotbar_y: u16,
    slot_count: usize,
) {
    let Ok(start) = u16::try_from(start) else {
        return;
    };
    for row in 0_u16..3 {
        for column in 0_u16..9 {
            push_slot(
                slots,
                start + row * 9 + column,
                8 + column * 18,
                main_y + row * 18,
                slot_count,
            );
        }
    }
    for column in 0_u16..9 {
        push_slot(
            slots,
            start + 27 + column,
            8 + column * 18,
            hotbar_y,
            slot_count,
        );
    }
}

fn append_slot_grid(slots: &mut Vec<SlotPlacement>, start: u16, rows: u16, x: u16, y: u16) {
    for row in 0..rows {
        for column in 0_u16..9 {
            slots.push(SlotPlacement {
                slot: SlotIndex(i16::try_from(start + row * 9 + column).unwrap_or(i16::MAX)),
                x: x + column * 18,
                y: y + row * 18,
            });
        }
    }
}

fn push_slot(slots: &mut Vec<SlotPlacement>, index: u16, x: u16, y: u16, slot_count: usize) {
    if usize::from(index) < slot_count {
        slots.push(SlotPlacement {
            slot: SlotIndex(i16::try_from(index).unwrap_or(i16::MAX)),
            x,
            y,
        });
    }
}

fn generic_chest_rows(identifier: &str) -> Option<u16> {
    identifier
        .strip_prefix("minecraft:generic_9x")
        .and_then(|rows| rows.parse::<u16>().ok())
        .filter(|rows| (1..=6).contains(rows))
}

fn apply_creative_quick_craft(
    state: &InventoryState,
    gesture: &CreativeQuickCraftGesture,
    carried: &mut Option<ItemStack>,
    changes: &mut Vec<(SlotIndex, Option<ItemStack>)>,
) {
    let Some(source) = carried.clone() else {
        return;
    };
    let eligible = gesture
        .visited
        .iter()
        .filter_map(|slot| {
            let index = usize::try_from(slot.0).ok()?;
            let existing = state.player().slots.get(index)?.clone();
            let accepted = existing.as_ref().is_none_or(|stack| {
                stack.stackable_with(&source) && stack.count < stack.max_stack_size()
            });
            accepted.then_some((*slot, existing))
        })
        .collect::<Vec<_>>();
    if eligible.is_empty() {
        return;
    }

    if gesture.button == egui::PointerButton::Middle {
        for (slot, _) in eligible {
            let mut placed = source.clone();
            placed.count = placed.max_stack_size();
            changes.push((slot, Some(placed)));
        }
        return;
    }

    let per_slot = if gesture.button == egui::PointerButton::Secondary {
        1
    } else {
        source.count / u32::try_from(eligible.len()).unwrap_or(u32::MAX)
    };
    if per_slot == 0 {
        return;
    }
    let mut remaining = source.count;
    for (slot, existing) in eligible {
        if remaining == 0 {
            break;
        }
        let current = existing.as_ref().map_or(0, |stack| stack.count);
        let maximum = existing
            .as_ref()
            .map_or_else(|| source.max_stack_size(), ItemStack::max_stack_size);
        let added = per_slot.min(maximum.saturating_sub(current)).min(remaining);
        if added == 0 {
            continue;
        }
        let mut placed = existing.unwrap_or_else(|| source.clone());
        placed.count = current + added;
        changes.push((slot, Some(placed)));
        remaining -= added;
    }
    if remaining == 0 {
        *carried = None;
    } else {
        let mut retained = source;
        retained.count = remaining;
        *carried = Some(retained);
    }
}

fn quick_craft_slot_eligible(
    container: &ContainerState,
    slot: SlotIndex,
    existing: Option<&ItemStack>,
    carried: &ItemStack,
) -> bool {
    let Ok(index) = usize::try_from(slot.0) else {
        return false;
    };
    let accepts_items = match &container.menu {
        MenuIdentity::PlayerInventory => index != 0,
        MenuIdentity::Menu(identifier) => {
            !matches!(
                identifier.as_str(),
                "minecraft:furnace" | "minecraft:crafting"
            ) || index
                != if identifier.as_str() == "minecraft:furnace" {
                    2
                } else {
                    0
                }
        }
    };
    accepts_items
        && existing.is_none_or(|stack| {
            stack.stackable_with(carried) && stack.count < stack.max_stack_size()
        })
}

fn furnace_lit_height(properties: &std::collections::BTreeMap<i16, i16>) -> u16 {
    scaled_property(properties, 0, 1, 13)
        .saturating_add(1)
        .min(14)
}

fn furnace_progress_width(properties: &std::collections::BTreeMap<i16, i16>) -> u16 {
    scaled_property(properties, 2, 3, 24)
}

fn scaled_property(
    properties: &std::collections::BTreeMap<i16, i16>,
    value_key: i16,
    total_key: i16,
    size: u16,
) -> u16 {
    let value = u32::try_from(*properties.get(&value_key).unwrap_or(&0)).unwrap_or(0);
    let total = u32::try_from(*properties.get(&total_key).unwrap_or(&0)).unwrap_or(0);
    if value == 0 || total == 0 {
        return 0;
    }
    let numerator = value.saturating_mul(u32::from(size));
    let scaled = numerator.saturating_add(total - 1) / total;
    u16::try_from(scaled.min(u32::from(size))).unwrap_or(size)
}

#[derive(Clone)]
pub struct InventoryOverlay {
    state: InventoryState,
    visible: bool,
    quick_craft: Option<QuickCraftUiGesture>,
    item_atlases: std::collections::BTreeMap<u32, InstalledItemAtlas>,
    screen_textures: std::collections::BTreeMap<String, InstalledTexture>,
    translations: std::collections::BTreeMap<String, String>,
    game_mode: GameMode,
    creative: CreativeState,
    creative_tabs: Vec<CreativeUiTab>,
    creative_data: Option<cubic_version::CreativeData>,
    operator_items_enabled: bool,
}

#[derive(Clone)]
struct InstalledTexture {
    handle: egui::TextureHandle,
    size: [u32; 2],
    glyph_widths: Option<Box<[u8; 256]>>,
}

#[derive(Clone)]
struct InstalledItemAtlas {
    handle: egui::TextureHandle,
    regions: std::collections::BTreeMap<String, egui::Rect>,
}

#[derive(Clone, Debug)]
struct CreativeUiTab {
    metadata: cubic_version::CreativeTabData,
    icon: ItemStack,
    items: Vec<ItemStack>,
    search_text: Vec<Vec<String>>,
}

fn build_creative_tabs(
    data: &cubic_version::CreativeData,
    privileged: bool,
    translations: &std::collections::BTreeMap<String, String>,
) -> Vec<CreativeUiTab> {
    data.tabs(privileged)
        .iter()
        .filter(|tab| tab.should_display)
        .filter_map(|tab| {
            let icon = creative_stack(&tab.icon, &data.banner_patterns)?;
            let items = tab
                .items
                .iter()
                .filter_map(|stack| creative_stack(stack, &data.banner_patterns))
                .collect::<Vec<_>>();
            let search_text = items
                .iter()
                .map(|stack| creative_name_search_text(stack, translations))
                .collect();
            Some(CreativeUiTab {
                metadata: tab.clone(),
                icon,
                items,
                search_text,
            })
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CreativePickerInput {
    Primary,
    Secondary,
    QuickMove,
    Clone,
}

fn apply_creative_picker_input(
    carried: &mut Option<ItemStack>,
    template: &ItemStack,
    input: CreativePickerInput,
) {
    if input == CreativePickerInput::Clone {
        if carried.is_none() {
            let mut picked = template.clone();
            picked.count = picked.max_stack_size();
            *carried = Some(picked);
        }
        return;
    }
    let same = carried
        .as_ref()
        .is_some_and(|stack| stack.stackable_with(template));
    if same {
        let Some(stack) = carried.as_mut() else {
            return;
        };
        match input {
            CreativePickerInput::Primary => {
                if stack.count < stack.max_stack_size() {
                    stack.count += 1;
                }
            }
            CreativePickerInput::Secondary => {
                stack.count -= 1;
                if stack.count == 0 {
                    *carried = None;
                }
            }
            CreativePickerInput::QuickMove => stack.count = stack.max_stack_size(),
            CreativePickerInput::Clone => {}
        }
        return;
    }
    match (carried.is_some(), input) {
        (true, CreativePickerInput::Primary | CreativePickerInput::QuickMove) => *carried = None,
        (true, CreativePickerInput::Secondary) => {
            let Some(stack) = carried.as_mut() else {
                return;
            };
            stack.count -= 1;
            if stack.count == 0 {
                *carried = None;
            }
        }
        (false, CreativePickerInput::Primary | CreativePickerInput::QuickMove) => {
            let mut picked = template.clone();
            if input == CreativePickerInput::QuickMove {
                picked.count = picked.max_stack_size();
            }
            *carried = Some(picked);
        }
        (false, CreativePickerInput::Secondary) => {
            let mut picked = template.clone();
            picked.count = 1;
            *carried = Some(picked);
        }
        (_, CreativePickerInput::Clone) => {}
    }
}

fn creative_picker_throw(template: &ItemStack, entire_stack: bool) -> ItemStack {
    let mut dropped = template.clone();
    dropped.count = if entire_stack {
        dropped.max_stack_size()
    } else {
        1
    };
    dropped
}

fn creative_picker_hotbar_copy(template: &ItemStack) -> ItemStack {
    let mut picked = template.clone();
    picked.count = picked.max_stack_size();
    picked
}

fn clear_creative_empty_picker_slot(carried: &mut Option<ItemStack>) {
    *carried = None;
}

fn creative_synchronization_actions(
    initial_carried: Option<&ItemStack>,
    final_carried: Option<&ItemStack>,
    changes: impl IntoIterator<Item = (SlotIndex, Option<ItemStack>)>,
) -> Vec<InventoryAction> {
    let mut actions = Vec::new();
    if initial_carried != final_carried {
        actions.push(InventoryAction::CreativeCarried(final_carried.cloned()));
    }
    actions.extend(
        changes
            .into_iter()
            .map(|(slot, stack)| InventoryAction::CreativeSlot { slot, stack }),
    );
    actions
}

fn clear_creative_category_hotbar_slot(
    state: &InventoryState,
    slot: SlotIndex,
    changes: &mut Vec<(SlotIndex, Option<ItemStack>)>,
) {
    if (36..=44).contains(&slot.0)
        && state
            .player()
            .slots
            .get(slot.0 as usize)
            .is_some_and(Option::is_some)
    {
        changes.push((slot, None));
    }
}

fn creative_scroll_row(current: usize, maximum: usize, wheel_steps: i32) -> usize {
    if wheel_steps > 0 {
        current.saturating_sub(wheel_steps as usize)
    } else if wheel_steps < 0 {
        current
            .saturating_add(wheel_steps.unsigned_abs() as usize)
            .min(maximum)
    } else {
        current.min(maximum)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct CreativeScrollAccumulator {
    x: f32,
    y: f32,
}

impl CreativeScrollAccumulator {
    fn push(&mut self, x: f32, y: f32) -> [i32; 2] {
        if x != 0.0 && self.x != 0.0 && x.signum() != self.x.signum() {
            self.x = 0.0;
        }
        if y != 0.0 && self.y != 0.0 && y.signum() != self.y.signum() {
            self.y = 0.0;
        }
        self.x += x;
        self.y += y;
        let whole = [self.x.trunc() as i32, self.y.trunc() as i32];
        self.x -= whole[0] as f32;
        self.y -= whole[1] as f32;
        whole
    }
}

fn creative_wheel_steps(
    input: &egui::InputState,
    accumulator: &mut CreativeScrollAccumulator,
) -> [i32; 2] {
    let mut whole = [0_i32; 2];
    for event in &input.events {
        if let egui::Event::MouseWheel { unit, delta, .. } = event {
            let delta = normalize_creative_wheel(*unit, *delta, input.pixels_per_point);
            let emitted = accumulator.push(delta.x, delta.y);
            whole[0] = whole[0].saturating_add(emitted[0]);
            whole[1] = whole[1].saturating_add(emitted[1]);
        }
    }
    whole
}

fn normalize_creative_wheel(
    unit: egui::MouseWheelUnit,
    delta: egui::Vec2,
    pixels_per_point: f32,
) -> egui::Vec2 {
    match unit {
        // Winit/egui line deltas are already logical wheel steps. Pixel
        // deltas were divided by pixels_per_point by egui-winit, so restore
        // physical pixels before applying Windows' 120-unit wheel quantum.
        egui::MouseWheelUnit::Line => delta,
        egui::MouseWheelUnit::Point => delta * (pixels_per_point / 120.0),
        egui::MouseWheelUnit::Page => delta,
    }
}

const fn empty_player_slot_sprite(slot: SlotIndex) -> Option<&'static str> {
    match slot.0 {
        5 => Some("gui/sprites/container/slot/helmet"),
        6 => Some("gui/sprites/container/slot/chestplate"),
        7 => Some("gui/sprites/container/slot/leggings"),
        8 => Some("gui/sprites/container/slot/boots"),
        45 => Some("gui/sprites/container/slot/shield"),
        _ => None,
    }
}

fn creative_outside_drop_allowed(
    any_click: bool,
    pointer_inside_screen: bool,
    tab_consumed_click: bool,
) -> bool {
    any_click && !pointer_inside_screen && !tab_consumed_click
}

fn apply_creative_real_slot(
    state: &InventoryState,
    carried: &mut Option<ItemStack>,
    slot: SlotIndex,
    button: i8,
    kind: ContainerClickKind,
    changes: &mut Vec<(SlotIndex, Option<ItemStack>)>,
) {
    let mut predicted = state.clone();
    predicted.set_carried(carried.clone());
    let before = predicted.player().slots.clone();
    if predicted
        .apply_click(ContainerClick {
            container: cubic_world::PLAYER_CONTAINER_ID,
            state_id: predicted.player().state_id,
            slot,
            button,
            kind,
        })
        .is_err()
    {
        return;
    }
    *carried = predicted.carried().cloned();
    changes.extend(
        before
            .into_iter()
            .zip(&predicted.player().slots)
            .enumerate()
            .filter_map(|(index, (before, after))| {
                if before == *after {
                    return None;
                }
                i16::try_from(index)
                    .ok()
                    .map(|index| (SlotIndex(index), after.clone()))
            }),
    );
}

fn creative_stack(
    stack: &cubic_version::CreativeStackData,
    banner_patterns: &[cubic_version::MinecraftIdentifier],
) -> Option<ItemStack> {
    let mut patch = ComponentPatch::default();
    for component in &stack.components {
        if component.removed {
            patch.removed.insert(component.id.clone());
            continue;
        }
        let bytes = component.decoded_value().ok()??;
        let value = match component.id.as_str() {
            "minecraft:item_model" => {
                cubic_world::ComponentValue::Text(stack.effective_item_model.to_string())
            }
            "minecraft:max_stack_size" | "minecraft:max_damage" | "minecraft:damage" => {
                decode_creative_varint(&bytes)
                    .map(cubic_world::ComponentValue::VarInt)
                    .unwrap_or(cubic_world::ComponentValue::Opaque(bytes))
            }
            "minecraft:unbreakable" | "minecraft:glider" => cubic_world::ComponentValue::Unit,
            "minecraft:custom_name" | "minecraft:item_name" => component
                .text_translation_key()
                .ok()
                .flatten()
                .map(|plain| cubic_world::ComponentValue::RichText {
                    plain,
                    wire: bytes.clone(),
                })
                .unwrap_or(cubic_world::ComponentValue::Opaque(bytes)),
            "minecraft:block_state" => component
                .block_state_properties()
                .ok()
                .flatten()
                .map(|values| cubic_world::ComponentValue::StringMap {
                    values,
                    wire: bytes.clone(),
                })
                .unwrap_or(cubic_world::ComponentValue::Opaque(bytes)),
            "minecraft:banner_patterns" => cubic_world::ComponentValue::RegistryEncoded {
                wire: bytes,
                source_registry: banner_patterns.to_vec(),
            },
            _ => cubic_world::ComponentValue::Opaque(bytes),
        };
        patch.added.insert(component.id.clone(), value);
    }
    ItemStack::new(stack.item.clone(), stack.count, patch).ok()
}

fn decode_creative_varint(bytes: &[u8]) -> Option<i32> {
    let mut value = 0_u32;
    for (index, byte) in bytes.iter().copied().enumerate().take(5) {
        value |= u32::from(byte & 0x7f) << (index * 7);
        if byte & 0x80 == 0 {
            return (index + 1 == bytes.len()).then_some(value as i32);
        }
    }
    None
}

fn creative_tab_x(tab: &cubic_version::CreativeTabData) -> u16 {
    let column = u16::from(tab.column);
    if tab.aligned_right {
        195_u16
            .saturating_sub(27_u16.saturating_mul(7_u16.saturating_sub(column)))
            .saturating_add(1)
    } else {
        27_u16.saturating_mul(column)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CreativePaintLayer {
    UnselectedTabs,
    MainPanel,
    SelectedTab,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ItemTooltipMode {
    Normal,
    Advanced,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ItemTooltip {
    name: String,
    lines: Vec<String>,
}

fn item_tooltip(
    stack: &ItemStack,
    translations: &std::collections::BTreeMap<String, String>,
    mode: ItemTooltipMode,
    creative_category: Option<&str>,
) -> ItemTooltip {
    let component = |name: &str| {
        stack
            .components
            .added
            .iter()
            .find(|(identifier, _)| identifier.as_str() == name)
            .map(|(_, value)| value)
    };
    let explicit_name = ["minecraft:custom_name", "minecraft:item_name"]
        .into_iter()
        .find_map(|name| match component(name) {
            Some(ComponentValue::RichText { plain, .. }) if !plain.is_empty() => Some(
                translations
                    .get(plain)
                    .cloned()
                    .unwrap_or_else(|| plain.clone()),
            ),
            _ => None,
        });
    let (namespace, path) = stack
        .item
        .as_str()
        .split_once(':')
        .unwrap_or(("minecraft", stack.item.as_str()));
    let translated = [
        format!("item.{namespace}.{path}"),
        format!("block.{namespace}.{path}"),
    ]
    .into_iter()
    .find_map(|key| translations.get(&key).cloned());
    let name = explicit_name
        .or(translated)
        .unwrap_or_else(|| stack.item.to_string());
    let mut lines = match component("minecraft:lore") {
        Some(ComponentValue::Lore { lines, .. }) => lines.clone(),
        _ => Vec::new(),
    };
    if let Some(category) = creative_category {
        lines.push(category.to_owned());
    }
    if mode == ItemTooltipMode::Advanced {
        lines.push(stack.item.to_string());
        lines.push(format!(
            "{} component(s)",
            stack
                .components
                .added
                .len()
                .saturating_add(stack.components.removed.len())
        ));
    }
    lines.truncate(64);
    ItemTooltip { name, lines }
}

fn slot_item_tooltip(
    stack: Option<&ItemStack>,
    translations: &std::collections::BTreeMap<String, String>,
    mode: ItemTooltipMode,
    creative_category: Option<&str>,
) -> Option<ItemTooltip> {
    stack.map(|stack| item_tooltip(stack, translations, mode, creative_category))
}

fn creative_name_search_text(
    stack: &ItemStack,
    translations: &std::collections::BTreeMap<String, String>,
) -> Vec<String> {
    let tooltip = item_tooltip(stack, translations, ItemTooltipMode::Normal, None);
    std::iter::once(tooltip.name)
        .chain(tooltip.lines)
        .map(|line| line.trim().to_lowercase())
        .filter(|line| !line.is_empty())
        .collect()
}

fn creative_name_search_matches(stack: &ItemStack, search_text: &[String], query: &str) -> bool {
    if query.starts_with('#') {
        // Item-tag membership is not part of the Phase 11 artifact yet.
        return false;
    }
    if let Some((namespace, path)) = query.split_once(':') {
        let Some((item_namespace, item_path)) = stack.item.as_str().split_once(':') else {
            return false;
        };
        return item_namespace.to_ascii_lowercase().contains(namespace)
            && item_path.to_ascii_lowercase().contains(path);
    }
    search_text.iter().any(|line| line.contains(query))
}

const fn creative_paint_order() -> [CreativePaintLayer; 3] {
    [
        CreativePaintLayer::UnselectedTabs,
        CreativePaintLayer::MainPanel,
        CreativePaintLayer::SelectedTab,
    ]
}

fn creative_background_key(identifier: &str) -> &str {
    identifier
        .strip_prefix("minecraft:textures/")
        .and_then(|path| path.strip_suffix(".png"))
        .unwrap_or(identifier)
}

const CREATIVE_SEARCH_BOUNDS: [u16; 4] = [82, 6, 80, 9];

#[derive(Clone, Debug)]
struct CreativeState {
    tab: String,
    search: String,
    search_focus_pending: bool,
    scroll_row: usize,
    quick_craft: Option<CreativeQuickCraftGesture>,
    audited_page: Option<(String, usize)>,
    scroll_accumulator: CreativeScrollAccumulator,
}

impl Default for CreativeState {
    fn default() -> Self {
        Self {
            tab: "minecraft:building_blocks".to_owned(),
            search: String::new(),
            search_focus_pending: false,
            scroll_row: 0,
            quick_craft: None,
            audited_page: None,
            scroll_accumulator: CreativeScrollAccumulator::default(),
        }
    }
}

impl CreativeState {
    fn select_tab(&mut self, tab: String) {
        if self.tab == tab {
            return;
        }
        self.tab = tab;
        self.search.clear();
        self.search_focus_pending = self.tab == "minecraft:search";
        self.scroll_row = 0;
        self.quick_craft = None;
        self.audited_page = None;
    }
}

#[derive(Clone, Debug)]
struct CreativeQuickCraftGesture {
    button: egui::PointerButton,
    visited: BTreeSet<SlotIndex>,
}

#[derive(Clone, Debug)]
struct QuickCraftUiGesture {
    container: ContainerId,
    state_id: i32,
    kind: u8,
    visited: BTreeSet<SlotIndex>,
}

impl Default for InventoryOverlay {
    fn default() -> Self {
        let creative_data = cubic_version::CreativeData::builtin_26_1_2().ok();
        Self {
            state: InventoryState::new(),
            visible: false,
            quick_craft: None,
            item_atlases: std::collections::BTreeMap::new(),
            screen_textures: std::collections::BTreeMap::new(),
            translations: std::collections::BTreeMap::new(),
            game_mode: GameMode::Survival,
            creative: CreativeState::default(),
            creative_tabs: creative_data
                .as_ref()
                .map(|data| build_creative_tabs(data, false, &std::collections::BTreeMap::new()))
                .unwrap_or_default(),
            creative_data,
            operator_items_enabled: true,
        }
    }
}

impl InventoryOverlay {
    pub fn replace(&mut self, state: InventoryState) {
        let prior_gesture_state = self.quick_craft.as_ref().map(|gesture| {
            (
                gesture.container,
                gesture.state_id,
                self.state.open().map(|open| open.id),
            )
        });
        let had_open_container = self.state.open().is_some();
        let has_open_container = state.open().is_some();
        if has_open_container {
            self.visible = true;
        } else if had_open_container {
            self.visible = false;
            self.quick_craft = None;
        }
        self.state = state;
        self.refresh_creative_tabs();
        if let Some((container, state_id, prior_open)) = prior_gesture_state {
            let active = self.state.open().unwrap_or_else(|| self.state.player());
            if active.id != container
                || active.state_id != state_id
                || self.state.open().map(|open| open.id) != prior_open
            {
                self.quick_craft = None;
            }
        }
    }

    pub fn toggle_player_inventory(&mut self) {
        if self.state.open().is_none() {
            self.visible = !self.visible;
        }
    }

    pub fn install_icon_atlas(
        &mut self,
        context: &egui::Context,
        gui_scale: u32,
        icons: impl IntoIterator<Item = (String, u32, u32, Vec<u8>)>,
    ) {
        let Some(icon_size) = usize::try_from(gui_scale)
            .ok()
            .and_then(|scale| 16_usize.checked_mul(scale))
        else {
            return;
        };
        if !(16..=64).contains(&icon_size) {
            return;
        }
        const GUTTER: usize = 1;
        const MAX_ICONS: usize = 4_096;
        let stride = icon_size + GUTTER * 2;
        let icons = icons
            .into_iter()
            .filter_map(|(identifier, width, height, rgba)| {
                (usize::try_from(width).ok() == Some(icon_size)
                    && usize::try_from(height).ok() == Some(icon_size)
                    && rgba.len() == icon_size * icon_size * 4)
                    .then_some((identifier, rgba))
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        if icons.is_empty() || icons.len() > MAX_ICONS {
            return;
        }
        let mut columns = 1_usize;
        while columns.saturating_mul(columns) < icons.len() {
            columns += 1;
        }
        let rows = icons.len().div_ceil(columns);
        let width = columns * stride;
        let height = rows * stride;
        if width > 4096 || height > 4096 {
            return;
        }
        let mut atlas = vec![0_u8; width * height * 4];
        let mut regions = std::collections::BTreeMap::new();
        for (index, (identifier, icon)) in icons.into_iter().enumerate() {
            let x = index % columns * stride + GUTTER;
            let y = index / columns * stride + GUTTER;
            for row in 0..icon_size {
                let source = row * icon_size * 4;
                let destination = ((y + row) * width + x) * 4;
                atlas[destination..destination + icon_size * 4]
                    .copy_from_slice(&icon[source..source + icon_size * 4]);
            }
            regions.insert(
                identifier,
                egui::Rect::from_min_max(
                    egui::pos2(x as f32 / width as f32, y as f32 / height as f32),
                    egui::pos2(
                        (x + icon_size) as f32 / width as f32,
                        (y + icon_size) as f32 / height as f32,
                    ),
                ),
            );
        }
        // ColorImage converts unassociated resource pixels to egui's
        // premultiplied Color32 representation before the atlas reaches the
        // standard premultiplied GUI composition pipeline.
        let image = egui::ColorImage::from_rgba_unmultiplied([width, height], &atlas);
        let handle = context.load_texture(
            format!("cubic-item-atlas-{gui_scale}"),
            image,
            egui::TextureOptions::NEAREST,
        );
        self.item_atlases
            .insert(gui_scale, InstalledItemAtlas { handle, regions });
    }

    pub fn install_screen_texture(
        &mut self,
        context: &egui::Context,
        identifier: String,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    ) {
        let (Ok(image_width), Ok(image_height)) = (usize::try_from(width), usize::try_from(height))
        else {
            return;
        };
        if width == 0
            || height == 0
            || image_width
                .checked_mul(image_height)
                .and_then(|pixels| pixels.checked_mul(4))
                != Some(rgba.len())
        {
            return;
        }
        let glyph_widths = (identifier == "font/ascii" && width == 128 && height == 128)
            .then(|| Box::new(bitmap_glyph_widths(&rgba)));
        let image = egui::ColorImage::from_rgba_unmultiplied([image_width, image_height], &rgba);
        let handle = context.load_texture(
            format!("cubic-gui:{identifier}"),
            image,
            egui::TextureOptions::NEAREST,
        );
        self.screen_textures.insert(
            identifier,
            InstalledTexture {
                handle,
                size: [width, height],
                glyph_widths,
            },
        );
    }

    pub fn install_translations(
        &mut self,
        translations: std::collections::BTreeMap<String, String>,
    ) {
        self.translations = translations;
        self.refresh_creative_tabs();
    }

    pub fn set_game_mode(&mut self, game_mode: GameMode) {
        self.game_mode = game_mode;
        self.refresh_creative_tabs();
        if game_mode != GameMode::Creative {
            self.creative = CreativeState::default();
        }
    }

    fn refresh_creative_tabs(&mut self) {
        let privileged = self.game_mode == GameMode::Creative
            && self.operator_items_enabled
            && self.state.can_use_game_master_blocks();
        if let Some(data) = &self.creative_data {
            self.creative_tabs = build_creative_tabs(data, privileged, &self.translations);
        }
    }

    #[must_use]
    fn creative_active(&self) -> bool {
        self.visible && self.state.open().is_none() && self.game_mode == GameMode::Creative
    }

    pub fn close(&mut self) -> Option<InventoryAction> {
        self.visible = false;
        self.quick_craft = None;
        self.state
            .open()
            .map(|container| InventoryAction::Close(container.id))
    }

    #[must_use]
    pub const fn visible(&self) -> bool {
        self.visible
    }

    #[must_use]
    pub const fn selected_hotbar_slot(&self) -> u8 {
        self.state.selected_hotbar_slot()
    }

    pub fn select_hotbar_local(&mut self, slot: u8) {
        let _ = self.state.set_selected_hotbar_slot(slot);
    }

    pub fn show(&mut self, context: &egui::Context) -> Vec<InventoryAction> {
        let mut actions = Vec::new();
        if self.creative_active() {
            self.show_creative(context, &mut actions);
        } else if self.visible {
            self.show_container(context, &mut actions);
        } else {
            self.show_hotbar(context, &mut actions);
        }
        self.apply_local_clicks(&actions);
        actions
    }

    fn apply_local_clicks(&mut self, actions: &[InventoryAction]) {
        for action in actions {
            if let InventoryAction::Click(click) = action {
                // This is the same deterministic menu operation used by the
                // network predictor. A later authoritative snapshot replaces
                // this presentation state if the server disagrees.
                let _ = self.state.apply_click(*click);
            }
        }
    }

    fn show_hotbar(&self, context: &egui::Context, actions: &mut Vec<InventoryAction>) {
        let metrics = MinecraftGuiMetrics::from_context(context);
        let origin = metrics.logical_origin(182, 22, 0, metrics.gui_height.saturating_sub(22));
        egui::Area::new(egui::Id::new("cubic-hotbar"))
            .fixed_pos(origin)
            .show(context, |ui| {
                let size = metrics.size(182, 22);
                let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                self.paint_texture(ui.painter(), "gui/sprites/hud/hotbar", rect);
                if let Some(selection) =
                    self.screen_textures.get("gui/sprites/hud/hotbar_selection")
                {
                    let selected = f32::from(self.state.selected_hotbar_slot());
                    let min = rect.min
                        + egui::vec2(
                            (-1.0 + selected * 20.0) * metrics.points_per_gui,
                            -metrics.points_per_gui,
                        );
                    let selection_rect = egui::Rect::from_min_size(min, metrics.size(24, 23));
                    ui.painter().image(
                        selection.handle.id(),
                        selection_rect,
                        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                }
                for hotbar in 0_u8..9 {
                    let slot_min = rect.min
                        + egui::vec2(
                            (3.0 + 20.0 * f32::from(hotbar)) * metrics.points_per_gui,
                            3.0 * metrics.points_per_gui,
                        );
                    let slot_rect = egui::Rect::from_min_size(slot_min, metrics.size(16, 16));
                    let response = ui.interact(
                        slot_rect,
                        egui::Id::new(("cubic-hotbar-slot", hotbar)),
                        egui::Sense::click(),
                    );
                    if response.clicked() {
                        actions.push(InventoryAction::SelectHotbar(hotbar));
                    }
                    self.paint_stack(
                        ui.painter(),
                        slot_rect,
                        self.state.player().slots[36 + usize::from(hotbar)].as_ref(),
                        metrics,
                    );
                }
            });
    }

    fn show_creative(&mut self, context: &egui::Context, actions: &mut Vec<InventoryAction>) {
        const WIDTH: u16 = 195;
        const HEIGHT: u16 = 136;
        const COLUMNS: usize = 9;
        const VISIBLE_ROWS: usize = 5;
        let metrics = MinecraftGuiMetrics::from_context(context);
        let screen = context.viewport_rect();
        let size = metrics.size(WIDTH, HEIGHT);
        let origin = egui::pos2(
            screen.center().x - size.x * 0.5,
            screen.center().y - size.y * 0.5,
        );
        let tab_id = self.creative.tab.clone();
        let Some(tab) = self
            .creative_tabs
            .iter()
            .find(|tab| tab.metadata.id.as_str() == tab_id)
            .cloned()
        else {
            return;
        };
        let mut selected_tab = tab_id.clone();
        let mut search = self.creative.search.clone();
        let items = self.creative_items(&tab_id, &search);
        let max_scroll = items.len().div_ceil(COLUMNS).saturating_sub(VISIBLE_ROWS);
        let mut scroll_row = self.creative.scroll_row.min(max_scroll);
        let mut carried = self.state.carried().cloned();
        let initial_carried = carried.clone();
        let mut player_changes = Vec::<(SlotIndex, Option<ItemStack>)>::new();
        let mut tab_consumed_click = false;
        if tracing::enabled!(tracing::Level::TRACE)
            && self.creative.audited_page.as_ref() != Some(&(tab_id.clone(), scroll_row))
        {
            for (visible, stack) in items
                .iter()
                .skip(scroll_row.saturating_mul(COLUMNS))
                .take(COLUMNS * VISIBLE_ROWS)
                .enumerate()
            {
                let model = stack.effective_item_model_owned();
                let atlas_key = model
                    .as_ref()
                    .map(cubic_version::MinecraftIdentifier::as_str);
                tracing::trace!(
                    slot = visible,
                    canonical_item = stack.item.as_str(),
                    component_count = stack.components.added.len() + stack.components.removed.len(),
                    resolved_item_model = ?model.as_ref().map(cubic_version::MinecraftIdentifier::as_str),
                    gui_atlas_key = ?atlas_key,
                    atlas_present = atlas_key.is_some_and(|key| self.item_atlases.get(&metrics.gui_scale).is_some_and(|atlas| atlas.regions.contains_key(key))),
                    "Creative visible-slot identity audit"
                );
            }
            self.creative.audited_page = Some((tab_id.clone(), scroll_row));
        }

        egui::Area::new(egui::Id::new("cubic-creative-inventory"))
            .fixed_pos(origin)
            .order(egui::Order::Foreground)
            .show(context, |ui| {
                let [unselected_tabs, main_panel, selected_tab_layer] = creative_paint_order();
                let (rect, _response) = ui.allocate_exact_size(size, egui::Sense::hover());
                let background = creative_background_key(tab.metadata.background.as_str());
                let (consumed, _) = self.paint_creative_tabs(
                    ui,
                    rect,
                    metrics,
                    &tab_id,
                    &mut selected_tab,
                    unselected_tabs,
                );
                tab_consumed_click |= consumed;
                debug_assert_eq!(main_panel, CreativePaintLayer::MainPanel);
                self.paint_cropped_texture(ui.painter(), background, rect, [WIDTH, HEIGHT]);

                self.paint_creative_title(ui.painter(), rect, &tab, metrics);

                if tab.metadata.tab_type == cubic_version::CreativeTabType::Search {
                    let [search_x, search_y, search_width, search_height] = CREATIVE_SEARCH_BOUNDS;
                    let search_rect = egui::Rect::from_min_size(
                        rect.min + metrics.offset(search_x, search_y),
                        metrics.size(search_width, search_height),
                    );
                    let response = ui.put(
                        search_rect,
                        egui::TextEdit::singleline(&mut search)
                            .id(egui::Id::new("cubic-creative-search-editor"))
                            .frame(egui::Frame::NONE)
                            .margin(egui::Margin::ZERO)
                            .text_color(egui::Color32::WHITE)
                            .desired_width(search_rect.width())
                            .font(egui::FontId::monospace(7.0 * metrics.points_per_gui)),
                    );
                    if self.creative.search_focus_pending || !response.has_focus() {
                        response.request_focus();
                        self.creative.search_focus_pending = false;
                    }
                    if response.changed() {
                        scroll_row = 0;
                    }
                }

                if tab.metadata.tab_type == cubic_version::CreativeTabType::Inventory {
                    self.show_creative_inventory_tab(
                        ui,
                        rect,
                        metrics,
                        &mut carried,
                        &mut player_changes,
                    );
                } else {
                    let pointer_over_screen = ui
                        .input(|input| input.pointer.hover_pos())
                        .is_some_and(|pointer| rect.contains(pointer));
                    if pointer_over_screen && tab.metadata.can_scroll {
                        let [whole_x, whole_y] = ui.input(|input| {
                            creative_wheel_steps(input, &mut self.creative.scroll_accumulator)
                        });
                        let steps = if whole_y != 0 { whole_y } else { -whole_x };
                        if steps != 0 {
                            scroll_row = creative_scroll_row(scroll_row, max_scroll, steps);
                        }
                    }
                    let track = egui::Rect::from_min_size(
                        rect.min + metrics.offset(175, 18),
                        metrics.size(12, 90),
                    );
                    let scroll_response = ui.interact(
                        track,
                        egui::Id::new("cubic-creative-scrollbar"),
                        egui::Sense::click_and_drag(),
                    );
                    if max_scroll > 0
                        && (scroll_response.dragged() || scroll_response.clicked())
                        && let Some(pointer) = scroll_response.interact_pointer_pos()
                    {
                        let fraction = ((pointer.y - track.top()) / track.height()).clamp(0.0, 1.0);
                        scroll_row = (fraction * max_scroll as f32).round() as usize;
                    }
                    let scroller_name = if !tab.metadata.can_scroll || max_scroll == 0 {
                        "gui/sprites/container/creative_inventory/scroller_disabled"
                    } else {
                        "gui/sprites/container/creative_inventory/scroller"
                    };
                    let thumb_offset = scroll_row
                        .saturating_mul(74)
                        .checked_div(max_scroll)
                        .and_then(|value| u16::try_from(value).ok())
                        .unwrap_or(0);
                    let thumb_y = 18 + thumb_offset;
                    self.paint_texture(
                        ui.painter(),
                        scroller_name,
                        egui::Rect::from_min_size(
                            rect.min + metrics.offset(175, thumb_y),
                            metrics.size(12, 15),
                        ),
                    );

                    for visible in 0..COLUMNS * VISIBLE_ROWS {
                        let item_index = scroll_row * COLUMNS + visible;
                        let item = items.get(item_index);
                        let x = 9 + u16::try_from(visible % COLUMNS).unwrap_or(0) * 18;
                        let y = 18 + u16::try_from(visible / COLUMNS).unwrap_or(0) * 18;
                        let slot_rect = egui::Rect::from_min_size(
                            rect.min + metrics.offset(x, y),
                            metrics.size(16, 16),
                        );
                        let response = ui.interact(
                            slot_rect,
                            egui::Id::new(("cubic-creative-picker", item_index)),
                            egui::Sense::click_and_drag(),
                        );
                        self.paint_stack(ui.painter(), slot_rect, item, metrics);
                        if response.hovered()
                            && carried.is_none()
                            && let Some(item) = item
                        {
                            let category = (tab_id == "minecraft:search")
                                .then(|| self.creative_category_name(item))
                                .flatten();
                            self.paint_item_tooltip(ui.ctx(), item, metrics, category.as_deref());
                        }
                        if response.middle_clicked()
                            && let Some(item) = item
                        {
                            apply_creative_picker_input(
                                &mut carried,
                                item,
                                CreativePickerInput::Clone,
                            );
                        } else if response.clicked()
                            && let Some(item) = item
                        {
                            let input = if ui.input(|input| input.modifiers.shift) {
                                CreativePickerInput::QuickMove
                            } else {
                                CreativePickerInput::Primary
                            };
                            apply_creative_picker_input(&mut carried, item, input);
                        } else if response.clicked() {
                            clear_creative_empty_picker_slot(&mut carried);
                        } else if response.secondary_clicked()
                            && let Some(item) = item
                        {
                            apply_creative_picker_input(
                                &mut carried,
                                item,
                                CreativePickerInput::Secondary,
                            );
                        }
                        if response.hovered()
                            && let Some(hotbar) = pressed_hotbar_number(ui)
                            && let Some(item) = item
                        {
                            let slot = SlotIndex(36 + i16::from(hotbar));
                            player_changes.push((slot, Some(creative_picker_hotbar_copy(item))));
                        }
                        if response.hovered()
                            && let Some(item) = item
                            && ui.input(|input| input.key_pressed(egui::Key::Q))
                        {
                            let entire_stack = ui.input(|input| input.modifiers.command);
                            player_changes.push((
                                SlotIndex(-1),
                                Some(creative_picker_throw(item, entire_stack)),
                            ));
                        }
                    }

                    for hotbar in 0_u8..9 {
                        let index = 36 + usize::from(hotbar);
                        let slot = SlotIndex(36 + i16::from(hotbar));
                        let existing = self.state.player().slots.get(index).cloned().flatten();
                        let slot_rect = egui::Rect::from_min_size(
                            rect.min + metrics.offset(9 + u16::from(hotbar) * 18, 112),
                            metrics.size(16, 16),
                        );
                        let response = ui.interact(
                            slot_rect,
                            egui::Id::new(("cubic-creative-hotbar", hotbar)),
                            egui::Sense::click(),
                        );
                        self.paint_stack(ui.painter(), slot_rect, existing.as_ref(), metrics);
                        if response.hovered()
                            && carried.is_none()
                            && let Some(stack) = existing.as_ref()
                        {
                            self.paint_item_tooltip(ui.ctx(), stack, metrics, None);
                        }
                        if response.double_clicked() {
                            apply_creative_real_slot(
                                &self.state,
                                &mut carried,
                                slot,
                                0,
                                ContainerClickKind::PickupAll,
                                &mut player_changes,
                            );
                        } else if response.clicked() {
                            if ui.input(|input| input.modifiers.shift) {
                                clear_creative_category_hotbar_slot(
                                    &self.state,
                                    slot,
                                    &mut player_changes,
                                );
                            } else {
                                apply_creative_real_slot(
                                    &self.state,
                                    &mut carried,
                                    slot,
                                    0,
                                    ContainerClickKind::Pickup,
                                    &mut player_changes,
                                );
                            }
                        } else if response.secondary_clicked() {
                            apply_creative_real_slot(
                                &self.state,
                                &mut carried,
                                slot,
                                1,
                                ContainerClickKind::Pickup,
                                &mut player_changes,
                            );
                        } else if response.middle_clicked() {
                            apply_creative_real_slot(
                                &self.state,
                                &mut carried,
                                slot,
                                2,
                                ContainerClickKind::Clone,
                                &mut player_changes,
                            );
                        }
                    }
                }

                let (consumed, _) = self.paint_creative_tabs(
                    ui,
                    rect,
                    metrics,
                    &tab_id,
                    &mut selected_tab,
                    selected_tab_layer,
                );
                tab_consumed_click |= consumed;

                if let Some(stack) = &carried
                    && let Some(pointer) = ui.input(|input| input.pointer.hover_pos())
                {
                    self.paint_stack(
                        ui.painter(),
                        egui::Rect::from_min_size(
                            pointer - metrics.offset(8, 8),
                            metrics.size(16, 16),
                        ),
                        Some(stack),
                        metrics,
                    );
                }
            });

        let screen_rect = egui::Rect::from_min_size(origin, size);
        let any_click = context.input(|input| input.pointer.any_click());
        let pointer_inside_screen = context
            .input(|input| input.pointer.interact_pos())
            .is_some_and(|position| screen_rect.contains(position));
        if creative_outside_drop_allowed(any_click, pointer_inside_screen, tab_consumed_click)
            && let Some(dropped) = carried.take()
        {
            actions.push(InventoryAction::CreativeSlot {
                slot: SlotIndex(-1),
                stack: Some(dropped),
            });
        }

        if selected_tab != self.creative.tab {
            if self.creative.tab == "minecraft:search" {
                context.memory_mut(|memory| {
                    memory.surrender_focus(egui::Id::new("cubic-creative-search-editor"));
                });
            }
            self.creative.select_tab(selected_tab);
        } else {
            self.creative.scroll_row = scroll_row;
            self.creative.search = search;
            self.creative.search = self.creative.search.chars().take(50).collect();
        }
        self.state.set_carried(carried);
        let synchronization = creative_synchronization_actions(
            initial_carried.as_ref(),
            self.state.carried(),
            player_changes,
        );
        for action in &synchronization {
            if let InventoryAction::CreativeSlot { slot, stack } = action {
                let _ = self.state.set_creative_slot(*slot, stack.clone());
            }
        }
        actions.extend(synchronization);
    }

    fn show_creative_inventory_tab(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        metrics: MinecraftGuiMetrics,
        carried: &mut Option<ItemStack>,
        changes: &mut Vec<(SlotIndex, Option<ItemStack>)>,
    ) {
        let mut placements = Vec::new();
        for index in 0_u16..4 {
            let row = index / 2;
            let column = index % 2;
            placements.push((
                SlotIndex(i16::try_from(5 + index).unwrap_or(5)),
                54 + row * 54,
                6 + column * 27,
            ));
        }
        placements.push((SlotIndex(45), 35, 20));
        for row in 0_u16..3 {
            for column in 0_u16..9 {
                placements.push((
                    SlotIndex(i16::try_from(9 + row * 9 + column).unwrap_or(9)),
                    9 + column * 18,
                    54 + row * 18,
                ));
            }
        }
        for column in 0_u16..9 {
            placements.push((
                SlotIndex(i16::try_from(36 + column).unwrap_or(36)),
                9 + column * 18,
                112,
            ));
        }
        for (slot, x, y) in &placements {
            let Some(index) = usize::try_from(slot.0).ok() else {
                continue;
            };
            let existing = self.state.player().slots.get(index).cloned().flatten();
            let slot_rect =
                egui::Rect::from_min_size(rect.min + metrics.offset(*x, *y), metrics.size(16, 16));
            let response = ui.interact(
                slot_rect,
                egui::Id::new(("cubic-creative-player", slot.0)),
                egui::Sense::click_and_drag(),
            );
            if existing.is_none() {
                self.paint_empty_player_slot(ui.painter(), slot_rect, *slot);
            }
            self.paint_stack(ui.painter(), slot_rect, existing.as_ref(), metrics);
            if response.hovered()
                && carried.is_none()
                && let Some(stack) = existing.as_ref()
            {
                self.paint_item_tooltip(ui.ctx(), stack, metrics, None);
            }
            let began_quick_craft = [
                egui::PointerButton::Primary,
                egui::PointerButton::Secondary,
                egui::PointerButton::Middle,
            ]
            .into_iter()
            .find(|button| response.drag_started_by(*button) && carried.is_some());
            if let Some(button) = began_quick_craft {
                self.creative.quick_craft = Some(CreativeQuickCraftGesture {
                    button,
                    visited: BTreeSet::from([*slot]),
                });
            }
            if response.hovered()
                && let Some(gesture) = &mut self.creative.quick_craft
                && ui.input(|input| input.pointer.button_down(gesture.button))
            {
                gesture.visited.insert(*slot);
            }

            if response.double_clicked() && self.creative.quick_craft.is_none() {
                apply_creative_real_slot(
                    &self.state,
                    carried,
                    *slot,
                    0,
                    ContainerClickKind::PickupAll,
                    changes,
                );
            } else if response.clicked() && self.creative.quick_craft.is_none() {
                apply_creative_real_slot(
                    &self.state,
                    carried,
                    *slot,
                    0,
                    if ui.input(|input| input.modifiers.shift) {
                        ContainerClickKind::QuickMove
                    } else {
                        ContainerClickKind::Pickup
                    },
                    changes,
                );
            } else if response.secondary_clicked() && self.creative.quick_craft.is_none() {
                apply_creative_real_slot(
                    &self.state,
                    carried,
                    *slot,
                    1,
                    ContainerClickKind::Pickup,
                    changes,
                );
            } else if response.middle_clicked() && self.creative.quick_craft.is_none() {
                apply_creative_real_slot(
                    &self.state,
                    carried,
                    *slot,
                    2,
                    ContainerClickKind::Clone,
                    changes,
                );
            }
            if response.hovered()
                && let Some(hotbar) = pressed_hotbar_number(ui)
            {
                let destination = SlotIndex(36 + i16::from(hotbar));
                let destination_stack = self
                    .state
                    .player()
                    .slots
                    .get(36 + usize::from(hotbar))
                    .cloned()
                    .flatten();
                changes.push((*slot, destination_stack));
                changes.push((destination, existing.clone()));
            }
            if response.hovered()
                && ui.input(|input| input.key_pressed(egui::Key::Q))
                && let Some(mut dropped) = existing.clone()
            {
                let drop_all = ui.input(|input| input.modifiers.command);
                if drop_all || dropped.count == 1 {
                    changes.push((*slot, None));
                } else {
                    dropped.count -= 1;
                    changes.push((*slot, Some(dropped.clone())));
                }
                let dropped_count = if drop_all { dropped.count } else { 1 };
                dropped.count = dropped_count;
                changes.push((SlotIndex(-1), Some(dropped)));
            }
        }

        let released =
            self.creative.quick_craft.as_ref().is_some_and(|gesture| {
                ui.input(|input| input.pointer.button_released(gesture.button))
            });
        if released && let Some(gesture) = self.creative.quick_craft.take() {
            apply_creative_quick_craft(&self.state, &gesture, carried, changes);
        }

        let trash =
            egui::Rect::from_min_size(rect.min + metrics.offset(173, 112), metrics.size(16, 16));
        let response = ui
            .interact(
                trash,
                egui::Id::new("cubic-creative-trash"),
                egui::Sense::click(),
            )
            .on_hover_text("Destroy Item");
        if response.clicked() {
            if ui.input(|input| input.modifiers.shift) {
                for slot in 5_i16..=45 {
                    changes.push((SlotIndex(slot), None));
                }
            } else {
                *carried = None;
            }
        }
    }

    fn paint_creative_tabs(
        &self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        metrics: MinecraftGuiMetrics,
        selected: &str,
        next: &mut String,
        layer: CreativePaintLayer,
    ) -> (bool, Option<String>) {
        let selected_pass = match layer {
            CreativePaintLayer::UnselectedTabs => false,
            CreativePaintLayer::SelectedTab => true,
            CreativePaintLayer::MainPanel => return (false, None),
        };
        let mut consumed_click = false;
        let mut hovered_title = None;
        for tab in &self.creative_tabs {
            if (tab.metadata.id.as_str() == selected) != selected_pass {
                continue;
            }
            let top = tab.metadata.row == cubic_version::CreativeTabRow::Top;
            let column = usize::from(tab.metadata.column);
            let x = creative_tab_x(&tab.metadata);
            let y = if top { -28.0 } else { 132.0 };
            let position = rect.min
                + egui::vec2(
                    f32::from(x) * metrics.points_per_gui,
                    y * metrics.points_per_gui,
                );
            let tab_rect = egui::Rect::from_min_size(position, metrics.size(26, 32));
            let ordinal = (column + 1).clamp(1, 7);
            let name = format!(
                "gui/sprites/container/creative_inventory/tab_{}_{}_{}",
                if top { "top" } else { "bottom" },
                if tab.metadata.id.as_str() == selected {
                    "selected"
                } else {
                    "unselected"
                },
                ordinal
            );
            self.paint_texture(ui.painter(), &name, tab_rect);
            let icon_rect = egui::Rect::from_center_size(
                tab_rect.center()
                    + egui::vec2(0.0, if top { 1.0 } else { -1.0 } * metrics.points_per_gui),
                metrics.size(16, 16),
            );
            self.paint_stack(ui.painter(), icon_rect, Some(&tab.icon), metrics);
            let interaction_rect = egui::Rect::from_min_size(
                rect.min
                    + egui::vec2(
                        f32::from(x) * metrics.points_per_gui,
                        if top { -32.0 } else { 136.0 } * metrics.points_per_gui,
                    ),
                metrics.size(26, 32),
            );
            let response = ui.interact(
                interaction_rect,
                egui::Id::new(("cubic-creative-tab", tab.metadata.id.as_str())),
                egui::Sense::click(),
            );
            let pointer_over_tab = ui
                .input(|input| input.pointer.hover_pos())
                .is_some_and(|pointer| interaction_rect.contains(pointer));
            if pointer_over_tab {
                let title = self.translate(&tab.metadata.title_key).to_owned();
                self.paint_creative_tab_tooltip(ui.ctx(), &title, metrics);
                hovered_title = Some(title);
            }
            if response.clicked() {
                *next = tab.metadata.id.to_string();
                consumed_click = true;
            }
        }
        (consumed_click, hovered_title)
    }

    fn creative_items(&self, tab: &str, search: &str) -> Vec<ItemStack> {
        if tab == "minecraft:hotbar" {
            return self.state.player().slots[36..45]
                .iter()
                .filter_map(Clone::clone)
                .collect();
        }
        let query = search.trim().to_ascii_lowercase();
        self.creative_tabs
            .iter()
            .find(|candidate| candidate.metadata.id.as_str() == tab)
            .into_iter()
            .flat_map(|candidate| candidate.items.iter().zip(&candidate.search_text))
            .filter(|(stack, search_text)| {
                tab != "minecraft:search"
                    || query.is_empty()
                    || creative_name_search_matches(stack, search_text, &query)
            })
            .map(|(stack, _)| stack.clone())
            .collect()
    }

    fn show_container(&mut self, context: &egui::Context, actions: &mut Vec<InventoryAction>) {
        let container = self
            .state
            .open()
            .unwrap_or_else(|| self.state.player())
            .clone();
        let preview_state = self.quick_craft_preview_state();
        let preview_container = preview_state
            .as_ref()
            .map(|state| state.open().unwrap_or_else(|| state.player()).clone());
        let layout = MenuLayout::for_container(&container);
        let metrics = MinecraftGuiMetrics::from_context(context);
        let screen = context.viewport_rect();
        let size = metrics.size(layout.width, layout.height);
        let origin = egui::pos2(
            screen.center().x - size.x * 0.5,
            screen.center().y - size.y * 0.5,
        );
        egui::Area::new(egui::Id::new("cubic-inventory"))
            .fixed_pos(origin)
            .order(egui::Order::Foreground)
            .show(context, |ui| {
                let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                self.paint_menu_background(ui.painter(), rect, &layout, metrics);
                self.paint_menu_labels(ui.painter(), rect, &container, &layout, metrics);
                self.paint_furnace_progress(ui.painter(), rect, &container, &layout, metrics);
                for placement in &layout.slots {
                    let Ok(index) = usize::try_from(placement.slot.0) else {
                        continue;
                    };
                    let Some(stack) = container.slots.get(index) else {
                        continue;
                    };
                    let displayed_stack = preview_container
                        .as_ref()
                        .and_then(|preview| preview.slots.get(index))
                        .unwrap_or(stack);
                    let slot_rect = egui::Rect::from_min_size(
                        rect.min
                            + egui::vec2(
                                f32::from(placement.x) * metrics.points_per_gui,
                                f32::from(placement.y) * metrics.points_per_gui,
                            ),
                        metrics.size(16, 16),
                    );
                    if stack.is_none() && layout.kind == MenuKind::Player {
                        self.paint_empty_player_slot(ui.painter(), slot_rect, placement.slot);
                    }
                    self.interact_slot(
                        ui,
                        &container,
                        SlotPresentation {
                            slot: placement.slot,
                            rect: slot_rect,
                            stack: stack.as_ref(),
                            displayed_stack: displayed_stack.as_ref(),
                            quick_craft_preview: self
                                .quick_craft
                                .as_ref()
                                .is_some_and(|gesture| gesture.visited.contains(&placement.slot)),
                        },
                        metrics,
                        actions,
                    );
                }
                self.finish_quick_craft(ui, &container, actions);
                let displayed_carried = match &preview_state {
                    Some(preview) => preview.carried(),
                    None => self.state.carried(),
                };
                if let Some(carried) = displayed_carried
                    && let Some(pointer) = ui.input(|input| input.pointer.hover_pos())
                {
                    let carried_rect = egui::Rect::from_min_size(
                        pointer - egui::vec2(8.0, 8.0) * metrics.points_per_gui,
                        metrics.size(16, 16),
                    );
                    self.paint_stack(ui.painter(), carried_rect, Some(carried), metrics);
                }
            });
    }

    pub fn cancel_pointer_gesture(&mut self) {
        self.quick_craft = None;
    }

    fn quick_craft_preview_state(&self) -> Option<InventoryState> {
        let gesture = self.quick_craft.as_ref()?;
        let mut preview = self.state.clone();
        let start = ContainerClick {
            container: gesture.container,
            state_id: gesture.state_id,
            slot: SlotIndex(-999),
            button: i8::try_from(gesture.kind << 2).ok()?,
            kind: ContainerClickKind::QuickCraft,
        };
        preview.apply_click(start).ok()?;
        for slot in &gesture.visited {
            preview
                .apply_click(ContainerClick {
                    slot: *slot,
                    button: i8::try_from((gesture.kind << 2) | 1).ok()?,
                    ..start
                })
                .ok()?;
        }
        preview
            .apply_click(ContainerClick {
                button: i8::try_from((gesture.kind << 2) | 2).ok()?,
                ..start
            })
            .ok()?;
        Some(preview)
    }

    fn paint_empty_player_slot(&self, painter: &egui::Painter, rect: egui::Rect, slot: SlotIndex) {
        let Some(sprite) = empty_player_slot_sprite(slot) else {
            return;
        };
        self.paint_texture(painter, sprite, rect);
    }

    fn paint_texture(&self, painter: &egui::Painter, name: &str, rect: egui::Rect) {
        if let Some(texture) = self.screen_textures.get(name) {
            painter.image(
                texture.handle.id(),
                rect,
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        } else {
            painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(198, 198, 198));
        }
    }

    fn paint_cropped_texture(
        &self,
        painter: &egui::Painter,
        name: &str,
        rect: egui::Rect,
        source_size: [u16; 2],
    ) {
        if let Some(texture) = self.screen_textures.get(name) {
            painter.image(
                texture.handle.id(),
                rect,
                egui::Rect::from_min_max(
                    egui::Pos2::ZERO,
                    egui::pos2(
                        f32::from(source_size[0]) / texture.size[0] as f32,
                        f32::from(source_size[1]) / texture.size[1] as f32,
                    ),
                ),
                egui::Color32::WHITE,
            );
        } else {
            painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(198, 198, 198));
        }
    }

    fn paint_menu_background(
        &self,
        painter: &egui::Painter,
        rect: egui::Rect,
        layout: &MenuLayout,
        metrics: MinecraftGuiMetrics,
    ) {
        if layout.kind == MenuKind::Chest {
            let Some(texture) = self.screen_textures.get(layout.texture) else {
                painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(198, 198, 198));
                return;
            };
            let top_height = layout.height.saturating_sub(96);
            let source_height = texture.size[1] as f32;
            let top = egui::Rect::from_min_size(rect.min, metrics.size(layout.width, top_height));
            painter.image(
                texture.handle.id(),
                top,
                egui::Rect::from_min_max(
                    egui::Pos2::ZERO,
                    egui::pos2(
                        f32::from(layout.width) / texture.size[0] as f32,
                        f32::from(top_height) / source_height,
                    ),
                ),
                egui::Color32::WHITE,
            );
            let bottom =
                egui::Rect::from_min_size(top.left_bottom(), metrics.size(layout.width, 96));
            painter.image(
                texture.handle.id(),
                bottom,
                egui::Rect::from_min_max(
                    egui::pos2(0.0, 126.0 / source_height),
                    egui::pos2(
                        f32::from(layout.width) / texture.size[0] as f32,
                        222.0 / source_height,
                    ),
                ),
                egui::Color32::WHITE,
            );
        } else {
            if let Some(texture) = self.screen_textures.get(layout.texture) {
                painter.image(
                    texture.handle.id(),
                    rect,
                    egui::Rect::from_min_max(
                        egui::Pos2::ZERO,
                        egui::pos2(
                            f32::from(layout.width) / texture.size[0] as f32,
                            f32::from(layout.height) / texture.size[1] as f32,
                        ),
                    ),
                    egui::Color32::WHITE,
                );
            } else {
                painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(198, 198, 198));
            }
        }
    }

    fn paint_menu_labels(
        &self,
        painter: &egui::Painter,
        rect: egui::Rect,
        container: &ContainerState,
        layout: &MenuLayout,
        metrics: MinecraftGuiMetrics,
    ) {
        let color = egui::Color32::from_rgb(64, 64, 64);
        let title = if layout.kind == MenuKind::Player {
            self.translate("container.crafting")
        } else {
            self.translate(&container.title)
        };
        self.paint_minecraft_text(
            painter,
            rect.min + metrics.offset(layout.title[0], layout.title[1]),
            title,
            color,
            metrics,
        );
        if let Some(position) = layout.inventory_label {
            self.paint_minecraft_text(
                painter,
                rect.min + metrics.offset(position[0], position[1]),
                self.translate("container.inventory"),
                color,
                metrics,
            );
        }
    }

    fn translate<'a>(&'a self, value: &'a str) -> &'a str {
        self.translations.get(value).map_or(value, String::as_str)
    }

    fn paint_minecraft_text(
        &self,
        painter: &egui::Painter,
        origin: egui::Pos2,
        text: &str,
        color: egui::Color32,
        metrics: MinecraftGuiMetrics,
    ) {
        let Some(font) = self.screen_textures.get("font/ascii") else {
            painter.text(
                origin,
                egui::Align2::LEFT_TOP,
                text,
                egui::FontId::monospace(8.0 * metrics.points_per_gui),
                color,
            );
            return;
        };
        let Some(widths) = font.glyph_widths.as_ref() else {
            return;
        };
        let mut x = origin.x;
        for character in text.chars() {
            let Some(code) = usize::try_from(u32::from(character))
                .ok()
                .filter(|code| *code < 256)
            else {
                continue;
            };
            let cell_x = (code % 16) as f32;
            let cell_y = (code / 16) as f32;
            let width = widths[code].max(1);
            if character != ' ' {
                painter.image(
                    font.handle.id(),
                    egui::Rect::from_min_size(
                        egui::pos2(x, origin.y),
                        egui::vec2(f32::from(width), 8.0) * metrics.points_per_gui,
                    ),
                    egui::Rect::from_min_max(
                        egui::pos2(cell_x / 16.0, cell_y / 16.0),
                        egui::pos2(
                            (cell_x + f32::from(width) / 8.0) / 16.0,
                            (cell_y + 1.0) / 16.0,
                        ),
                    ),
                    color,
                );
            }
            x += f32::from(if character == ' ' { 4 } else { width + 1 }) * metrics.points_per_gui;
        }
    }

    fn paint_creative_title(
        &self,
        painter: &egui::Painter,
        panel: egui::Rect,
        tab: &CreativeUiTab,
        metrics: MinecraftGuiMetrics,
    ) {
        if tab.metadata.show_title {
            self.paint_minecraft_text(
                painter,
                panel.min + metrics.offset(8, 6),
                self.translate(&tab.metadata.title_key),
                egui::Color32::from_gray(64),
                metrics,
            );
        }
    }

    fn paint_furnace_progress(
        &self,
        painter: &egui::Painter,
        rect: egui::Rect,
        container: &ContainerState,
        layout: &MenuLayout,
        metrics: MinecraftGuiMetrics,
    ) {
        if layout.kind != MenuKind::Furnace {
            return;
        }
        let lit = furnace_lit_height(&container.properties);
        if lit > 0 {
            self.paint_partial_sprite(
                painter,
                "gui/sprites/container/furnace/lit_progress",
                rect.min + metrics.offset(56, 36 + 14 - lit),
                [14, lit],
                [0, 14 - lit],
                metrics,
            );
        }
        let progress = furnace_progress_width(&container.properties);
        if progress > 0 {
            self.paint_partial_sprite(
                painter,
                "gui/sprites/container/furnace/burn_progress",
                rect.min + metrics.offset(79, 34),
                [progress, 16],
                [0, 0],
                metrics,
            );
        }
    }

    fn paint_partial_sprite(
        &self,
        painter: &egui::Painter,
        name: &str,
        origin: egui::Pos2,
        logical_size: [u16; 2],
        source_origin: [u16; 2],
        metrics: MinecraftGuiMetrics,
    ) {
        let Some(texture) = self.screen_textures.get(name) else {
            return;
        };
        let tw = texture.size[0] as f32;
        let th = texture.size[1] as f32;
        let uv = egui::Rect::from_min_max(
            egui::pos2(
                f32::from(source_origin[0]) / tw,
                f32::from(source_origin[1]) / th,
            ),
            egui::pos2(
                f32::from(source_origin[0] + logical_size[0]) / tw,
                f32::from(source_origin[1] + logical_size[1]) / th,
            ),
        );
        painter.image(
            texture.handle.id(),
            egui::Rect::from_min_size(origin, metrics.size(logical_size[0], logical_size[1])),
            uv,
            egui::Color32::WHITE,
        );
    }

    fn interact_slot(
        &mut self,
        ui: &mut egui::Ui,
        container: &ContainerState,
        presentation: SlotPresentation<'_>,
        metrics: MinecraftGuiMetrics,
        actions: &mut Vec<InventoryAction>,
    ) {
        let SlotPresentation {
            slot,
            rect,
            stack,
            displayed_stack,
            quick_craft_preview,
        } = presentation;
        let response = ui.interact(
            rect,
            egui::Id::new(("cubic-inventory-slot", container.id, slot)),
            egui::Sense::click_and_drag(),
        );
        let pointer_over = ui
            .input(|input| input.pointer.hover_pos())
            .is_some_and(|position| rect.contains(position));
        if response.hovered() || pointer_over || quick_craft_preview {
            self.paint_texture(
                ui.painter(),
                "gui/sprites/container/slot_highlight_back",
                rect.expand(metrics.points_per_gui),
            );
        }
        self.paint_stack(ui.painter(), rect, displayed_stack, metrics);
        if response.hovered()
            && self.state.carried().is_none()
            && let Some(stack) = stack
        {
            self.paint_item_tooltip(ui.ctx(), stack, metrics, None);
        }
        if response.hovered() || pointer_over || quick_craft_preview {
            self.paint_texture(
                ui.painter(),
                "gui/sprites/container/slot_highlight_front",
                rect.expand(metrics.points_per_gui),
            );
        }

        if response.double_clicked() {
            actions.push(click(container, slot, 0, ContainerClickKind::PickupAll));
        } else if response.middle_clicked() {
            actions.push(click(container, slot, 2, ContainerClickKind::Clone));
        } else if response.clicked() || response.secondary_clicked() {
            actions.push(InventoryAction::Click(ContainerClick {
                container: container.id,
                state_id: container.state_id,
                slot,
                button: i8::from(response.secondary_clicked()),
                kind: if ui.input(|input| input.modifiers.shift) {
                    ContainerClickKind::QuickMove
                } else {
                    ContainerClickKind::Pickup
                },
            }));
        }
        if response.hovered() || pointer_over {
            if let Some(hotbar) = pressed_hotbar_number(ui) {
                actions.push(click(
                    container,
                    slot,
                    i8::try_from(hotbar).unwrap_or(0),
                    ContainerClickKind::Swap,
                ));
            }
            if ui.input(|input| input.key_pressed(egui::Key::Q)) {
                let whole_stack = ui.input(|input| input.modifiers.command);
                actions.push(click(
                    container,
                    slot,
                    i8::from(whole_stack),
                    ContainerClickKind::Throw,
                ));
            }
        }
        for (button, kind) in [
            (egui::PointerButton::Primary, 0_u8),
            (egui::PointerButton::Secondary, 1_u8),
            (egui::PointerButton::Middle, 2_u8),
        ] {
            if response.drag_started_by(button) && self.state.carried().is_some() {
                actions.push(click(
                    container,
                    SlotIndex(-999),
                    i8::try_from(kind << 2).unwrap_or(0),
                    ContainerClickKind::QuickCraft,
                ));
                self.quick_craft = Some(QuickCraftUiGesture {
                    container: container.id,
                    state_id: container.state_id,
                    kind,
                    visited: BTreeSet::new(),
                });
            }
        }
        let eligible = self
            .state
            .carried()
            .is_some_and(|carried| quick_craft_slot_eligible(container, slot, stack, carried));
        if pointer_over
            && eligible
            && let Some(gesture) = &mut self.quick_craft
            && gesture.container == container.id
            && gesture.state_id == container.state_id
            && ui.input(|input| {
                input
                    .pointer
                    .button_down(quick_craft_pointer_button(gesture.kind))
            })
            && gesture.visited.insert(slot)
        {
            actions.push(click(
                container,
                slot,
                i8::try_from((gesture.kind << 2) | 1).unwrap_or(1),
                ContainerClickKind::QuickCraft,
            ));
        }
    }

    fn finish_quick_craft(
        &mut self,
        ui: &egui::Ui,
        container: &ContainerState,
        actions: &mut Vec<InventoryAction>,
    ) {
        if let Some(gesture) = self.quick_craft.take() {
            let button = quick_craft_pointer_button(gesture.kind);
            if ui.input(|input| input.pointer.button_released(button)) {
                actions.push(click(
                    container,
                    SlotIndex(-999),
                    i8::try_from((gesture.kind << 2) | 2).unwrap_or(2),
                    ContainerClickKind::QuickCraft,
                ));
            } else if ui.input(|input| input.pointer.button_down(button)) {
                self.quick_craft = Some(gesture);
            }
        }
    }

    fn creative_category_name(&self, stack: &ItemStack) -> Option<String> {
        self.creative_tabs
            .iter()
            .filter(|tab| tab.metadata.tab_type == cubic_version::CreativeTabType::Category)
            .find(|tab| tab.items.iter().any(|candidate| candidate == stack))
            .map(|tab| self.translate(&tab.metadata.title_key).to_owned())
    }

    fn paint_item_tooltip(
        &self,
        context: &egui::Context,
        stack: &ItemStack,
        metrics: MinecraftGuiMetrics,
        creative_category: Option<&str>,
    ) {
        let Some(pointer) = context.input(|input| input.pointer.hover_pos()) else {
            return;
        };
        let Some(tooltip) = slot_item_tooltip(
            Some(stack),
            &self.translations,
            ItemTooltipMode::Normal,
            creative_category,
        ) else {
            return;
        };
        let glyph_widths = self
            .screen_textures
            .get("font/ascii")
            .and_then(|font| font.glyph_widths.as_deref());
        let logical_width = std::iter::once(tooltip.name.as_str())
            .chain(tooltip.lines.iter().map(String::as_str))
            .map(|line| minecraft_text_width(glyph_widths, line))
            .fold(0.0_f32, f32::max)
            + 8.0;
        let logical_height = 8.0 + tooltip.lines.len() as f32 * 10.0 + 6.0;
        let size = egui::vec2(
            logical_width * metrics.points_per_gui,
            logical_height * metrics.points_per_gui,
        );
        let viewport = context.viewport_rect();
        let wanted = pointer
            + egui::vec2(
                12.0 * metrics.points_per_gui,
                -12.0 * metrics.points_per_gui,
            );
        let origin = egui::pos2(
            wanted.x.min(viewport.right() - size.x).max(viewport.left()),
            wanted.y.min(viewport.bottom() - size.y).max(viewport.top()),
        );
        egui::Area::new(egui::Id::new("cubic-item-tooltip"))
            .order(egui::Order::Tooltip)
            .fixed_pos(origin)
            .interactable(false)
            .show(context, |ui| {
                let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                self.paint_texture(ui.painter(), "gui/sprites/tooltip/background", rect);
                self.paint_texture(ui.painter(), "gui/sprites/tooltip/frame", rect);
                self.paint_minecraft_text(
                    ui.painter(),
                    rect.min + metrics.offset(4, 3),
                    &tooltip.name,
                    egui::Color32::WHITE,
                    metrics,
                );
                for (index, line) in tooltip.lines.iter().enumerate() {
                    let y = 13 + u16::try_from(index).unwrap_or(0) * 10;
                    self.paint_minecraft_text(
                        ui.painter(),
                        rect.min + metrics.offset(4, y),
                        line,
                        egui::Color32::from_rgb(170, 170, 170),
                        metrics,
                    );
                }
            });
    }

    fn paint_creative_tab_tooltip(
        &self,
        context: &egui::Context,
        text: &str,
        metrics: MinecraftGuiMetrics,
    ) {
        let Some(pointer) = context.input(|input| input.pointer.hover_pos()) else {
            return;
        };
        let width = minecraft_text_width(
            self.screen_textures
                .get("font/ascii")
                .and_then(|font| font.glyph_widths.as_deref()),
            text,
        ) + 8.0;
        let size = egui::vec2(
            width * metrics.points_per_gui,
            14.0 * metrics.points_per_gui,
        );
        let viewport = context.viewport_rect();
        let wanted = pointer
            + egui::vec2(
                12.0 * metrics.points_per_gui,
                -12.0 * metrics.points_per_gui,
            );
        let position = egui::pos2(
            wanted.x.min(viewport.max.x - size.x).max(viewport.min.x),
            wanted.y.min(viewport.max.y - size.y).max(viewport.min.y),
        );
        egui::Area::new(egui::Id::new("cubic-creative-tab-tooltip"))
            .fixed_pos(position)
            .order(egui::Order::Tooltip)
            .show(context, |ui| {
                let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                self.paint_texture(ui.painter(), "gui/sprites/tooltip/background", rect);
                self.paint_texture(ui.painter(), "gui/sprites/tooltip/frame", rect);
                self.paint_minecraft_text(
                    ui.painter(),
                    rect.min + metrics.offset(4, 3),
                    text,
                    egui::Color32::WHITE,
                    metrics,
                );
            });
    }

    fn paint_stack(
        &self,
        painter: &egui::Painter,
        rect: egui::Rect,
        stack: Option<&ItemStack>,
        metrics: MinecraftGuiMetrics,
    ) {
        let Some(stack) = stack else {
            return;
        };
        let model = stack.gui_render_key();
        if let Some((atlas, region)) = model.as_ref().and_then(|model| {
            let atlas = self.item_atlases.get(&metrics.gui_scale)?;
            Some((atlas, atlas.regions.get(model)?))
        }) {
            painter.image(atlas.handle.id(), rect, *region, egui::Color32::WHITE);
        }
        if let Some((width, color)) = durability_bar(stack) {
            painter.rect_filled(
                egui::Rect::from_min_size(rect.min + metrics.offset(2, 13), metrics.size(13, 2)),
                0.0,
                egui::Color32::BLACK,
            );
            painter.rect_filled(
                egui::Rect::from_min_size(rect.min + metrics.offset(2, 13), metrics.size(width, 1)),
                0.0,
                color,
            );
        }
        if stack.count > 1 {
            let text = stack.count.to_string();
            let width = minecraft_text_width(
                self.screen_textures
                    .get("font/ascii")
                    .and_then(|font| font.glyph_widths.as_deref()),
                &text,
            );
            let origin = egui::pos2(
                rect.right() - width * metrics.points_per_gui,
                rect.bottom() - 8.0 * metrics.points_per_gui,
            );
            self.paint_minecraft_text(
                painter,
                origin + metrics.offset(1, 1),
                &text,
                egui::Color32::from_black_alpha(220),
                metrics,
            );
            self.paint_minecraft_text(painter, origin, &text, egui::Color32::WHITE, metrics);
        }
    }
}

fn quick_craft_pointer_button(kind: u8) -> egui::PointerButton {
    match kind {
        0 => egui::PointerButton::Primary,
        1 => egui::PointerButton::Secondary,
        _ => egui::PointerButton::Middle,
    }
}

fn durability_bar(stack: &ItemStack) -> Option<(u16, egui::Color32)> {
    let component = |name: &str| {
        stack
            .components
            .added
            .iter()
            .find(|(identifier, _)| identifier.as_str() == name)
            .map(|(_, value)| value)
    };
    if component("minecraft:unbreakable").is_some() {
        return None;
    }
    let cubic_world::ComponentValue::VarInt(maximum) = component("minecraft:max_damage")? else {
        return None;
    };
    let cubic_world::ComponentValue::VarInt(damage) = component("minecraft:damage")? else {
        return None;
    };
    if *maximum <= 0 {
        return None;
    }
    let fraction = (1.0 - *damage as f32 / *maximum as f32).clamp(0.0, 1.0);
    let width = (13.0 * fraction).round().clamp(0.0, 13.0) as u16;
    let hue = fraction / 3.0;
    let (red, green) = if hue <= 1.0 / 6.0 {
        (1.0, hue * 6.0)
    } else {
        (2.0 - hue * 6.0, 1.0)
    };
    Some((
        width,
        egui::Color32::from_rgb(
            (red.clamp(0.0, 1.0) * 255.0).round() as u8,
            (green.clamp(0.0, 1.0) * 255.0).round() as u8,
            0,
        ),
    ))
}

fn click(
    container: &cubic_world::ContainerState,
    slot: SlotIndex,
    button: i8,
    kind: ContainerClickKind,
) -> InventoryAction {
    InventoryAction::Click(ContainerClick {
        container: container.id,
        state_id: container.state_id,
        slot,
        button,
        kind,
    })
}

fn pressed_hotbar_number(ui: &egui::Ui) -> Option<u8> {
    [
        egui::Key::Num1,
        egui::Key::Num2,
        egui::Key::Num3,
        egui::Key::Num4,
        egui::Key::Num5,
        egui::Key::Num6,
        egui::Key::Num7,
        egui::Key::Num8,
        egui::Key::Num9,
    ]
    .into_iter()
    .position(|key| ui.input(|input| input.key_pressed(key)))
    .and_then(|index| u8::try_from(index).ok())
}

fn bitmap_glyph_widths(rgba: &[u8]) -> [u8; 256] {
    let mut widths = [0_u8; 256];
    widths[usize::from(b' ')] = 3;
    for (glyph, width) in widths.iter_mut().enumerate() {
        if glyph == usize::from(b' ') {
            continue;
        }
        let cell_x = (glyph % 16) * 8;
        let cell_y = (glyph / 16) * 8;
        for x in (0..8_usize).rev() {
            let occupied = (0..8_usize).any(|y| {
                rgba.get(((cell_y + y) * 128 + cell_x + x) * 4 + 3)
                    .is_some_and(|alpha| *alpha != 0)
            });
            if occupied {
                *width = u8::try_from(x + 1).unwrap_or(8);
                break;
            }
        }
    }
    widths
}

fn minecraft_text_width(widths: Option<&[u8; 256]>, text: &str) -> f32 {
    text.chars()
        .filter_map(|character| usize::try_from(u32::from(character)).ok())
        .filter(|code| *code < 256)
        .map(|code| {
            if code == usize::from(b' ') {
                4.0
            } else {
                f32::from(widths.map_or(5, |widths| widths[code]).max(1) + 1)
            }
        })
        .sum()
}

/// Explicit presentation state for one persistent session. Switching this
/// value never owns or recreates the underlying connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PresentationModeController {
    mode: SessionPresentationMode,
}

impl PresentationModeController {
    #[must_use]
    pub const fn new(mode: SessionPresentationMode) -> Self {
        Self { mode }
    }

    #[must_use]
    pub const fn mode(self) -> SessionPresentationMode {
        self.mode
    }

    pub fn enter_chat(&mut self) -> bool {
        self.replace(SessionPresentationMode::Chat)
    }

    pub fn enter_play(&mut self) -> bool {
        self.replace(SessionPresentationMode::Play)
    }

    fn replace(&mut self, mode: SessionPresentationMode) -> bool {
        if self.mode == mode {
            return false;
        }
        self.mode = mode;
        true
    }
}

pub trait ChatSessionPort: Send {
    fn try_next_event(&mut self) -> Option<ChatEvent>;
    fn take_critical_event(&mut self) -> Option<ChatEvent>;
    fn dropped_event_count(&mut self) -> usize;
    fn send_message(&mut self, message: String) -> Result<(), String>;
    fn set_presentation_mode(&self, _mode: SessionPresentationMode) {}
    fn disconnect(&mut self);
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisplayedMessage {
    pub kind: ChatMessageKind,
    pub sender: Option<String>,
    pub text: String,
}

pub struct ChatModel {
    state: ChatConnectionState,
    history: VecDeque<DisplayedMessage>,
    history_bytes: usize,
    input: String,
}

impl Default for ChatModel {
    fn default() -> Self {
        Self {
            state: ChatConnectionState::Connecting,
            history: VecDeque::new(),
            history_bytes: 0,
            input: String::new(),
        }
    }
}

impl ChatModel {
    #[must_use]
    pub const fn state(&self) -> ChatConnectionState {
        self.state
    }

    #[must_use]
    pub fn history(&self) -> &VecDeque<DisplayedMessage> {
        &self.history
    }

    #[must_use]
    pub fn input(&self) -> &str {
        &self.input
    }

    pub fn set_input(&mut self, value: String) {
        self.input = value
            .chars()
            .filter(|character| !character.is_control())
            .scan(0_usize, |units, character| {
                let next = units.saturating_add(character.len_utf16());
                (next <= MAX_INPUT_UTF16_UNITS).then(|| {
                    *units = next;
                    character
                })
            })
            .collect();
    }

    pub fn apply(&mut self, event: ChatEvent) {
        match event {
            ChatEvent::Connected => self.state = ChatConnectionState::Connected,
            ChatEvent::Message {
                kind,
                sender,
                message,
            } => self.push(DisplayedMessage {
                kind,
                sender,
                text: message.plain_text,
            }),
            ChatEvent::Warning(text) => self.push(DisplayedMessage {
                kind: ChatMessageKind::ServerNotice,
                sender: None,
                text,
            }),
            ChatEvent::SafetyAlert { message, .. } => self.push(DisplayedMessage {
                kind: ChatMessageKind::ServerNotice,
                sender: None,
                text: format!("⚠ {message}"),
            }),
            ChatEvent::Disconnected { reason } => {
                self.state = ChatConnectionState::Disconnected;
                self.push(DisplayedMessage {
                    kind: ChatMessageKind::ServerNotice,
                    sender: None,
                    text: format!("Disconnected: {reason}"),
                });
            }
        }
    }

    pub fn take_message_to_send(&mut self) -> Option<String> {
        let trimmed = self.input.trim();
        if trimmed.is_empty() {
            return None;
        }
        let message = trimmed.to_owned();
        self.input.clear();
        Some(message)
    }

    fn push(&mut self, message: DisplayedMessage) {
        let bytes = retained_bytes(&message);
        if bytes > MAX_HISTORY_TEXT_BYTES {
            return;
        }
        self.history_bytes = self.history_bytes.saturating_add(bytes);
        self.history.push_back(message);
        while self.history.len() > MAX_HISTORY_MESSAGES
            || self.history_bytes > MAX_HISTORY_TEXT_BYTES
        {
            let Some(removed) = self.history.pop_front() else {
                break;
            };
            self.history_bytes = self.history_bytes.saturating_sub(retained_bytes(&removed));
        }
    }
}

pub struct ChatMode {
    model: ChatModel,
    port: Box<dyn ChatSessionPort>,
    focus_input: bool,
}

impl ChatMode {
    #[must_use]
    pub fn new(port: Box<dyn ChatSessionPort>) -> Self {
        Self {
            model: ChatModel::default(),
            port,
            focus_input: true,
        }
    }

    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Some(event) = self.port.try_next_event() {
            self.model.apply(event);
            changed = true;
        }
        if let Some(event) = self.port.take_critical_event() {
            self.model.apply(event);
            changed = true;
        }
        let dropped = self.port.dropped_event_count();
        if dropped > 0 {
            self.model.apply(ChatEvent::Warning(format!(
                "{dropped} incoming messages were dropped because the UI queue was full"
            )));
            changed = true;
        }
        changed
    }

    pub fn show(&mut self, root: &mut egui::Ui) {
        let _ = self.show_with_play_control(root, false);
    }

    /// Draws the existing Chat Mode and optionally exposes its prominent mode
    /// switch. Returns true only when PLAY was activated.
    pub fn show_with_play_control(&mut self, root: &mut egui::Ui, show_play: bool) -> bool {
        let mut play_requested = false;
        let connected = self.model.state() == ChatConnectionState::Connected;
        egui::Panel::top("cubic-chat-header").show(root, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.heading("Cubic");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if show_play
                        && ui
                            .add(egui::Button::new("PLAY").min_size([72.0, 36.0].into()))
                            .clicked()
                    {
                        play_requested = true;
                    }
                    let (label, color) = match self.model.state() {
                        ChatConnectionState::Connecting => ("Connecting ●", egui::Color32::YELLOW),
                        ChatConnectionState::Connected => {
                            ("Connected ●", egui::Color32::LIGHT_GREEN)
                        }
                        ChatConnectionState::Disconnected => {
                            ("Disconnected ●", egui::Color32::LIGHT_RED)
                        }
                    };
                    ui.colored_label(color, label);
                });
            });
            ui.add_space(8.0);
        });

        egui::Panel::bottom("cubic-chat-input").show(root, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let available = (ui.available_width() - 84.0).max(80.0);
                let response = ui.add_sized(
                    [available, 44.0],
                    egui::TextEdit::singleline(&mut self.model.input)
                        .hint_text("Message…")
                        .desired_width(f32::INFINITY),
                );
                if self.focus_input {
                    response.request_focus();
                    self.focus_input = false;
                }
                let enter =
                    response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                let send = ui
                    .add_enabled(
                        connected,
                        egui::Button::new("Send").min_size([72.0, 44.0].into()),
                    )
                    .clicked();
                if connected && (send || enter) {
                    self.send_current();
                    response.request_focus();
                }
            });
            self.model.set_input(self.model.input.clone());
            ui.add_space(8.0);
        });

        egui::CentralPanel::default().show(root, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    for message in self.model.history() {
                        ui.horizontal_wrapped(|ui| {
                            match message.kind {
                                ChatMessageKind::Player => {
                                    if let Some(sender) = &message.sender {
                                        ui.strong(format!("<{sender}>"));
                                    }
                                }
                                ChatMessageKind::System => {
                                    ui.colored_label(egui::Color32::LIGHT_BLUE, "Server:");
                                }
                                ChatMessageKind::ServerNotice => {
                                    ui.colored_label(egui::Color32::YELLOW, "Notice:");
                                }
                            }
                            ui.label(&message.text);
                        });
                        ui.add_space(4.0);
                    }
                });
        });
        play_requested
    }

    pub fn disconnect(&mut self) {
        self.port.disconnect();
    }

    pub fn set_presentation_mode(&self, mode: SessionPresentationMode) {
        self.port.set_presentation_mode(mode);
    }

    pub fn focus_input(&mut self) {
        self.focus_input = true;
    }

    fn send_current(&mut self) {
        let Some(message) = self.model.take_message_to_send() else {
            return;
        };
        if let Err(error) = self.port.send_message(message) {
            self.model.apply(ChatEvent::Warning(error));
        }
    }
}

fn retained_bytes(message: &DisplayedMessage) -> usize {
    message
        .sender
        .as_ref()
        .map_or(0, String::len)
        .saturating_add(message.text.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cubic_core::{ChatMessage, ChatMessageTrust, StructuredText};

    fn message(text: &str) -> ChatEvent {
        ChatEvent::Message {
            kind: ChatMessageKind::System,
            sender: None,
            message: ChatMessage {
                plain_text: text.to_owned(),
                structured: StructuredText::String(text.to_owned()),
                trust: ChatMessageTrust::NotApplicable,
            },
        }
    }

    fn test_stack(item: &str, count: u32) -> ItemStack {
        ItemStack::new(
            cubic_version_identifier(item),
            count,
            ComponentPatch::default(),
        )
        .unwrap()
    }

    #[test]
    fn creative_picker_matches_the_verified_carried_stack_state_table() {
        let stone = test_stack("minecraft:stone", 1);
        let dirt = test_stack("minecraft:dirt", 1);
        let mut carried = Some(test_stack("minecraft:stone", 5));
        apply_creative_picker_input(&mut carried, &dirt, CreativePickerInput::Primary);
        assert!(
            carried.is_none(),
            "different primary clears instead of replacing"
        );

        apply_creative_picker_input(&mut carried, &dirt, CreativePickerInput::Secondary);
        assert_eq!(
            carried.as_ref().map(|stack| (&stack.item, stack.count)),
            Some((&dirt.item, 1))
        );
        apply_creative_picker_input(&mut carried, &stone, CreativePickerInput::Secondary);
        assert!(carried.is_none(), "different secondary decrements carried");

        carried = Some(test_stack("minecraft:stone", 5));
        apply_creative_picker_input(&mut carried, &stone, CreativePickerInput::Primary);
        assert_eq!(carried.as_ref().map(|stack| stack.count), Some(6));
        apply_creative_picker_input(&mut carried, &stone, CreativePickerInput::Secondary);
        assert_eq!(carried.as_ref().map(|stack| stack.count), Some(5));
        apply_creative_picker_input(&mut carried, &stone, CreativePickerInput::QuickMove);
        assert_eq!(
            carried.as_ref().map(ItemStack::max_stack_size),
            carried.as_ref().map(|stack| stack.count)
        );

        let before = carried.clone();
        apply_creative_picker_input(&mut carried, &dirt, CreativePickerInput::Clone);
        assert_eq!(carried, before, "clone does nothing while already carrying");
        carried = None;
        apply_creative_picker_input(&mut carried, &dirt, CreativePickerInput::Clone);
        assert_eq!(carried.as_ref().map(|stack| stack.count), Some(64));

        assert_eq!(creative_picker_throw(&stone, false).count, 1);
        assert_eq!(creative_picker_throw(&stone, true).count, 64);
        assert_eq!(creative_picker_hotbar_copy(&stone).count, 64);
    }

    #[test]
    fn primary_click_on_empty_creative_template_clears_only_carried_stack() {
        let mut carried = Some(test_stack("minecraft:stone", 32));
        let player_before = InventoryState::new();
        clear_creative_empty_picker_slot(&mut carried);
        assert!(carried.is_none());
        assert_eq!(player_before, InventoryState::new());
    }

    #[test]
    fn creative_real_hotbar_and_inventory_use_the_player_menu_semantics() {
        let mut state = InventoryState::new();
        state
            .set_slot(
                cubic_world::PLAYER_CONTAINER_ID,
                4,
                SlotIndex(36),
                Some(test_stack("minecraft:stone", 12)),
            )
            .unwrap();
        let mut carried = None;
        let mut changes = Vec::new();
        apply_creative_real_slot(
            &state,
            &mut carried,
            SlotIndex(36),
            0,
            ContainerClickKind::Pickup,
            &mut changes,
        );
        assert_eq!(carried.as_ref().map(|stack| stack.count), Some(12));
        assert_eq!(changes, vec![(SlotIndex(36), None)]);
        state.set_carried(carried.clone());
        state.set_creative_slot(SlotIndex(36), None).unwrap();
        assert_eq!(
            state
                .carried()
                .map(|stack| (stack.item.as_str(), stack.count)),
            Some(("minecraft:stone", 12))
        );
        let actions =
            creative_synchronization_actions(None, state.carried(), [(SlotIndex(36), None)]);
        assert!(matches!(
            actions.first(),
            Some(InventoryAction::CreativeCarried(Some(stack)))
                if stack.item.as_str() == "minecraft:stone" && stack.count == 12
        ));
        assert!(matches!(
            actions.get(1),
            Some(InventoryAction::CreativeSlot {
                slot: SlotIndex(36),
                stack: None
            })
        ));

        changes.clear();
        apply_creative_real_slot(
            &state,
            &mut carried,
            SlotIndex(36),
            0,
            ContainerClickKind::Pickup,
            &mut changes,
        );
        assert!(carried.is_none());
        assert_eq!(changes[0].0, SlotIndex(36));
        assert_eq!(changes[0].1.as_ref().map(|stack| stack.count), Some(12));
    }

    #[test]
    fn operator_tab_requires_creative_instabuild_permission_and_enabled_option() {
        let mut overlay = InventoryOverlay::default();
        overlay.set_game_mode(GameMode::Creative);
        assert_eq!(overlay.creative_tabs.len(), 13);
        let mut state = InventoryState::new();
        state.set_instant_build(true);
        state.set_permission_level(2);
        overlay.replace(state);
        assert_eq!(overlay.creative_tabs.len(), 14);
        assert!(overlay.creative_tabs.iter().any(|tab| {
            tab.metadata.id.as_str() == "minecraft:op_blocks" && tab.items.len() == 34
        }));
        overlay.operator_items_enabled = false;
        overlay.refresh_creative_tabs();
        assert_eq!(overlay.creative_tabs.len(), 13);
    }

    #[test]
    fn creative_search_editor_is_borderless_inside_the_runtime_background_artwork() {
        assert_eq!(CREATIVE_SEARCH_BOUNDS, [82, 6, 80, 9]);
        let frame = egui::Frame::NONE;
        assert_eq!(frame.fill, egui::Color32::TRANSPARENT);
        assert_eq!(frame.stroke, egui::Stroke::NONE);
    }

    #[test]
    fn creative_search_tab_uses_the_exact_runtime_localized_name() {
        let mut overlay = InventoryOverlay::default();
        overlay.install_translations(std::collections::BTreeMap::from([(
            "itemGroup.search".to_owned(),
            "Search Items".to_owned(),
        )]));
        let tab = overlay
            .creative_tabs
            .iter()
            .find(|tab| tab.metadata.id.as_str() == "minecraft:search")
            .unwrap();
        assert_eq!(overlay.translate(&tab.metadata.title_key), "Search Items");
    }

    #[test]
    fn selected_search_tab_paints_its_title_inside_the_panel_with_the_editor() {
        let context = egui::Context::default();
        let mut overlay = InventoryOverlay::default();
        overlay.install_translations(std::collections::BTreeMap::from([(
            "itemGroup.search".to_owned(),
            "Search Items".to_owned(),
        )]));
        overlay.set_game_mode(GameMode::Creative);
        overlay.toggle_player_inventory();
        overlay.creative.tab = "minecraft:search".to_owned();
        assert!(overlay.creative_active());
        assert!(
            overlay.creative_tabs.iter().any(
                |tab| tab.metadata.id.as_str() == "minecraft:search" && tab.metadata.show_title
            )
        );
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0));
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 720.0),
            )),
            ..egui::RawInput::default()
        };
        input.viewports.insert(
            egui::ViewportId::ROOT,
            egui::ViewportInfo {
                native_pixels_per_point: Some(1.0),
                inner_rect: Some(screen),
                ..egui::ViewportInfo::default()
            },
        );
        let search = overlay
            .creative_tabs
            .iter()
            .find(|tab| tab.metadata.id.as_str() == "minecraft:search")
            .unwrap()
            .clone();
        let metrics = MinecraftGuiMetrics::new(1280, 720, 1.0);
        let size = metrics.size(195, 136);
        let origin = egui::pos2(1280.0 * 0.5 - size.x * 0.5, 720.0 * 0.5 - size.y * 0.5);
        let panel = egui::Rect::from_min_size(origin, size);
        let mut output = context.run_ui(input, |root| {
            overlay.paint_creative_title(root.painter(), panel, &search, metrics);
        });
        let text = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some((text.pos, text.galley.text())),
                _ => None,
            })
            .collect::<Vec<_>>();
        output.textures_delta.clear();
        let expected = origin + metrics.offset(8, 6);
        assert!(
            text.iter().any(|(position, value)| {
                *value == "Search Items" && position.distance(expected) < 0.01
            }),
            "painted text {text:?}, expected title at {expected:?}"
        );
        assert_eq!(CREATIVE_SEARCH_BOUNDS, [82, 6, 80, 9]);
    }

    #[test]
    fn creative_search_focus_follows_tab_selection_and_survives_slot_focus_loss() {
        let context = egui::Context::default();
        let mut overlay = InventoryOverlay::default();
        overlay.install_translations(std::collections::BTreeMap::from([(
            "item.minecraft.copper_chain".to_owned(),
            "Copper Chain".to_owned(),
        )]));
        overlay.set_game_mode(GameMode::Creative);
        overlay.toggle_player_inventory();
        let editor = egui::Id::new("cubic-creative-search-editor");
        let run = |overlay: &mut InventoryOverlay, events: Vec<egui::Event>| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 720.0),
                )),
                events,
                ..egui::RawInput::default()
            };
            let mut output = context.run_ui(input, |_root| {
                overlay.show(&context);
            });
            output.textures_delta.clear();
        };

        overlay.creative.select_tab("minecraft:search".to_owned());
        run(&mut overlay, Vec::new());
        assert_eq!(context.memory(|memory| memory.focused()), Some(editor));
        run(
            &mut overlay,
            vec![egui::Event::Text("copper chain".to_owned())],
        );
        assert_eq!(overlay.creative.search, "copper chain");
        assert!(
            !overlay
                .creative_items("minecraft:search", "copper chain")
                .is_empty()
        );
        run(&mut overlay, vec![egui::Event::Text("e".to_owned())]);
        assert!(overlay.visible());
        assert_eq!(overlay.creative.search, "copper chaine");

        // A slot interaction can temporarily move keyboard focus; the Search
        // editor reacquires it on the next presentation frame.
        context.memory_mut(|memory| memory.surrender_focus(editor));
        run(&mut overlay, Vec::new());
        assert_eq!(context.memory(|memory| memory.focused()), Some(editor));
        overlay
            .creative
            .select_tab("minecraft:building_blocks".to_owned());
        run(&mut overlay, Vec::new());
        assert!(overlay.creative.search.is_empty());
        assert_ne!(context.memory(|memory| memory.focused()), Some(editor));
        overlay.creative.select_tab("minecraft:search".to_owned());
        run(&mut overlay, Vec::new());
        assert_eq!(context.memory(|memory| memory.focused()), Some(editor));
        assert!(overlay.creative.search.is_empty());
    }

    #[test]
    fn hovering_the_search_tab_uses_the_visible_creative_tab_tooltip_path() {
        let context = egui::Context::default();
        let mut overlay = InventoryOverlay::default();
        overlay.install_translations(std::collections::BTreeMap::from([(
            "itemGroup.search".to_owned(),
            "Search Items".to_owned(),
        )]));
        let metrics = MinecraftGuiMetrics::new(1280, 720, 1.0);
        let size = metrics.size(195, 136);
        let origin = egui::pos2(1280.0 * 0.5 - size.x * 0.5, 720.0 * 0.5 - size.y * 0.5);
        let search = overlay
            .creative_tabs
            .iter()
            .find(|tab| tab.metadata.id.as_str() == "minecraft:search")
            .unwrap();
        let pointer = origin
            + egui::vec2(
                f32::from(creative_tab_x(&search.metadata) + 2) * metrics.points_per_gui,
                -30.0 * metrics.points_per_gui,
            );
        context.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 720.0),
            )),
            ..egui::RawInput::default()
        });
        egui::Area::new(egui::Id::new("creative-tab-tooltip-test"))
            .fixed_pos(origin)
            .show(&context, |ui| {
                let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                let _ = overlay.paint_creative_tabs(
                    ui,
                    rect,
                    metrics,
                    "minecraft:building_blocks",
                    &mut String::new(),
                    CreativePaintLayer::UnselectedTabs,
                );
            });
        let mut warmup = context.end_pass();
        warmup.textures_delta.clear();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 720.0),
            )),
            events: vec![egui::Event::PointerMoved(pointer)],
            ..egui::RawInput::default()
        };
        let mut hovered = None;
        context.begin_pass(input);
        {
            egui::Area::new(egui::Id::new("creative-tab-tooltip-test"))
                .fixed_pos(origin)
                .show(&context, |ui| {
                    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                    let (_, title) = overlay.paint_creative_tabs(
                        ui,
                        rect,
                        metrics,
                        "minecraft:building_blocks",
                        &mut String::new(),
                        CreativePaintLayer::UnselectedTabs,
                    );
                    hovered = title;
                });
        }
        let mut output = context.end_pass();
        // This headless interaction test has no GPU renderer to consume the
        // font-atlas update. Production applies every delta in its render path.
        output.textures_delta.clear();
        assert_eq!(hovered.as_deref(), Some("Search Items"));
    }

    #[test]
    fn creative_tab_paint_order_places_panel_between_inactive_and_selected_tabs() {
        assert_eq!(
            creative_paint_order(),
            [
                CreativePaintLayer::UnselectedTabs,
                CreativePaintLayer::MainPanel,
                CreativePaintLayer::SelectedTab,
            ]
        );
    }

    #[test]
    fn creative_category_hotbar_quick_move_clears_without_using_hidden_main_inventory() {
        let mut state = InventoryState::new();
        state
            .set_creative_slot(SlotIndex(36), Some(test_stack("minecraft:stone", 12)))
            .unwrap();
        let mut changes = Vec::new();
        clear_creative_category_hotbar_slot(&state, SlotIndex(36), &mut changes);
        assert_eq!(changes, [(SlotIndex(36), None)]);

        let mut wrapped_carried = None;
        let mut wrapped_changes = Vec::new();
        apply_creative_real_slot(
            &state,
            &mut wrapped_carried,
            SlotIndex(36),
            0,
            ContainerClickKind::QuickMove,
            &mut wrapped_changes,
        );
        assert!(
            wrapped_changes
                .iter()
                .any(|(slot, stack)| (9..=35).contains(&slot.0) && stack.is_some()),
            "the Inventory tab retains wrapped InventoryMenu routing"
        );
    }

    #[test]
    fn creative_scroll_is_one_row_at_bounds_over_items_or_background() {
        let maximum = 12;
        assert_eq!(creative_scroll_row(0, maximum, -1), 1);
        assert_eq!(creative_scroll_row(1, maximum, 1), 0);
        assert_eq!(creative_scroll_row(6, maximum, -3), 9);
        assert_eq!(creative_scroll_row(maximum, maximum, -1), maximum);
        assert_eq!(creative_scroll_row(0, maximum, 1), 0);
        assert_eq!(creative_scroll_row(6, maximum, 0), 6);
    }

    #[test]
    fn creative_wheel_accumulates_logical_steps_and_resets_fraction_on_reversal() {
        let mut accumulator = CreativeScrollAccumulator::default();
        assert_eq!(accumulator.push(0.0, -0.25), [0, 0]);
        assert_eq!(accumulator.push(0.0, -0.75), [0, -1]);
        assert_eq!(accumulator.push(0.0, 0.75), [0, 0]);
        assert_eq!(accumulator.push(0.0, -0.5), [0, 0]);
        assert_eq!(accumulator.push(0.0, -0.5), [0, -1]);

        // A conventional line-wheel notch is already one logical step; the
        // 120-unit Windows pixel quantum is normalized before this boundary.
        assert_eq!(accumulator.push(0.0, 1.0), [0, 1]);

        assert_eq!(
            normalize_creative_wheel(egui::MouseWheelUnit::Line, egui::vec2(0.0, -1.0), 2.0,),
            egui::vec2(0.0, -1.0)
        );
        assert_eq!(
            normalize_creative_wheel(egui::MouseWheelUnit::Point, egui::vec2(0.0, -60.0), 2.0,),
            egui::vec2(0.0, -1.0)
        );
    }

    #[test]
    fn creative_wrapped_equipment_slots_delegate_exact_no_item_sprites() {
        assert_eq!(
            empty_player_slot_sprite(SlotIndex(5)),
            Some("gui/sprites/container/slot/helmet")
        );
        assert_eq!(
            empty_player_slot_sprite(SlotIndex(6)),
            Some("gui/sprites/container/slot/chestplate")
        );
        assert_eq!(
            empty_player_slot_sprite(SlotIndex(7)),
            Some("gui/sprites/container/slot/leggings")
        );
        assert_eq!(
            empty_player_slot_sprite(SlotIndex(8)),
            Some("gui/sprites/container/slot/boots")
        );
        assert_eq!(
            empty_player_slot_sprite(SlotIndex(45)),
            Some("gui/sprites/container/slot/shield")
        );
        assert_eq!(empty_player_slot_sprite(SlotIndex(36)), None);
    }

    #[test]
    fn creative_tab_clicks_never_fall_through_to_outside_drop() {
        for tab in ["top", "bottom", "search", "inventory"] {
            assert!(
                !creative_outside_drop_allowed(true, false, true),
                "{tab} tab click must be consumed before outside-drop handling"
            );
        }
        assert!(creative_outside_drop_allowed(true, false, false));
        assert!(!creative_outside_drop_allowed(true, true, false));
    }

    #[test]
    fn normal_item_tooltips_use_runtime_names_lore_and_optional_creative_category() {
        let mut stack = test_stack("minecraft:diamond_sword", 1);
        stack.components.added.insert(
            cubic_version_identifier("minecraft:custom_name"),
            ComponentValue::RichText {
                plain: "A Fine Sword".to_owned(),
                wire: vec![8, 0, 0],
            },
        );
        stack.components.added.insert(
            cubic_version_identifier("minecraft:lore"),
            ComponentValue::Lore {
                lines: vec!["First line".to_owned(), "Second line".to_owned()],
                wire: vec![2],
            },
        );
        let translations = std::collections::BTreeMap::from([(
            "item.minecraft.diamond_sword".to_owned(),
            "Diamond Sword".to_owned(),
        )]);
        let tooltip = slot_item_tooltip(
            Some(&stack),
            &translations,
            ItemTooltipMode::Normal,
            Some("Combat"),
        )
        .unwrap();
        assert_eq!(tooltip.name, "A Fine Sword");
        assert_eq!(tooltip.lines, ["First line", "Second line", "Combat"]);
        assert!(slot_item_tooltip(None, &translations, ItemTooltipMode::Normal, None).is_none());

        stack.components.added.clear();
        let advanced = item_tooltip(&stack, &translations, ItemTooltipMode::Advanced, None);
        assert_eq!(advanced.name, "Diamond Sword");
        assert_eq!(advanced.lines[0], "minecraft:diamond_sword");

        let creative = cubic_version::CreativeData::builtin_26_1_2().unwrap();
        let ominous = creative
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
            .and_then(|stack| creative_stack(stack, &creative.banner_patterns))
            .unwrap();
        let banner_names = std::collections::BTreeMap::from([
            (
                "block.minecraft.white_banner".to_owned(),
                "White Banner".to_owned(),
            ),
            (
                "block.minecraft.ominous_banner".to_owned(),
                "Ominous Banner".to_owned(),
            ),
        ]);
        assert_eq!(
            item_tooltip(&ominous, &banner_names, ItemTooltipMode::Normal, None).name,
            "Ominous Banner"
        );
        assert_eq!(
            item_tooltip(
                &test_stack("minecraft:white_banner", 1),
                &banner_names,
                ItemTooltipMode::Normal,
                None,
            )
            .name,
            "White Banner"
        );
    }

    #[test]
    fn creative_full_text_search_uses_normal_display_tooltip_text() {
        let translations = std::collections::BTreeMap::from([
            (
                "item.minecraft.copper_chain".to_owned(),
                "Copper Chain".to_owned(),
            ),
            ("item.minecraft.stone".to_owned(), "Stone".to_owned()),
        ]);
        let copper_chain = test_stack("minecraft:copper_chain", 1);
        let copper_text = creative_name_search_text(&copper_chain, &translations);
        for query in ["copper chain", "copper", "chain", "CoPpEr ChAiN"] {
            assert!(creative_name_search_matches(
                &copper_chain,
                &copper_text,
                &query.to_lowercase()
            ));
        }
        assert!(creative_name_search_matches(
            &copper_chain,
            &copper_text,
            "minecraft:copper_chain"
        ));
        assert!(creative_name_search_matches(
            &copper_chain,
            &copper_text,
            "mine:chain"
        ));
        assert!(!creative_name_search_matches(
            &copper_chain,
            &copper_text,
            "minecraft:stone"
        ));

        let mut renamed = test_stack("minecraft:stone", 1);
        renamed.components.added.insert(
            cubic_version_identifier("minecraft:custom_name"),
            ComponentValue::RichText {
                plain: "Polished Display Sample".to_owned(),
                wire: vec![8, 0, 0],
            },
        );
        let renamed_text = creative_name_search_text(&renamed, &translations);
        assert!(creative_name_search_matches(
            &renamed,
            &renamed_text,
            "display sample"
        ));
    }

    #[test]
    fn preexisting_carried_stack_can_pick_up_all_component_exact_matches() {
        let mut state = InventoryState::new();
        state
            .set_slot(
                cubic_world::PLAYER_CONTAINER_ID,
                1,
                SlotIndex(9),
                Some(test_stack("minecraft:stone", 10)),
            )
            .unwrap();
        state
            .set_slot(
                cubic_world::PLAYER_CONTAINER_ID,
                1,
                SlotIndex(10),
                Some(test_stack("minecraft:stone", 20)),
            )
            .unwrap();
        let mut carried = Some(test_stack("minecraft:stone", 5));
        let mut changes = Vec::new();
        apply_creative_real_slot(
            &state,
            &mut carried,
            SlotIndex(9),
            0,
            ContainerClickKind::PickupAll,
            &mut changes,
        );
        assert_eq!(carried.as_ref().map(|stack| stack.count), Some(35));
        assert_eq!(changes.len(), 2);
    }

    #[test]
    fn inventory_overlay_tracks_authoritative_hotbar_and_visibility() {
        let mut overlay = InventoryOverlay::default();
        let mut state = InventoryState::new();
        state.set_selected_hotbar_slot(6).unwrap();
        overlay.replace(state);
        assert_eq!(overlay.state.selected_hotbar_slot(), 6);
        assert!(!overlay.visible());
        overlay.toggle_player_inventory();
        assert!(overlay.visible());
        assert_eq!(overlay.close(), None);
        assert!(!overlay.visible());
    }

    #[test]
    fn local_click_prediction_updates_slot_and_cursor_without_server_round_trip() {
        let mut overlay = InventoryOverlay::default();
        let stack = cubic_world::ItemStack::new(
            cubic_version_identifier("minecraft:stone"),
            9,
            cubic_world::ComponentPatch::default(),
        )
        .unwrap();
        overlay
            .state
            .set_slot(
                cubic_world::PLAYER_CONTAINER_ID,
                4,
                SlotIndex(9),
                Some(stack),
            )
            .unwrap();

        overlay.apply_local_clicks(&[InventoryAction::Click(ContainerClick {
            container: cubic_world::PLAYER_CONTAINER_ID,
            state_id: 4,
            slot: SlotIndex(9),
            button: 0,
            kind: ContainerClickKind::Pickup,
        })]);
        assert!(overlay.state.player().slots[9].is_none());
        assert_eq!(overlay.state.carried().map(|stack| stack.count), Some(9));

        overlay.apply_local_clicks(&[InventoryAction::Click(ContainerClick {
            container: cubic_world::PLAYER_CONTAINER_ID,
            state_id: 4,
            slot: SlotIndex(10),
            button: 1,
            kind: ContainerClickKind::Pickup,
        })]);
        assert_eq!(
            overlay.state.player().slots[10]
                .as_ref()
                .map(|stack| stack.count),
            Some(1)
        );
        assert_eq!(overlay.state.carried().map(|stack| stack.count), Some(8));
    }

    #[test]
    fn local_right_click_split_and_quick_craft_preview_share_menu_semantics() {
        let mut overlay = InventoryOverlay::default();
        let stack = cubic_world::ItemStack::new(
            cubic_version_identifier("minecraft:stone"),
            9,
            cubic_world::ComponentPatch::default(),
        )
        .unwrap();
        overlay
            .state
            .set_slot(
                cubic_world::PLAYER_CONTAINER_ID,
                7,
                SlotIndex(9),
                Some(stack),
            )
            .unwrap();
        overlay.apply_local_clicks(&[InventoryAction::Click(ContainerClick {
            container: cubic_world::PLAYER_CONTAINER_ID,
            state_id: 7,
            slot: SlotIndex(9),
            button: 1,
            kind: ContainerClickKind::Pickup,
        })]);
        assert_eq!(
            overlay.state.player().slots[9]
                .as_ref()
                .map(|stack| stack.count),
            Some(4)
        );
        assert_eq!(overlay.state.carried().map(|stack| stack.count), Some(5));

        overlay.quick_craft = Some(QuickCraftUiGesture {
            container: cubic_world::PLAYER_CONTAINER_ID,
            state_id: 7,
            kind: 0,
            visited: BTreeSet::from([SlotIndex(10), SlotIndex(11)]),
        });
        let preview = overlay.quick_craft_preview_state().unwrap();
        assert_eq!(
            preview.player().slots[10].as_ref().map(|stack| stack.count),
            Some(2)
        );
        assert_eq!(
            preview.player().slots[11].as_ref().map(|stack| stack.count),
            Some(2)
        );
        assert_eq!(preview.carried().map(|stack| stack.count), Some(1));
    }

    #[test]
    fn exact_supported_menu_layouts_preserve_vanilla_slot_coordinates() {
        let player = MenuLayout::for_container(InventoryState::new().player());
        assert_eq!(
            (player.kind, player.width, player.height),
            (MenuKind::Player, 176, 166)
        );
        assert_eq!(player.inventory_label, None);
        assert!(player.slots.contains(&SlotPlacement {
            slot: SlotIndex(0),
            x: 154,
            y: 28
        }));
        assert!(player.slots.contains(&SlotPlacement {
            slot: SlotIndex(5),
            x: 8,
            y: 8
        }));
        assert!(player.slots.contains(&SlotPlacement {
            slot: SlotIndex(9),
            x: 8,
            y: 84
        }));
        assert!(player.slots.contains(&SlotPlacement {
            slot: SlotIndex(36),
            x: 8,
            y: 142
        }));
        assert!(player.slots.contains(&SlotPlacement {
            slot: SlotIndex(45),
            x: 77,
            y: 62
        }));

        let chest = ContainerState::new(
            ContainerId(2),
            MenuIdentity::Menu(cubic_version_identifier("minecraft:generic_9x6")),
            "Large Chest".to_owned(),
            90,
        )
        .unwrap();
        let chest = MenuLayout::for_container(&chest);
        assert_eq!(
            (chest.kind, chest.height, chest.slots.len()),
            (MenuKind::Chest, 222, 90)
        );
        assert!(chest.slots.contains(&SlotPlacement {
            slot: SlotIndex(53),
            x: 152,
            y: 108
        }));

        let shulker = ContainerState::new(
            ContainerId(5),
            MenuIdentity::Menu(cubic_version_identifier("minecraft:shulker_box")),
            "Shulker Box".to_owned(),
            63,
        )
        .unwrap();
        let shulker = MenuLayout::for_container(&shulker);
        assert_eq!(
            (
                shulker.kind,
                shulker.texture,
                shulker.height,
                shulker.slots.len()
            ),
            (MenuKind::Chest, "gui/container/generic_54", 168, 63)
        );
        assert_eq!(shulker.inventory_label, Some([8, 74]));
        assert!(shulker.slots.contains(&SlotPlacement {
            slot: SlotIndex(26),
            x: 152,
            y: 54,
        }));
        assert!(shulker.slots.contains(&SlotPlacement {
            slot: SlotIndex(27),
            x: 8,
            y: 85,
        }));
        assert!(chest.slots.contains(&SlotPlacement {
            slot: SlotIndex(54),
            x: 8,
            y: 139
        }));
        assert!(chest.slots.contains(&SlotPlacement {
            slot: SlotIndex(81),
            x: 8,
            y: 197
        }));

        let furnace = ContainerState::new(
            ContainerId(3),
            MenuIdentity::Menu(cubic_version_identifier("minecraft:furnace")),
            "Furnace".to_owned(),
            39,
        )
        .unwrap();
        let furnace = MenuLayout::for_container(&furnace);
        assert!(furnace.slots.contains(&SlotPlacement {
            slot: SlotIndex(0),
            x: 56,
            y: 17
        }));
        assert!(furnace.slots.contains(&SlotPlacement {
            slot: SlotIndex(2),
            x: 116,
            y: 35
        }));
        assert!(furnace.slots.contains(&SlotPlacement {
            slot: SlotIndex(3),
            x: 8,
            y: 84
        }));

        let crafting = ContainerState::new(
            ContainerId(4),
            MenuIdentity::Menu(cubic_version_identifier("minecraft:crafting")),
            "Crafting".to_owned(),
            46,
        )
        .unwrap();
        let crafting = MenuLayout::for_container(&crafting);
        assert!(crafting.slots.contains(&SlotPlacement {
            slot: SlotIndex(0),
            x: 124,
            y: 35
        }));
        assert!(crafting.slots.contains(&SlotPlacement {
            slot: SlotIndex(1),
            x: 30,
            y: 17
        }));
        assert!(crafting.slots.contains(&SlotPlacement {
            slot: SlotIndex(10),
            x: 8,
            y: 84
        }));
    }

    #[test]
    fn gui_scale_quick_craft_filter_and_furnace_progress_are_deterministic() {
        let metrics = MinecraftGuiMetrics::new(1280, 720, 1.0);
        assert_eq!(
            (
                metrics.points_per_gui,
                metrics.gui_width,
                metrics.gui_height,
                metrics.gui_scale,
            ),
            (3.0, 427, 240, 3)
        );
        assert_eq!(MinecraftGuiMetrics::new(640, 480, 1.0).gui_scale, 2);
        assert_eq!(MinecraftGuiMetrics::new(1920, 1080, 1.0).gui_scale, 4);
        assert_eq!(MinecraftGuiMetrics::new(7680, 4320, 1.0).gui_scale, 4);
        let mut player = InventoryState::new().player().clone();
        let carried = cubic_world::ItemStack::new(
            cubic_version_identifier("minecraft:stone"),
            8,
            cubic_world::ComponentPatch::default(),
        )
        .unwrap();
        assert!(!quick_craft_slot_eligible(
            &player,
            SlotIndex(0),
            None,
            &carried
        ));
        assert!(quick_craft_slot_eligible(
            &player,
            SlotIndex(9),
            None,
            &carried
        ));
        player.slots[9] = Some(
            cubic_world::ItemStack::new(
                cubic_version_identifier("minecraft:dirt"),
                1,
                cubic_world::ComponentPatch::default(),
            )
            .unwrap(),
        );
        assert!(!quick_craft_slot_eligible(
            &player,
            SlotIndex(9),
            player.slots[9].as_ref(),
            &carried,
        ));

        let properties = std::collections::BTreeMap::from([(0, 100), (1, 200), (2, 50), (3, 200)]);
        assert_eq!(furnace_lit_height(&properties), 8);
        assert_eq!(furnace_progress_width(&properties), 6);
    }

    #[test]
    fn minecraft_ascii_metrics_and_inventory_localization_are_resource_driven() {
        let mut rgba = vec![0_u8; 128 * 128 * 4];
        let code = usize::from(b'A');
        let cell_x = (code % 16) * 8;
        let cell_y = (code / 16) * 8;
        for y in 0..8 {
            for x in 0..3 {
                rgba[((cell_y + y) * 128 + cell_x + x) * 4 + 3] = 255;
            }
        }
        let widths = bitmap_glyph_widths(&rgba);
        assert_eq!(widths[code], 3);
        assert_eq!(widths[usize::from(b' ')], 3);
        assert_eq!(minecraft_text_width(Some(&widths), "A A"), 12.0);

        let mut overlay = InventoryOverlay::default();
        overlay.install_translations(std::collections::BTreeMap::from([
            ("container.inventory".to_owned(), "Inventory".to_owned()),
            ("container.furnace".to_owned(), "Furnace".to_owned()),
        ]));
        assert_eq!(overlay.translate("container.inventory"), "Inventory");
        assert_eq!(overlay.translate("container.furnace"), "Furnace");
        assert_eq!(overlay.translate("Server title"), "Server title");
    }

    #[test]
    fn authoritative_inventory_change_cancels_an_in_progress_pointer_gesture() {
        let mut overlay = InventoryOverlay {
            visible: true,
            quick_craft: Some(QuickCraftUiGesture {
                container: cubic_world::PLAYER_CONTAINER_ID,
                state_id: 0,
                kind: 0,
                visited: BTreeSet::from([SlotIndex(9)]),
            }),
            ..InventoryOverlay::default()
        };
        let mut state = InventoryState::new();
        state
            .replace_content(cubic_world::PLAYER_CONTAINER_ID, 1, vec![None; 46], None)
            .unwrap();
        overlay.replace(state);
        assert!(overlay.quick_craft.is_none());
    }

    #[test]
    fn durability_decoration_uses_the_exposed_stack_components_and_vanilla_width() {
        let mut components = cubic_world::ComponentPatch::default();
        components.added.insert(
            cubic_version_identifier("minecraft:max_damage"),
            cubic_world::ComponentValue::VarInt(100),
        );
        components.added.insert(
            cubic_version_identifier("minecraft:damage"),
            cubic_world::ComponentValue::VarInt(50),
        );
        let stack = cubic_world::ItemStack::new(
            cubic_version_identifier("minecraft:diamond_sword"),
            1,
            components,
        )
        .unwrap();
        assert_eq!(durability_bar(&stack), Some((7, egui::Color32::YELLOW)));
    }

    #[test]
    fn item_icon_atlas_is_guttered_deterministic_and_nearest_sampled_as_one_texture() {
        let context = egui::Context::default();
        let mut overlay = InventoryOverlay::default();
        overlay.install_icon_atlas(
            &context,
            1,
            [
                ("minecraft:zeta".to_owned(), 16, 16, vec![0xff; 16 * 16 * 4]),
                (
                    "minecraft:alpha".to_owned(),
                    16,
                    16,
                    vec![0x80; 16 * 16 * 4],
                ),
            ],
        );
        let atlas = overlay.item_atlases.get(&1).unwrap();
        assert_eq!(atlas.handle.size(), [36, 18]);
        assert_eq!(atlas.regions.len(), 2);
        let alpha = atlas.regions["minecraft:alpha"];
        let zeta = atlas.regions["minecraft:zeta"];
        assert!(
            alpha.min.x < zeta.min.x,
            "BTree ordering fixes atlas placement"
        );
        assert_eq!(alpha.min, egui::pos2(1.0 / 36.0, 1.0 / 18.0));
        assert_eq!(alpha.max, egui::pos2(17.0 / 36.0, 17.0 / 18.0));
        overlay.install_icon_atlas(
            &context,
            3,
            [(
                "minecraft:physical".to_owned(),
                48,
                48,
                vec![0xff; 48 * 48 * 4],
            )],
        );
        let physical = overlay.item_atlases.get(&3).unwrap();
        assert_eq!(physical.handle.size(), [50, 50]);
        assert_eq!(
            physical.regions["minecraft:physical"],
            egui::Rect::from_min_max(
                egui::pos2(1.0 / 50.0, 1.0 / 50.0),
                egui::pos2(49.0 / 50.0, 49.0 / 50.0)
            )
        );
        assert!(!overlay.creative_tabs.is_empty());
    }

    #[test]
    fn creative_mode_selects_creative_screen_and_catalog_search_is_generated() {
        let context = egui::Context::default();
        let mut overlay = InventoryOverlay {
            visible: true,
            ..InventoryOverlay::default()
        };
        assert!(!overlay.creative_active());
        overlay.set_game_mode(GameMode::Creative);
        assert!(overlay.creative_active());
        overlay.install_icon_atlas(
            &context,
            1,
            [
                (
                    "minecraft:stone".to_owned(),
                    16,
                    16,
                    vec![0xff; 16 * 16 * 4],
                ),
                (
                    "minecraft:golden_sword".to_owned(),
                    16,
                    16,
                    vec![0xff; 16 * 16 * 4],
                ),
                (
                    "minecraft:pig_spawn_egg".to_owned(),
                    16,
                    16,
                    vec![0xff; 16 * 16 * 4],
                ),
            ],
        );
        assert!(
            overlay
                .creative_items("minecraft:search", "sword")
                .iter()
                .all(|stack| stack.item.as_str().contains("sword"))
        );
        assert_eq!(overlay.creative_items("minecraft:spawn_eggs", "").len(), 88);
        overlay.set_game_mode(GameMode::Survival);
        assert!(!overlay.creative_active());
    }

    #[test]
    fn creative_tabs_and_first_page_are_driven_by_exact_version_data() {
        let overlay = InventoryOverlay::default();
        assert_eq!(overlay.creative_tabs.len(), 13);
        let source = cubic_version::CreativeData::builtin_26_1_2().unwrap();
        for rendered in &overlay.creative_tabs {
            let canonical = source
                .tabs(false)
                .iter()
                .find(|tab| tab.id == rendered.metadata.id)
                .unwrap();
            assert_eq!(rendered.items.len(), canonical.items.len());
            assert!(
                rendered
                    .items
                    .iter()
                    .zip(&canonical.items)
                    .all(|(renderer_input, canonical)| renderer_input.item == canonical.item)
            );
        }
        let positions = overlay
            .creative_tabs
            .iter()
            .map(|tab| (tab.metadata.id.as_str(), creative_tab_x(&tab.metadata)))
            .collect::<Vec<_>>();
        assert_eq!(
            positions,
            [
                ("minecraft:building_blocks", 0),
                ("minecraft:colored_blocks", 27),
                ("minecraft:natural_blocks", 54),
                ("minecraft:functional_blocks", 81),
                ("minecraft:redstone_blocks", 108),
                ("minecraft:hotbar", 142),
                ("minecraft:search", 169),
                ("minecraft:tools_and_utilities", 0),
                ("minecraft:combat", 27),
                ("minecraft:food_and_drinks", 54),
                ("minecraft:ingredients", 81),
                ("minecraft:spawn_eggs", 108),
                ("minecraft:inventory", 169),
            ]
        );
        let building = overlay.creative_items("minecraft:building_blocks", "");
        assert_eq!(building.len(), 436);
        assert_eq!(
            building
                .iter()
                .take(20)
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
                "minecraft:spruce_log",
                "minecraft:spruce_wood",
                "minecraft:stripped_spruce_log",
                "minecraft:stripped_spruce_wood",
                "minecraft:spruce_planks",
                "minecraft:spruce_stairs",
                "minecraft:spruce_slab",
            ]
        );
        assert!(
            building
                .iter()
                .all(|stack| stack.effective_item_model_owned().is_some())
        );
    }

    #[test]
    fn creative_version_stack_preserves_component_variants_for_shared_renderer_and_wire_codec() {
        let data = cubic_version::CreativeData::builtin_26_1_2().unwrap();
        let potion = data
            .tabs(false)
            .iter()
            .flat_map(|tab| &tab.items)
            .find(|stack| stack.item.as_str() == "minecraft:potion" && !stack.components.is_empty())
            .unwrap();
        let stack = creative_stack(potion, &data.banner_patterns).unwrap();
        assert_eq!(stack.item, potion.item);
        assert_eq!(
            stack.effective_item_model_owned().unwrap(),
            potion.effective_item_model
        );
        assert!(!stack.components.added.is_empty());
        let ominous = data
            .tabs(false)
            .iter()
            .flat_map(|tab| &tab.items)
            .find(|stack| {
                stack.item.as_str() == "minecraft:white_banner"
                    && stack
                        .components
                        .iter()
                        .any(|component| component.id.as_str() == "minecraft:banner_patterns")
            })
            .unwrap();
        let ominous = creative_stack(ominous, &data.banner_patterns).unwrap();
        let patterns = cubic_version_identifier("minecraft:banner_patterns");
        assert!(matches!(
            ominous.components.added.get(&patterns),
            Some(cubic_world::ComponentValue::RegistryEncoded { wire, source_registry })
                if !wire.is_empty() && source_registry == &data.banner_patterns
        ));
        assert!(
            ominous
                .gui_render_key()
                .unwrap()
                .contains("|banner_patterns=")
        );
    }

    #[test]
    fn creative_slot_update_is_immediate_and_uses_the_shared_stack_identity() {
        let mut overlay = InventoryOverlay::default();
        let stack = ItemStack::new(
            cubic_version_identifier("minecraft:stone"),
            64,
            ComponentPatch::default(),
        )
        .unwrap();
        overlay
            .state
            .set_creative_slot(SlotIndex(36), Some(stack.clone()))
            .unwrap();
        assert_eq!(overlay.state.player().slots[36], Some(stack));
    }

    #[test]
    fn creative_quick_craft_distributes_deterministically_without_container_clicks() {
        let stack = ItemStack::new(
            cubic_version_identifier("minecraft:stone"),
            8,
            ComponentPatch::default(),
        )
        .unwrap();
        let state = InventoryState::new();
        let gesture = CreativeQuickCraftGesture {
            button: egui::PointerButton::Primary,
            visited: BTreeSet::from([SlotIndex(10), SlotIndex(9)]),
        };
        let mut carried = Some(stack.clone());
        let mut changes = Vec::new();
        apply_creative_quick_craft(&state, &gesture, &mut carried, &mut changes);
        assert_eq!(
            changes,
            vec![
                (
                    SlotIndex(9),
                    Some(ItemStack {
                        count: 4,
                        ..stack.clone()
                    })
                ),
                (
                    SlotIndex(10),
                    Some(ItemStack {
                        count: 4,
                        ..stack.clone()
                    })
                ),
            ]
        );
        assert_eq!(carried, None);

        let middle = CreativeQuickCraftGesture {
            button: egui::PointerButton::Middle,
            visited: BTreeSet::from([SlotIndex(36), SlotIndex(37)]),
        };
        let mut carried = Some(ItemStack { count: 1, ..stack });
        let mut changes = Vec::new();
        apply_creative_quick_craft(&state, &middle, &mut carried, &mut changes);
        assert_eq!(changes.len(), 2);
        assert!(
            changes
                .iter()
                .all(|(_, stack)| { stack.as_ref().is_some_and(|stack| stack.count == 64) })
        );
        assert_eq!(carried.as_ref().map(|stack| stack.count), Some(1));
    }

    fn cubic_version_identifier(value: &str) -> cubic_version::MinecraftIdentifier {
        cubic_version::MinecraftIdentifier::new(value).unwrap()
    }

    #[test]
    fn history_evicts_oldest_messages_deterministically() {
        let mut model = ChatModel::default();
        for index in 0..=MAX_HISTORY_MESSAGES {
            model.apply(message(&index.to_string()));
        }
        assert_eq!(model.history().len(), MAX_HISTORY_MESSAGES);
        assert_eq!(
            model.history().front().map(|entry| entry.text.as_str()),
            Some("1")
        );
    }

    #[test]
    fn input_is_bounded_and_control_characters_are_removed() {
        let mut model = ChatModel::default();
        model.set_input(format!("a\n{}", "😀".repeat(200)));
        assert!(!model.input().contains('\n'));
        assert!(model.input().encode_utf16().count() <= MAX_INPUT_UTF16_UNITS);
    }

    #[test]
    fn send_action_rejects_empty_and_clears_nonempty_input() {
        let mut model = ChatModel::default();
        model.set_input("   ".to_owned());
        assert_eq!(model.take_message_to_send(), None);
        model.set_input(" hello ".to_owned());
        assert_eq!(model.take_message_to_send().as_deref(), Some("hello"));
        assert!(model.input().is_empty());
    }

    #[test]
    fn connection_transitions_and_disconnect_reason_are_visible() {
        let mut model = ChatModel::default();
        model.apply(ChatEvent::Connected);
        assert_eq!(model.state(), ChatConnectionState::Connected);
        model.apply(ChatEvent::Disconnected {
            reason: "bye".to_owned(),
        });
        assert_eq!(model.state(), ChatConnectionState::Disconnected);
        assert_eq!(
            model.history().back().map(|entry| entry.text.as_str()),
            Some("Disconnected: bye")
        );
    }

    #[test]
    fn common_unicode_is_preserved_in_input_and_history() {
        const SAMPLE: &str = "£ € café Привет 😄 漢字";
        let mut model = ChatModel::default();
        model.set_input(SAMPLE.to_owned());
        assert_eq!(model.input(), SAMPLE);
        model.apply(message(SAMPLE));
        assert_eq!(
            model.history().back().map(|entry| entry.text.as_str()),
            Some(SAMPLE)
        );
    }

    #[test]
    fn presentation_transitions_preserve_chat_history_and_are_idempotent() {
        let mut model = ChatModel::default();
        model.apply(message("before transition"));
        let mut mode = PresentationModeController::new(SessionPresentationMode::Play);
        assert!(mode.enter_chat());
        assert!(!mode.enter_chat());
        assert_eq!(mode.mode(), SessionPresentationMode::Chat);
        assert!(mode.enter_play());
        assert!(!mode.enter_play());
        assert_eq!(
            model.history().front().map(|entry| entry.text.as_str()),
            Some("before transition")
        );
    }

    #[test]
    fn semantic_safety_alert_is_retained_as_an_obvious_notice() {
        let mut model = ChatModel::default();
        model.apply(ChatEvent::SafetyAlert {
            kind: cubic_core::SafetyAlertKind::LowHealth,
            message: "Dangerously low health: 4.0".to_owned(),
        });
        let alert = model.history().back().expect("alert must be visible");
        assert_eq!(alert.kind, ChatMessageKind::ServerNotice);
        assert_eq!(alert.text, "⚠ Dangerously low health: 4.0");
    }
}
