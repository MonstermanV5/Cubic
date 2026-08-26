# Cubic

Cubic is an independent project intended to become a Minecraft: Java Edition-compatible multiplayer client, primarily implemented in Rust.

Cubic does not contain Mojang or Microsoft code or assets. Minecraft functionality has not yet been implemented. The repository currently contains the architectural scaffold and a Phase 2 native graphics bootstrap.

## Current state

The workspace builds a small `cubic-app` executable that opens a native window and clears it through wgpu. Networking, authentication, protocol handling, game state, Minecraft resources, and gameplay remain unimplemented.

## Build and validate

```text
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p cubic-app
```
