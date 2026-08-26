# Cubic

Cubic is an independent project intended to become a Minecraft: Java Edition-compatible multiplayer client, primarily implemented in Rust.

Cubic does not contain Mojang or Microsoft code or assets. Minecraft gameplay has not yet been implemented. The repository currently contains the architectural scaffold, a native graphics bootstrap, and low-level binary protocol primitives.

## Current state

The workspace builds a small `cubic-app` executable that opens a native window and clears it through wgpu. `cubic-protocol` provides transport-independent primitive codecs and uncompressed frame reconstruction, but no sockets, packet schemas, state-specific packet IDs, compression, authentication, game state, Minecraft resources, or gameplay.

## Build and validate

```text
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p cubic-app
```
