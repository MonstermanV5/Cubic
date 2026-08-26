# Architecture

This document describes current and intended boundaries. Phase 2 implements only native application lifecycle and a clear-frame GPU bootstrap; Minecraft systems remain unimplemented.

## Workspace responsibilities

- `cubic-app`: final executable and application composition root. It initializes diagnostics and delegates to the platform layer without reusable engine logic.
- `cubic-core`: platform-independent, high-level client/engine state and shared abstractions.
- `cubic-protocol`: future network protocol codecs and connection-state logic.
- `cubic-version`: future Minecraft version metadata and loading of generated version data.
- `cubic-resources`: future resource-pack resolution, resource lookup, and caching.
- `cubic-world`: future world, chunk, block, biome, and entity state.
- `cubic-render`: owns the Phase 2 wgpu instance, adapter/device/queue, presentation surface, resizing, clear-frame submission, and surface recovery. Future rendering remains unimplemented.
- `cubic-ui`: future application, HUD, menu, and chat UI.
- `cubic-platform`: owns the winit event loop, native window lifecycle, redraw scheduling, suspension, and the isolated future iOS host handoff.
- `version-generator`: future development/build utility that converts external Minecraft metadata into Cubic's internal version-data format.

## Dependency direction

`cubic-app` is the composition root and currently depends on `cubic-core` and `cubic-platform`. `cubic-platform` depends on `cubic-render` to service native redraw events. `cubic-render` depends on cross-platform winit window handles and wgpu, but not on `cubic-platform` or `cubic-app`. `cubic-core` remains independent. Lower-level crates must not depend on `cubic-app`, and dependencies must remain acyclic.

Platform-specific implementations belong in `cubic-platform` or clearly marked platform-specific modules. Shared engine and rendering code do not assume Windows or iOS APIs. The only current target-specific source is the future native-host handoff in `cubic-platform::ios`.

## Phase 2 application lifecycle

winit creates the native window after the application receives `resumed`. The platform layer initializes `cubic-render` for that window and requests the first redraw. Each redraw clears and presents one frame through wgpu, then schedules the next redraw; wgpu's vsynchronized surface presentation provides pacing while winit remains in `ControlFlow::Wait`. Resize events reconfigure only non-zero surfaces. Zero-sized or occluded windows pause useful rendering, and restoration requests another frame. Close requests exit the event loop cleanly.

On Windows, the enabled wgpu backend is Direct3D 12. On iOS/iPadOS, wgpu uses Metal. See `GRAPHICS_BOOTSTRAP.md` for target and CI details.

Networking, world state, resources, and rendering will remain separate concerns. Network code will produce validated domain data rather than mutate renderer internals. World state will not perform rendering. Resource handling will resolve data without owning GPU submission. Rendering will consume prepared state through narrow interfaces and will not perform blocking network or filesystem operations.

## Future concurrency model

- Networking and async I/O will progress independently of rendering.
- CPU-heavy work such as chunk decoding and meshing will run on worker threads or a worker pool.
- The render thread will be primarily responsible for rendering and GPU submission.

The exact scheduling design is intentionally deferred until those systems exist and can be profiled.

## Version boundaries

Engine behavior should not depend directly on Minecraft numeric IDs where avoidable. Version-specific values and ordinary data differences should be isolated in generated version data. Small compatibility adapters may handle genuine behavioral differences. Version-dependent conditions must not be scattered throughout engine code.
