# Roadmap

Status markers: `[ ]` not started, `[~]` partial, `[x]` complete, `[!]` blocked.

- [x] Phase 1 - repository and architecture
- [x] Phase 2 - Windows/iOS graphical bootstrap
- [x] Phase 3 - protocol primitives
- [x] Phase 4 - NBT
- [x] Phase 5 - server status ping
- [x] Phase 6 - version-data architecture
- [x] Phase 7 - offline-mode development-server login
- [x] Phase 8 - Chat Mode MVP
- [x] Phase 9 - Microsoft/Minecraft authentication
- [x] Phase 10 - Minecraft version/resource bootstrap
- [x] Phase 11 - generated game registries/data
- [x] Phase 12 - generated packet codecs
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

Phase 7 is complete. In addition to the isolated Java 26.1.2 / protocol 775 bootstrap profile, shared framed transport, explicit Login/Configuration state machine, development CLI, bounds, structured errors, and deterministic mock-server coverage, the required real local vanilla-server test passed with `online-mode=false` and `network-compression-threshold=-1`. Cubic completed Login and Configuration, entered Play, and the vanilla server spawned `CubicTest` into the world before Cubic deliberately disconnected.

Phase 8 is complete. Its final real-server and Windows Chat Mode acceptance passed against vanilla Java Edition 26.1.2, including persistent bidirectional chat, Unicode transport, common CJK glyph fallback, native clipboard interoperability in both directions, spam handling, bounded history, scrolling, resize/minimize/restore, Enter and button send actions, warnings, long idle operation, and clean disconnect. Release-mode idle resource use was acceptable for the MVP and is intentionally deferred from optimization work.

Phase 9 is complete for its accepted Windows technical scope. Cubic's preferred own-Entra-client-ID path passed real Microsoft OAuth, Xbox User Authentication, and XSTS, then Minecraft Services returned HTTP 403 `Invalid app registration`; this remains an external registration-approval blocker and Cubic does not bypass it. The separate experimental XAL/SISU backend passed real end-to-end authentication, automatic private WebView2 redirect capture, Credential Manager persistence, restart-time silent refresh, Mojang session join, encrypted/compressed Login, Configuration, and persistent Play against vanilla 26.1.2 with `online-mode=true`, `enforce-secure-profile=true`, and compression enabled. Mojang player-certificate acquisition, RSA-2048 outgoing chat signing, the version-isolated Player Chat Session, bounded last-seen/acknowledgement state, signed chat accepted by vanilla, System Chat reception, and clean disconnect all passed real acceptance. The hardened Login-to-Play handoff accepts bounded legal early Play traffic and identifies protocol-775 clientbound `0x18` as Custom Payload. Exploratory BlossomCraft and Autcraft sessions reached persistent authenticated Chat Mode, but this is not a Phase 27 compatibility claim.

Known limitations remain explicit: XAL is experimental and not assumed authorized for public distribution; CubicEntra awaits external Minecraft Services approval; native iOS Keychain/callback support is incomplete; incoming signed messages are not cryptographically verified against other players' session-key graph; certificates are refreshed per launch rather than rotated inside exceptionally long sessions; slash-command signing awaits command-tree data; and complete rich-text/translation presentation remains Phase 25 work. Autcraft exposed examples of that presentation limitation.

Phase 10 is complete. Real official-Mojang acceptance for exact version `26.1.2` passed: a fresh network bootstrap resolved Release metadata and asset index `30` with 4,750 logical assets; a second run reused the verified cache; invalid-version selection failed cleanly; explicit client-JAR acquisition verified the published 38,113,927-byte size and SHA-1 before promotion; a repeated JAR request reused the cache; and a fully offline metadata bootstrap succeeded from already-verified artifacts. Persistent runtime logging also passed real-use acceptance.

Phase 11 is complete. Mojang's official 26.1.2 Data Generator ran with the official launcher-resolved classpath; its registry and block reports produced 95 registries, 1,168 blocks, 29,873 block states, 1,506 items, and 157 entity types. The deterministic 7,763,125-byte schema 1 artifact had SHA-1 `936dcc94a71fc8006807819a88f45ec6bfd23f2c` on both generations, independently validated, and passed representative block/item/entity spot checks. No client JAR, Mojang library/report, or real generated artifact is committed.

Phase 12 passed real manual acceptance. The official 26.1.2 Data Generator report remains authoritative for 256 exact packet identities/IDs; pinned PrismarineJS revision `8a80816cbfb3fe2b609f2cde4e57796c8033af61` supplements ordered structure. The accepted deterministic merged artifact contains 96 bounded layouts and 160 categorized identity-only definitions, passes 34 bootstrap ID and 14 structural checks, is 114,441 bytes, and has SHA-1 `c43e6035f08d250cf3f0e91a558fe105e3b6d040`. No live network codec was migrated, no real report/raw source/artifact is committed, and Phase 13 has not started.
