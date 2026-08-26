# Protocol Primitive Foundation

Phase 3 implements only synchronous Minecraft: Java Edition binary primitives and uncompressed packet framing in `cubic-protocol`. It is a byte-codec foundation for future generated packet schemas, not a network client.

## Layer boundary

The intended data flow is:

```text
future TCP transport -> uncompressed frame decoder -> completed frame body
                     -> raw packet ID/payload split -> future packet schema codec
```

Only the framing and primitive-codec portions exist. The optional raw packet helper separates a VarInt ID from uninterpreted payload bytes but assigns no meaning to that ID. TCP, async I/O, connection states, packet schemas, compression, encryption, NBT, login, and status ping are not implemented.

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

Minecraft-version-specific packet IDs and behavior do not belong in this layer. If a future protocol version changes a primitive representation, it should gain an isolated version-specific codec rather than silently changing unrelated primitives.
