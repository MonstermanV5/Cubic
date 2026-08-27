# Status, Development Login, and Chat Mode Networking

Phase 5 implements Status, Phase 7 adds the validated development login, and Phase 8 retains that connection for one narrowly scoped persistent Chat Mode session. This is not general Play networking.

## Ownership and flow

`cubic-app` parses the command and owns the Tokio runtime. `cubic-network` owns address parsing, DNS/TCP connection, deadlines, stream reads and writes, and query sequencing. `cubic-protocol` remains synchronous and owns packet bytes, framing, JSON bounds, and typed Status values.

```text
parse address
  -> resolve/connect TCP
  -> Handshake(next state = Status)
  -> Status Request
  <- bounded Status Response frame and JSON
  -> Ping(unique i64 nonce)
  <- Pong(exact echoed nonce)
  -> typed result plus monotonic elapsed latency
```

The shared connection retains one `FrameDecoder` across reads. Consequently a split length prefix or body is reconstructed incrementally, and bytes belonging to a later frame remain buffered without repeatedly copying the entire input. Reads use an 8 KiB scratch buffer. The Status latency measurement starts immediately before writing Ping and ends after a valid Pong is decoded, using Tokio's monotonic `Instant`.

## Phase 7 development login

The explicit connection states are `Handshake`, `Login`, `Configuration`, `Play`, and `Closed`; state transitions are validated rather than inferred from arbitrary incoming IDs. The implemented flow is:

```text
Handshake(next state = Login, protocol 775)
  -> Login Start(name, zero UUID for offline assignment)
  <- Login Success
  -> Login Acknowledged
  -> Client Information
  <-> bounded Configuration requests/responses
  <- Finish Configuration
  -> Finish Configuration acknowledgement
  <- initial Play Login packet
  -> report Play reached and close
```

Login Plugin Requests are declined and cookie requests receive an empty cookie. Configuration Keep Alive and Ping are echoed correctly. Known Packs receives an empty list, which tells vanilla to transmit full registry data; Cubic accepts the complete bounded registry frame but does not parse or retain registries because Phase 11 owns that model. Other complete future-state packets listed in `PROTOCOL.md` are bounded and skipped. Encryption Request and Set Compression produce errors that name `online-mode=false` and `network-compression-threshold=-1` respectively. Server resource packs, transfer, and configured code-of-conduct prompts are also explicit unsupported errors.

The built-in `DevLoginProtocolProfile` uses `MinecraftVersionId("26.1.2")` and `ProtocolVersion(775)` from `cubic-version`; it is the sole selection point. The packet IDs remain in `cubic_protocol::bootstrap::v775`. This boundary is temporary until Phase 12 packet generation and does not require an installed Phase 6 dataset.

## Address and protocol behavior

Accepted forms are `hostname`, `hostname:port`, IPv4 with an optional port, and bracketed IPv6 such as `[::1]:25565`. An omitted port means 25565. Whitespace, control characters, port zero, invalid or missing ports, malformed brackets, and ambiguous unbracketed IPv6 are rejected. The logical host, without IPv6 brackets, is sent in the Handshake; the explicit/default port is sent separately. Tokio performs ordinary host resolution while connecting.

Minecraft DNS SRV lookup, proxy forwarding-address extensions, Internationalized Domain Name normalization, version detection, and connection fallback are not implemented. The default handshake protocol is `-1`, a commonly used generic probing value, but server behavior is not universal. `--protocol <i32>` makes the choice explicit without embedding a version table in engine code.

## Bounds and errors

Default policy is:

- connect timeout: 3 seconds;
- each write/read phase timeout: 5 seconds;
- overall query timeout: 10 seconds;
- Status frame body: 128 KiB;
- total decoder buffer: 256 KiB;
- Status JSON: 32,767 Java UTF-16 units and at most 98,301 encoded bytes;
- player sample: 100 entries;
- version/player text field: 4 KiB each;
- favicon text: 32 KiB.

The JSON-wide cap is enforced before JSON parsing. More specific caps are enforced after structural decoding, while still operating within that global bound. Errors distinguish invalid addresses, resolution/connect failures, connect timeout, phase timeout, overall timeout, I/O failure, premature disconnect, malformed framing, oversized Status frames, malformed/invalid JSON or packet data, and Pong mismatch. Tokio exposes name-resolution and connect failures through the same connection operation, so those share one source-bearing error category.

Development-login defaults are a 3-second connect timeout, a 5-second deadline for each read or write, and a 20-second overall Login-plus-Configuration deadline. Uncompressed frames are capped at 2,097,151 bytes and accumulated framed input at 4 MiB. Usernames are 1-16 ASCII letters, digits, or underscores. Login profile properties and Known Packs are capped at 64 entries; disconnect reasons are capped at 32 KiB on input and 512 displayed characters; custom payloads are bounded; Login and Configuration also have 64- and 2,048-packet progress limits. Malformed input, unexpected state packet IDs, EOF, per-operation timeout, overall timeout, server disconnect, unsupported settings/features, profile mismatch, and invalid state transitions have structured errors.

Required JSON fields are `version`, `players`, and `description`. The player sample and favicon are optional. Unknown top-level fields and the original bounded JSON text are preserved. Rich descriptions remain JSON values because chat-component interpretation belongs to a later UI/text phase. Cubic never decodes or renders favicon data in Phase 5.

## Running and testing

The graphical default remains unchanged:

```text
cargo run -p cubic-app
```

A status query is explicit:

```text
cargo run -p cubic-app -- status <host[:port]> [--protocol <number>]
```

Automated coverage uses only an in-process mock TCP server and never reaches a public Minecraft server. Phase 5's separate authorized real-server smoke test is recorded as complete.

A Phase 7 development login is explicit and does not open the graphical window:

```text
cargo run -p cubic-app -- dev-login localhost:25565
cargo run -p cubic-app -- dev-login localhost:25565 --username CubicTest
```

The real acceptance test passed against vanilla Java Edition 26.1.2 at `localhost:25565`, configured with `online-mode=false` and `network-compression-threshold=-1`. Cubic completed Login and Configuration, reported `State: Play`, and the vanilla server logged that `CubicTest` joined and spawned into the world before Cubic deliberately disconnected. The first setting avoided Phase 9 authentication/encryption and the second kept framing uncompressed. Automated tests still never start, download, or contact a real server.

## Phase 8 persistent Chat Mode

`connect_to_play` is the reusable internal seam beneath the unchanged one-shot `dev-login` command. Chat Mode takes ownership of the resulting framed connection on a dedicated current-thread Tokio runtime. A `tokio::select!` loop independently awaits the next frame or a bounded UI command.

The protocol-775 control plane handles Keep Alive, Ping/Pong, teleport confirmation, cookie absence, chunk-batch rate acknowledgement, one Player Loaded marker after the first discarded batch, and Play-to-Configuration re-entry. Initial Play Client Information is sent. Other complete frames—most notably chunks and world/entity traffic—are dropped immediately after the shared decoder has enforced the 2,097,151-byte frame and 4 MiB aggregate-buffer limits.

Incoming Player Chat, Disguised Chat, System Chat, and Disconnect become stable `cubic-core` events. The regular event channel defaults to 128 entries; overflow drops the newest regular event and increments a counter surfaced as a warning. Disconnect/terminal state has a separate one-slot critical path and channel closure is observable. The outgoing command channel defaults to 16 entries. No attacker-controlled unbounded queue exists.

The server settings remain:

```text
online-mode=false
network-compression-threshold=-1
resource-pack=
require-resource-pack=false
enable-code-of-conduct=false
```

Offline mode permits the absent authenticated chat session used by the MVP. Cubic sends ordinary Chat Message packets without a signature; it does not weaken or bypass secure-chat enforcement. Authentication and legitimate signing belong to Phase 9.

## Deliberate exclusions

Phase 8 does not implement DNS SRV, compression, encryption, authentication, signing, automatic reconnect, general Play schemas, registries, world/chunk/entity state, movement, resource packs, or gameplay. Phase 12 will replace the manual packet profile; Phase 18 will add Chat/Play switching.

## Phase 9 online transport

The in-progress authenticated path extends the same `MinecraftConnection`. Encryption and compression are explicit transport state changes: AES-128/CFB8 decrypts before outer framing, and zlib decompression occurs only after a complete bounded frame; writes reverse that order. Set Compression itself is decoded in the previous framing mode. Phase 5 Status and Phase 7/8 offline traffic use the same transport with both transforms disabled.

Authentication providers remain outside this transport. `cubic-network` accepts an authenticated profile/token plus the narrow `MinecraftSessionJoiner` capability, so the existing RSA, session-server, AES, compression, Login, and Configuration pipeline is identical for `CubicEntra` and experimental `XalInterop`. The experimental backend therefore adds no XAL/SISU concepts to packet or transport code.

`online-login` currently proves only the one-shot authenticated Login/Configuration boundary. Persistent authenticated Chat Mode and the protocol-775 player-certificate/session signing path remain incomplete; see `AUTHENTICATION.md`.
