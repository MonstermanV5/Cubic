# Phase 16 block resources and models

Phase 16 replaces diagnostic state colors with an independently implemented reader for official vanilla block resources. Cubic never embeds or commits those resources. World Mode selects exact version `26.1.2`, verifies its Phase 11 game-data provenance against the Phase 10 client descriptor, and asks the existing official bootstrap to supply the size- and SHA-1-verified client JAR. A read-only `cubic-resources` archive source then exposes validated logical paths and bounded entry reads. Resource preparation completes before winit creates the window; rendering and networking perform no filesystem access.

## Resolution pipeline

Generated game data maps each `RuntimeBlockStateId` to a namespaced block identifier and exact property map. `cubic-render` loads the corresponding blockstate JSON and supports default/property variants, weighted alternatives, quarter-turn x/y rotations, `uvlock`, multipart property alternatives, and nested AND/OR conditions. Weighted choices now reproduce vanilla's default position-seeded model selection: Minecraft's signed/wrapping position hash seeds its standard 48-bit Java-compatible LCG, then each weighted model group consumes one bounded integer in JSON order. State IDs and face directions are not mixed into the seed. Multiple multipart groups share the per-block RNG sequentially, matching the current client lifecycle.

Models support bounded parent inheritance, inherited elements and texture maps, multi-level `#texture` indirection, current string and extended `{ "sprite": ... }` texture references, cycle/depth detection, ordinary elements/faces, explicit or generated UVs, face rotation, cull faces, tint indexes, element rotation/rescale, and blockstate rotation. Unknown, malformed, cyclic, or missing resources produce Cubic's generated magenta/black fallback. Repeated failures are coalesced into bounded startup diagnostics.

Official PNGs are decoded with explicit dimension and decoded-byte limits. Phase 18B retains bounded vertical-strip frames and validates `.mcmeta` default/explicit sequences, tick durations, indices, and interpolation. The render owner updates only the affected atlas subregion and its duplicated edge gutters; chunks are not remeshed for animation. Textures are sorted and shelf-packed into one bounded atlas. Atlas regions cover each texture's complete pixel domain, so nearest sampling gives every source texel equal face-space width. The exact-version adapter classifies opaque, alpha-tested, and translucent materials; texture alpha remains a conservative cutout fallback.

Minecraft model UVs and decoded PNG rows share a top-left origin. Cubic preserves Minecraft's direction-specific baked-quad vertex order rather than imposing one generic face basis: local U points +X on down/up/south, -X on north, +Z on west, and -Z on east. The canonical four UV indices are `(min U,min V)`, `(min U,max V)`, `(max U,max V)`, and `(max U,min V)`; face rotation advances that source index exactly as the model format specifies. For `uvlock`, the blockstate model transform is expressed between those source/destination face bases and its affine inverse is applied to every actual UV coordinate around the texture centre, matching the current model baker. A corner permutation alone is insufficient because a rotated asymmetric UV rectangle also swaps or reflects its numeric bounds. Axis-aligned cuboid faces and their paired UVs are then reordered into the transformed face's canonical winding, matching `FaceBakery` output rather than retaining a cyclic source-face order. This one model-baking boundary covers generated and explicit UVs, face and element rotations, blockstate X/Y rotations, and `uvlock` without global U/V flips, stair-name branches, or changes to world coordinates.

Tint indexes survive model inheritance and baking. The exact-version resource adapter maps each face's tint index to grass, foliage, dry-foliage, water, age-dependent stem, fixed, or none semantics; generic meshing does not switch on block names. Runtime biome temperature/downfall and explicit overrides feed the corresponding verified client colormaps, with a default radius-two square blend across chunk boundaries. Packed registry colours are decoded as sRGB and converted to linear before multiplication with the sRGB atlas sample, avoiding the previously over-bright grass-overlay and incorrect water colour. Authoritative server sky/block nibble arrays, directional shading, four-sample corner light, model-controlled ambient occlusion, and three-neighbour AO feed terrain vertices; emissive materials bypass local darkness. The current neutral sky input deliberately defers time-of-day lightmap/environment color to Phase 24.

Resource preparation produces an immutable, directly indexed `RuntimeBlockStateId` table. Static model inheritance, texture indirection, atlas regions, blockstate transforms, UV locking, render layers, and conservative full-opaque-cube classification are resolved before meshing. Unknown IDs reference one shared visible fallback rather than cloning model data. In the hot loop, a provably full opaque cube checks its six neighbors first and exits before model selection when fully buried; non-full, cutout, and uncertain models never take this shortcut.

Chunk meshes remain worker-built, revision- and generation-tagged, neighbor-aware, bounded, and selected near-player-first. Up to four workers are selected from available parallelism while total active-plus-queued work remains bounded at 32 jobs. Workers share immutable prepared resources; scheduling, coalescing, stale revision/generation rejection, and neighbor remeshing remain unchanged. A vertex contains world position, atlas UV, tint/shade, and material classification. One atlas bind group serves all chunk meshes; Cubic does not create per-block textures, bind groups, or draw calls. The worker copy drops atlas pixel bytes after GPU upload. The platform retains one prepared source copy so a suspended/lost platform renderer can recreate its device resources without reopening the archive from the render loop.

World Mode emits an INFO mesh-progress summary every ten seconds and on reset/shutdown, with loaded, resident/visible, dirty, queued, active, completed, throughput, and average/maximum CPU time. DEBUG adds aggregate visited/air/non-air/occlusion/model/geometry/neighbor/model-selection/quad counters every two seconds. Logging is aggregate only; no per-block or per-mesh line is emitted.

## Current local validation facts

The read-only exact-version validator used Cubic's verified official 26.1.2 cache and resolved 1,168 blockstates, 2,303 models, and 1,072 textures into a deterministic 2048×512 (4 MiB) atlas. Fifty-two of 29,873 generated block states used the visible fallback. This local validation contacted no Minecraft server and committed no Mojang content.

Read-only inspection of the official 26.1.2 resources confirmed that netherrack has sixteen equal-weight applications of the same cube model: all combinations of X and Y quarter-turns, with X varying fastest in JSON order. The installed client's `SectionCompiler`, `BlockStateBase`, `Mth`, `ModelBlockRenderer`, `WeightedVariants`, and `WeightedList` registrations confirmed that the block-position seed—not runtime state ID—is reset once per block and consumed by weighted groups. Cubic independently implements those observable arithmetic and selection rules; it does not contain Mojang implementation code or resources.

Phase 16 passed final localhost visual acceptance against vanilla Java 26.1.2. Phase 18B's biome tint/blending, material batches, atlas animation, fluid surfaces, and authoritative local lighting have also passed localhost acceptance without changing that model/UV foundation. Water and lava use resolved fluid semantics, neighbour face culling, weighted corner heights, animated still/flow top and side textures, water biome tint/translucency, and lava emissive output. Exact-version fluid semantics allow one state to contribute both ordinary model geometry and contained water (including intrinsic aquatic plants and property-waterlogged blocks). Remaining limitations include chunk-level rather than per-quad translucent sorting, approximate fluid occlusion around partial/waterlogged models, no generated atlas mipmaps, no Phase 24 environment lightmap or underwater camera fog/colour, simplified rather than pixel-identical vanilla smooth-light weighting, incomplete clipping for arbitrarily rotated models, specialized coordinated rendering seeds, arbitrary resource-pack layering, entities, and later optimization/budget work.

## Manual acceptance

```powershell
$env:CUBIC_LOG_LEVEL='debug'
cargo run -p cubic-app -- world localhost:25565 --username CubicTest2
```

Repeat with optimized code to compare the same progress diagnostics:

```powershell
$env:CUBIC_LOG_LEVEL='debug'
cargo run --release -p cubic-app -- world localhost:25565 --username CubicTest2
```

After Overworld inspection, transition with:

```text
execute in minecraft:the_nether run teleport CubicTest2 0 80 0
execute in minecraft:overworld run teleport CubicTest2 0 80 0
```

Acceptance requires recognizable official block textures/models, correct cutout presentation, stable UV/orientation/atlas output, preserved near-first loading and window lifecycle, and clean dimension replacement. It does not require pixel-identical vanilla lighting or environment rendering.
