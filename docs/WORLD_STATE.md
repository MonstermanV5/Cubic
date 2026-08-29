# World State

Phase 13 introduces Cubic's first persistent semantic representation of an active Minecraft world session. It is deliberately small: it records authoritative context needed by later systems but contains no chunks, entities, movement simulation, collision, or rendering.

## Ownership and data flow

The data flow is:

```text
exact-version packet
    -> version-specific decoder/adapter
    -> WorldEvent
    -> connection-owned WorldState
```

`cubic-world` owns `WorldState` and the stable event types. It has no dependency on `cubic-protocol`, sockets, Tokio, UI, or rendering and contains no packet IDs or version checks. During Chat Mode, the existing network task owns one state value and applies events synchronously. The UI cannot mutate authoritative state, and no global `Arc<Mutex<WorldState>>` is used.

## Lifecycle and reset rules

The explicit lifecycle is `Disconnected -> Configuring -> Active`. `BeginConfiguration` clears the prior session and server registry summaries. `EnterWorld` atomically installs a new session. Every respawn invalidates authoritative player coordinates and emits a future world-content reset hook, while preserving or resetting the independently modeled rotation baseline according to the selected protocol adapter. This allows vanilla's first post-respawn synchronization to use relative yaw/pitch without treating the old dimension's coordinates as valid. A dimension change additionally clears dimension-scoped spawn, time, weather, and border data. Reconfiguration repeats the configuration/enter sequence. Disconnect removes all connection-owned state, preventing reuse by a later server.

## Modeled state

An active session records the player entity ID, hardcore and presentation flags, known dimensions, server view/simulation metadata, current dimension and dimension-type registry reference, hashed seed, game mode and previous mode, debug/flat flags, last-death location, portal cooldown, sea level, secure-chat enforcement, authoritative position/yaw/pitch and teleport ID, spawn point, clock updates, difficulty, weather, and initialized border data.

Dimensions are validated `MinecraftIdentifier` values, not a vanilla-only enum, so custom namespaces remain valid. Current 26.1.2 dimension types arrive as server registry raw IDs; Cubic represents that unresolved reference explicitly instead of treating generated vanilla IDs as authoritative.

## Registry boundary and limits

Generated Phase 11 `GameData` is an immutable vanilla baseline. `RuntimeRegistrySnapshot` is a separate connection-owned boundary for server-authoritative registry summaries. The live Configuration path does not yet safely decode full dynamic registry contents, so Phase 13 leaves the snapshot empty rather than fabricating mappings. Later phases can populate it without mutating generated data.

Known dimensions are capped at 1,024, runtime registry summaries at 512, summarized entries per registry at 1,048,576, and world-clock updates at 64. Identifiers, counts, enum discriminants, relative-position flags, finite floating-point values, and transition order are validated before state mutation. Active state is never persisted.

## Current protocol adapter

Minecraft Java 26.1.2 / protocol 775 is the first reference adapter, not the permanent world-state design. Official generated packet reports supply exact packet identities and IDs. The Phase 13 packets are conservatively `IdentityOnly` in the Phase 12 artifact, so their small layouts were reviewed independently against the pinned structural source and the installed official 26.1.2 class codecs before being implemented in `bootstrap::v775`. `cubic-network` converts those wire structures to stable events; a future generated profile can replace this adapter without changing `WorldState`.

## Deferred work

Phase 14 owns chunk section, palette, biome, heightmap, lighting, and chunk-cache work. Later phases own entity populations, movement/collision, gameplay, dimensions/environment depth, and world rendering. Phase 13's reset scopes exist so those stores can be invalidated cleanly when added; they are not implemented here.

## Manual acceptance

Phase 13 passed this acceptance against offline-mode vanilla Java 26.1.2. Both Overworld -> Nether and Nether -> Overworld transitions cleared world contents, invalidated coordinates, preserved the appropriate rotation baseline, accepted the following relative yaw/pitch synchronization, advanced teleport sequencing, and left Chat Mode functional through clean disconnect.

Against a controlled offline-mode vanilla 26.1.2 server on `localhost:25565`, PowerShell users can run:

```powershell
$env:CUBIC_LOG_LEVEL='debug'
cargo run -p cubic-app -- chat localhost:25565 --username CubicTest
```

The log should report `lifecycle=Active`, the current namespaced dimension, dimension-type raw reference, player entity ID, game mode, and then the authoritative position/rotation after the server synchronizes it. Chat Mode should remain functional and world/chunk traffic should not accumulate. An operator may optionally move the player through a vanilla dimension transition to verify the respawn/reset log; this is not required when no second dimension is available.
