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

## Phase 7

Phase 7 adds independent hand-authored protocol-775 vectors for the Login Handshake, Login Start, Login Success, acknowledgements, plugin/cookie responses, Client Information, Keep Alive, Pong, Known Packs, Finish Configuration, and initial Play Login identification. Malformed Login Success, unexpected IDs, excessive Known Packs, malformed disconnect NBT, and an empty initial Play Login assert structured failures.

`cubic-network` tests use only an in-process Tokio TCP listener. They verify the exact handshake protocol/host/port/Login next-state and Login Start username/zero UUID, successful Login-to-Configuration-to-Play progression, Login Acknowledged, Client Information, declined plugin negotiation, empty Known Packs, keepalive/ping replies, bounded skip behavior, fragmented packets, coalesced packets, Login and Configuration disconnects, unexpected and malformed packets, Encryption/Compression rejection, EOF in each state, individual and overall timeouts, and oversized framing. The shared Play handoff suite additionally places an irrelevant protocol-775 Custom Payload before initial Play Login, coalesces early Keep Alive/Ping/teleport/cookie controls, exercises immediate reconfiguration, and verifies that an early Disconnect retains its reason. The existing Phase 5 Status suite runs unchanged against the extracted shared transport.

No automated test downloads, starts, or contacts a Minecraft server. The required manual Phase 7 acceptance test passed against vanilla 26.1.2 at `localhost:25565` with `online-mode=false` and `network-compression-threshold=-1`. Running `cargo run -p cubic-app -- dev-login localhost:25565 --username CubicTest` completed Login and Configuration and reported `State: Play`; the vanilla server logged that `CubicTest` joined and spawned into the world before Cubic deliberately disconnected.

## Phase 8

Nine independent protocol-775 Play vectors cover exact control replies, the complete unsigned outbound chat body and frame length (including offset, raw fixed 20-bit acknowledgement bytes, and trailing checksum), signed-versus-unsigned Player Chat acknowledgement classification, Player/Disguised/System Chat, simple/nested text, Unicode, malformed known packets, input bounds, and safe identification of irrelevant world frames. The persistent in-process mock server completes Login and Configuration, then checks Play Client Information, Keep Alive, Pong, teleport confirmation, chunk-batch acknowledgement, Player Loaded, fragmented Unicode System Chat, verifies that unsigned Player Chat does not produce an invalid acknowledgement, checks the complete outgoing unsigned chat payload with no trailing bytes, and performs a clean disconnect. Existing Status and one-shot development-login suites remain unchanged and passing.

Five headless `cubic-ui` tests cover deterministic oldest-first history eviction, Java UTF-16 input bounds/control filtering, empty/nonempty send actions, visible connection/disconnection transitions, and byte-for-byte preservation of common Latin, Cyrillic, emoji, and CJK text. `cubic-platform` tests verify that its system CJK font is appended behind—not substituted for—egui's existing fallback chain and that Windows candidate priority is deterministic. Three network unit tests cover regular-event backpressure, the separate critical-event slot, and explicit Unicode/command/input policy. These tests do not create a GPU or native window. Native clipboard round trips and actual glyph appearance remain manual because headless tests cannot prove OS clipboard ownership or GPU text rendering.

The final real Phase 8 server/UI acceptance passed against a local vanilla Java Edition 26.1.2 server. It verified persistent bidirectional chat, Unicode transport, visible common CJK fallback glyphs, message spam, bounded eviction, scrolling, resize/minimize/restore, Enter and button send actions, alerts, long idle operation, clean disconnect, external application → Cubic paste, and Cubic copy/cut → Windows system clipboard. Release-mode idle use on the tested Windows machine was approximately 5% CPU with brief spikes near 10%, 115 MiB RAM, and 1.3% GPU; this is accepted for the MVP, with deeper optimization deferred.

## Phase 9

Headless `cubic-auth` tests cover the RFC 7636 S256 vector, random verifier/state shape, strict callback parsing, OAuth error callbacks, client/profile identifiers, redacted secret formatting, deterministic Xbox User Token and XSTS JSON vectors, known XSTS account-error mapping, and fake secure-store behavior. Experimental XAL tests cover provider selection, separate backend/device records, targeted logout, P-256 key generation and restoration, public JWK construction, the canonical signed-message bytes, raw ES256 signature verification, desktop redirect/state validation, duplicate/missing callback fields, misleading hosts, redacted captured codes, device and XSTS request shapes, missing SISU headers, malformed responses, entitlement failure, and refresh-token rotation. `cubic-platform` tests cover the exact initial authorization endpoint, reviewed HTTPS identity-host allowlist, cancellation, and timeout transitions without creating a browser. No automated test contains or contacts a real account.

The experimental XAL backend passed real account authentication, the automatic WebView2 login UX, Credential Manager persistence, restart-time silent refresh, Mojang session join, encrypted/compressed Login, Configuration, and Play against online-mode vanilla 26.1.2. The final persistent test additionally passed `enforce-secure-profile=true`, Mojang player-certificate/session establishment, signed outgoing chat accepted by vanilla, System Chat reception, bounded acknowledgement traffic, and clean disconnect. Headless CI does not instantiate WebView2 or use a real account.

Protocol/network tests add an independent Encryption Response vector, positive and negative Java-BigInteger server-hash vectors, continuous AES/CFB8 fragmentation, compression below/at threshold, malformed declared-size/decompression rejection, complete signed-chat/session-update wire vectors, signing-input field-order tests, a synthetic alternate secure-chat profile, bounded last-seen/checksum behavior, strict incoming indices, and the bounded early-Play handoff. A deterministic in-process persistent-Play test supplies a synthetic signing certificate, verifies the session-update and full signed outgoing packet, and closes through the bounded command channel. Authentication tests cover certificate response parsing, key-pair/algorithm/size validation, timestamps/expiry/refresh policy, deterministic RSA signing/verification, and secret redaction. Existing Status, offline Login, and Chat Mode suites remain regression coverage. A full mock XAL HTTP sequence, cryptographic verification of other players' chat, in-session certificate rotation, and iOS Keychain host tests remain deferred limitations.

## Runtime logging and Phase 10

Real Phase 10 acceptance passed against the official Mojang metadata/artifact chain for exact version `26.1.2`. The first metadata bootstrap resolved asset index `30` with 4,750 logical assets from the network; the second reused verified cache data. Invalid-version rejection, explicit 38,113,927-byte client-JAR acquisition with SHA-1 `4e618f09a0c649dde3fdf829df443ce0b8831e65`, repeated JAR cache reuse, and a fully offline cached bootstrap all passed. The downloaded JAR was never executed.

Persistent logging also passed manual acceptance for graphics, auth/network state transitions, encryption, compression, chat-session establishment, outgoing plaintext, decoded Player/System/Disguised Chat, and Phase 10 cache activity. Autcraft diagnostics proved its missing visible message body was caused by the deliberately incomplete plain-text component projection after correct protocol decoding; this is retained as a Phase 25 regression case. Autcraft is no longer an allowed Cubic test target.

## Phase 11 generated game data

Phase 11 tests use only small synthetic `registries.json`, `blocks.json`, client bytes, and metadata. They cover deterministic byte-for-byte generation, schema rejection, exact version identity, provenance hashes, verified source-client size/hash, malformed reports, duplicate keys and identifiers, duplicate raw/state IDs, sparse IDs, unknown registries and non-`minecraft` namespaces, block/default/property/state validation, item/entity lookups, version isolation, added/removed entries, narrow inspection, and generated-artifact validation. Tests do not use the developer's Phase 10 cache, execute Minecraft, contact Mojang, or connect to a server.

Real Phase 11 acceptance used Mojang's official 26.1.2 Data Generator with the official 76-entry launcher classpath. Its 516,149-byte registry report and 6,239,720-byte block report generated 95 registries, 1,168 blocks, 29,873 block states, 1,506 items, and 157 entity types. The 7,763,125-byte artifact had content SHA-1 `936dcc94a71fc8006807819a88f45ec6bfd23f2c`; two identical generations, independent validation, and block/item/entity spot checks all passed. No real report or generated artifact is a test fixture.

## Phase 12 packet schemas

Phase 12 tests use small synthetic `packets.json` and ProtoDef inputs only. They cover strict recursive duplicate-key parsing, duplicate ID/identity rejection, invalid state/direction/schema versions, sparse IDs, exact revision/hash/version checks, official-plus-supplemental merge, Mojang ID disagreement, state/direction-scoped aliases, ambiguous identities, unsupported constructs, field order and name normalization, Boolean conditionals, optionals, nested structures, arrays, enums, NBT/UUID/Position/BitSet composition, bounded strings/byte arrays/lists, deterministic multi-version isolation, artifact re-parsing, malformed/truncated/trailing payloads, and encode/decode symmetry. A property test exercises arbitrary bounded VarInt lists. Generator CLI tests validate and inspect temporary artifacts without network or launcher dependencies.

The real 26.1.2 `reports/packets.json` was generated offline from the installed official client and merged with PrismarineJS minecraft-data revision `8a80816cbfb3fe2b609f2cde4e57796c8033af61`. Manual acceptance passed for the resulting artifact: it produced all 256 official definitions, 96 bounded layouts, and 160 categorized identity-only definitions; 34 ID and 14 structural overlap checks passed against the working manual v775 profile, and repeated generation was byte-identical. No real Mojang report, client JAR, raw third-party dataset, or generated packet artifact is committed or used by normal tests. Since no live codec was migrated, existing regression tests protect live behavior.

Logging unit tests use temporary directories to verify `latest.log` creation, five-launch retention, deterministic rotation, invalid destinations, and the deliberately small INFO/DEBUG configuration. Protocol coverage retains signed plaintext and optional decorated Player Chat components independently before UI presentation. Tests do not initialize the process-global subscriber or touch the user's log directory.

`cubic-version` unit tests parse synthetic release/snapshot manifests, latest selection, forward-compatible version kinds, timestamps, unknown benign fields, selected version/download descriptors, malformed JSON, malformed hashes, and non-HTTPS URLs. Existing Phase 6 identity/store tests remain unchanged.

`cubic-resources` tests use an in-memory fetcher and synthetic bytes, never Mojang services. They cover first-network bootstrap, offline verified-cache reuse, corrupt-cache refetch, exact/missing/pathological version selection, deterministic asset lookup, content-addressed deduplication, hash/size failures, untrusted URLs, explicit client-JAR acquisition and cache reuse, and non-promoted partial files. No Mojang JAR or asset is present in fixtures.

## Future testing

- Unit tests will cover isolated logic and error cases.
- Property tests will cover parsers, codecs, and other invariant-heavy code where suitable.
- Integration tests will verify interactions across crate boundaries.
- Full packet-schema fixtures will later cover known valid and malformed packets without containing copyrighted game assets. Phase 3 already includes small public wire-format vectors for primitive codecs.
- Mock-server tests exercise Status, development Login/Configuration, and Phase 8's bounded persistent Play/chat subset.
- Real vanilla-server tests validate end-to-end compatibility in controlled environments. The Phase 5, Phase 7, Phase 8, and Phase 9 acceptance tests passed, including Phase 9's persistent signed Chat Mode with secure-profile enforcement. Exploratory BlossomCraft and Autcraft successes are diagnostic evidence only, not comprehensive proxy/plugin compatibility certification.
- Rendering regression tests will later compare deterministic scenes or render outputs.
- Performance benchmarks will later track hot paths and memory behavior.
- Every previously supported Minecraft version must eventually remain regression-tested as new versions are added.

The future suites above do not yet exist unless explicitly identified as Phase 3 or Phase 4 coverage. Tests must not be weakened, skipped, deleted, or rewritten simply to make an implementation pass.
