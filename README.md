# Cubic

Cubic is an independent project intended to become a Minecraft: Java Edition-compatible multiplayer client, primarily implemented in Rust.

Cubic does not contain Mojang or Microsoft code or assets. Minecraft gameplay has not yet been implemented. The repository currently contains the architectural scaffold, a native graphics bootstrap, low-level binary protocol primitives, a raw Java Edition NBT codec, and a bounded server-list Status query.

## Current state

With no arguments, `cubic-app` opens a native window and clears it through wgpu. Its `status` subcommand performs only the unencrypted, uncompressed Java Edition server-list query through `cubic-network`. `cubic-protocol` provides transport-independent primitives, framing, raw NBT, and the narrow Status packet codecs. Login/play packets, compression, encryption, authentication, game state, Minecraft resources, and gameplay remain unimplemented.

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
