# Roadmap

Status markers: `[ ]` not started, `[~]` partial, `[x]` complete, `[!]` blocked.

- [x] Phase 1 - repository and architecture
- [x] Phase 2 - Windows/iOS graphical bootstrap
- [x] Phase 3 - protocol primitives
- [x] Phase 4 - NBT
- [x] Phase 5 - server status ping
- [x] Phase 6 - version-data architecture
- [x] Phase 7 - offline-mode development-server login
- [ ] Phase 8 - Chat Mode MVP
- [ ] Phase 9 - Microsoft/Minecraft authentication
- [ ] Phase 10 - Minecraft version/resource bootstrap
- [ ] Phase 11 - generated game registries/data
- [ ] Phase 12 - generated packet codecs
- [ ] Phase 13 - world state
- [ ] Phase 14 - chunk decoding
- [ ] Phase 15 - first simple 3D world
- [ ] Phase 16 - Minecraft block resources/models
- [ ] Phase 17 - basic movement/collision
- [ ] Phase 18 - seamless Play/Chat mode switching
- [ ] Phase 19 - block interaction
- [ ] Phase 20 - inventory/items
- [ ] Phase 21 - entity state
- [ ] Phase 22 - entity rendering/animation
- [ ] Phase 23 - audio
- [ ] Phase 24 - environment/dimensions
- [ ] Phase 25 - text/UI/server presentation features
- [ ] Phase 26 - resource packs
- [ ] Phase 27 - plugin/proxy compatibility
- [ ] Phase 28 - iPad controls/platform polish
- [ ] Phase 29 - profiling/multithreading optimization
- [ ] Phase 30 - explicit memory budgets
- [ ] Phase 31 - automatic stable-version update pipeline
- [ ] Phase 32 - snapshot pipeline
- [ ] Phase 33 - version compatibility CI
- [ ] Phase 34 - torture/performance testing
- [ ] Phase 35 - iPad soak/thermal/memory testing
- [ ] Phase 36 - final acceptance testing

Phases 1-6 are complete while their acceptance criteria and validation commands continue to pass. Phase 5's implementation, deterministic mock-server coverage, and required real Java Edition server smoke test against vanilla Java Edition 26.1.2 on localhost:25565 all passed. Phase 6 delivered the `cubic-version` runtime library and `version-generator` tool: path-safe opaque version IDs, typed protocol and schema versions, release/snapshot kinds, compatibility profile identifiers, bounded JSON loading, catalog validation, a filesystem-backed version-data store, an offline deterministic catalog builder, and synthetic fixtures including two releases and one snapshot with a shared protocol number.

Phase 7 is complete. In addition to the isolated Java 26.1.2 / protocol 775 bootstrap profile, shared framed transport, explicit Login/Configuration state machine, development CLI, bounds, structured errors, and deterministic mock-server coverage, the required real local vanilla-server test passed with `online-mode=false` and `network-compression-threshold=-1`. Cubic completed Login and Configuration, entered Play, and the vanilla server spawned `CubicTest` into the world before Cubic deliberately disconnected. Phase 8 and later phases have not begun.
