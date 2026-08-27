# Project made almost entirely by AI, including this file. I am testing current AI limits

# Cubic

Cubic is an independent project intended to become a Minecraft: Java Edition-compatible multiplayer client, primarily implemented in Rust.

Cubic does not contain Mojang or Microsoft code or assets. Minecraft gameplay and world rendering have not yet been implemented. The repository currently contains the architectural scaffold, native graphics bootstrap, low-level protocol/NBT foundations, bounded server-list Status query, version-data architecture, development-only offline login bootstrap, and the completed Phase 8 Chat Mode MVP. Its real-server and Windows UI acceptance passed against a local vanilla Java Edition 26.1.2 server.

## Current state

With no arguments, `cubic-app` opens the Phase 2 clear-frame window. `status` queries a server list entry, and `dev-login` retains the completed Phase 7 one-shot acceptance path. `chat` opens Cubic's full-window, low-idle-redraw Chat Mode and runs its persistent network task independently of the UI. This temporary development path targets only Java 26.1.2 / protocol 775 on an offline, uncompressed local server. Compression, encryption, authentication, game/world state, Minecraft resources, and 3D gameplay remain unimplemented.

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

The server also needs `resource-pack=`, `require-resource-pack=false`, and `enable-code-of-conduct=false`. Chat Mode retains the connection, handles the required protocol-775 control plane, displays bounded chat history, and sends legitimate unsigned messages for the offline development profile. Its Windows integration uses the native system clipboard and an installed system CJK font fallback; Cubic redistributes no platform font files. Slash commands are deliberately unsupported because Cubic does not yet possess the generated command graph or authenticated signing session.
