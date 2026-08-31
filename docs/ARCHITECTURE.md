# Architecture

This document describes current and intended boundaries. Phases 1-17B provide the scaffold through accepted official-resource terrain rendering, movement, and vanilla block-collision fidelity. Phase 18's Play/Chat lifecycle is in progress; entities, interaction, and later environment systems remain unimplemented.

## Workspace responsibilities

- `cubic-auth`: UI-independent Microsoft/Xbox/Minecraft authentication with explicit `CubicEntra` and experimental `XalInterop` providers, token lifecycle, typed account identity, bounded HTTPS services, session-server join, proof-of-possession signing, short-lived player-certificate acquisition/signing, and secure credential-store abstraction. It has no renderer or Minecraft packet codecs.
- `cubic-app`: composition root. It parses graphical, Status, one-shot development-login, and Chat Mode commands; Chat Mode starts the network runtime on a dedicated thread and passes only a bounded session port to the platform/UI side.
- `cubic-core`: platform-independent shared concepts, including protocol-independent rich text, chat events, session commands, and connection presentation state.
- `cubic-protocol`: owns synchronous codecs, the isolated temporary protocol-775 bootstrap profile, and Phase 12's exact-version packet registry/bounded layout interpreter. It contains no sockets, async runtime, compression, authentication, or game semantics.
- `cubic-network`: owns TCP, framing, deadlines, Status, Login/Configuration, and the persistent Chat Mode Play task. It converts versioned packets into stable `cubic-core` chat events and `cubic-world` updates and never touches UI/GPU state.
- `cubic-version`: Minecraft version metadata, typed version/protocol/schema identifiers, release and snapshot kinds, compatibility profile identifiers, bounded filesystem-backed version-data store, plus the transport-independent official manifest/selected-version descriptor model.
- `cubic-resources`: Phase 10 official HTTPS acquisition, hash/size verification, immutable cache ownership, asset-index lookup, content-addressed asset objects, explicit client-JAR acquisition, and Phase 16's validated bounded read-only vanilla-resource archive source. It does not interpret blockstate/model semantics or Mojang code.
- `cubic-version`: also owns Phase 11's schema-versioned immutable vanilla game-data model and runtime lookups. It validates generated artifacts but does not parse JARs or perform network access.
- `cubic-world`: owns connection-scoped semantic `WorldState`, lifecycle/reset rules, dimensions, player world metadata, authoritative position, spawn/time/difficulty/weather/border state, and the bounded runtime-registry summary boundary. It knows no packet IDs, sockets, UI, renderer, or protocol version.
- `cubic-render`: owns wgpu surface/device submission, Chat Mode's direct `egui-wgpu` paint integration, the bounded chunk mesher, blockstate/model resolution, deterministic texture atlas, material/tint metadata, and textured terrain submission.
- `cubic-ui`: owns the protocol-independent Chat Mode model and egui presentation: bounded history, text input, send action, scrolling, and connection/error state.
- `cubic-platform`: owns the winit event loop, native window lifecycle, redraw scheduling, suspension, the Windows-only private WebView2 navigation host for experimental XAL authorization capture, and the isolated future iOS host handoff.
- `version-generator`: offline development/build utility for version catalogs, generated game data, and exact-version packet-schema artifacts. It reuses `cubic-version` identities and `cubic-protocol` schema validation and performs no network access.

## Dependency direction

`cubic-app` composes `cubic-network` with `cubic-platform`/`cubic-ui`. `cubic-network` depends on `cubic-core`, `cubic-protocol`, and `cubic-version`. `cubic-ui` depends only on `cubic-core` and egui; its session-port trait prevents it from owning or naming TCP. `cubic-platform` depends on `cubic-ui` and `cubic-render`; `cubic-render` owns the wgpu/egui-wgpu integration. Dependencies remain acyclic.

Phase 9 adds `cubic-app -> cubic-auth` for orchestration and `cubic-network -> cubic-auth` only for authenticated identity and the provider-neutral `MinecraftSessionJoiner` capability. The network path cannot distinguish whether an access token came from Cubic's Entra registration or experimental XAL interoperability. OAuth/Xbox HTTP concepts do not enter `cubic-protocol`, and authentication never touches rendering. See `AUTHENTICATION.md`.

Authenticated Chat Mode reuses the Phase 8 UI and bounded channels. The app silently refreshes the selected provider, requests a provider-neutral Mojang player certificate, and supplies both to the shared authenticated connection bootstrap. `cubic-network` then owns generic bounded session/index/last-seen state while `cubic-protocol::bootstrap::v775` owns the replaceable 26.1.2 packet IDs and field order. A selected `SecureChatRules` value supplies signing-domain and acknowledgement policy; tests use a deliberately different synthetic profile to prevent core signing state from depending on raw 775 IDs. Phases 10–12 may replace the bootstrap codecs/profile without moving OAuth into networking or protocol details into the UI.

Phase 10 keeps official metadata parsing separate from acquisition. `cubic-version` represents manifest and selected-version facts; `cubic-resources` follows official descriptors, verifies immutable bytes, and owns the platform-selected cache. `cubic-app` only selects a version and reports the result. Runtime logs use the same `tracing` events across crates and are installed once by the application composition root; platform code supplies the persistent data directory. See `RESOURCES.md` and `LOGGING.md`.

Phase 11 extends `version-generator` rather than adding another tool. The offline generator consumes Mojang Data Generator reports, binds them to an exact Phase 10-verified client descriptor, and emits canonical JSON. The runtime receives `GameData`, not raw reports: generic registries retain version-scoped sparse raw IDs, while blocks add default state, property domains, concrete property assignments, and global state IDs. Exact Minecraft version IDs scope every artifact; protocol numbers are not identities.

Generated data is explicitly a vanilla baseline. A future server-authoritative registry layer may overlay or replace relevant mappings during Configuration without mutating the baseline. Phase 11 does not implement that runtime overlay, packet codecs, world state, or resource interpretation. See `GAME_DATA.md`.

Phase 12 uses a hybrid packet boundary. Mojang's `packets.json` supplies authoritative exact-version state/direction/identity/ID facts; pinned PrismarineJS ProtoDef data supplies only supplemental ordered wire structure. Generation fails on an ID disagreement and uses exact names or a small reviewed 26.1.2 alias table—never fuzzy matching. Unsupported source constructs and ambiguous names remain explicit identity-only definitions. A schema-versioned artifact and immutable runtime indexes expose the merged result without filesystem knowledge in networking. Stable semantic events remain outside the dynamic wire representation. The working `bootstrap::v775` path remains active; 36 IDs and 14 representative layouts are cross-checked against it. See `PACKETS.md`.

Phases 13-16 follow `exact-version wire packet -> version adapter -> WorldEvent -> WorldState -> coalesced semantic render deltas`. The persistent network task owns one `WorldState` and applies events synchronously; the UI and renderer cannot mutate it, and no global world lock is introduced. Begin Configuration, Enter World, Respawn, reconfiguration, and Disconnect have explicit reset semantics. Phase 15 shares immutable changed chunks through a bounded latest-value mailbox. Phase 16 prepares a directly indexed exact-version state/model/atlas table before window creation and shares it across a bounded pool of at most four mesh workers; total active-plus-queued work remains capped at 32. Network and world crates know no JSON, PNG, archive, or GPU type. See `WORLD_STATE.md`, `CHUNKS.md`, `WORLD_RENDERING.md`, and `BLOCK_RESOURCES.md`.

Phase 17 keeps raw input in `cubic-platform`, local f64 simulation and semantic block collision in `cubic-world`, exact-version packet selection/correction/ability orchestration in `cubic-network`, and camera/GPU consumption in `cubic-render`. Monotonically sequenced held snapshots and a bounded durable transition journal preserve final state plus press/release/focus-loss edges for the network-owned 20 Hz clock; cumulative sequenced mouse totals provide constant-size simulation acknowledgement and display-rate preview. Missed simulation ticks are skipped rather than accumulated. Exact-version heavy-chunk classification lets decoding run off the async control path while movement ticks continue. Network-to-render chunks are immutable `Arc` snapshots with copy-on-write live mutation and latest-revision coalescing; no renderer consumption is required for publication to progress. Bounded background diagnostics remove file/console I/O from movement and render paths. Workers perform CPU meshing and GPU-buffer creation, while the event loop shares a foreground time budget across result installation and dirty-job dispatch. Canonical server authority and local prediction remain separate: corrections and server abilities explicitly reconcile prediction, while ordinary local ticks do not overwrite `WorldState`. In predicted World Mode the movement controller is the sole timed render-pose publisher, a narrow event-loop wake makes each new pose observable without idle polling latency, and the renderer performs vanilla-style partial-tick position/eye interpolation without creating another physics state. See `MOVEMENT.md`.

Phase 17B keeps the solver generic and isolates exact-version physical block behavior in `cubic-world::collision_vanilla`. The generated Phase 11 artifact supplies immutable runtime IDs, identifiers, and complete properties; the 26.1.2 adapter maps those facts to bounded empty/full/multi-AABB physical shapes. Visual models, block-entity rendering/payloads, selection/outline shapes, and occlusion remain separate. Connected fence/wall/pane geometry uses neighbour-derived properties already encoded by vanilla in each runtime state. A profile-level local-shape envelope lets the bounded source-cell broadphase find legitimate geometry extending beyond its owner cell, such as 24/16-high fences, without clamping physical boxes. Unknown versions/states remain conservative rather than becoming noclip.

`cubic-protocol` remains independent of the application, platform, renderer, async runtime, and network transport. It now depends narrowly on `cubic-version` for validated exact-version/protocol/identifier types. `cubic-network` feeds arbitrary TCP fragments into its synchronous frame decoder and gives completed frame bodies to state-specific codecs. The packet-schema layer consumes completed payloads and decodes NBT directly from the same primitive reader. Root-format selection is an explicit schema choice, not a Minecraft-version check inside NBT.

The current Status path is deliberately separate from the graphical lifecycle:

```text
cubic-app status -> cubic-network -> Tokio TCP/DNS and timeouts
                                 -> cubic-protocol framing and Status codecs
```

It performs no renderer, window, world, authentication, or resource work. See `NETWORKING.md` for the exact exchange and limitations.

The Phase 7 path is similarly separate:

```text
cubic-app dev-login -> cubic-network state machine and framed TCP transport
                    -> cubic-version typed version/protocol identity
                    -> cubic-protocol protocol-775 bootstrap packet profile
```

All manually authored 26.1.2 packet IDs and layouts live in the single `cubic_protocol::bootstrap::v775` module. Phase 12 cross-checks its packet identities/IDs and provides the generated-registry replacement boundary, but does not replace verified codecs with missing official layouts. Network and engine code must not grow scattered version strings, numeric protocol checks, or giant version match statements.

## Phase 8 Chat Mode lifecycle

```text
Tokio network thread -> bounded ChatEvent queue -> ChatSessionPort -> cubic-ui model
UI send action       -> bounded command queue   -> network thread -> TCP
winit events         -> egui input/layout       -> cubic-render   -> wgpu
```

The network task retains `MinecraftConnection` after Phase 7 reaches Play. It handles required control/chat packets, Phase 14 chunk lifecycle, Phase 17 single/section live block updates, and the optional World Mode movement controller. Live updates mutate loaded semantic sections in place, immediately affect collision, and publish coalesced affected-chunk render replacements; the renderer's existing neighbor invalidation and revision/generation checks prevent stale geometry from winning. Unloaded/out-of-dimension updates are ignored safely. The UI never owns a socket and rendering never blocks on network I/O.

Phase 18 composes those same endpoints in one native application: an explicit `Play`/`Chat` presentation mode controls input routing and render workload, not connection ownership. The network-owned `WorldState`, 20 Hz controller, keepalive/control handling, and bounded chat queues continue unchanged in both modes. Chat mode applies coalesced immutable chunk updates to render-side desired state but does not call terrain preparation or submit a world pass. The bounded dirty map is therefore the backlog; superseded revisions coalesce. Returning to Play uses the latest predicted pose as the existing near-first dispatch origin. There is currently no particle or audio subsystem to suspend; adding either later must follow this presentation policy rather than own session lifetime.

winit waits rather than polls. A 200 ms low-frequency wake checks bounded cross-thread state, but requests a GPU redraw only for changed input/session presentation, resize/recovery, or direct window interaction. Networking remains active independently. `egui-winit` owns native clipboard event/output integration. `cubic-platform` may supply platform-installed font bytes to egui at Chat Mode startup; this target-specific font discovery remains outside `cubic-ui` and the renderer. See `CHAT_MODE.md` for the exact MVP boundary.

Platform-specific implementations belong in `cubic-platform` or clearly marked platform-specific modules. Shared engine and rendering code do not assume Windows or iOS APIs. The only current target-specific source is the future native-host handoff in `cubic-platform::ios`.

## Phase 2 application lifecycle

winit creates the native window after the application receives `resumed`. The platform layer initializes `cubic-render` for that window and requests the first redraw. Each redraw clears and presents one frame through wgpu, then schedules the next redraw; wgpu's vsynchronized surface presentation provides pacing while winit remains in `ControlFlow::Wait`. Resize events reconfigure only non-zero surfaces. Zero-sized or occluded windows pause useful rendering, and restoration requests another frame. Close requests exit the event loop cleanly.

On Windows, the enabled wgpu backend is Direct3D 12. On iOS/iPadOS, wgpu uses Metal. See `GRAPHICS_BOOTSTRAP.md` for target and CI details.

Networking, protocol framing, raw NBT, packet semantics, world state, resources, and rendering remain separate concerns. Future network code will provide bytes to framing and produce validated domain data rather than mutate renderer internals. World state will not perform rendering. Resource handling will resolve data without owning GPU submission. Rendering will consume prepared state through narrow interfaces and will not perform blocking network or filesystem operations. See `PROTOCOL.md` and `NBT.md` for the implemented codec boundaries.

## Future concurrency model

- Networking and async I/O will progress independently of rendering.
- CPU-heavy work such as chunk decoding and meshing will run on worker threads or a worker pool.
- The render thread will be primarily responsible for rendering and GPU submission.

The exact scheduling design is intentionally deferred until those systems exist and can be profiled.

## Version boundaries

Engine behavior should not depend directly on Minecraft numeric IDs where avoidable. Version-specific values and ordinary data differences should be isolated in generated version data. Small compatibility adapters may handle genuine behavioral differences. Version-dependent conditions must not be scattered throughout engine code.
