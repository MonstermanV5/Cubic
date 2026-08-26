# Testing Strategy

## Phase 1

Current testing is deliberately small. A workspace integration test verifies that `cubic-app` can consume the intentional public API from `cubic-core`. Cargo also builds and tests every placeholder crate. Formatting and warning-free Clippy checks are part of Phase 1 acceptance.

## Future testing

- Unit tests will cover isolated logic and error cases.
- Property tests will cover parsers, codecs, and other invariant-heavy code where suitable.
- Integration tests will verify interactions across crate boundaries.
- Protocol fixtures will later cover known valid and malformed byte streams without containing copyrighted game assets.
- Mock-server tests will later exercise connection behavior deterministically.
- Real vanilla-server tests will later validate end-to-end compatibility in controlled environments.
- Rendering regression tests will later compare deterministic scenes or render outputs.
- Performance benchmarks will later track hot paths and memory behavior.
- Every previously supported Minecraft version must eventually remain regression-tested as new versions are added.

These future suites do not exist in Phase 1. Tests must not be weakened, skipped, deleted, or rewritten simply to make an implementation pass.

