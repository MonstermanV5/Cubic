# Chunk decoding

Phase 14 adds bounded semantic terrain state without rendering it. The data path is:

```text
26.1.2 / protocol-775 packet
  -> bootstrap::v775 bounded wire decoder
  -> cubic-network adapter
  -> cubic-world WorldEvent
  -> connection-owned LoadedChunks
```

`cubic-world` contains no packet IDs and does not know that the first reviewed implementation is protocol 775. Runtime block-state and biome values remain typed raw IDs. They are not treated as generated vanilla IDs: a later server-authoritative runtime registry resolver may assign their names and properties.

## Reviewed 26.1.2 wire format

Phase 12's official generated report is authoritative for packet identities and IDs but classifies these essential packets as `IdentityOnly`. The narrow field layouts were independently reimplemented after read-only inspection of the installed official 26.1.2 classes (`ClientboundLevelChunkWithLightPacket`, `ClientboundLevelChunkPacketData`, `LevelChunkSection`, `PalettedContainer`, `ClientboundLightUpdatePacketData`, `ClientboundForgetLevelChunkPacket`, and `ChunkPos`) and cross-checking the pinned structural data already documented for Phase 12.

- `level_chunk_with_light` (`0x2d`) contains big-endian `i32` X/Z, a bounded heightmap map, a VarInt-length section byte buffer, bounded block-entity summaries, and light data.
- `forget_level_chunk` (`0x25`) contains one packed `i64`: signed X in the low 32 bits and signed Z in the high 32 bits.
- `light_update` (`0x30`) contains VarInt X/Z and the same bounded light-data structure.
- Chunk-batch start/finish remain control-plane packets; the existing batch acknowledgement behavior is unchanged.

Each current section contains two big-endian signed shorts (non-empty block count and fluid count), then a 4,096-entry block-state paletted container and a 64-entry biome paletted container. Section count is derived by consuming the exact bounded section buffer, not by assuming classic world height. Sections are stored lowest-to-highest, but their absolute minimum section Y remains unresolved until authoritative dimension-height metadata is modeled.

## Palettes and packed storage

Canonical containers are `Single` (one value and logical length), `Indirect` (a bounded runtime-ID palette plus unpacked `u16` indices), or `Direct` (unpacked typed runtime IDs). Protocol 775 uses non-crossing packed entries: each word contains `floor(64 / bits)` entries, unused high bits are padding, and entries never straddle words. The decoder computes the exact word count; the current container has no independent long-array count.

Block local palettes normalize wire widths 1–3 to four storage bits, matching current vanilla behavior; block widths 5–8 are local and larger reviewed widths are direct. Biome widths 1–3 are local and larger widths are direct. Zero selects the single-value form. All widths, runtime IDs, palette lengths/duplicates/indices, computed word counts, shifts, counts, truncation, and trailing bytes are validated. Lookup uses checked local coordinates and X-fastest index `(y * width + z) * width + x`.

## Auxiliary data and bounds

- Section data: at most 2 MiB and 64 decoded sections.
- Loaded chunks: at most 512 coordinate-keyed entries. Replacement is deterministic; a new coordinate beyond the cap is an explicit error.
- Heightmaps: at most 16 distinct raw kinds and 256 longs per map, retained as raw semantic attachments.
- Block entities: at most 1,024 summaries per chunk. Bounded NBT is validated; only local position, typed raw type ID, and compound-data presence are retained.
- Lighting: masks cover at most 66 bits, data/empty masks must not overlap, layer counts must equal mask cardinality, and every data layer must be exactly 2,048 bytes. Layer bytes are validated and discarded; masks/counts are retained.

The pessimistic direct-palette representation is about 1.05 MiB for a 64-section chunk before allocator/metadata overhead. The 512-chunk cap therefore limits dominant canonical block arrays to roughly 0.54 GiB. Ordinary modern dimensions use far fewer than 64 sections. This is a local safety ceiling, not Phase 30's global memory budget or a renderer cache policy.

## Lifecycle and boundaries

The network task remains sole owner of `WorldState`. Load replaces by signed coordinate, unload removes explicitly, and every `WorldContents`/connection reset clears all chunks during respawn, dimension change, reconfiguration, and disconnect. No global locks or renderer mutation are introduced.

Chat Mode still renders only chat. Chunk packets produce no UI event or redraw; concise debug summaries show coordinate, section/palette/light counts, and total loaded chunks without dumping arrays or NBT.

## Real acceptance

Phase 14 passed real localhost acceptance against offline-mode vanilla Java 26.1.2. Overworld traffic produced plausible signed coordinates, 24-section chunks, single and indirect palettes, validated heightmaps/lighting, and a loaded count of 329. Overworld -> Nether cleared the store from 329 to zero before 16-section Nether chunks with appropriate lighting loaded back to 329. Nether -> Overworld repeated the `329 -> 0` reset and Overworld reloaded to 329. Chat Mode remained functional through both transitions and the final disconnect was clean. No `ERROR`, `WARN`, malformed-packet, trailing-data, or loaded-chunk-limit failure appeared. Process memory rose while terrain was retained and visibly fell when each dimension transition cleared the old chunk set.

Phase 15 will consume this semantic representation. Phase 14 deliberately contains no meshing, GPU buffers, camera, texture/model resolution, movement, collision, or renderer dependency.
