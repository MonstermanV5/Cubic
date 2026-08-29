use cubic_protocol::{CodecWriter, bootstrap::v775};

fn packed(values: &[u16], bits: u8) -> Vec<u64> {
    let per_word = 64 / usize::from(bits);
    let mut words = vec![0; values.len().div_ceil(per_word)];
    for (index, value) in values.iter().copied().enumerate() {
        words[index / per_word] |= u64::from(value) << ((index % per_word) * usize::from(bits));
    }
    words
}

fn single_section(block: u32, biome: u32) -> Vec<u8> {
    let mut section = CodecWriter::new();
    section.write_i16(0);
    section.write_i16(0);
    section.write_u8(0);
    section.write_var_int(block as i32);
    section.write_u8(0);
    section.write_var_int(biome as i32);
    section.into_inner()
}

fn local_section() -> Vec<u8> {
    let mut section = CodecWriter::new();
    section.write_i16(2);
    section.write_i16(0);
    section.write_u8(4);
    section.write_var_int(2);
    section.write_var_int(5);
    section.write_var_int(9);
    let values: Vec<_> = (0..4_096).map(|index| (index % 2) as u16).collect();
    for word in packed(&values, 4) {
        section.write_u64(word);
    }
    section.write_u8(1);
    section.write_var_int(2);
    section.write_var_int(3);
    section.write_var_int(7);
    let biomes: Vec<_> = (0..64).map(|index| (index % 2) as u16).collect();
    for word in packed(&biomes, 1) {
        section.write_u64(word);
    }
    section.into_inner()
}

fn direct_section() -> Vec<u8> {
    let mut section = CodecWriter::new();
    section.write_i16(1);
    section.write_i16(1);
    section.write_u8(15);
    let blocks: Vec<_> = (0..4_096).map(|index| (index % 30_000) as u16).collect();
    for word in packed(&blocks, 15) {
        section.write_u64(word);
    }
    section.write_u8(4);
    let biomes: Vec<_> = (0..64).map(|index| (index % 12) as u16).collect();
    for word in packed(&biomes, 4) {
        section.write_u64(word);
    }
    section.into_inner()
}

fn chunk_packet(x: i32, z: i32, sections: &[u8]) -> Vec<u8> {
    let mut packet = CodecWriter::new();
    packet.write_var_int(0x2d);
    packet.write_i32(x);
    packet.write_i32(z);
    packet.write_var_int(1); // one heightmap
    packet.write_var_int(2); // reviewed raw heightmap kind ID
    packet.write_var_int(1);
    packet.write_u64(0x0123_4567_89ab_cdef);
    packet.write_byte_array(sections, 2 * 1024 * 1024).unwrap();
    packet.write_var_int(0); // block entities
    for _ in 0..4 {
        packet.write_var_int(0);
    } // light masks
    packet.write_var_int(0); // sky layers
    packet.write_var_int(0); // block layers
    packet.into_inner()
}

#[test]
fn independent_single_palette_chunk_vector_decodes_negative_coordinates() {
    let decoded =
        v775::decode_play_clientbound(&chunk_packet(-4, 9, &single_section(12, 3))).unwrap();
    let v775::PlayClientbound::LevelChunkWithLight(chunk) = decoded else {
        panic!("expected chunk")
    };
    assert_eq!((chunk.x, chunk.z), (-4, 9));
    assert_eq!(chunk.sections.len(), 1);
    assert_eq!(chunk.heightmaps[0].data, [0x0123_4567_89ab_cdef]);
    assert_eq!(
        chunk.sections[0].blocks,
        v775::WirePalettedContainer::Single {
            value: 12,
            entries: 4_096
        }
    );
    assert_eq!(
        chunk.sections[0].biomes,
        v775::WirePalettedContainer::Single {
            value: 3,
            entries: 64
        }
    );
}

#[test]
fn indirect_and_direct_palettes_are_canonical_and_deterministic() {
    let mut sections = local_section();
    sections.extend(direct_section());
    let packet = chunk_packet(1, 2, &sections);
    let first = v775::decode_play_clientbound(&packet).unwrap();
    let second = v775::decode_play_clientbound(&packet).unwrap();
    assert_eq!(first, second);
    let v775::PlayClientbound::LevelChunkWithLight(chunk) = first else {
        panic!("expected chunk")
    };
    assert_eq!(chunk.sections.len(), 2);
    let v775::WirePalettedContainer::Indirect { palette, indices } = &chunk.sections[0].blocks
    else {
        panic!("expected local palette")
    };
    assert_eq!(palette, &[5, 9]);
    assert_eq!(&indices[..4], &[0, 1, 0, 1]);
    let v775::WirePalettedContainer::Direct { values } = &chunk.sections[1].blocks else {
        panic!("expected direct palette")
    };
    assert_eq!(&values[..5], &[0, 1, 2, 3, 4]);
}

#[test]
fn packed_forget_chunk_vector_preserves_signed_halves() {
    let packed = (u64::from((-7_i32) as u32) & 0xffff_ffff) | (u64::from(11_u32) << 32);
    let mut packet = CodecWriter::new();
    packet.write_var_int(0x25);
    packet.write_u64(packed);
    assert_eq!(
        v775::decode_play_clientbound(&packet.into_inner()).unwrap(),
        v775::PlayClientbound::ForgetLevelChunk { x: -7, z: 11 }
    );
}

#[test]
fn malformed_palette_index_and_truncated_chunk_are_structured_errors() {
    let mut section = CodecWriter::new();
    section.write_i16(1);
    section.write_i16(0);
    section.write_u8(4);
    section.write_var_int(1);
    section.write_var_int(5);
    for _ in 0..256 {
        section.write_u64(1);
    }
    section.write_u8(0);
    section.write_var_int(0);
    let error =
        v775::decode_play_clientbound(&chunk_packet(0, 0, &section.into_inner())).unwrap_err();
    assert!(error.to_string().contains("palette index"));

    let mut truncated = chunk_packet(0, 0, &single_section(0, 0));
    truncated.pop();
    assert!(v775::decode_play_clientbound(&truncated).is_err());
}

#[test]
fn hostile_lengths_and_malformed_light_are_rejected_before_retention() {
    let mut huge = CodecWriter::new();
    huge.write_var_int(0x2d);
    huge.write_i32(0);
    huge.write_i32(0);
    huge.write_var_int(17);
    assert!(v775::decode_play_clientbound(&huge.into_inner()).is_err());

    let mut light = CodecWriter::new();
    light.write_var_int(0x30);
    light.write_var_int(0);
    light.write_var_int(0);
    light.write_var_int(1);
    light.write_u64(1);
    light.write_var_int(0);
    light.write_var_int(1);
    light.write_u64(1); // overlap sky/empty sky
    light.write_var_int(0);
    light.write_var_int(1);
    light.write_byte_array(&vec![0; 2_048], 2_048).unwrap();
    light.write_var_int(0);
    assert!(v775::decode_play_clientbound(&light.into_inner()).is_err());
}

#[test]
fn invalid_bits_empty_sections_and_malformed_biomes_are_rejected() {
    let mut invalid_blocks = single_section(0, 0);
    invalid_blocks[4] = 16;
    assert!(v775::decode_play_clientbound(&chunk_packet(0, 0, &invalid_blocks)).is_err());

    assert!(v775::decode_play_clientbound(&chunk_packet(0, 0, &[])).is_err());

    let mut malformed_biome = CodecWriter::new();
    malformed_biome.write_i16(0);
    malformed_biome.write_i16(0);
    malformed_biome.write_u8(0);
    malformed_biome.write_var_int(0);
    malformed_biome.write_u8(1);
    malformed_biome.write_var_int(0); // empty indirect biome palette
    assert!(
        v775::decode_play_clientbound(&chunk_packet(0, 0, &malformed_biome.into_inner())).is_err()
    );

    let mut invalid_count = single_section(0, 0);
    invalid_count[..2].copy_from_slice(&4_097_i16.to_be_bytes());
    assert!(v775::decode_play_clientbound(&chunk_packet(0, 0, &invalid_count)).is_err());

    let mut oversized_palette = CodecWriter::new();
    oversized_palette.write_i16(0);
    oversized_palette.write_i16(0);
    oversized_palette.write_u8(4);
    oversized_palette.write_var_int(17);
    assert!(
        v775::decode_play_clientbound(&chunk_packet(0, 0, &oversized_palette.into_inner()))
            .is_err()
    );
}

#[test]
fn valid_standalone_light_update_is_summarized() {
    let mut packet = CodecWriter::new();
    packet.write_var_int(0x30);
    packet.write_var_int(-2);
    packet.write_var_int(5);
    packet.write_var_int(1);
    packet.write_u64(1);
    packet.write_var_int(0);
    packet.write_var_int(0);
    packet.write_var_int(1);
    packet.write_u64(2);
    packet.write_var_int(1);
    packet.write_byte_array(&vec![0x7f; 2_048], 2_048).unwrap();
    packet.write_var_int(0);
    let v775::PlayClientbound::LightUpdate(update) =
        v775::decode_play_clientbound(&packet.into_inner()).unwrap()
    else {
        panic!("expected light update")
    };
    assert_eq!((update.x, update.z), (-2, 5));
    assert_eq!(update.light.sky_layer_count, 1);
    assert_eq!(update.light.sky_mask, [1]);
    assert_eq!(update.light.empty_block_mask, [2]);
}

#[test]
fn block_entity_compound_is_bounded_and_summarized() {
    let mut packet = CodecWriter::new();
    packet.write_var_int(0x2d);
    packet.write_i32(0);
    packet.write_i32(0);
    packet.write_var_int(0);
    packet
        .write_byte_array(&single_section(0, 0), 2 * 1024 * 1024)
        .unwrap();
    packet.write_var_int(1);
    packet.write_u8(0xa3);
    packet.write_i16(-12);
    packet.write_var_int(4);
    packet.write_bytes(&[10, 0]); // unnamed empty compound
    for _ in 0..4 {
        packet.write_var_int(0);
    }
    packet.write_var_int(0);
    packet.write_var_int(0);
    let v775::PlayClientbound::LevelChunkWithLight(chunk) =
        v775::decode_play_clientbound(&packet.into_inner()).unwrap()
    else {
        panic!("expected chunk")
    };
    assert_eq!(
        (
            chunk.block_entities[0].local_x,
            chunk.block_entities[0].local_z
        ),
        (10, 3)
    );
    assert_eq!(chunk.block_entities[0].y, -12);
    assert!(chunk.block_entities[0].has_data);
}
