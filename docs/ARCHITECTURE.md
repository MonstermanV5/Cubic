# Architecture

This document describes current and intended boundaries. Phases 1-4 implement the repository scaffold, native clear-frame graphics bootstrap, transport-independent protocol primitives, and raw Java Edition NBT. Phase 5 adds only the Java Edition server-list Status exchange. Minecraft game systems remain unimplemented.

## Workspace responsibilities

- `cubic-app`: final executable and application composition root. With no arguments it delegates to the graphical platform layer; its `status` subcommand creates the small async runtime used by `cubic-network`.
- `cubic-core`: platform-independent, high-level client/engine state and shared abstractions.
- `cubic-protocol`: owns the synchronous binary reader/writer, structured codec errors, bounded primitive codecs, incremental uncompressed packet framing, bounded raw Java Edition NBT, and the isolated Phase 5 Handshake/Status packet codecs. It contains no sockets, async runtime, login/play schemas, compression, or version selection.
- `cubic-network`: owns Phase 5's asynchronous TCP status-query workflow, strict server-address parsing, timeouts, frame-stream integration, and typed query errors. It does not own protocol byte layouts.
- `cubic-version`: future Minecraft version metadata and loading of generated version data.
- `cubic-resources`: future resource-pack resolution, resource lookup, and caching.
- `cubic-world`: future world, chunk, block, biome, and entity state.
- `cubic-render`: owns the Phase 2 wgpu instance, adapter/device/queue, presentation surface, resizing, clear-frame submission, and surface recovery. Future rendering remains unimplemented.
- `cubic-ui`: future application, HUD, menu, and chat UI.
- `cubic-platform`: owns the winit event loop, native window lifecycle, redraw scheduling, suspension, and the isolated future iOS host handoff.
- `version-generator`: future development/build utility that converts external Minecraft metadata into Cubic's internal version-data format.

## Dependency direction

`cubic-app` is the composition root and currently depends on `cubic-core`, `cubic-platform`, and `cubic-network`. `cubic-network` depends on `cubic-protocol`; the protocol crate does not depend on the network crate or Tokio. `cubic-platform` depends on `cubic-render` to service native redraw events. `cubic-render` depends on cross-platform winit window handles and wgpu, but not on `cubic-platform` or `cubic-app`. `cubic-core` remains independent. Lower-level crates must not depend on `cubic-app`, and dependencies must remain acyclic.

`cubic-protocol` remains independent of the application, platform, renderer, async runtime, and network transport. `cubic-network` feeds arbitrary TCP fragments into its synchronous frame decoder and gives completed frame bodies to the narrow Status codecs. A future generated packet-schema layer may consume completed frame bodies and decode NBT directly from the same primitive reader without copying the remainder of a packet. Root-format selection is an explicit call-site choice, not a Minecraft-version check inside NBT.

The current Status path is deliberately separate from the graphical lifecycle:

```text
cubic-app status -> cubic-network -> Tokio TCP/DNS and timeouts
                                 -> cubic-protocol framing and Status codecs
```

It performs no renderer, window, world, authentication, or resource work. See `NETWORKING.md` for the exact exchange and limitations.

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
