use std::collections::BTreeSet;

use thiserror::Error;

use crate::{
    BitSetLimits, CodecError, CodecReader,
    nbt::{NbtError, NbtLimits, NbtTag, decode_unnamed_network_tag},
};

pub const MAX_CHUNK_DATA_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_CHUNK_SECTIONS: usize = 64;
pub const MAX_HEIGHTMAPS: usize = 16;
pub const MAX_HEIGHTMAP_LONGS: usize = 256;
pub const MAX_BLOCK_ENTITIES: usize = 1_024;
pub const MAX_LIGHT_LAYERS: usize = 66;
pub const LIGHT_LAYER_BYTES: usize = 2_048;

const BLOCK_ENTRIES: usize = 4_096;
const BIOME_ENTRIES: usize = 64;
const MAX_BLOCK_BITS: u8 = 15;
const MAX_BIOME_BITS: u8 = 15;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WirePalettedContainer {
    Single {
        value: u32,
        entries: usize,
    },
    Indirect {
        palette: Vec<u32>,
        indices: Vec<u16>,
    },
    Direct {
        values: Vec<u32>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WireChunkSection {
    pub non_empty_block_count: u16,
    pub fluid_count: u16,
    pub blocks: WirePalettedContainer,
    pub biomes: WirePalettedContainer,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WireHeightmap {
    pub kind_raw_id: u32,
    pub data: Vec<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WireBlockEntity {
    pub local_x: u8,
    pub y: i16,
    pub local_z: u8,
    pub type_raw_id: u32,
    pub has_data: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WireLightData {
    pub sky_mask: Vec<u64>,
    pub block_mask: Vec<u64>,
    pub empty_sky_mask: Vec<u64>,
    pub empty_block_mask: Vec<u64>,
    pub sky_layer_count: usize,
    pub block_layer_count: usize,
    pub sky_layers: Vec<WireLightLayer>,
    pub block_layers: Vec<WireLightLayer>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WireLightLayer {
    pub mask_index: usize,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LevelChunkWithLight {
    pub x: i32,
    pub z: i32,
    pub sections: Vec<WireChunkSection>,
    pub heightmaps: Vec<WireHeightmap>,
    pub block_entities: Vec<WireBlockEntity>,
    pub light: WireLightData,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LightUpdate {
    pub x: i32,
    pub z: i32,
    pub light: WireLightData,
}

#[derive(Debug, Error)]
pub enum ChunkDecodeError {
    #[error(transparent)]
    Codec(#[from] CodecError),
    #[error("malformed bounded chunk NBT")]
    Nbt(#[source] NbtError),
    #[error("{context} value {value} is negative")]
    Negative { context: &'static str, value: i32 },
    #[error("{context} count {count} exceeds limit {max}")]
    CountTooLarge {
        context: &'static str,
        count: usize,
        max: usize,
    },
    #[error("invalid {container} bits-per-entry {bits}")]
    InvalidBits { container: &'static str, bits: u8 },
    #[error("{container} palette length {length} is invalid for {bits} storage bits")]
    InvalidPaletteLength {
        container: &'static str,
        length: usize,
        bits: u8,
    },
    #[error("{container} palette contains duplicate runtime ID {value}")]
    DuplicatePaletteValue { container: &'static str, value: u32 },
    #[error(
        "{container} palette index {index} at entry {entry} exceeds palette length {palette_len}"
    )]
    PaletteIndexOutOfRange {
        container: &'static str,
        entry: usize,
        index: usize,
        palette_len: usize,
    },
    #[error("{container} packed storage requires {expected} words, not {actual}")]
    PackedWordCount {
        container: &'static str,
        expected: usize,
        actual: usize,
    },
    #[error("chunk section count exceeds limit {max}")]
    SectionLimit { max: usize },
    #[error("chunk section buffer contains no sections")]
    EmptySections,
    #[error("{field} value {value} exceeds section entry count {max}")]
    InvalidSectionCount {
        field: &'static str,
        value: i16,
        max: usize,
    },
    #[error("duplicate heightmap kind raw ID {0}")]
    DuplicateHeightmap(u32),
    #[error("block entity NBT root must be a compound when present")]
    InvalidBlockEntityNbt,
    #[error("{kind} light mask overlaps its empty-light mask")]
    OverlappingLightMasks { kind: &'static str },
    #[error("{kind} light mask has {mask_bits} set bits but {layers} data layers")]
    LightLayerCount {
        kind: &'static str,
        mask_bits: usize,
        layers: usize,
    },
    #[error("{kind} light layer has {length} bytes; expected {expected}")]
    LightLayerLength {
        kind: &'static str,
        length: usize,
        expected: usize,
    },
}

pub fn decode_level_chunk_with_light(
    reader: &mut CodecReader<'_>,
) -> Result<LevelChunkWithLight, ChunkDecodeError> {
    let x = reader.read_i32()?;
    let z = reader.read_i32()?;
    let heightmaps = decode_heightmaps(reader)?;
    let section_bytes = reader.read_byte_array(MAX_CHUNK_DATA_BYTES)?;
    let sections = decode_sections(section_bytes)?;
    let block_entities = decode_block_entities(reader)?;
    let light = decode_light_data(reader)?;
    Ok(LevelChunkWithLight {
        x,
        z,
        sections,
        heightmaps,
        block_entities,
        light,
    })
}

pub fn decode_forget_level_chunk(
    reader: &mut CodecReader<'_>,
) -> Result<(i32, i32), ChunkDecodeError> {
    let packed = reader.read_u64()?;
    Ok((packed as u32 as i32, (packed >> 32) as u32 as i32))
}

pub fn decode_light_update(reader: &mut CodecReader<'_>) -> Result<LightUpdate, ChunkDecodeError> {
    let x = reader.read_var_int()?;
    let z = reader.read_var_int()?;
    let light = decode_light_data(reader)?;
    Ok(LightUpdate { x, z, light })
}

fn decode_sections(bytes: &[u8]) -> Result<Vec<WireChunkSection>, ChunkDecodeError> {
    let mut reader = CodecReader::new(bytes);
    let mut sections = Vec::new();
    while reader.remaining() != 0 {
        if sections.len() >= MAX_CHUNK_SECTIONS {
            return Err(ChunkDecodeError::SectionLimit {
                max: MAX_CHUNK_SECTIONS,
            });
        }
        let non_empty = reader.read_i16()?;
        validate_section_count("non-empty block count", non_empty, BLOCK_ENTRIES)?;
        let fluid = reader.read_i16()?;
        validate_section_count("fluid count", fluid, BLOCK_ENTRIES)?;
        sections.push(WireChunkSection {
            non_empty_block_count: non_empty as u16,
            fluid_count: fluid as u16,
            blocks: decode_container(&mut reader, ContainerKind::Blocks)?,
            biomes: decode_container(&mut reader, ContainerKind::Biomes)?,
        });
    }
    if sections.is_empty() {
        return Err(ChunkDecodeError::EmptySections);
    }
    Ok(sections)
}

fn validate_section_count(
    field: &'static str,
    value: i16,
    max: usize,
) -> Result<(), ChunkDecodeError> {
    if value < 0 || usize::from(value as u16) > max {
        return Err(ChunkDecodeError::InvalidSectionCount { field, value, max });
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum ContainerKind {
    Blocks,
    Biomes,
}

impl ContainerKind {
    const fn name(self) -> &'static str {
        match self {
            Self::Blocks => "block-state",
            Self::Biomes => "biome",
        }
    }
    const fn entries(self) -> usize {
        match self {
            Self::Blocks => BLOCK_ENTRIES,
            Self::Biomes => BIOME_ENTRIES,
        }
    }
    const fn max_bits(self) -> u8 {
        match self {
            Self::Blocks => MAX_BLOCK_BITS,
            Self::Biomes => MAX_BIOME_BITS,
        }
    }
    const fn local_max(self) -> u8 {
        match self {
            Self::Blocks => 8,
            Self::Biomes => 3,
        }
    }
    const fn storage_bits(self, wire_bits: u8) -> u8 {
        match self {
            Self::Blocks if wire_bits > 0 && wire_bits < 4 => 4,
            _ => wire_bits,
        }
    }
}

fn decode_container(
    reader: &mut CodecReader<'_>,
    kind: ContainerKind,
) -> Result<WirePalettedContainer, ChunkDecodeError> {
    let wire_bits = reader.read_u8()?;
    if wire_bits > kind.max_bits() {
        return Err(ChunkDecodeError::InvalidBits {
            container: kind.name(),
            bits: wire_bits,
        });
    }
    if wire_bits == 0 {
        return Ok(WirePalettedContainer::Single {
            value: read_runtime_id(reader, kind.name())?,
            entries: kind.entries(),
        });
    }
    let storage_bits = kind.storage_bits(wire_bits);
    if wire_bits <= kind.local_max() {
        let palette_len = read_count(reader, "palette", 1_usize << storage_bits)?;
        if palette_len == 0 || palette_len > (1_usize << storage_bits) {
            return Err(ChunkDecodeError::InvalidPaletteLength {
                container: kind.name(),
                length: palette_len,
                bits: storage_bits,
            });
        }
        let mut palette = Vec::new();
        palette
            .try_reserve_exact(palette_len)
            .map_err(|_| CodecError::AllocationFailed {
                context: "chunk palette",
                requested: palette_len,
            })?;
        let mut seen = BTreeSet::new();
        for _ in 0..palette_len {
            let value = read_runtime_id(reader, kind.name())?;
            if !seen.insert(value) {
                return Err(ChunkDecodeError::DuplicatePaletteValue {
                    container: kind.name(),
                    value,
                });
            }
            palette.push(value);
        }
        let indices = read_packed(reader, storage_bits, kind.entries(), kind.name())?;
        for (entry, index) in indices.iter().copied().enumerate() {
            if usize::from(index) >= palette.len() {
                return Err(ChunkDecodeError::PaletteIndexOutOfRange {
                    container: kind.name(),
                    entry,
                    index: usize::from(index),
                    palette_len: palette.len(),
                });
            }
        }
        Ok(WirePalettedContainer::Indirect { palette, indices })
    } else {
        let raw = read_packed(reader, storage_bits, kind.entries(), kind.name())?;
        Ok(WirePalettedContainer::Direct {
            values: raw.into_iter().map(u32::from).collect(),
        })
    }
}

fn read_runtime_id(
    reader: &mut CodecReader<'_>,
    context: &'static str,
) -> Result<u32, ChunkDecodeError> {
    let value = reader.read_var_int()?;
    u32::try_from(value).map_err(|_| ChunkDecodeError::Negative { context, value })
}

fn read_count(
    reader: &mut CodecReader<'_>,
    context: &'static str,
    max: usize,
) -> Result<usize, ChunkDecodeError> {
    let value = reader.read_var_int()?;
    if value < 0 {
        return Err(ChunkDecodeError::Negative { context, value });
    }
    let count = usize::try_from(value).map_err(|_| ChunkDecodeError::CountTooLarge {
        context,
        count: usize::MAX,
        max,
    })?;
    if count > max {
        return Err(ChunkDecodeError::CountTooLarge {
            context,
            count,
            max,
        });
    }
    Ok(count)
}

fn packed_word_count(bits: u8, entries: usize) -> Option<usize> {
    let per_word = 64_usize.checked_div(usize::from(bits))?;
    (per_word != 0).then(|| entries.div_ceil(per_word))
}

fn read_packed(
    reader: &mut CodecReader<'_>,
    bits: u8,
    entries: usize,
    context: &'static str,
) -> Result<Vec<u16>, ChunkDecodeError> {
    let words = packed_word_count(bits, entries).ok_or(ChunkDecodeError::InvalidBits {
        container: context,
        bits,
    })?;
    let mut packed = Vec::new();
    packed
        .try_reserve_exact(words)
        .map_err(|_| CodecError::AllocationFailed {
            context: "packed chunk words",
            requested: words,
        })?;
    for _ in 0..words {
        packed.push(reader.read_u64()?);
    }
    unpack_packed(&packed, bits, entries, context)
}

fn unpack_packed(
    words: &[u64],
    bits: u8,
    entries: usize,
    context: &'static str,
) -> Result<Vec<u16>, ChunkDecodeError> {
    let expected = packed_word_count(bits, entries).ok_or(ChunkDecodeError::InvalidBits {
        container: context,
        bits,
    })?;
    if words.len() != expected {
        return Err(ChunkDecodeError::PackedWordCount {
            container: context,
            expected,
            actual: words.len(),
        });
    }
    let per_word = 64 / usize::from(bits);
    let mask = (1_u64 << bits) - 1;
    let mut values = Vec::new();
    values
        .try_reserve_exact(entries)
        .map_err(|_| CodecError::AllocationFailed {
            context: "unpacked chunk entries",
            requested: entries,
        })?;
    for index in 0..entries {
        let word = words[index / per_word];
        let shift = (index % per_word) * usize::from(bits);
        values.push(((word >> shift) & mask) as u16);
    }
    Ok(values)
}

fn decode_heightmaps(reader: &mut CodecReader<'_>) -> Result<Vec<WireHeightmap>, ChunkDecodeError> {
    let count = read_count(reader, "heightmap", MAX_HEIGHTMAPS)?;
    let mut maps = Vec::new();
    maps.try_reserve_exact(count)
        .map_err(|_| CodecError::AllocationFailed {
            context: "heightmaps",
            requested: count,
        })?;
    let mut seen = BTreeSet::new();
    for _ in 0..count {
        let kind_raw_id = read_runtime_id(reader, "heightmap kind")?;
        if !seen.insert(kind_raw_id) {
            return Err(ChunkDecodeError::DuplicateHeightmap(kind_raw_id));
        }
        let longs = read_count(reader, "heightmap long", MAX_HEIGHTMAP_LONGS)?;
        let mut data = Vec::new();
        data.try_reserve_exact(longs)
            .map_err(|_| CodecError::AllocationFailed {
                context: "heightmap longs",
                requested: longs,
            })?;
        for _ in 0..longs {
            data.push(reader.read_u64()?);
        }
        maps.push(WireHeightmap { kind_raw_id, data });
    }
    Ok(maps)
}

fn decode_block_entities(
    reader: &mut CodecReader<'_>,
) -> Result<Vec<WireBlockEntity>, ChunkDecodeError> {
    let count = read_count(reader, "block entity", MAX_BLOCK_ENTITIES)?;
    let mut entities = Vec::new();
    entities
        .try_reserve_exact(count)
        .map_err(|_| CodecError::AllocationFailed {
            context: "block entities",
            requested: count,
        })?;
    for _ in 0..count {
        let packed_xz = reader.read_u8()?;
        let y = reader.read_i16()?;
        let type_raw_id = read_runtime_id(reader, "block entity type")?;
        let mut probe = reader.clone();
        let root_type = probe.read_u8()?;
        let has_data = if root_type == 0 {
            reader.read_u8()?;
            false
        } else {
            if !matches!(
                decode_unnamed_network_tag(reader, NbtLimits::default())
                    .map_err(ChunkDecodeError::Nbt)?,
                NbtTag::Compound(_)
            ) {
                return Err(ChunkDecodeError::InvalidBlockEntityNbt);
            }
            true
        };
        entities.push(WireBlockEntity {
            local_x: packed_xz >> 4,
            y,
            local_z: packed_xz & 0x0f,
            type_raw_id,
            has_data,
        });
    }
    Ok(entities)
}

fn decode_light_data(reader: &mut CodecReader<'_>) -> Result<WireLightData, ChunkDecodeError> {
    let limits = BitSetLimits::new(2, MAX_LIGHT_LAYERS);
    let sky = reader.read_bitset(limits)?;
    let block = reader.read_bitset(limits)?;
    let empty_sky = reader.read_bitset(limits)?;
    let empty_block = reader.read_bitset(limits)?;
    validate_disjoint("sky", sky.words(), empty_sky.words())?;
    validate_disjoint("block", block.words(), empty_block.words())?;
    let sky_layers = decode_light_layers(reader, "sky", sky.words())?;
    let block_layers = decode_light_layers(reader, "block", block.words())?;
    Ok(WireLightData {
        sky_mask: sky.words().to_vec(),
        block_mask: block.words().to_vec(),
        empty_sky_mask: empty_sky.words().to_vec(),
        empty_block_mask: empty_block.words().to_vec(),
        sky_layer_count: sky_layers.len(),
        block_layer_count: block_layers.len(),
        sky_layers,
        block_layers,
    })
}

fn validate_disjoint(
    kind: &'static str,
    data: &[u64],
    empty: &[u64],
) -> Result<(), ChunkDecodeError> {
    if data
        .iter()
        .zip(empty)
        .any(|(left, right)| left & right != 0)
    {
        return Err(ChunkDecodeError::OverlappingLightMasks { kind });
    }
    Ok(())
}

fn count_bits(words: &[u64]) -> usize {
    words.iter().map(|word| word.count_ones() as usize).sum()
}

fn decode_light_layers(
    reader: &mut CodecReader<'_>,
    kind: &'static str,
    mask: &[u64],
) -> Result<Vec<WireLightLayer>, ChunkDecodeError> {
    let expected = count_bits(mask);
    let count = read_count(reader, "light layer", MAX_LIGHT_LAYERS)?;
    if count != expected {
        return Err(ChunkDecodeError::LightLayerCount {
            kind,
            mask_bits: expected,
            layers: count,
        });
    }
    let mut layers = Vec::with_capacity(count);
    for mask_index in set_bit_indices(mask) {
        let bytes = reader.read_byte_array(LIGHT_LAYER_BYTES)?;
        if bytes.len() != LIGHT_LAYER_BYTES {
            return Err(ChunkDecodeError::LightLayerLength {
                kind,
                length: bytes.len(),
                expected: LIGHT_LAYER_BYTES,
            });
        }
        layers.push(WireLightLayer {
            mask_index,
            data: bytes.to_vec(),
        });
    }
    Ok(layers)
}

fn set_bit_indices(words: &[u64]) -> impl Iterator<Item = usize> + '_ {
    words.iter().enumerate().flat_map(|(word_index, word)| {
        (0..64).filter_map(move |bit| (word & (1_u64 << bit) != 0).then_some(word_index * 64 + bit))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn pack(values: &[u16], bits: u8) -> Vec<u64> {
        let per_word = 64 / usize::from(bits);
        let mut words = vec![0; values.len().div_ceil(per_word)];
        for (index, value) in values.iter().copied().enumerate() {
            words[index / per_word] |= u64::from(value) << ((index % per_word) * usize::from(bits));
        }
        words
    }

    #[test]
    fn awkward_bit_widths_do_not_cross_word_boundaries() {
        for bits in [1, 2, 3, 4, 5, 6, 8, 15] {
            let mask = (1_u16 << bits) - 1;
            let values: Vec<_> = (0..137).map(|index| (index as u16) & mask).collect();
            assert_eq!(
                unpack_packed(&pack(&values, bits), bits, values.len(), "test").unwrap(),
                values
            );
        }
    }

    #[test]
    fn packed_storage_rejects_truncated_and_excess_words() {
        assert!(matches!(
            unpack_packed(&[], 5, 13, "test"),
            Err(ChunkDecodeError::PackedWordCount { .. })
        ));
        assert!(matches!(
            unpack_packed(&[0, 0, 0], 5, 13, "test"),
            Err(ChunkDecodeError::PackedWordCount { .. })
        ));
    }

    proptest! {
        #[test]
        fn packed_round_trip(values in proptest::collection::vec(0_u16..32, 0..300)) {
            let words = pack(&values, 5);
            prop_assert_eq!(unpack_packed(&words, 5, values.len(), "test").unwrap(), values);
        }
    }
}
