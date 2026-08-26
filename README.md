# Project made almost entirely by AI, including this file. I am testing current AI limits

# Cubic

Cubic is an independent project intended to become a Minecraft: Java Edition-compatible multiplayer client, primarily implemented in Rust.

Cubic does not contain Mojang or Microsoft code or assets. Minecraft gameplay has not yet been implemented. The repository currently contains the architectural scaffold, a native graphics bootstrap, low-level binary protocol primitives, a raw Java Edition NBT codec, a bounded server-list Status query, version-data architecture, and a development-only offline login bootstrap.

## Current state

With no arguments, `cubic-app` opens a native window and clears it through wgpu. Its `status` subcommand performs the unencrypted, uncompressed Java Edition server-list query. Its `dev-login` subcommand implements only the temporary Minecraft Java 26.1.2 / protocol 775 path needed to join a controlled offline-mode local server through Configuration and observe the initial Play Login packet. Persistent Play networking, compression, encryption, authentication, game state, Minecraft resources, and gameplay remain unimplemented.

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
