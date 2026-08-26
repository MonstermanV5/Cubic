# Phase 5 Server Status Networking

Phase 5 implements only the Java Edition server-list Status exchange. It proves that Cubic can connect over TCP, use its existing incremental framing safely, decode a bounded server response, and validate a latency Pong. It is not a general Minecraft connection layer.

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

The same `FrameDecoder` is retained across both reads. Consequently a split length prefix or body is reconstructed incrementally, and bytes belonging to a later frame remain buffered without repeatedly copying the entire input. Reads use an 8 KiB scratch buffer. The latency measurement starts immediately before writing Ping and ends after a valid Pong is decoded, using Tokio's monotonic `Instant`.

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

Automated coverage uses only an in-process mock TCP server and never reaches a public Minecraft server. A tester must run the command against an authorized real Java Edition server and compare the output with expected server-list data before Phase 5 can move from partial to complete.

## Deliberate exclusions

Phase 5 does not implement DNS SRV records, reusable/persistent connections, compression, encryption, authentication, login/play configuration states, packet generation, NBT-over-network use, proxy protocols, server icons, rich text rendering, automatic version selection, or any gameplay. It does not modify the render thread or graphical lifecycle.
