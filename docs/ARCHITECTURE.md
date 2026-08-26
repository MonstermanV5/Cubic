# Architecture

This document describes intended boundaries. Except for the startup scaffold, the systems below are not implemented.

## Workspace responsibilities

- `cubic-app`: final executable and application composition root. It orchestrates other crates and contains no reusable engine logic.
- `cubic-core`: platform-independent, high-level client/engine state and shared abstractions.
- `cubic-protocol`: future network protocol codecs and connection-state logic.
- `cubic-version`: future Minecraft version metadata and loading of generated version data.
- `cubic-resources`: future resource-pack resolution, resource lookup, and caching.
- `cubic-world`: future world, chunk, block, biome, and entity state.
- `cubic-render`: future renderer and rendering abstractions.
- `cubic-ui`: future application, HUD, menu, and chat UI.
- `cubic-platform`: future operating-system interfaces and implementations.
- `version-generator`: future development/build utility that converts external Minecraft metadata into Cubic's internal version-data format.

## Dependency direction

`cubic-app` is the composition root and may depend on the crates needed to assemble the client. `cubic-core` may depend on narrow, platform-independent abstractions, but must not depend on concrete operating-system implementations. Lower-level crates must not depend on `cubic-app`, and crate dependencies must remain acyclic. Public interfaces between crates should stay small and intentional.

Platform-specific implementations belong in `cubic-platform` or clearly marked platform-specific modules. Shared engine code must not assume Windows or iOS APIs. This separation is intended to support Windows x86-64 first and ARM64 iOS/iPadOS later.

Networking, world state, resources, and rendering will remain separate concerns. Network code will produce validated domain data rather than mutate renderer internals. World state will not perform rendering. Resource handling will resolve data without owning GPU submission. Rendering will consume prepared state through narrow interfaces and will not perform blocking network or filesystem operations.

## Future concurrency model

- Networking and async I/O will progress independently of rendering.
- CPU-heavy work such as chunk decoding and meshing will run on worker threads or a worker pool.
- The render thread will be primarily responsible for rendering and GPU submission.

The exact scheduling design is intentionally deferred until those systems exist and can be profiled.

## Version boundaries

Engine behavior should not depend directly on Minecraft numeric IDs where avoidable. Version-specific values and ordinary data differences should be isolated in generated version data. Small compatibility adapters may handle genuine behavioral differences. Version-dependent conditions must not be scattered throughout engine code.

