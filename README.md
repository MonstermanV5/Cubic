# Project made almost entirely by AI, including this file. I am testing current AI limits

# Cubic

Cubic is an independent project intended to become a Minecraft: Java Edition-compatible multiplayer client, primarily implemented in Rust.

Cubic does not contain Mojang or Microsoft code or assets. The repository currently contains the architectural scaffold, native graphics bootstrap, protocol/NBT and generated-data foundations, authenticated Chat Mode, semantic world/chunk state, Phase 16's accepted runtime-only official block-resource/model terrain path, Phase 17's accepted basic movement/collision foundation, and Phase 17B's accepted vanilla block-collision fidelity follow-up.

## Current state

With no arguments, `cubic-app` opens the Phase 2 clear-frame window. `status`, `dev-login`, and `chat` retain their accepted behavior. `world <server> --username <name>` opens the offline-development Play window that renders bounded decoded chunks using official exact-version block resources verified and cached by Cubic. It includes the accepted Phase 17 movement controller, Phase 17B collision fidelity, and Phase 18's in-progress same-session `CHAT`/`PLAY` presentation toggle. It requires the Phase 11 generated game-data artifact and may populate the Phase 10 official client cache before opening the window. Entities and block interaction remain unimplemented. See `docs/WORLD_RENDERING.md`, `docs/BLOCK_RESOURCES.md`, `docs/MOVEMENT.md`, and `docs/CHAT_MODE.md`.

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
