use std::{collections::BTreeMap, sync::Arc};

use thiserror::Error;

pub const SECTION_BLOCK_COUNT: usize = 16 * 16 * 16;
pub const SECTION_BIOME_COUNT: usize = 4 * 4 * 4;
pub const MAX_CHUNK_SECTIONS: usize = 64;
pub const MAX_LOADED_CHUNKS: usize = 512;
pub const MAX_HEIGHTMAPS: usize = 16;
pub const MAX_HEIGHTMAP_LONGS: usize = 256;
pub const MAX_BLOCK_ENTITIES_PER_CHUNK: usize = 1_024;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ChunkCoordinate {
    pub x: i32,
    pub z: i32,
}

impl ChunkCoordinate {
    #[must_use]
    pub const fn new(x: i32, z: i32) -> Self {
        Self { x, z }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RuntimeBlockStateId(pub u32);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RuntimeBiomeId(pub u32);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PalettedContainer<T> {
    Single { value: T, entries: usize },
    Indirect { palette: Vec<T>, indices: Vec<u16> },
    Direct { values: Vec<T> },
}

impl<T: Copy> PalettedContainer<T> {
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Self::Single { entries, .. } => *entries,
            Self::Indirect { indices, .. } => indices.len(),
            Self::Direct { values } => values.len(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[must_use]
    pub fn get(&self, index: usize) -> Option<T> {
        match self {
            Self::Single { value, entries } => (index < *entries).then_some(*value),
            Self::Indirect { palette, indices } => indices
                .get(index)
                .and_then(|palette_index| palette.get(usize::from(*palette_index)))
                .copied(),
            Self::Direct { values } => values.get(index).copied(),
        }
    }

    #[must_use]
    pub const fn form(&self) -> PaletteForm {
        match self {
            Self::Single { .. } => PaletteForm::Single,
            Self::Indirect { .. } => PaletteForm::Indirect,
            Self::Direct { .. } => PaletteForm::Direct,
        }
    }

    /// Replaces one bounded entry. Mutable network updates deliberately
    /// materialize palette storage into a fixed-size direct vector: this keeps
    /// Phase 17 mutation simple and bounded without coupling semantic world
    /// state to a particular wire palette representation.
    pub fn set(&mut self, index: usize, value: T) -> bool
    where
        T: PartialEq,
    {
        if index >= self.len() {
            return false;
        }
        if !matches!(self, Self::Direct { .. }) {
            let values = (0..self.len())
                .filter_map(|entry| self.get(entry))
                .collect::<Vec<_>>();
            if values.len() != self.len() {
                return false;
            }
            *self = Self::Direct { values };
        }
        let Self::Direct { values } = self else {
            return false;
        };
        let Some(entry) = values.get_mut(index) else {
            return false;
        };
        if *entry == value {
            return false;
        }
        *entry = value;
        true
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PaletteForm {
    Single,
    Indirect,
    Direct,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChunkSection {
    pub non_empty_block_count: u16,
    pub fluid_count: u16,
    pub blocks: PalettedContainer<RuntimeBlockStateId>,
    pub biomes: PalettedContainer<RuntimeBiomeId>,
}

impl ChunkSection {
    #[must_use]
    pub fn block(&self, x: u8, y: u8, z: u8) -> Option<RuntimeBlockStateId> {
        section_index(x, y, z, 16).and_then(|index| self.blocks.get(index))
    }

    #[must_use]
    pub fn biome(&self, x: u8, y: u8, z: u8) -> Option<RuntimeBiomeId> {
        section_index(x, y, z, 4).and_then(|index| self.biomes.get(index))
    }
}

fn section_index(x: u8, y: u8, z: u8, width: usize) -> Option<usize> {
    let (x, y, z) = (usize::from(x), usize::from(y), usize::from(z));
    if x >= width || y >= width || z >= width {
        return None;
    }
    y.checked_mul(width)?
        .checked_add(z)?
        .checked_mul(width)?
        .checked_add(x)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeightmapData {
    pub kind_raw_id: u32,
    pub data: Vec<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockEntitySummary {
    pub local_x: u8,
    pub y: i16,
    pub local_z: u8,
    pub type_raw_id: u32,
    pub has_data: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ChunkLightSummary {
    pub sky_mask: Vec<u64>,
    pub block_mask: Vec<u64>,
    pub empty_sky_mask: Vec<u64>,
    pub empty_block_mask: Vec<u64>,
    pub sky_layer_count: usize,
    pub block_layer_count: usize,
    pub sky_layers: Vec<LightLayerData>,
    pub block_layers: Vec<LightLayerData>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LightLayerData {
    pub mask_index: usize,
    pub data: Vec<u8>,
}

impl ChunkLightSummary {
    #[must_use]
    pub fn sky(&self, section: usize, x: u8, y: u8, z: u8) -> Option<u8> {
        light_value(&self.sky_layers, section.checked_add(1)?, x, y, z)
            .or_else(|| mask_contains(&self.empty_sky_mask, section + 1).then_some(0))
    }

    #[must_use]
    pub fn block(&self, section: usize, x: u8, y: u8, z: u8) -> Option<u8> {
        light_value(&self.block_layers, section.checked_add(1)?, x, y, z)
            .or_else(|| mask_contains(&self.empty_block_mask, section + 1).then_some(0))
    }
}

fn light_value(layers: &[LightLayerData], mask_index: usize, x: u8, y: u8, z: u8) -> Option<u8> {
    if x >= 16 || y >= 16 || z >= 16 {
        return None;
    }
    let layer = layers.iter().find(|layer| layer.mask_index == mask_index)?;
    let index = usize::from(y) * 256 + usize::from(z) * 16 + usize::from(x);
    let byte = *layer.data.get(index / 2)?;
    Some(if index & 1 == 0 {
        byte & 0x0f
    } else {
        byte >> 4
    })
}

fn mask_contains(words: &[u64], index: usize) -> bool {
    words
        .get(index / 64)
        .is_some_and(|word| word & (1_u64 << (index % 64)) != 0)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Chunk {
    pub coordinate: ChunkCoordinate,
    /// Ordered lowest-to-highest as supplied by the current dimension.
    /// Phase 13 does not yet resolve the dimension's minimum section Y.
    pub sections: Vec<ChunkSection>,
    pub heightmaps: Vec<HeightmapData>,
    pub block_entities: Vec<BlockEntitySummary>,
    pub light: ChunkLightSummary,
}

impl Chunk {
    #[must_use]
    pub fn summary(&self) -> ChunkSummary {
        ChunkSummary {
            coordinate: self.coordinate,
            sections: self.sections.len(),
            non_empty_sections: self
                .sections
                .iter()
                .filter(|section| section.non_empty_block_count != 0)
                .count(),
            single_block_palettes: self
                .sections
                .iter()
                .filter(|section| section.blocks.form() == PaletteForm::Single)
                .count(),
            indirect_block_palettes: self
                .sections
                .iter()
                .filter(|section| section.blocks.form() == PaletteForm::Indirect)
                .count(),
            direct_block_palettes: self
                .sections
                .iter()
                .filter(|section| section.blocks.form() == PaletteForm::Direct)
                .count(),
            heightmaps: self.heightmaps.len(),
            block_entities: self.block_entities.len(),
            sky_layers: self.light.sky_layer_count,
            block_layers: self.light.block_layer_count,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChunkSummary {
    pub coordinate: ChunkCoordinate,
    pub sections: usize,
    pub non_empty_sections: usize,
    pub single_block_palettes: usize,
    pub indirect_block_palettes: usize,
    pub direct_block_palettes: usize,
    pub heightmaps: usize,
    pub block_entities: usize,
    pub sky_layers: usize,
    pub block_layers: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LoadedChunks {
    chunks: BTreeMap<ChunkCoordinate, Arc<Chunk>>,
}

impl LoadedChunks {
    #[must_use]
    pub fn len(&self) -> usize {
        self.chunks.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    #[must_use]
    pub fn get(&self, coordinate: ChunkCoordinate) -> Option<&Chunk> {
        self.chunks.get(&coordinate).map(Arc::as_ref)
    }

    #[must_use]
    pub fn get_shared(&self, coordinate: ChunkCoordinate) -> Option<Arc<Chunk>> {
        self.chunks.get(&coordinate).map(Arc::clone)
    }

    pub fn insert(&mut self, chunk: Chunk) -> Result<Option<Arc<Chunk>>, ChunkStoreError> {
        if !self.chunks.contains_key(&chunk.coordinate) && self.chunks.len() >= MAX_LOADED_CHUNKS {
            return Err(ChunkStoreError::LoadedChunkLimit {
                max: MAX_LOADED_CHUNKS,
            });
        }
        Ok(self.chunks.insert(chunk.coordinate, Arc::new(chunk)))
    }

    pub fn remove(&mut self, coordinate: ChunkCoordinate) -> Option<Arc<Chunk>> {
        self.chunks.remove(&coordinate)
    }

    pub fn clear(&mut self) {
        self.chunks.clear();
    }

    pub fn update_light(&mut self, coordinate: ChunkCoordinate, light: ChunkLightSummary) -> bool {
        let Some(chunk) = self.chunks.get_mut(&coordinate) else {
            return false;
        };
        Arc::make_mut(chunk).light = light;
        true
    }

    pub(crate) fn update_block(
        &mut self,
        coordinate: ChunkCoordinate,
        section_offset: usize,
        local_x: u8,
        local_y: u8,
        local_z: u8,
        state: RuntimeBlockStateId,
    ) -> bool {
        let Some(section) = self
            .chunks
            .get_mut(&coordinate)
            .and_then(|chunk| Arc::make_mut(chunk).sections.get_mut(section_offset))
        else {
            return false;
        };
        let Some(index) = section_index(local_x, local_y, local_z, 16) else {
            return false;
        };
        section.blocks.set(index, state)
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ChunkStoreError {
    #[error("loaded chunk count would exceed safety limit {max}")]
    LoadedChunkLimit { max: usize },
}

#[cfg(test)]
mod light_tests {
    use super::*;

    #[test]
    fn authoritative_light_nibbles_use_section_guard_offset_and_xyz_indexing() {
        let mut data = vec![0_u8; 2048];
        let index = 2 * 256 + 3 * 16 + 4;
        data[index / 2] = if index & 1 == 0 { 0x0b } else { 0xb0 };
        let light = ChunkLightSummary {
            sky_layers: vec![LightLayerData {
                mask_index: 1,
                data,
            }],
            ..ChunkLightSummary::default()
        };
        assert_eq!(light.sky(0, 4, 2, 3), Some(11));
        assert_eq!(light.sky(1, 4, 2, 3), None);
        assert_eq!(light.sky(0, 16, 2, 3), None);
    }

    #[test]
    fn explicitly_empty_light_sections_resolve_to_zero() {
        let light = ChunkLightSummary {
            empty_block_mask: vec![1_u64 << 2],
            ..ChunkLightSummary::default()
        };
        assert_eq!(light.block(1, 0, 0, 0), Some(0));
        assert_eq!(light.block(0, 0, 0, 0), None);
    }
}
