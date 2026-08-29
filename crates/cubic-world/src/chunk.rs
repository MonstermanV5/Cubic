use std::collections::BTreeMap;

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

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
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
    chunks: BTreeMap<ChunkCoordinate, Chunk>,
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
        self.chunks.get(&coordinate)
    }

    pub fn insert(&mut self, chunk: Chunk) -> Result<Option<Chunk>, ChunkStoreError> {
        if !self.chunks.contains_key(&chunk.coordinate) && self.chunks.len() >= MAX_LOADED_CHUNKS {
            return Err(ChunkStoreError::LoadedChunkLimit {
                max: MAX_LOADED_CHUNKS,
            });
        }
        Ok(self.chunks.insert(chunk.coordinate, chunk))
    }

    pub fn remove(&mut self, coordinate: ChunkCoordinate) -> Option<Chunk> {
        self.chunks.remove(&coordinate)
    }

    pub fn clear(&mut self) {
        self.chunks.clear();
    }

    pub fn update_light(&mut self, coordinate: ChunkCoordinate, light: ChunkLightSummary) -> bool {
        let Some(chunk) = self.chunks.get_mut(&coordinate) else {
            return false;
        };
        chunk.light = light;
        true
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ChunkStoreError {
    #[error("loaded chunk count would exceed safety limit {max}")]
    LoadedChunkLimit { max: usize },
}
