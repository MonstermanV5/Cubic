# Testing Strategy

## Phase 1

Current testing is deliberately small. A workspace integration test verifies that `cubic-app` can consume the intentional public API from `cubic-core`. Cargo also builds and tests every placeholder crate. Formatting and warning-free Clippy checks are part of Phase 1 acceptance.

## Phase 2

Phase 2 retains the Phase 1 dependency test and adds a unit test for the platform-independent rule that a presentation surface is drawable only when both dimensions are non-zero. Normal workspace tests remain headless and do not create a native window or require a physical GPU.

Windows CI compiles `cubic-app`. A local Windows smoke test is required to prove native window creation, adapter/device initialization, visible presentation, resizing, minimizing/restoring, and clean close behavior.

The macOS CI job installs `aarch64-apple-ios`, confirms that the iPhoneOS SDK is available, compile-checks every workspace target for ARM64 iOS, and builds the `cubic-platform` Rust library for that target. This proves Rust code and dependency compatibility; it does not link a native application bundle or create, sign, install, or run an iOS application.

## Phase 3

Phase 3 adds deterministic known-vector and boundary tests for fixed-width primitives, VarInt, VarLong, strings, byte arrays, UUIDs, packed positions, BitSets, and uncompressed framing. Explicit malformed-input tests cover truncated values, invalid UTF-8, negative and oversized lengths, overlong variable integers, invalid coordinates, buffer limits, and incomplete frames. These tests assert specific structured errors.

Property tests exercise arbitrary integer and floating-point bit-pattern round trips, bounded Unicode strings, UUID values, representable positions, byte arrays, BitSets, and frame streams split at generated fragmentation boundaries. The framing property verifies exact, ordered reconstruction of multiple frames. Normal tests remain synchronous and headless; no socket, native window, or GPU is needed for protocol tests.

## Phase 4

Phase 4 adds independent hand-authored NBT vectors for named and unnamed roots, numeric payloads, nested compounds, lists, arrays, and Modified UTF-8 edge cases. Round-trip tests cover every tag type and compare floating-point values by raw bits. Modified UTF-8 vectors include NUL, BMP text, supplementary surrogate pairs, and unpaired surrogates.

Malformed-input tests assert structured failures for bad roots and type IDs, truncated names and payloads, malformed Modified UTF-8, invalid and oversized collection lengths, illegal positive TAG_End lists, unknown list types, unterminated compounds, trailing standalone input, excessive depth, total-tag exhaustion, and cumulative resource-budget exhaustion. The reader-based root APIs are separately tested to leave later packet fields available.

Property tests cover arbitrary numeric values and floating-point bits, arbitrary bounded `u16` sequences for lossless Java strings, all array types, homogeneous lists, shallow compounds, and bounded nested compound/list structures. All Phase 4 tests are raw, synchronous, and headless; no Minecraft files, server, socket, compression layer, window, or GPU is involved.

## Future testing

- Unit tests will cover isolated logic and error cases.
- Property tests will cover parsers, codecs, and other invariant-heavy code where suitable.
- Integration tests will verify interactions across crate boundaries.
- Full packet-schema fixtures will later cover known valid and malformed packets without containing copyrighted game assets. Phase 3 already includes small public wire-format vectors for primitive codecs.
- Mock-server tests will later exercise connection behavior deterministically.
- Real vanilla-server tests will later validate end-to-end compatibility in controlled environments.
- Rendering regression tests will later compare deterministic scenes or render outputs.
- Performance benchmarks will later track hot paths and memory behavior.
- Every previously supported Minecraft version must eventually remain regression-tested as new versions are added.

The future suites above do not yet exist unless explicitly identified as Phase 3 or Phase 4 coverage. Tests must not be weakened, skipped, deleted, or rewritten simply to make an implementation pass.
