# Phase 15 diagnostic world rendering

Phase 15 proves Cubic's first `server chunks -> semantic state -> CPU mesh -> GPU` path. It is intentionally not a Minecraft visual implementation.

## Semantic inputs

The protocol-775 Configuration Registry Data codec follows the vanilla 26.1.2 registration verified from `ClientboundRegistryDataPacket` and `RegistrySynchronization.PackedRegistryEntry`: registry identifier, bounded entry list, entry identifier, and optional bounded network NBT. For `minecraft:dimension_type`, Cubic extracts integer `min_y` and `height`. Vanilla's `DimensionType` codec and constructor require height of at least 16, height divisible by 16, and `min_y` divisible by 16; Cubic validates those rules before activating a world. Section zero maps to `min_y / 16`, so Overworld and Nether do not require hardcoded names or heights.

Runtime block states remain typed numeric IDs because that is what chunk palettes carry. At World Mode startup, Cubic loads the exact version's Phase 11 `game-data.json` and builds a small visual profile. Every state belonging to `minecraft:air`, `minecraft:cave_air`, or `minecraft:void_air` is non-rendered. Unrecognized states remain visible diagnostic solids rather than accidentally creating holes. This classification is data-driven and replaceable by Phase 16's model/material data.

## Ownership and work scheduling

The network task remains sole owner of canonical `WorldState`. It publishes a coalescing mailbox containing the latest pose, reset generation, geometry, and at most one load/unload delta per bounded chunk coordinate. Chunk payloads are shared immutably; the complete world is never cloned per frame and no lock is held across I/O or meshing.

`cubic-render` keeps only render-side chunk references and GPU meshes. A dedicated worker receives bounded jobs containing the center chunk plus four horizontal neighbors, emits only exposed cube faces, and returns revision-tagged results. Dirty chunks remain coalesced by coordinate; available worker slots select the nearest chunk to the current authoritative player chunk without sorting the complete world, with coordinate order as a deterministic equal-distance tie-breaker. A changed authoritative pose affects the next selection, while a reset clears all pending render-side state before establishing the new origin. Neighbor arrival, replacement, or unload marks both sides dirty. Reset generations discard stale results and immediately clear prior-dimension CPU/GPU state. Meshing never blocks the network or winit event loop.

Each nonempty chunk mesh owns vertex/index GPU buffers. The shader applies deterministic diagnostic colors, perspective projection, and a depth attachment. The camera uses the server-authoritative player coordinates/yaw/pitch and sends no movement packets. Resize recreates the depth target. The window title reports dimension, geometry, authoritative pose, loaded/meshed chunk counts, and pending jobs.

Minecraft block coordinates are passed to the renderer unchanged: +X is east, +Y is up, and +Z is south. Chunk-local X/Z use Euclidean division so negative chunk boundaries remain correct. Minecraft yaw 0 faces +Z, +90 faces -X, -90 faces +X, and positive pitch looks downward. At the render boundary this basis is converted to a right-handed view space whose camera-forward direction is -Z; when facing south, screen-right is therefore west (-X), avoiding a reflected world. The eye is the authoritative player position plus the diagnostic 1.62-block eye offset. Cubic does not feed camera state back to the server.

Faces are emitted only when the adjacent semantic state is air or the neighbor is not loaded. Section boundaries use the same absolute lookup as ordinary blocks. Horizontal chunk neighbors are included in each bounded mesh job; arrival, replacement, and unload remesh both sides so temporary boundary faces converge. A `WorldContents` generation reset drops render-side chunk references and GPU buffers immediately, and late worker results from the old generation are ignored. The renderer retains one immutable reference and at most one GPU mesh per loaded chunk; Phase 30 will establish explicit global budgets and deeper streaming policy.

## Development command and limits

```powershell
$env:CUBIC_LOG_LEVEL='debug'
cargo run -p cubic-app -- world localhost:25565 --username CubicTest2
```

World Mode is a separate offline-development entry point and expects the Phase 11 artifact at `%LOCALAPPDATA%\Cubic\generated\game-data\26.1.2\game-data.json`. Normal clear-frame, Status, Login, authenticated networking, and Chat Mode commands are unchanged. No server is contacted by automated tests.

The required real localhost acceptance against offline-mode vanilla Java 26.1.2 passed. Cubic rendered 329 real Overworld chunks with correct depth and vanilla-matching orientation, survived aggressive resize and minimize/restore, cleared old render state across Overworld -> Nether -> Overworld, and rendered each dimension with its authoritative geometry. The final near-player-first loading-order retest also passed, with substantially improved visible mesh completion and stable terrain. World Mode shut down cleanly; the separate Chat Mode retained bidirectional chat and no world chunks. No warning, error, panic, or stale cross-dimension geometry was observed.

The current renderer uses cubes and diagnostic colors only. It has no textures, block models, transparency, fluids, lighting model, frustum/occlusion optimization, entities, movement, collision, or local camera controls. Phase 16 owns Minecraft resources/models. Phase 17 owns movement/collision. Phase 18 owns seamless Chat/Play switching.
