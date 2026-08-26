# Instructions for Coding Agents

These rules apply throughout this repository.

## Scope and architecture

1. Read this `AGENTS.md` and the relevant files in `docs/` before changing architecture or implementing a subsystem.
2. Minecraft-version-specific data must not be scattered throughout engine code.
3. Prefer generated and version-data-driven behavior where practical.
4. Do not implement features outside the current task's stated scope.
5. Avoid premature optimization, but preserve an architecture that allows later profiling and multithreading.
6. Do not introduce a dependency without a reason.
7. Keep public APIs between crates intentionally small.
8. Avoid cyclic crate dependencies.
9. Prefer deterministic behavior in generators and tests.

## Intellectual property and secrets

10. Never commit, redistribute, or embed Mojang/Microsoft copyrighted Minecraft assets, Minecraft client/server JARs, or copied Minecraft source code.
11. Never commit credentials, Microsoft authentication tokens, refresh tokens, passwords, private keys, or signing material.

## Safety and reliability

12. Treat all network input, resource packs, downloaded metadata, archives, and server data as untrusted.
13. Parsers must return structured errors for malformed input rather than panic.
14. Avoid unchecked indexing or unbounded allocations based on external input.
15. Rendering must never perform blocking network or filesystem operations.
16. Chunk decoding, meshing, and similarly heavy CPU work must be designed so they can execute away from the render thread.

## Portability

17. Platform-specific functionality belongs behind abstractions and must remain isolated inside `cubic-platform` or clearly platform-specific modules.
18. The shared game/client engine must not assume Windows- or iOS-specific APIs.

## Tests and completion

19. New functionality requires appropriate tests.
20. Do not weaken, delete, skip, or rewrite tests merely to make an implementation pass.
21. Before completing applicable Rust tasks, run:

    ```text
    cargo fmt --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace
    ```

22. If one of those commands cannot run, state exactly why instead of silently skipping it.
23. At completion of a task, report:
    - files changed;
    - important design decisions;
    - commands executed;
    - test results; and
    - known limitations.

