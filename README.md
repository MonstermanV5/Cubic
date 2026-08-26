# Cubic

Cubic is an independent project intended to become a Minecraft: Java Edition-compatible multiplayer client, primarily implemented in Rust.

Cubic does not contain Mojang or Microsoft code or assets. Minecraft gameplay has not yet been implemented. The repository currently contains the architectural scaffold, a native graphics bootstrap, low-level binary protocol primitives, and a raw Java Edition NBT codec.

## Current state

The workspace builds a small `cubic-app` executable that opens a native window and clears it through wgpu. `cubic-protocol` provides transport-independent primitive codecs, uncompressed frame reconstruction, and bounded raw Java Edition NBT, but no sockets, packet schemas, state-specific packet IDs, compression, authentication, game state, Minecraft resources, or gameplay.

## Build and validate

```text
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p cubic-app
```
