# Protocol Primitive Foundation

Phase 12 adds the exact-version generated packet-registry boundary while retaining the isolated Java 26.1.2 / protocol 775 bootstrap profile for verified live codecs. TCP and session policy remain in `cubic-network`; packet schema details remain in `cubic-protocol`.

## Layer boundary

The intended data flow is:

```text
future TCP transport -> uncompressed frame decoder -> completed frame body
                     -> raw packet ID/payload split -> future packet schema codec
```

The framing, primitive-codec, raw NBT, Status, temporary protocol-775 bootstrap, and generated packet registry exist. The raw packet helper separates a VarInt ID from payload bytes. `PacketRegistry` resolves exact-version state/direction identities and interprets validated bounded layouts merged from the official registry and pinned structural data. Unsupported layouts retain a categorized reason. Compression, encryption, authentication, and Play semantics remain outside the registry.

## Phase 7 protocol-775 bootstrap profile

`cubic_protocol::bootstrap::v775` is the only production module containing the manually authored packet IDs and layouts needed by Phase 7. It implements Login Start with a bounded 16-character ASCII development username and a 128-bit UUID, Login Disconnect, Encryption Request classification, Login Success, Set Compression classification, Login Plugin Request, Login Cookie Request, and their minimal responses. The client supplies an all-zero UUID in Login Start so the offline-mode vanilla server performs its normal offline profile assignment; Login Success must return the requested name and a non-zero UUID.

Configuration semantically handles Client Information, cookie requests, bounded custom payloads, Disconnect NBT, Finish Configuration, Keep Alive, Ping/Pong, and Known Packs. Cubic answers Known Packs with an empty list because it possesses no generated packs yet. Complete bounded Reset Chat, Registry Data, Resource Pack Pop, Store Cookie, Enabled Features, Update Tags, Report Details, Server Links, Clear Dialog, and Show Dialog frames are skipped without constructing future registry/UI models. Resource Pack Push, Transfer, and Code of Conduct are explicit Phase 7 unsupported errors rather than being silently accepted. After acknowledging Finish Configuration, the bootstrap identifies protocol 775's initial clientbound Play Login packet (`0x31`) through a bounded network-layer handoff loop. Legal preceding Play packets are decoded as supported control traffic or bounded irrelevant traffic rather than being mistaken for an invalid state transition.

The packet mapping was cross-checked against the [VoidMC 26.1.2 Handshake](https://voidminecraft.github.io/VoidMC/reference/protocol-specs/v26.1.2/handshake.html), [Login](https://voidminecraft.github.io/VoidMC/reference/protocol-specs/v26.1.2/login.html), [Configuration](https://voidminecraft.github.io/VoidMC/reference/protocol-specs/v26.1.2/configuration.html), and [Play clientbound](https://voidminecraft.github.io/VoidMC/reference/protocol-specs/v26.1.2/play-clientbound.html) tables, plus the independently generated [go-theft-craft v26_1 package](https://pkg.go.dev/github.com/go-theft-craft/minecraft-protocol/generated/java/v26_1). The [PrismarineJS protocol-775 tracking issue](https://github.com/PrismarineJS/mineflayer/issues/3888) was used as a caution that carried-forward packet mappings can be wrong, not as the sole packet source. Mojang source code is not copied.

## Phase 5 Status packets

The implemented uncompressed packet bodies are deliberately limited to:

- Handshake (`0x00`): protocol-version VarInt, bounded logical server host string, big-endian `u16` port, and next-state VarInt `1` for Status.
- Status Request (`0x00`): no payload.
- Status Response (`0x00`): one bounded Minecraft UTF-8 string containing JSON.
- Ping (`0x01`) and Pong (`0x01`): one big-endian signed 64-bit nonce. A Pong must echo the exact nonce and contain no trailing bytes.

The handshake default is protocol `-1`, a conventional generic status probe rather than a claim that it represents every server. Callers can explicitly select another signed 32-bit protocol number. Cubic does not yet have generated version data or protocol negotiation.

Status JSON requires `version`, `players`, and `description`. Version name/protocol and non-negative online/maximum counts become typed values. The optional player sample and favicon are retained with explicit bounds. `description` remains a `serde_json::Value` so rich chat-component structures are not flattened; unknown top-level fields and the original JSON string are also retained. Favicon text is inert data and is neither decoded nor rendered.

## Wire semantics

- Fixed-width multi-byte integers and IEEE-754 floating-point bit patterns use network (big-endian) byte order.
- Boolean writers emit canonical `0` or `1`. Readers treat zero as false and every non-zero byte as true, matching the underlying Java byte-buffer boolean convention while remaining permissive toward non-canonical peers.
- VarInt and VarLong are signed `i32` and `i64` values encoded from their two's-complement bit patterns, not ZigZag values. Encoders are canonical. Decoders accept encodings that terminate within five or ten bytes respectively and reject a continuation past that limit.
- Strings contain a VarInt byte length followed by UTF-8 bytes. `StringLimits` separately bounds logical length and encoded bytes. Logical length is counted in Java UTF-16 code units, because Minecraft's documented string limits use Java `String.length()` semantics: a supplementary Unicode scalar counts as two units, while a combining scalar is counted independently. The effective encoded-byte limit is the smaller of the caller's byte cap and three times its UTF-16-unit cap. Strings are borrowed from the input and are never truncated.
- `ProtocolUuid` is a strongly typed 16-byte value. Its wire representation is the unsigned 128-bit value in big-endian order, equivalent to Java's most-significant 64 bits followed by least-significant 64 bits. Text parsing is outside the wire boundary.
- A modern `BlockPosition` packs signed X into bits 63-38, signed Z into bits 37-12, and signed Y into bits 11-0. X and Z are 26-bit signed values; Y is a 12-bit signed value. Construction rejects out-of-range coordinates, and decoding explicitly sign-extends each field.
- A variable `BitSet` starts with a non-negative VarInt word count followed by big-endian 64-bit words. Word zero contains bits 0-63, with bit zero in that word's least-significant bit. Trailing zero words are removed for deterministic encoding. `BitSetLimits` independently bounds encoded words and usable bits. Fixed-size bitsets required by future schemas are a separate type of field.
- A length-prefixed byte array is distinct from a field that consumes the remaining packet bytes. Its VarInt length is validated against the caller's bound before returning a borrowed slice.

## Framing and limits

An uncompressed frame is a VarInt packet-body length followed by exactly that many bytes. `FrameDecoder::push` accepts arbitrary fragments, and `next_frame` returns one owned completed body or `None` when more bytes are required. It retains incomplete bytes, emits concatenated frames in order, and uses a read offset plus occasional in-place compaction rather than removing bytes from the front after every frame.

Minecraft's current uncompressed framing restricts the length prefix to three bytes, making 2,097,151 bytes the largest representable frame body accepted here. `FrameLimits` can select a stricter per-frame maximum and also requires a separate aggregate buffer maximum. Defaults are 2,097,151 bytes per body and 8 MiB of accumulated input. A negative length, a four-or-more-byte positive length prefix, an oversized body, or an aggregate-buffer violation is rejected before body allocation. Completed bodies use fallible allocation.

The decoder does not provide transport backpressure or connection recovery policy. A codec error should be treated by a future connection layer as terminal for the malformed stream unless that layer deliberately resets the decoder.

## Public API shape

- `CodecReader` and `CodecWriter`: fixed-width and bounded primitive operations.
- `CodecError` and `LengthKind`: structured failures and length context.
- `StringLimits`, `BitSetLimits`, and `FrameLimits`: explicit safety policy.
- `ProtocolUuid`, `BlockPosition`, and `BitSet`: strongly typed wire values.
- `FrameDecoder` and `encode_frame`: incremental uncompressed framing.
- `RawPacket` and `split_raw_packet`: schema-neutral packet ID/payload separation.
- `nbt`: raw Java Edition NBT values, Modified UTF-8, explicit named/unnamed compound roots, and bounded encoding/decoding. See `NBT.md`.
- `packet_schema`: schema-versioned exact-version artifacts, immutable state/direction indexes, explicit unknown-versus-unsupported results, and bounded compositional packet encoding/decoding. See `PACKETS.md`.

Minecraft-version-specific packet IDs and behavior must not be scattered through this layer. The single `bootstrap::v775` exception is intentionally isolated and temporary. Phase 8 adds clientbound Login, Keep Alive, Ping, Player Position, Chunk Batch Finished, Cookie Request, Player/Disguised/System Chat, Disconnect, Set Health, Start Configuration, Resource Pack Push, and Transfer classifications, plus required responses and unsigned Chat Message encoding. Unknown complete Play frames remain bounded and are identified for immediate discard. Official 26.1.2 registration and the independent current packet table identify clientbound Play `0x18` as Custom Payload; it is ordinary bounded Play traffic, not the initial Login packet.

Text Components in protocol 775 use an unnamed generic NBT root, so the raw NBT layer now exposes a bounded any-tag network-root decoder in addition to its compound-root API. The MVP projects strings, `text`, nested `extra`, and a conservative translation fallback to plain text while retaining a bounded protocol-independent tree. Unknown style/click/hover data is inert and never executed.

Outgoing messages are limited to 256 Java UTF-16 units and reject empty/control-bearing input. Protocol 775 encodes the message, epoch-millisecond timestamp, salt, absent-signature discriminator, VarInt last-seen offset, fixed 20-bit acknowledgement window as three raw little-endian bit bytes, and a trailing checksum byte. Outgoing unsigned chat carries an empty update (`offset = 0`, three zero bytes) and checksum `0`, which disables checksum verification. Only a Player Chat packet carrying a message signature enters the server's tracked last-seen window and requires the dedicated acknowledgement; unsigned Player Chat, including Cubic's own vanilla-server echo, must not advance that window. System and Disguised Chat likewise do not enter it. No fake signature or chat session is created. Commands are rejected because Cubic does not yet retain the command graph needed to know whether arguments require signing.

Phase 9 adds the narrowly scoped protocol-775 Encryption Request/Response, serverbound Player Chat (`0x09`), and Player Chat Session Update (`0x0a`) fields to the same bootstrap profile. The session update is UUID, certificate expiry epoch milliseconds, bounded X.509/SPKI RSA public-key DER, and bounded Mojang public-key signature. A signed Chat Message retains Phase 8's exact message/timestamp/salt/nullable-signature/update order; the nullable signature is exactly 256 bytes for the reference profile.

The 26.1.2 signing input was verified against the installed deobfuscated client classes and cross-checked with the independent [go-theft-craft v26_1 package](https://pkg.go.dev/github.com/go-theft-craft/minecraft-protocol/generated/java/v26_1) and [VoidMC chat documentation](https://voidminecraft.github.io/VoidMC/reference/protocol-specs/v26.1.2/chat.html). It is big-endian chain version `1`, sender UUID, session UUID, per-session message index, salt, timestamp in epoch seconds, UTF-8 byte length and bytes, then the ordered acknowledged-signature count and 256-byte signatures. RSA-2048 PKCS#1 v1.5 with SHA-256 produces the wire signature. Incoming Player Chat carries a global index, sender UUID, sender-local index, nullable signature, signed body, optional unsigned component, filter, and bound chat type.

The last-seen tracker is bounded to 20 entries for this profile. It advances only for a new signed Player Chat, encodes the fixed 20 bits in three little-endian Java-BitSet bytes, computes the trailing Java-compatible checksum over displayed signatures, and emits a standalone acknowledgement after more than 64 pending signed messages. Unsigned/System/Disguised messages do not advance this signed window. Global Player Chat indices are strict and reset with a new/reconfigured session. These values live in the versioned bootstrap/profile boundary; they are not asserted as rules for other Minecraft versions.

The network transport—not packet codecs—owns AES-128/CFB8 and compressed framing. Compression uses a VarInt Data Length, zlib rather than gzip, zero for an uncompressed body, and strict compressed/decompressed bounds. Phase 12 does not migrate live traffic: the proven bootstrap remains the live reference while generated layouts are adopted deliberately after semantic and bounds review.
