use std::collections::{BTreeMap, BTreeSet};

use cubic_version::MinecraftIdentifier;
use thiserror::Error;

pub const PLAYER_CONTAINER_ID: ContainerId = ContainerId(0);
pub const PLAYER_INVENTORY_SLOTS: usize = 46;
pub const MAX_CONTAINER_SLOTS: usize = 512;
pub const MAX_COMPONENTS_PER_STACK: usize = 256;
pub const MAX_COMPONENT_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ContainerId(pub u32);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SlotIndex(pub i16);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ComponentValue {
    Unit,
    VarInt(i32),
    Bool(bool),
    Text(String),
    RichText {
        plain: String,
        wire: Vec<u8>,
    },
    Lore {
        lines: Vec<String>,
        wire: Vec<u8>,
    },
    StringMap {
        values: BTreeMap<String, String>,
        wire: Vec<u8>,
    },
    /// Raw static-resource bytes plus the registry order in which those bytes
    /// were authored. A versioned protocol profile rebases referenced values
    /// to the live connection's registry when sending them.
    RegistryEncoded {
        wire: Vec<u8>,
        source_registry: Vec<MinecraftIdentifier>,
    },
    /// Canonical, bounded representation owned by the selected protocol profile.
    /// Generic inventory logic compares and retains it without interpreting it.
    Opaque(Vec<u8>),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ComponentPatch {
    pub added: BTreeMap<MinecraftIdentifier, ComponentValue>,
    pub removed: BTreeSet<MinecraftIdentifier>,
    /// Optional exact-version reconciliation fingerprints. Generic inventory
    /// semantics never interpret these values; the selected protocol profile
    /// supplies and consumes them when constructing click acknowledgements.
    pub fingerprints: BTreeMap<MinecraftIdentifier, i32>,
}

impl ComponentPatch {
    pub fn validate(&self) -> Result<(), InventoryError> {
        let count = self.added.len().saturating_add(self.removed.len());
        if count > MAX_COMPONENTS_PER_STACK {
            return Err(InventoryError::TooManyComponents {
                count,
                max: MAX_COMPONENTS_PER_STACK,
            });
        }
        let retained = self.added.values().try_fold(0_usize, |total, value| {
            total.checked_add(value.retained_bytes())
        });
        let retained = retained.ok_or(InventoryError::ComponentDataTooLarge {
            size: usize::MAX,
            max: MAX_COMPONENT_BYTES,
        })?;
        if retained > MAX_COMPONENT_BYTES {
            return Err(InventoryError::ComponentDataTooLarge {
                size: retained,
                max: MAX_COMPONENT_BYTES,
            });
        }
        if self.added.keys().any(|key| self.removed.contains(key)) {
            return Err(InventoryError::ConflictingComponentPatch);
        }
        if self
            .fingerprints
            .keys()
            .any(|component| !self.added.contains_key(component))
        {
            return Err(InventoryError::OrphanComponentFingerprint);
        }
        Ok(())
    }
}

impl ComponentValue {
    pub(crate) fn retained_bytes(&self) -> usize {
        match self {
            Self::Unit => 0,
            Self::VarInt(_) => size_of::<i32>(),
            Self::Bool(_) => size_of::<bool>(),
            Self::Text(value) => value.len(),
            Self::RichText { plain, wire } => plain.len().saturating_add(wire.len()),
            Self::Lore { lines, wire } => lines
                .iter()
                .map(String::len)
                .sum::<usize>()
                .saturating_add(wire.len()),
            Self::StringMap { values, wire } => values
                .iter()
                .map(|(key, value)| key.len().saturating_add(value.len()))
                .sum::<usize>()
                .saturating_add(wire.len()),
            Self::RegistryEncoded {
                wire,
                source_registry,
            } => wire.len().saturating_add(
                source_registry
                    .iter()
                    .map(|identifier| identifier.as_str().len())
                    .sum::<usize>(),
            ),
            Self::Opaque(value) => value.len(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ItemStack {
    pub item: MinecraftIdentifier,
    pub count: u32,
    pub components: ComponentPatch,
}

impl ItemStack {
    pub fn new(
        item: MinecraftIdentifier,
        count: u32,
        components: ComponentPatch,
    ) -> Result<Self, InventoryError> {
        if count == 0 {
            return Err(InventoryError::ZeroItemCount);
        }
        components.validate()?;
        Ok(Self {
            item,
            count,
            components,
        })
    }

    #[must_use]
    pub fn max_stack_size(&self) -> u32 {
        self.components
            .added
            .iter()
            .find(|(identifier, _)| identifier.as_str() == "minecraft:max_stack_size")
            .and_then(|(_, value)| match value {
                ComponentValue::VarInt(value) => u32::try_from(*value).ok(),
                _ => None,
            })
            .unwrap_or(64)
            .clamp(1, 99)
    }

    #[must_use]
    pub fn stackable_with(&self, other: &Self) -> bool {
        self.item == other.item && self.components == other.components
    }

    /// Resolves the effective current-version item-model identity.
    ///
    /// Network stacks carry a component *patch*, not the item's complete
    /// component map. Vanilla items default `minecraft:item_model` to their
    /// own registered identity; an added patch value replaces that default
    /// and an explicit removal suppresses it.
    #[must_use]
    pub fn effective_item_model_owned(&self) -> Option<MinecraftIdentifier> {
        const ITEM_MODEL: &str = "minecraft:item_model";
        if self
            .components
            .removed
            .iter()
            .any(|component| component.as_str() == ITEM_MODEL)
        {
            return None;
        }
        match self
            .components
            .added
            .iter()
            .find(|(component, _)| component.as_str() == ITEM_MODEL)
        {
            Some((_, ComponentValue::Text(value))) => MinecraftIdentifier::new(value.clone()).ok(),
            Some(_) => None,
            None => Some(self.item.clone()),
        }
    }

    /// Stable GUI cache identity for component state consumed by current item
    /// model graphs. Unrelated component bytes deliberately do not fragment
    /// the atlas.
    #[must_use]
    pub fn gui_render_key(&self) -> Option<String> {
        let model = self.effective_item_model_owned()?;
        let mut key = model.to_string();
        if let Some(ComponentValue::StringMap { values, .. }) = self
            .components
            .added
            .iter()
            .find(|(component, _)| component.as_str() == "minecraft:block_state")
            .map(|(_, value)| value)
        {
            key.push_str("|block_state");
            for (name, value) in values {
                key.push('|');
                key.push_str(name);
                key.push('=');
                key.push_str(value);
            }
        }
        if let Some(bytes) = self
            .components
            .added
            .iter()
            .find(|(component, _)| component.as_str() == "minecraft:banner_patterns")
            .and_then(|(_, value)| match value {
                ComponentValue::Opaque(bytes)
                | ComponentValue::RegistryEncoded { wire: bytes, .. } => Some(bytes),
                _ => None,
            })
            && bytes.len() <= 128
        {
            const HEX: &[u8; 16] = b"0123456789abcdef";
            key.push_str("|banner_patterns=");
            for byte in bytes {
                key.push(char::from(HEX[usize::from(byte >> 4)]));
                key.push(char::from(HEX[usize::from(byte & 0x0f)]));
            }
        }
        Some(key)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MenuIdentity {
    PlayerInventory,
    Menu(MinecraftIdentifier),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContainerState {
    pub id: ContainerId,
    pub menu: MenuIdentity,
    pub title: String,
    pub state_id: i32,
    pub slots: Vec<Option<ItemStack>>,
    pub carried: Option<ItemStack>,
    pub properties: BTreeMap<i16, i16>,
}

impl ContainerState {
    pub fn new(
        id: ContainerId,
        menu: MenuIdentity,
        title: String,
        slot_count: usize,
    ) -> Result<Self, InventoryError> {
        if slot_count > MAX_CONTAINER_SLOTS {
            return Err(InventoryError::TooManySlots {
                count: slot_count,
                max: MAX_CONTAINER_SLOTS,
            });
        }
        Ok(Self {
            id,
            menu,
            title,
            state_id: 0,
            slots: vec![None; slot_count],
            carried: None,
            properties: BTreeMap::new(),
        })
    }

    pub fn replace_content(
        &mut self,
        state_id: i32,
        slots: Vec<Option<ItemStack>>,
        carried: Option<ItemStack>,
    ) -> Result<(), InventoryError> {
        if slots.len() > MAX_CONTAINER_SLOTS {
            return Err(InventoryError::TooManySlots {
                count: slots.len(),
                max: MAX_CONTAINER_SLOTS,
            });
        }
        self.state_id = state_id;
        self.slots = slots;
        self.carried = carried;
        Ok(())
    }

    pub fn set_slot(
        &mut self,
        state_id: i32,
        slot: SlotIndex,
        stack: Option<ItemStack>,
    ) -> Result<(), InventoryError> {
        let index = usize::try_from(slot.0).map_err(|_| InventoryError::InvalidSlot(slot.0))?;
        let destination = self
            .slots
            .get_mut(index)
            .ok_or(InventoryError::InvalidSlot(slot.0))?;
        self.state_id = state_id;
        *destination = stack;
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InventoryState {
    player: ContainerState,
    open: Option<ContainerState>,
    selected_hotbar_slot: u8,
    quick_craft: Option<QuickCraftGesture>,
    instant_build: bool,
    permission_level: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct QuickCraftGesture {
    container: ContainerId,
    state_id: i32,
    kind: u8,
    slots: BTreeSet<SlotIndex>,
}

impl InventoryState {
    pub fn new() -> Self {
        Self {
            player: ContainerState {
                id: PLAYER_CONTAINER_ID,
                menu: MenuIdentity::PlayerInventory,
                title: "Inventory".to_owned(),
                state_id: 0,
                slots: vec![None; PLAYER_INVENTORY_SLOTS],
                carried: None,
                properties: BTreeMap::new(),
            },
            open: None,
            selected_hotbar_slot: 0,
            quick_craft: None,
            instant_build: false,
            permission_level: 0,
        }
    }

    #[must_use]
    pub const fn player(&self) -> &ContainerState {
        &self.player
    }

    #[must_use]
    pub const fn open(&self) -> Option<&ContainerState> {
        self.open.as_ref()
    }

    #[must_use]
    pub const fn selected_hotbar_slot(&self) -> u8 {
        self.selected_hotbar_slot
    }

    #[must_use]
    pub const fn can_use_game_master_blocks(&self) -> bool {
        self.instant_build && self.permission_level >= 2
    }

    pub fn set_instant_build(&mut self, instant_build: bool) {
        self.instant_build = instant_build;
    }

    pub fn set_permission_level(&mut self, permission_level: u8) {
        self.permission_level = permission_level.min(4);
    }

    #[must_use]
    pub fn held_item(&self) -> Option<&ItemStack> {
        self.player
            .slots
            .get(36 + usize::from(self.selected_hotbar_slot))
            .and_then(Option::as_ref)
    }

    #[must_use]
    pub fn offhand_item(&self) -> Option<&ItemStack> {
        self.player.slots.get(45).and_then(Option::as_ref)
    }

    pub fn set_selected_hotbar_slot(&mut self, slot: u8) -> Result<(), InventoryError> {
        if slot > 8 {
            return Err(InventoryError::InvalidHotbarSlot(slot));
        }
        self.selected_hotbar_slot = slot;
        Ok(())
    }

    /// Updates the presentation-side carried stack for Creative picker
    /// interactions. No protocol action is implied until a real inventory
    /// slot is changed or the carried stack is dropped.
    pub fn set_carried(&mut self, stack: Option<ItemStack>) {
        if let Some(open) = &mut self.open {
            open.carried = stack.clone();
        }
        self.player.carried = stack;
    }

    /// Applies a Creative slot assignment to the authoritative player-menu
    /// numbering used by Serverbound Set Creative Mode Slot.
    pub fn set_creative_slot(
        &mut self,
        slot: SlotIndex,
        stack: Option<ItemStack>,
    ) -> Result<(), InventoryError> {
        if slot.0 == -1 {
            return Ok(());
        }
        self.player.set_slot(self.player.state_id, slot, stack)
    }

    pub fn open_container(&mut self, container: ContainerState) -> Result<(), InventoryError> {
        if container.id == PLAYER_CONTAINER_ID {
            return Err(InventoryError::InvalidOpenContainerId);
        }
        self.open = Some(container);
        self.quick_craft = None;
        Ok(())
    }

    pub fn close_container(&mut self, id: ContainerId) -> Result<(), InventoryError> {
        match self.open.as_ref() {
            Some(open) if open.id == id => {
                self.open = None;
                self.quick_craft = None;
                Ok(())
            }
            _ => Err(InventoryError::UnknownContainer(id.0)),
        }
    }

    pub fn container_mut(
        &mut self,
        id: ContainerId,
    ) -> Result<&mut ContainerState, InventoryError> {
        if id == PLAYER_CONTAINER_ID {
            return Ok(&mut self.player);
        }
        self.open
            .as_mut()
            .filter(|container| container.id == id)
            .ok_or(InventoryError::UnknownContainer(id.0))
    }

    #[must_use]
    pub fn carried(&self) -> Option<&ItemStack> {
        self.open
            .as_ref()
            .map_or(self.player.carried.as_ref(), |container| {
                container.carried.as_ref()
            })
    }

    pub fn replace_content(
        &mut self,
        id: ContainerId,
        state_id: i32,
        slots: Vec<Option<ItemStack>>,
        carried: Option<ItemStack>,
    ) -> Result<(), InventoryError> {
        let cancel_quick_craft = self
            .quick_craft
            .as_ref()
            .is_some_and(|gesture| gesture.container == id && gesture.state_id != state_id);
        self.container_mut(id)?
            .replace_content(state_id, slots, carried)?;
        if cancel_quick_craft {
            self.quick_craft = None;
        }
        self.synchronize_player_from(id);
        Ok(())
    }

    pub fn set_slot(
        &mut self,
        id: ContainerId,
        state_id: i32,
        slot: SlotIndex,
        stack: Option<ItemStack>,
    ) -> Result<(), InventoryError> {
        let cancel_quick_craft = self
            .quick_craft
            .as_ref()
            .is_some_and(|gesture| gesture.container == id && gesture.state_id != state_id);
        self.container_mut(id)?.set_slot(state_id, slot, stack)?;
        if cancel_quick_craft {
            self.quick_craft = None;
        }
        self.synchronize_player_from(id);
        Ok(())
    }

    fn synchronize_player_from(&mut self, id: ContainerId) {
        if id == PLAYER_CONTAINER_ID {
            if let Some(open) = &mut self.open {
                open.carried = self.player.carried.clone();
            }
            return;
        }
        let Some(open) = &self.open else {
            return;
        };
        self.player.carried = open.carried.clone();
        if open.slots.len() < 36 {
            return;
        }
        let start = open.slots.len() - 36;
        for (offset, stack) in open.slots[start..].iter().cloned().enumerate() {
            if let Some(player_slot) = self.player.slots.get_mut(9 + offset) {
                *player_slot = stack;
            }
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    pub fn apply_click(&mut self, click: ContainerClick) -> Result<ClickOutcome, InventoryError> {
        if click.kind == ContainerClickKind::QuickCraft {
            return self.apply_quick_craft(click);
        }
        self.quick_craft = None;
        let container = self.container_mut(click.container)?;
        if container.state_id != click.state_id {
            return Err(InventoryError::StaleStateId {
                expected: container.state_id,
                actual: click.state_id,
            });
        }
        let before = container.slots.clone();
        match click.kind {
            ContainerClickKind::Pickup => pickup(container, click.slot, click.button)?,
            ContainerClickKind::QuickMove => quick_move(container, click.slot)?,
            ContainerClickKind::Swap => swap_hotbar(container, click.slot, click.button)?,
            ContainerClickKind::Clone => clone_stack(container, click.slot)?,
            ContainerClickKind::Throw => throw_stack(container, click.slot, click.button)?,
            ContainerClickKind::PickupAll => pickup_all(container, click.button)?,
            ContainerClickKind::QuickCraft => unreachable!("quick craft handled above"),
        }
        let changed_slots = before
            .into_iter()
            .zip(container.slots.iter())
            .enumerate()
            .filter_map(|(index, (before, after))| {
                (before != *after)
                    .then(|| {
                        i16::try_from(index)
                            .ok()
                            .map(|index| (SlotIndex(index), after.clone()))
                    })
                    .flatten()
            })
            .collect();
        let outcome = ClickOutcome {
            changed_slots,
            carried: container.carried.clone(),
        };
        self.synchronize_player_from(click.container);
        Ok(outcome)
    }

    fn apply_quick_craft(&mut self, click: ContainerClick) -> Result<ClickOutcome, InventoryError> {
        let stage = click.button & 3;
        let kind = u8::try_from(click.button >> 2)
            .map_err(|_| InventoryError::InvalidClickButton(click.button))?;
        if kind > 2 || stage > 2 {
            return Err(InventoryError::InvalidClickButton(click.button));
        }
        match stage {
            0 => {
                if click.slot.0 != -999 {
                    return Err(InventoryError::InvalidQuickCraftSlot(click.slot.0));
                }
                let carried = {
                    let container = self.container_mut(click.container)?;
                    if container.state_id != click.state_id {
                        return Err(InventoryError::StaleStateId {
                            expected: container.state_id,
                            actual: click.state_id,
                        });
                    }
                    container
                        .carried
                        .clone()
                        .ok_or(InventoryError::QuickCraftWithoutCarriedStack)?
                };
                self.quick_craft = Some(QuickCraftGesture {
                    container: click.container,
                    state_id: click.state_id,
                    kind,
                    slots: BTreeSet::new(),
                });
                Ok(ClickOutcome {
                    changed_slots: BTreeMap::new(),
                    carried: Some(carried),
                })
            }
            1 => {
                let index = {
                    let container = self.container_mut(click.container)?;
                    slot_index(container, click.slot)?
                };
                let gesture = self
                    .quick_craft
                    .as_ref()
                    .ok_or(InventoryError::QuickCraftNotStarted)?;
                if gesture.container != click.container
                    || gesture.state_id != click.state_id
                    || gesture.kind != kind
                {
                    return Err(InventoryError::QuickCraftGestureMismatch);
                }
                let eligible = {
                    let container = self.container_mut(click.container)?;
                    let carried = container
                        .carried
                        .as_ref()
                        .ok_or(InventoryError::QuickCraftWithoutCarriedStack)?;
                    quick_craft_slot_eligible(container, index, carried)
                };
                let gesture = self
                    .quick_craft
                    .as_mut()
                    .ok_or(InventoryError::QuickCraftNotStarted)?;
                if gesture.slots.len() >= MAX_CONTAINER_SLOTS {
                    return Err(InventoryError::TooManyQuickCraftSlots);
                }
                if eligible {
                    gesture.slots.insert(click.slot);
                }
                let carried = self.container_mut(click.container)?.carried.clone();
                Ok(ClickOutcome {
                    changed_slots: BTreeMap::new(),
                    carried,
                })
            }
            2 => {
                if click.slot.0 != -999 {
                    return Err(InventoryError::InvalidQuickCraftSlot(click.slot.0));
                }
                let gesture = self
                    .quick_craft
                    .take()
                    .ok_or(InventoryError::QuickCraftNotStarted)?;
                if gesture.container != click.container
                    || gesture.state_id != click.state_id
                    || gesture.kind != kind
                {
                    return Err(InventoryError::QuickCraftGestureMismatch);
                }
                let container = self.container_mut(click.container)?;
                let before = container.slots.clone();
                finish_quick_craft(container, &gesture)?;
                let changed_slots = changed_slots(&before, &container.slots);
                let outcome = ClickOutcome {
                    changed_slots,
                    carried: container.carried.clone(),
                };
                self.synchronize_player_from(click.container);
                Ok(outcome)
            }
            _ => Err(InventoryError::InvalidClickButton(click.button)),
        }
    }
}

fn quick_craft_slot_eligible(
    container: &ContainerState,
    index: usize,
    carried: &ItemStack,
) -> bool {
    let accepts_items = match &container.menu {
        MenuIdentity::PlayerInventory => index != 0,
        MenuIdentity::Menu(identifier) => match identifier.as_str() {
            "minecraft:furnace" => index != 2,
            "minecraft:crafting" => index != 0,
            _ => true,
        },
    };
    accepts_items
        && container.slots[index].as_ref().is_none_or(|stack| {
            stack.stackable_with(carried) && stack.count < stack.max_stack_size()
        })
}

fn changed_slots(
    before: &[Option<ItemStack>],
    after: &[Option<ItemStack>],
) -> BTreeMap<SlotIndex, Option<ItemStack>> {
    before
        .iter()
        .zip(after)
        .enumerate()
        .filter_map(|(index, (before, after))| {
            (before != after)
                .then(|| {
                    i16::try_from(index)
                        .ok()
                        .map(|index| (SlotIndex(index), after.clone()))
                })
                .flatten()
        })
        .collect()
}

fn finish_quick_craft(
    container: &mut ContainerState,
    gesture: &QuickCraftGesture,
) -> Result<(), InventoryError> {
    let Some(mut carried) = container.carried.take() else {
        return Err(InventoryError::QuickCraftWithoutCarriedStack);
    };
    if gesture.slots.is_empty() {
        container.carried = Some(carried);
        return Ok(());
    }
    let per_slot = match gesture.kind {
        0 => carried.count / u32::try_from(gesture.slots.len()).unwrap_or(u32::MAX),
        1 => 1,
        2 => carried.max_stack_size(),
        _ => return Err(InventoryError::InvalidClickButton(i8::MAX)),
    };
    for slot in &gesture.slots {
        if carried.count == 0 && gesture.kind != 2 {
            break;
        }
        let index = slot_index(container, *slot)?;
        let existing = container.slots[index].take();
        let (mut destination, capacity) = match existing {
            Some(stack) if stack.stackable_with(&carried) => {
                let capacity = stack.max_stack_size().saturating_sub(stack.count);
                (Some(stack), capacity)
            }
            Some(stack) => {
                container.slots[index] = Some(stack);
                continue;
            }
            None => (None, carried.max_stack_size()),
        };
        let amount = if gesture.kind == 2 {
            per_slot.min(capacity)
        } else {
            per_slot.min(capacity).min(carried.count)
        };
        if amount == 0 {
            container.slots[index] = destination;
            continue;
        }
        if let Some(stack) = &mut destination {
            stack.count += amount;
        } else {
            destination = Some(ItemStack {
                count: amount,
                ..carried.clone()
            });
        }
        if gesture.kind != 2 {
            carried.count -= amount;
        }
        container.slots[index] = destination;
    }
    if carried.count != 0 {
        container.carried = Some(carried);
    }
    Ok(())
}

impl Default for InventoryState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContainerClickKind {
    Pickup,
    QuickMove,
    Swap,
    Clone,
    Throw,
    QuickCraft,
    PickupAll,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContainerClick {
    pub container: ContainerId,
    pub state_id: i32,
    pub slot: SlotIndex,
    pub button: i8,
    pub kind: ContainerClickKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClickOutcome {
    pub changed_slots: BTreeMap<SlotIndex, Option<ItemStack>>,
    pub carried: Option<ItemStack>,
}

fn slot_index(container: &ContainerState, slot: SlotIndex) -> Result<usize, InventoryError> {
    let index = usize::try_from(slot.0).map_err(|_| InventoryError::InvalidSlot(slot.0))?;
    if index >= container.slots.len() {
        return Err(InventoryError::InvalidSlot(slot.0));
    }
    Ok(index)
}

fn pickup(
    container: &mut ContainerState,
    slot: SlotIndex,
    button: i8,
) -> Result<(), InventoryError> {
    if !matches!(button, 0 | 1) {
        return Err(InventoryError::InvalidClickButton(button));
    }
    if slot.0 == -999 {
        if button == 0 {
            container.carried = None;
        } else if let Some(carried) = &mut container.carried {
            carried.count = carried.count.saturating_sub(1);
            if carried.count == 0 {
                container.carried = None;
            }
        }
        return Ok(());
    }
    let index = slot_index(container, slot)?;
    let mut target = container.slots[index].take();
    match (container.carried.take(), target.take()) {
        (None, None) => {}
        (None, Some(mut stack)) => {
            if button == 0 {
                container.carried = Some(stack);
            } else {
                let taken = stack.count.div_ceil(2);
                let remaining = stack.count - taken;
                container.carried = Some(ItemStack {
                    count: taken,
                    ..stack.clone()
                });
                if remaining != 0 {
                    stack.count = remaining;
                    target = Some(stack);
                }
            }
        }
        (Some(mut carried), None) => {
            if button == 0 {
                target = Some(carried);
            } else {
                target = Some(ItemStack {
                    count: 1,
                    ..carried.clone()
                });
                carried.count -= 1;
                if carried.count != 0 {
                    container.carried = Some(carried);
                }
            }
        }
        (Some(mut carried), Some(mut stack)) if carried.stackable_with(&stack) => {
            let moved = if button == 0 {
                carried.count
            } else {
                carried.count.min(1)
            }
            .min(stack.max_stack_size().saturating_sub(stack.count));
            stack.count += moved;
            carried.count -= moved;
            target = Some(stack);
            if carried.count != 0 {
                container.carried = Some(carried);
            }
        }
        (Some(carried), Some(stack)) => {
            target = Some(carried);
            container.carried = Some(stack);
        }
    }
    container.slots[index] = target;
    Ok(())
}

fn quick_move(container: &mut ContainerState, slot: SlotIndex) -> Result<(), InventoryError> {
    let source = slot_index(container, slot)?;
    let Some(mut moving) = container.slots[source].take() else {
        return Ok(());
    };
    let routes = quick_move_routes(container, source, &moving);
    for route in routes {
        move_stack_to(container, &mut moving, route);
        if moving.count == 0 {
            return Ok(());
        }
    }
    container.slots[source] = Some(moving);
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SlotRoute {
    start: usize,
    end: usize,
    reverse: bool,
}

impl SlotRoute {
    const fn new(start: usize, end: usize, reverse: bool) -> Self {
        Self {
            start,
            end,
            reverse,
        }
    }

    fn indices(self) -> Box<dyn Iterator<Item = usize>> {
        if self.reverse {
            Box::new((self.start..self.end).rev())
        } else {
            Box::new(self.start..self.end)
        }
    }
}

fn quick_move_routes(
    container: &ContainerState,
    source: usize,
    moving: &ItemStack,
) -> Vec<SlotRoute> {
    if container.id == PLAYER_CONTAINER_ID && container.slots.len() == PLAYER_INVENTORY_SLOTS {
        return match source {
            // Result, crafting and equipment slots enter the combined player
            // inventory in vanilla's reverse order.
            0..=8 | 45 => vec![SlotRoute::new(9, 45, true)],
            9..=35 => {
                let mut routes = equipment_destination(container, moving)
                    .into_iter()
                    .map(|slot| SlotRoute::new(slot, slot + 1, false))
                    .collect::<Vec<_>>();
                routes.push(SlotRoute::new(36, 45, false));
                routes
            }
            36..=44 => vec![SlotRoute::new(9, 36, false)],
            _ => Vec::new(),
        };
    }

    let player_start = container.slots.len().saturating_sub(36);
    match &container.menu {
        MenuIdentity::Menu(identifier) if identifier.as_str() == "minecraft:furnace" => {
            match source {
                0..=2 => vec![SlotRoute::new(3, container.slots.len(), true)],
                3..=29 => furnace_player_routes(container, moving, 30, 39),
                30..=38 => furnace_player_routes(container, moving, 3, 30),
                _ => Vec::new(),
            }
        }
        MenuIdentity::Menu(identifier) if identifier.as_str() == "minecraft:crafting" => {
            match source {
                0 => vec![SlotRoute::new(10, container.slots.len(), true)],
                1..=9 => vec![SlotRoute::new(10, container.slots.len(), false)],
                10..=36 => vec![SlotRoute::new(37, 46.min(container.slots.len()), false)],
                37..=45 => vec![SlotRoute::new(10, 37.min(container.slots.len()), false)],
                _ => Vec::new(),
            }
        }
        _ if source < player_start => {
            vec![SlotRoute::new(player_start, container.slots.len(), true)]
        }
        _ => vec![SlotRoute::new(0, player_start, false)],
    }
}

fn furnace_player_routes(
    container: &ContainerState,
    moving: &ItemStack,
    alternate_start: usize,
    alternate_end: usize,
) -> Vec<SlotRoute> {
    let mut routes = Vec::new();
    if is_furnace_fuel(moving) {
        routes.push(SlotRoute::new(1, 2, false));
    } else if is_probable_furnace_input(moving) {
        routes.push(SlotRoute::new(0, 1, false));
    }
    routes.push(SlotRoute::new(
        alternate_start,
        alternate_end.min(container.slots.len()),
        false,
    ));
    routes
}

fn is_furnace_fuel(stack: &ItemStack) -> bool {
    let name = stack.item.as_str();
    matches!(
        name,
        "minecraft:coal" | "minecraft:charcoal" | "minecraft:blaze_rod"
    ) || name.ends_with("_planks")
        || name.ends_with("_log")
        || name.ends_with("_wood")
}

fn is_probable_furnace_input(stack: &ItemStack) -> bool {
    let name = stack.item.as_str();
    name.ends_with("_ore")
        || name.contains("raw_")
        || matches!(
            name,
            "minecraft:sand"
                | "minecraft:red_sand"
                | "minecraft:cobblestone"
                | "minecraft:clay_ball"
                | "minecraft:wet_sponge"
                | "minecraft:cactus"
                | "minecraft:kelp"
                | "minecraft:potato"
                | "minecraft:beef"
                | "minecraft:chicken"
                | "minecraft:porkchop"
                | "minecraft:mutton"
                | "minecraft:rabbit"
                | "minecraft:cod"
                | "minecraft:salmon"
        )
}

fn equipment_destination(container: &ContainerState, stack: &ItemStack) -> Option<usize> {
    let name = stack.item.as_str();
    let destination = if name.ends_with("_helmet") || name == "minecraft:carved_pumpkin" {
        5
    } else if name.ends_with("_chestplate") || name == "minecraft:elytra" {
        6
    } else if name.ends_with("_leggings") {
        7
    } else if name.ends_with("_boots") {
        8
    } else if name == "minecraft:shield" {
        45
    } else {
        return None;
    };
    container
        .slots
        .get(destination)
        .is_some_and(Option::is_none)
        .then_some(destination)
}

fn move_stack_to(container: &mut ContainerState, moving: &mut ItemStack, route: SlotRoute) {
    for index in route.indices() {
        if let Some(stack) = &mut container.slots[index]
            && stack.stackable_with(moving)
        {
            let amount = moving
                .count
                .min(stack.max_stack_size().saturating_sub(stack.count));
            stack.count += amount;
            moving.count -= amount;
            if moving.count == 0 {
                return;
            }
        }
    }
    if let Some(index) = route
        .indices()
        .find(|index| container.slots[*index].is_none())
    {
        container.slots[index] = Some(moving.clone());
        moving.count = 0;
    }
}

fn swap_hotbar(
    container: &mut ContainerState,
    slot: SlotIndex,
    button: i8,
) -> Result<(), InventoryError> {
    let source = slot_index(container, slot)?;
    let hotbar = usize::try_from(button).map_err(|_| InventoryError::InvalidClickButton(button))?;
    if hotbar > 8 || container.slots.len() < 9 {
        return Err(InventoryError::InvalidClickButton(button));
    }
    let destination =
        if container.id == PLAYER_CONTAINER_ID && container.slots.len() == PLAYER_INVENTORY_SLOTS {
            36 + hotbar
        } else {
            container.slots.len() - 9 + hotbar
        };
    container.slots.swap(source, destination);
    Ok(())
}

fn clone_stack(container: &mut ContainerState, slot: SlotIndex) -> Result<(), InventoryError> {
    let index = slot_index(container, slot)?;
    container.carried = container.slots[index].clone().map(|mut stack| {
        stack.count = stack.max_stack_size();
        stack
    });
    Ok(())
}

fn throw_stack(
    container: &mut ContainerState,
    slot: SlotIndex,
    button: i8,
) -> Result<(), InventoryError> {
    let index = slot_index(container, slot)?;
    if !matches!(button, 0 | 1) {
        return Err(InventoryError::InvalidClickButton(button));
    }
    if button == 1 {
        container.slots[index] = None;
    } else if let Some(stack) = &mut container.slots[index] {
        stack.count -= 1;
        if stack.count == 0 {
            container.slots[index] = None;
        }
    }
    Ok(())
}

fn pickup_all(container: &mut ContainerState, button: i8) -> Result<(), InventoryError> {
    if button != 0 {
        return Err(InventoryError::InvalidClickButton(button));
    }
    let Some(carried) = &mut container.carried else {
        return Ok(());
    };
    // AbstractContainerMenu performs two deterministic passes: non-full
    // stacks first, then full stacks. This prevents a full early slot from
    // starving partial stacks later in the menu.
    for full_pass in [false, true] {
        for (index, slot) in container.slots.iter_mut().enumerate() {
            if !can_take_for_pick_all(&container.menu, index) {
                continue;
            }
            if let Some(stack) = slot
                && stack.stackable_with(carried)
                && (stack.count == stack.max_stack_size()) == full_pass
            {
                let moved = stack
                    .count
                    .min(carried.max_stack_size().saturating_sub(carried.count));
                stack.count -= moved;
                carried.count += moved;
                if stack.count == 0 {
                    *slot = None;
                }
                if carried.count == carried.max_stack_size() {
                    return Ok(());
                }
            }
        }
    }
    Ok(())
}

fn can_take_for_pick_all(menu: &MenuIdentity, index: usize) -> bool {
    match menu {
        MenuIdentity::PlayerInventory => index != 0,
        MenuIdentity::Menu(identifier) if identifier.as_str() == "minecraft:crafting" => index != 0,
        MenuIdentity::Menu(identifier) if identifier.as_str() == "minecraft:furnace" => index != 2,
        MenuIdentity::Menu(_) => true,
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum InventoryError {
    #[error("item count must be positive for a non-empty stack")]
    ZeroItemCount,
    #[error("item stack has {count} components, maximum is {max}")]
    TooManyComponents { count: usize, max: usize },
    #[error("item component data retains {size} bytes, maximum is {max}")]
    ComponentDataTooLarge { size: usize, max: usize },
    #[error("component patch adds and removes the same component")]
    ConflictingComponentPatch,
    #[error("component fingerprint has no matching added component")]
    OrphanComponentFingerprint,
    #[error("container has {count} slots, maximum is {max}")]
    TooManySlots { count: usize, max: usize },
    #[error("invalid container slot {0}")]
    InvalidSlot(i16),
    #[error("invalid hotbar slot {0}; expected 0 through 8")]
    InvalidHotbarSlot(u8),
    #[error("open container cannot use the player inventory container ID")]
    InvalidOpenContainerId,
    #[error("unknown container ID {0}")]
    UnknownContainer(u32),
    #[error("container state ID changed: expected {expected}, click used {actual}")]
    StaleStateId { expected: i32, actual: i32 },
    #[error("invalid container click button {0}")]
    InvalidClickButton(i8),
    #[error("quick-craft drag has not been started")]
    QuickCraftNotStarted,
    #[error("quick-craft drag metadata does not match its start packet")]
    QuickCraftGestureMismatch,
    #[error("quick-craft drag requires a carried stack")]
    QuickCraftWithoutCarriedStack,
    #[error("invalid quick-craft sentinel slot {0}")]
    InvalidQuickCraftSlot(i16),
    #[error("quick-craft drag exceeds the bounded container slot set")]
    TooManyQuickCraftSlots,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack(name: &str, count: u32) -> ItemStack {
        ItemStack::new(
            MinecraftIdentifier::new(name).unwrap(),
            count,
            ComponentPatch::default(),
        )
        .unwrap()
    }

    #[test]
    fn player_inventory_maps_selected_hotbar_to_protocol_slots() {
        let mut inventory = InventoryState::new();
        inventory.player.slots[40] = Some(stack("minecraft:stone", 32));
        inventory.set_selected_hotbar_slot(4).unwrap();
        assert_eq!(
            inventory.held_item().unwrap().item.as_str(),
            "minecraft:stone"
        );
    }

    #[test]
    fn effective_item_model_uses_default_patch_override_and_removal() {
        let mut default = stack("minecraft:stone", 1);
        assert_eq!(
            default.effective_item_model_owned().unwrap().as_str(),
            "minecraft:stone"
        );

        let component = MinecraftIdentifier::new("minecraft:item_model").unwrap();
        default.components.added.insert(
            component.clone(),
            ComponentValue::Text("minecraft:diamond".to_owned()),
        );
        assert_eq!(
            default.effective_item_model_owned().unwrap().as_str(),
            "minecraft:diamond"
        );
        default.components.added.insert(
            component.clone(),
            ComponentValue::Text("../invalid".to_owned()),
        );
        assert_eq!(default.effective_item_model_owned(), None);
        default.components.added.clear();
        default.components.removed.insert(component);
        assert_eq!(default.effective_item_model_owned(), None);
    }

    #[test]
    fn gui_render_key_tracks_only_component_state_consumed_by_model_graphs() {
        let component = MinecraftIdentifier::new("minecraft:block_state").unwrap();
        let mut start = stack("minecraft:test_block", 1);
        start.components.added.insert(
            component.clone(),
            ComponentValue::StringMap {
                values: BTreeMap::from([("mode".to_owned(), "start".to_owned())]),
                wire: vec![
                    1, 4, b'm', b'o', b'd', b'e', 5, b's', b't', b'a', b'r', b't',
                ],
            },
        );
        let mut log = start.clone();
        log.components.added.insert(
            component,
            ComponentValue::StringMap {
                values: BTreeMap::from([("mode".to_owned(), "log".to_owned())]),
                wire: vec![1, 4, b'm', b'o', b'd', b'e', 3, b'l', b'o', b'g'],
            },
        );
        assert_ne!(start.gui_render_key(), log.gui_render_key());
        assert_eq!(start.gui_render_key(), start.clone().gui_render_key());

        let mut patterned = stack("minecraft:white_banner", 1);
        patterned.components.added.insert(
            MinecraftIdentifier::new("minecraft:banner_patterns").unwrap(),
            ComponentValue::Opaque(vec![1, 24, 9]),
        );
        assert_eq!(
            patterned.gui_render_key().as_deref(),
            Some("minecraft:white_banner|banner_patterns=011809")
        );
        assert_ne!(
            patterned.gui_render_key(),
            stack("minecraft:white_banner", 1).gui_render_key()
        );
    }

    #[test]
    fn offhand_slot_is_independent_from_selected_main_hand() {
        let mut inventory = InventoryState::new();
        inventory.player.slots[36] = Some(stack("minecraft:stone", 1));
        inventory.player.slots[45] = Some(stack("minecraft:shield", 1));
        assert_eq!(
            inventory.held_item().unwrap().item.as_str(),
            "minecraft:stone"
        );
        assert_eq!(
            inventory.offhand_item().unwrap().item.as_str(),
            "minecraft:shield"
        );
    }

    #[test]
    fn content_and_slot_updates_are_authoritative() {
        let mut inventory = InventoryState::new();
        let mut content = vec![None; PLAYER_INVENTORY_SLOTS];
        content[36] = Some(stack("minecraft:dirt", 5));
        inventory
            .player
            .replace_content(7, content, Some(stack("minecraft:stick", 1)))
            .unwrap();
        inventory
            .player
            .set_slot(8, SlotIndex(36), Some(stack("minecraft:stone", 2)))
            .unwrap();
        assert_eq!(inventory.player.state_id, 8);
        assert_eq!(
            inventory.held_item().unwrap().item.as_str(),
            "minecraft:stone"
        );
        assert_eq!(inventory.player.carried.as_ref().unwrap().count, 1);
    }

    #[test]
    fn open_container_lifecycle_is_explicit() {
        let mut inventory = InventoryState::new();
        let chest = ContainerState::new(
            ContainerId(3),
            MenuIdentity::Menu(MinecraftIdentifier::new("minecraft:generic_9x3").unwrap()),
            "Chest".to_owned(),
            63,
        )
        .unwrap();
        inventory.open_container(chest).unwrap();
        assert_eq!(inventory.open().unwrap().id, ContainerId(3));
        inventory.close_container(ContainerId(3)).unwrap();
        assert!(inventory.open().is_none());
    }

    #[test]
    fn malformed_counts_and_bounds_are_rejected() {
        assert_eq!(
            ItemStack::new(
                MinecraftIdentifier::new("minecraft:air").unwrap(),
                0,
                ComponentPatch::default()
            ),
            Err(InventoryError::ZeroItemCount)
        );
        assert_eq!(
            ContainerState::new(
                ContainerId(1),
                MenuIdentity::Menu(MinecraftIdentifier::new("minecraft:test").unwrap()),
                String::new(),
                MAX_CONTAINER_SLOTS + 1
            ),
            Err(InventoryError::TooManySlots {
                count: MAX_CONTAINER_SLOTS + 1,
                max: MAX_CONTAINER_SLOTS
            })
        );
    }

    #[test]
    fn left_and_right_pickup_predict_changed_slot_and_cursor() {
        let mut inventory = InventoryState::new();
        inventory.player.slots[36] = Some(stack("minecraft:stone", 5));
        let right = inventory
            .apply_click(ContainerClick {
                container: PLAYER_CONTAINER_ID,
                state_id: 0,
                slot: SlotIndex(36),
                button: 1,
                kind: ContainerClickKind::Pickup,
            })
            .unwrap();
        assert_eq!(inventory.player.slots[36].as_ref().unwrap().count, 2);
        assert_eq!(inventory.player.carried.as_ref().unwrap().count, 3);
        assert_eq!(
            right.changed_slots[&SlotIndex(36)].as_ref().unwrap().count,
            2
        );

        let left = inventory
            .apply_click(ContainerClick {
                container: PLAYER_CONTAINER_ID,
                state_id: 0,
                slot: SlotIndex(37),
                button: 0,
                kind: ContainerClickKind::Pickup,
            })
            .unwrap();
        assert_eq!(inventory.player.slots[37].as_ref().unwrap().count, 3);
        assert!(inventory.player.carried.is_none());
        assert_eq!(left.changed_slots.len(), 1);
    }

    #[test]
    fn quick_move_uses_container_then_player_partition() {
        let mut inventory = InventoryState::new();
        let mut chest = ContainerState::new(
            ContainerId(2),
            MenuIdentity::Menu(MinecraftIdentifier::new("minecraft:generic_9x1").unwrap()),
            "Chest".to_owned(),
            45,
        )
        .unwrap();
        chest.slots[0] = Some(stack("minecraft:dirt", 8));
        inventory.open_container(chest).unwrap();
        let outcome = inventory
            .apply_click(ContainerClick {
                container: ContainerId(2),
                state_id: 0,
                slot: SlotIndex(0),
                button: 0,
                kind: ContainerClickKind::QuickMove,
            })
            .unwrap();
        let open = inventory.open().unwrap();
        assert!(open.slots[0].is_none());
        assert_eq!(open.slots[44].as_ref().unwrap().count, 8);
        assert_eq!(outcome.changed_slots.len(), 2);
    }

    #[test]
    fn quick_move_routes_supported_menus_with_vanilla_range_order() {
        let mut player = InventoryState::new();
        player.player.slots[9] = Some(stack("minecraft:diamond_helmet", 1));
        player
            .apply_click(ContainerClick {
                container: PLAYER_CONTAINER_ID,
                state_id: 0,
                slot: SlotIndex(9),
                button: 0,
                kind: ContainerClickKind::QuickMove,
            })
            .unwrap();
        assert_eq!(
            player.player.slots[5]
                .as_ref()
                .map(|stack| stack.item.as_str()),
            Some("minecraft:diamond_helmet")
        );

        let mut furnace = ContainerState::new(
            ContainerId(7),
            MenuIdentity::Menu(MinecraftIdentifier::new("minecraft:furnace").unwrap()),
            "Furnace".to_owned(),
            39,
        )
        .unwrap();
        furnace.slots[3] = Some(stack("minecraft:coal", 8));
        let mut inventory = InventoryState::new();
        inventory.open_container(furnace).unwrap();
        inventory
            .apply_click(ContainerClick {
                container: ContainerId(7),
                state_id: 0,
                slot: SlotIndex(3),
                button: 0,
                kind: ContainerClickKind::QuickMove,
            })
            .unwrap();
        assert_eq!(
            inventory.open().unwrap().slots[1].as_ref().unwrap().count,
            8
        );

        let mut crafting = ContainerState::new(
            ContainerId(8),
            MenuIdentity::Menu(MinecraftIdentifier::new("minecraft:crafting").unwrap()),
            "Crafting".to_owned(),
            46,
        )
        .unwrap();
        crafting.slots[10] = Some(stack("minecraft:stone", 4));
        let mut inventory = InventoryState::new();
        inventory.open_container(crafting).unwrap();
        inventory
            .apply_click(ContainerClick {
                container: ContainerId(8),
                state_id: 0,
                slot: SlotIndex(10),
                button: 0,
                kind: ContainerClickKind::QuickMove,
            })
            .unwrap();
        assert_eq!(
            inventory.open().unwrap().slots[37].as_ref().unwrap().count,
            4
        );
        assert!(
            inventory.open().unwrap().slots[0..10]
                .iter()
                .all(Option::is_none)
        );
    }

    #[test]
    fn pickup_all_uses_partial_then_full_component_exact_collection() {
        let mut inventory = InventoryState::new();
        inventory.player.carried = Some(stack("minecraft:stone", 50));
        inventory.player.slots[9] = Some(stack("minecraft:stone", 64));
        inventory.player.slots[10] = Some(stack("minecraft:stone", 3));
        let mut differing = stack("minecraft:stone", 2);
        differing.components.added.insert(
            MinecraftIdentifier::new("minecraft:damage").unwrap(),
            ComponentValue::VarInt(1),
        );
        inventory.player.slots[11] = Some(differing);
        inventory
            .apply_click(ContainerClick {
                container: PLAYER_CONTAINER_ID,
                state_id: 0,
                slot: SlotIndex(10),
                button: 0,
                kind: ContainerClickKind::PickupAll,
            })
            .unwrap();
        assert_eq!(inventory.carried().unwrap().count, 64);
        assert!(inventory.player.slots[10].is_none());
        assert_eq!(inventory.player.slots[9].as_ref().unwrap().count, 53);
        assert_eq!(inventory.player.slots[11].as_ref().unwrap().count, 2);
    }

    #[test]
    fn stale_click_state_is_rejected_without_mutation() {
        let mut inventory = InventoryState::new();
        inventory.player.state_id = 3;
        inventory.player.slots[36] = Some(stack("minecraft:stone", 1));
        let before = inventory.clone();
        assert_eq!(
            inventory.apply_click(ContainerClick {
                container: PLAYER_CONTAINER_ID,
                state_id: 2,
                slot: SlotIndex(36),
                button: 0,
                kind: ContainerClickKind::Pickup,
            }),
            Err(InventoryError::StaleStateId {
                expected: 3,
                actual: 2
            })
        );
        assert_eq!(inventory, before);
    }

    #[test]
    fn open_container_player_tail_updates_authoritative_hotbar_and_cursor() {
        let mut inventory = InventoryState::new();
        inventory
            .open_container(
                ContainerState::new(
                    ContainerId(4),
                    MenuIdentity::Menu(MinecraftIdentifier::new("minecraft:generic_9x1").unwrap()),
                    "Chest".to_owned(),
                    0,
                )
                .unwrap(),
            )
            .unwrap();
        let mut slots = vec![None; 45];
        slots[36] = Some(stack("minecraft:diamond", 2));
        inventory
            .replace_content(ContainerId(4), 9, slots, Some(stack("minecraft:stick", 1)))
            .unwrap();
        inventory.set_selected_hotbar_slot(0).unwrap();
        assert_eq!(
            inventory.held_item().unwrap().item.as_str(),
            "minecraft:diamond"
        );
        assert_eq!(
            inventory.carried().unwrap().item.as_str(),
            "minecraft:stick"
        );
        inventory
            .set_slot(
                ContainerId(4),
                10,
                SlotIndex(36),
                Some(stack("minecraft:stone", 3)),
            )
            .unwrap();
        assert_eq!(
            inventory.held_item().unwrap().item.as_str(),
            "minecraft:stone"
        );
    }

    #[test]
    fn armor_offhand_and_reset_are_explicit_player_slots() {
        let mut inventory = InventoryState::new();
        inventory.player.slots[5] = Some(stack("minecraft:diamond_helmet", 1));
        inventory.player.slots[45] = Some(stack("minecraft:shield", 1));
        assert_eq!(inventory.player.slots[5].as_ref().unwrap().count, 1);
        assert_eq!(inventory.player.slots[45].as_ref().unwrap().count, 1);
        inventory.reset();
        assert!(inventory.player.slots.iter().all(Option::is_none));
        assert!(inventory.open().is_none());
        assert!(inventory.carried().is_none());
    }

    #[test]
    fn game_master_items_require_both_instant_build_and_permission_level_two() {
        let mut inventory = InventoryState::new();
        assert!(!inventory.can_use_game_master_blocks());

        inventory.set_instant_build(true);
        assert!(!inventory.can_use_game_master_blocks());

        inventory.set_permission_level(2);
        assert!(inventory.can_use_game_master_blocks());

        inventory.set_instant_build(false);
        assert!(!inventory.can_use_game_master_blocks());

        inventory.set_permission_level(u8::MAX);
        inventory.set_instant_build(true);
        assert!(inventory.can_use_game_master_blocks());
        inventory.reset();
        assert!(!inventory.can_use_game_master_blocks());
    }

    #[test]
    fn swap_throw_clone_and_pickup_all_are_predicted_deterministically() {
        let mut inventory = InventoryState::new();
        inventory.player.slots[9] = Some(stack("minecraft:stone", 5));
        inventory.player.slots[36] = Some(stack("minecraft:dirt", 2));
        inventory
            .apply_click(ContainerClick {
                container: PLAYER_CONTAINER_ID,
                state_id: 0,
                slot: SlotIndex(9),
                button: 0,
                kind: ContainerClickKind::Swap,
            })
            .unwrap();
        assert_eq!(
            inventory.player.slots[9].as_ref().unwrap().item.as_str(),
            "minecraft:dirt"
        );
        inventory
            .apply_click(ContainerClick {
                container: PLAYER_CONTAINER_ID,
                state_id: 0,
                slot: SlotIndex(9),
                button: 0,
                kind: ContainerClickKind::Throw,
            })
            .unwrap();
        assert_eq!(inventory.player.slots[9].as_ref().unwrap().count, 1);
        inventory
            .apply_click(ContainerClick {
                container: PLAYER_CONTAINER_ID,
                state_id: 0,
                slot: SlotIndex(36),
                button: 0,
                kind: ContainerClickKind::Clone,
            })
            .unwrap();
        inventory.player.slots[10] = Some(stack("minecraft:stone", 3));
        inventory
            .apply_click(ContainerClick {
                container: PLAYER_CONTAINER_ID,
                state_id: 0,
                slot: SlotIndex(10),
                button: 0,
                kind: ContainerClickKind::PickupAll,
            })
            .unwrap();
        assert_eq!(inventory.carried().unwrap().count, 64);
    }

    #[test]
    fn bounded_quick_craft_spreads_cursor_and_requires_complete_gesture() {
        let mut inventory = InventoryState::new();
        inventory.player.carried = Some(stack("minecraft:stone", 8));
        for (slot, button) in [(-999, 0), (9, 1), (10, 1), (-999, 2)] {
            inventory
                .apply_click(ContainerClick {
                    container: PLAYER_CONTAINER_ID,
                    state_id: 0,
                    slot: SlotIndex(slot),
                    button,
                    kind: ContainerClickKind::QuickCraft,
                })
                .unwrap();
        }
        assert_eq!(inventory.player.slots[9].as_ref().unwrap().count, 4);
        assert_eq!(inventory.player.slots[10].as_ref().unwrap().count, 4);
        assert!(inventory.carried().is_none());
        assert_eq!(
            inventory.apply_click(ContainerClick {
                container: PLAYER_CONTAINER_ID,
                state_id: 0,
                slot: SlotIndex(9),
                button: 1,
                kind: ContainerClickKind::QuickCraft,
            }),
            Err(InventoryError::QuickCraftNotStarted)
        );
    }

    #[test]
    fn quick_craft_filters_slots_respects_capacity_and_supports_right_drag_and_cancel() {
        let mut inventory = InventoryState::new();
        inventory.player.carried = Some(stack("minecraft:stone", 10));
        inventory.player.slots[9] = Some(stack("minecraft:stone", 60));
        inventory.player.slots[10] = Some(stack("minecraft:dirt", 1));
        inventory.player.slots[11] = Some(stack("minecraft:stone", 64));
        for (slot, button) in [(-999, 0), (9, 1), (10, 1), (11, 1), (12, 1), (-999, 2)] {
            inventory
                .apply_click(ContainerClick {
                    container: PLAYER_CONTAINER_ID,
                    state_id: 0,
                    slot: SlotIndex(slot),
                    button,
                    kind: ContainerClickKind::QuickCraft,
                })
                .unwrap();
        }
        assert_eq!(inventory.player.slots[9].as_ref().unwrap().count, 64);
        assert_eq!(inventory.player.slots[10].as_ref().unwrap().count, 1);
        assert_eq!(inventory.player.slots[11].as_ref().unwrap().count, 64);
        assert_eq!(inventory.player.slots[12].as_ref().unwrap().count, 5);
        assert_eq!(inventory.carried().unwrap().count, 1);

        inventory.player.carried = Some(stack("minecraft:stone", 3));
        for (slot, button) in [(-999, 4), (13, 5), (14, 5), (15, 5), (-999, 6)] {
            inventory
                .apply_click(ContainerClick {
                    container: PLAYER_CONTAINER_ID,
                    state_id: 0,
                    slot: SlotIndex(slot),
                    button,
                    kind: ContainerClickKind::QuickCraft,
                })
                .unwrap();
        }
        for slot in 13..=15 {
            assert_eq!(inventory.player.slots[slot].as_ref().unwrap().count, 1);
        }
        assert!(inventory.carried().is_none());

        inventory.player.carried = Some(stack("minecraft:stone", 2));
        inventory
            .apply_click(ContainerClick {
                container: PLAYER_CONTAINER_ID,
                state_id: 0,
                slot: SlotIndex(-999),
                button: 0,
                kind: ContainerClickKind::QuickCraft,
            })
            .unwrap();
        inventory
            .apply_click(ContainerClick {
                container: PLAYER_CONTAINER_ID,
                state_id: 0,
                slot: SlotIndex(16),
                button: 1,
                kind: ContainerClickKind::QuickCraft,
            })
            .unwrap();
        inventory
            .apply_click(ContainerClick {
                container: PLAYER_CONTAINER_ID,
                state_id: 0,
                slot: SlotIndex(17),
                button: 0,
                kind: ContainerClickKind::Pickup,
            })
            .unwrap();
        assert_eq!(
            inventory.apply_click(ContainerClick {
                container: PLAYER_CONTAINER_ID,
                state_id: 0,
                slot: SlotIndex(-999),
                button: 2,
                kind: ContainerClickKind::QuickCraft,
            }),
            Err(InventoryError::QuickCraftNotStarted)
        );
    }

    #[test]
    fn authoritative_state_change_cancels_an_in_progress_quick_craft() {
        let mut inventory = InventoryState::new();
        inventory.player.carried = Some(stack("minecraft:stone", 4));
        inventory
            .apply_click(ContainerClick {
                container: PLAYER_CONTAINER_ID,
                state_id: 0,
                slot: SlotIndex(-999),
                button: 0,
                kind: ContainerClickKind::QuickCraft,
            })
            .unwrap();
        inventory
            .replace_content(
                PLAYER_CONTAINER_ID,
                1,
                vec![None; PLAYER_INVENTORY_SLOTS],
                Some(stack("minecraft:stone", 4)),
            )
            .unwrap();
        assert_eq!(
            inventory.apply_click(ContainerClick {
                container: PLAYER_CONTAINER_ID,
                state_id: 1,
                slot: SlotIndex(9),
                button: 1,
                kind: ContainerClickKind::QuickCraft,
            }),
            Err(InventoryError::QuickCraftNotStarted)
        );
    }
}
