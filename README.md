# Project made almost entirely by AI, including this file. I am testing current AI limits

# Cubic

Cubic is an independent project intended to become a Minecraft: Java Edition-compatible multiplayer client, primarily implemented in Rust.

Cubic does not contain Mojang or Microsoft code or assets. Minecraft gameplay and world rendering have not yet been implemented. The repository currently contains the architectural scaffold, native graphics bootstrap, low-level protocol/NBT foundations, bounded server-list Status query, version-data architecture, completed Chat Mode/authenticated secure-chat and generated-data foundations, persistent local diagnostics, and the Phase 13 semantic world-state model.

## Current state

With no arguments, `cubic-app` opens the Phase 2 clear-frame window. `status` queries a server list entry, and `dev-login` retains the completed Phase 7 one-shot acceptance path. `chat` opens Cubic's full-window, low-idle-redraw Chat Mode and runs its persistent network task independently of the UI. The original `--username` path remains the offline development mode; `chat <server> --backend xal` selects the authenticated experimental-XAL path. Java 26.1.2 / protocol 775 is the first manually implemented reference profile, not Cubic's permanent compatibility boundary. Phase 13 now maintains small authoritative session/dimension/position metadata during Chat Mode, but chunks, entities, movement, Minecraft resources, and 3D gameplay remain unimplemented. See `docs/WORLD_STATE.md`.

## Build and validate

```text
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p cubic-app
```

To manually query a server you are authorized to contact:

```text
cargo run -p cubic-app -- status example.org:25565
cargo run -p cubic-app -- status example.org --protocol <signed-protocol-number>
```

The default handshake protocol value is `-1`, a conventional generic probe. Use `--protocol` when testing a server that requires a specific protocol version. Cubic does not perform DNS SRV discovery in this phase.

For an authorized local vanilla 26.1.2 development server configured with `online-mode=false` and `network-compression-threshold=-1`:

```text
cargo run -p cubic-app -- dev-login localhost:25565
cargo run -p cubic-app -- dev-login localhost:25565 --username CubicTest
```

This command deliberately disconnects after confirming the initial Play Login packet. It does not provide gameplay or a persistent client session.

To exercise the Phase 8 Chat Mode MVP against that same controlled server:

```text
cargo run -p cubic-app -- chat localhost:25565 --username CubicTest
```

The server also needs `resource-pack=`, `require-resource-pack=false`, and `enable-code-of-conduct=false`. Chat Mode retains the connection, handles the required protocol-775 control plane, displays bounded chat history, and sends legitimate unsigned messages for the offline development profile. Its Windows integration uses the native system clipboard and an installed system CJK font fallback; Cubic redistributes no platform font files. Slash commands remain deliberately unsupported because Cubic does not yet possess the generated command graph needed to determine which arguments require signatures; Cubic never fabricates argument signatures.

Phase 9 keeps Cubic's own public Microsoft application-ID flow as its intended production backend. That real flow reaches Minecraft Services but is rejected with HTTP 403 `Invalid app registration`, confirming an external approval requirement. An explicit `--backend xal` development option adds experimentally validated first-party Xbox/Minecraft launcher interoperability without replacing Cubic's identity. Its Windows login uses a dedicated private WebView2 window to capture the state-validated redirect automatically. Real XAL authentication, secure persistence and silent refresh, player-certificate acquisition, online-mode session join, encryption, compression, secure player-chat session establishment, and persistent signed Chat Mode have passed against vanilla 26.1.2 with secure-profile enforcement. It is not assumed suitable for public distribution. Neither backend uses a client secret or receives the Microsoft password. See `docs/AUTHENTICATION.md`.

Phase 10's development bootstrap is `cargo run -p cubic-app -- bootstrap-version <version-id>`. It resolves only official Mojang manifest metadata, verifies and caches the selected version metadata and asset index, and exposes on-demand content-addressed assets without downloading the full asset set. Add `--client-jar` to explicitly acquire and verify the official client JAR; Cubic never executes or redistributes it. See `docs/RESOURCES.md`. Persistent local logs are documented in `docs/LOGGING.md`.

Phase 11's development generator consumes bounded `registries.json` and `blocks.json` reports produced by Mojang's version-scoped Data Generator, verifies the exact Phase 10 cached client artifact, and emits a deterministic Cubic `game-data.json`. Generated data is an immutable vanilla baseline; future server-supplied registries remain a separate runtime concern. See `docs/GAME_DATA.md`.

Phase 12 merges authoritative exact-version packet identities/IDs from Mojang's `reports/packets.json` with pinned, independently maintained ProtoDef layouts. The current 26.1.2 generation produces 96 bounded layouts and retains 160 explicit identity-only definitions; proven manual bootstrap codecs remain the live networking path. See `docs/PACKETS.md`.
