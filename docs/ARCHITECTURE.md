# Architecture

This document describes current and intended boundaries. Phases 1-7 provide the scaffold, graphics bootstrap, protocol/NBT foundations, Status, version data, and the validated development login. Phase 8 adds a narrow persistent Chat Mode candidate for Java 26.1.2 / protocol 775. Minecraft game systems and world rendering remain unimplemented.

## Workspace responsibilities

- `cubic-app`: composition root. It parses graphical, Status, one-shot development-login, and Chat Mode commands; Chat Mode starts the network runtime on a dedicated thread and passes only a bounded session port to the platform/UI side.
- `cubic-core`: platform-independent shared concepts, including protocol-independent rich text, chat events, session commands, and connection presentation state.
- `cubic-protocol`: owns synchronous codecs and the isolated temporary protocol-775 bootstrap profile, now including only the small Phase 8 Play control/chat subset. It contains no sockets, async runtime, compression, authentication, or general Play schema.
- `cubic-network`: owns TCP, framing, deadlines, Status, Login/Configuration, and the persistent Chat Mode Play task. It converts bootstrap packets into stable `cubic-core` events and never touches UI/GPU state.
- `cubic-version`: Minecraft version metadata, typed version/protocol/schema identifiers, release and snapshot kinds, compatibility profile identifiers, bounded filesystem-backed version-data store, catalog loading and validation. Synchronous, transport-independent, and free of rendering, world state, and platform dependencies.
- `cubic-resources`: future resource-pack resolution, resource lookup, and caching.
- `cubic-world`: future world, chunk, block, biome, and entity state.
- `cubic-render`: owns wgpu surface/device submission and the direct `egui-wgpu` paint integration used by Chat Mode. It still contains no world renderer.
- `cubic-ui`: owns the protocol-independent Chat Mode model and egui presentation: bounded history, text input, send action, scrolling, and connection/error state.
- `cubic-platform`: owns the winit event loop, native window lifecycle, redraw scheduling, suspension, and the isolated future iOS host handoff.
- `version-generator`: offline development/build utility that validates installed version datasets and builds a deterministic catalog from on-disk version data. Depends only on `cubic-version`; performs no network access.

## Dependency direction

`cubic-app` composes `cubic-network` with `cubic-platform`/`cubic-ui`. `cubic-network` depends on `cubic-core`, `cubic-protocol`, and `cubic-version`. `cubic-ui` depends only on `cubic-core` and egui; its session-port trait prevents it from owning or naming TCP. `cubic-platform` depends on `cubic-ui` and `cubic-render`; `cubic-render` owns the wgpu/egui-wgpu integration. Dependencies remain acyclic.

`cubic-protocol` remains independent of the application, platform, renderer, async runtime, and network transport. `cubic-network` feeds arbitrary TCP fragments into its synchronous frame decoder and gives completed frame bodies to state-specific codecs. A future generated packet-schema layer may consume completed frame bodies and decode NBT directly from the same primitive reader without copying the remainder of a packet. Root-format selection is an explicit call-site choice, not a Minecraft-version check inside NBT.

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

All manually authored 26.1.2 packet IDs and layouts live in the single `cubic_protocol::bootstrap::v775` module. Phase 12 will replace or absorb that temporary profile with generated packet data. Network and engine code must not grow scattered version strings, numeric protocol checks, or giant version match statements.

## Phase 8 Chat Mode lifecycle

```text
Tokio network thread -> bounded ChatEvent queue -> ChatSessionPort -> cubic-ui model
UI send action       -> bounded command queue   -> network thread -> TCP
winit events         -> egui input/layout       -> cubic-render   -> wgpu
```

The network task retains `MinecraftConnection` after Phase 7 reaches Play. It handles required control packets and selected chat packets, converts them to protocol-independent events, and discards other complete bounded frames immediately. No chunk, entity, inventory, or movement state is built. The UI never owns a socket and rendering never blocks on network I/O.

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
