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

## Phase 5

Phase 5 protocol tests use hand-authored packet vectors and cover Handshake, Status Request/Response, Ping/Pong nonce validation, rich and simple MOTDs, unknown JSON fields, malformed JSON, missing or invalid required fields, wrong packet IDs, trailing payloads, and configured JSON bounds.

`cubic-network` integration tests run only against an in-process Tokio TCP listener. They validate logical handshake host/port preservation, default protocol selection, byte-by-byte fragmentation (including a split multi-byte length prefix), multiple buffered frames, successful ping timing, early EOF, partial frames, malformed and oversized frames, wrong packet IDs/nonces, malformed/invalid Status JSON, and connect/read/overall timeout categories. Address tests cover hostnames, IPv4, bracketed IPv6, default ports, and rejected ambiguous input. No public server is contacted by the automated tests.

A real Java Edition server smoke test is still required before Phase 5 can be marked complete. Run `cargo run -p cubic-app -- status <host[:port]> [--protocol <number>]` only against a server the tester is authorized to query, then verify the reported version, protocol, players, MOTD, favicon presence, and plausible latency.

## Phase 6

Phase 6 adds 19 tests in `cubic-version` and 3 integration tests in `version-generator`. All tests are synchronous, headless, and contact no network.

`cubic-version` identity tests (5) cover: release and snapshot style IDs accepted as opaque strings; empty, dot, double-dot, and traversal IDs rejected; path separators, control characters, and overlong IDs rejected; Windows-reserved names and filesystem-reserved characters rejected; and the compatibility profile ID restricted namespace.

`cubic-version` model tests (7) cover: current format version accepted and exposed; unsupported format version rejected before typed parsing; malformed, missing, and invalid kind JSON producing distinct structured errors; negative protocol version and invalid compatibility profile rejected; duplicate compatibility profile IDs and duplicate catalog version IDs rejected; serialization producing canonical, newline-terminated, byte-identical output for different insertion orders; and catalog ordering and multi-result protocol lookup being deterministic.

`cubic-version` store tests (7) cover: exact version lookup returning `Some` or `None` explicitly; protocol lookup returning zero, one, or multiple results in version-ID order (exercising a shared protocol number across a release and a snapshot); release and snapshot datasets coexisting in deterministic sorted order; catalog generation producing byte-identical output across repeated runs; catalog/dataset protocol mismatch detected on open; directory/declared version ID mismatch detected during catalog build; and oversized metadata rejected before JSON parsing.

`version-generator` CLI tests (3) cover: `validate` accepting consistent synthetic data and reporting the correct count; `validate` rejecting inconsistent data (protocol mismatch written after fixture copy) with a non-zero exit code; and `build-catalog` producing byte-identical output across runs.

All tests use synthetic fixtures under `crates/cubic-version/tests/fixtures/version-data`: two releases (`cubic-test-release-a` with protocol 9000, `cubic-test-release-b` with protocol 9001 and one compatibility profile) and one snapshot (`cubic-test-snapshot` with protocol 9000 and one compatibility profile). The two versions sharing protocol 9000 exercise multi-result lookup. Tests that mutate fixtures copy them to temporary directories and clean up on drop.

## Future testing

- Unit tests will cover isolated logic and error cases.
- Property tests will cover parsers, codecs, and other invariant-heavy code where suitable.
- Integration tests will verify interactions across crate boundaries.
- Full packet-schema fixtures will later cover known valid and malformed packets without containing copyrighted game assets. Phase 3 already includes small public wire-format vectors for primitive codecs.
- Mock-server tests already exercise the Phase 5 Status exchange; broader state and adverse-network simulations remain future work.
- Real vanilla-server tests will validate end-to-end compatibility in controlled environments. The Phase 5 manual smoke test is not yet recorded as complete.
- Rendering regression tests will later compare deterministic scenes or render outputs.
- Performance benchmarks will later track hot paths and memory behavior.
- Every previously supported Minecraft version must eventually remain regression-tested as new versions are added.

The future suites above do not yet exist unless explicitly identified as Phase 3 or Phase 4 coverage. Tests must not be weakened, skipped, deleted, or rewritten simply to make an implementation pass.
