use cubic_world::{
    Chunk, ChunkCoordinate, ChunkLightSummary, ChunkSection, LoadedChunks, MAX_LOADED_CHUNKS,
    PalettedContainer, RuntimeBiomeId, RuntimeBlockStateId,
};

fn chunk(x: i32, z: i32, block: u32) -> Chunk {
    Chunk {
        coordinate: ChunkCoordinate::new(x, z),
        sections: vec![ChunkSection {
            non_empty_block_count: u16::from(block != 0),
            fluid_count: 0,
            blocks: PalettedContainer::Single {
                value: RuntimeBlockStateId(block),
                entries: 4_096,
            },
            biomes: PalettedContainer::Single {
                value: RuntimeBiomeId(2),
                entries: 64,
            },
        }],
        heightmaps: Vec::new(),
        block_entities: Vec::new(),
        light: ChunkLightSummary::default(),
    }
}

#[test]
fn coordinates_order_deterministically_and_preserve_negatives() {
    let mut values = [
        ChunkCoordinate::new(0, 0),
        ChunkCoordinate::new(-1, 7),
        ChunkCoordinate::new(-1, -2),
    ];
    values.sort();
    assert_eq!(
        values,
        [
            ChunkCoordinate::new(-1, -2),
            ChunkCoordinate::new(-1, 7),
            ChunkCoordinate::new(0, 0)
        ]
    );
}

#[test]
fn section_lookup_uses_x_fastest_and_checks_coordinates() {
    let values: Vec<_> = (0..4_096).map(RuntimeBlockStateId).collect();
    let section = ChunkSection {
        non_empty_block_count: 4_096,
        fluid_count: 0,
        blocks: PalettedContainer::Direct { values },
        biomes: PalettedContainer::Single {
            value: RuntimeBiomeId(1),
            entries: 64,
        },
    };
    assert_eq!(
        section.block(3, 2, 1),
        Some(RuntimeBlockStateId((2 * 16 * 16 + 16 + 3) as u32))
    );
    assert_eq!(section.block(16, 0, 0), None);
    assert_eq!(section.biome(3, 3, 3), Some(RuntimeBiomeId(1)));
}

#[test]
fn store_replaces_unloads_and_updates_light_without_growth() {
    let mut store = LoadedChunks::default();
    assert!(store.insert(chunk(-2, 3, 1)).unwrap().is_none());
    assert!(store.insert(chunk(-2, 3, 9)).unwrap().is_some());
    assert_eq!(store.len(), 1);
    assert_eq!(
        store.get(ChunkCoordinate::new(-2, 3)).unwrap().sections[0].block(0, 0, 0),
        Some(RuntimeBlockStateId(9))
    );
    assert!(store.update_light(
        ChunkCoordinate::new(-2, 3),
        ChunkLightSummary {
            sky_layer_count: 2,
            ..Default::default()
        }
    ));
    assert_eq!(
        store
            .remove(ChunkCoordinate::new(-2, 3))
            .unwrap()
            .coordinate,
        ChunkCoordinate::new(-2, 3)
    );
    assert!(store.is_empty());
}

#[test]
fn loaded_chunk_count_is_bounded() {
    let mut store = LoadedChunks::default();
    for index in 0..MAX_LOADED_CHUNKS {
        store.insert(chunk(index as i32, 0, 0)).unwrap();
    }
    assert!(store.insert(chunk(-1, 0, 0)).is_err());
    assert_eq!(store.len(), MAX_LOADED_CHUNKS);
}
