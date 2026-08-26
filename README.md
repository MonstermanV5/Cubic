# Cubic

Cubic is an independent project intended to become a Minecraft: Java Edition-compatible multiplayer client, primarily implemented in Rust.

Cubic does not contain Mojang or Microsoft code or assets. Minecraft functionality has not yet been implemented. The repository currently contains only the Phase 1 architectural scaffold: workspace boundaries, placeholder crates, project guidance, and documentation for future development.

## Current state

The workspace builds a small `cubic-app` executable that confirms the scaffold is initialized and exits. Networking, authentication, protocol handling, rendering, game state, resources, and platform integrations are deliberately out of scope for this phase.

## Build and validate

```text
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p cubic-app
```

