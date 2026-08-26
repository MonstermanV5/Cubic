# Java Edition NBT

Phase 4 implements the raw, big-endian Java Edition Named Binary Tag format in `cubic-protocol::nbt`. It is synchronous, transport-independent, and designed to decode directly from an existing `CodecReader` inside a future packet codec.

## Supported tags

`NbtTag` represents every payload-bearing Java Edition type:

| ID | Wire type | Rust representation |
| ---: | --- | --- |
| 0 | TAG_End | Structural terminator only; not an `NbtTag` value |
| 1 | TAG_Byte | `i8` |
| 2 | TAG_Short | `i16` |
| 3 | TAG_Int | `i32` |
| 4 | TAG_Long | `i64` |
| 5 | TAG_Float | `f32`, equality by raw bits |
| 6 | TAG_Double | `f64`, equality by raw bits |
| 7 | TAG_Byte_Array | `Vec<u8>` preserving the signed-byte wire bits exactly |
| 8 | TAG_String | `NbtString` |
| 9 | TAG_List | `NbtList` |
| 10 | TAG_Compound | `NbtCompound` |
| 11 | TAG_Int_Array | `Vec<i32>` |
| 12 | TAG_Long_Array | `Vec<i64>` |

All numeric payloads and signed 32-bit collection counts use big-endian byte order. NBT has no Boolean tag.

## Modified UTF-8 and `NbtString`

Tag names and TAG_String payloads start with a big-endian unsigned 16-bit encoded-byte length. Their bytes use Java Modified UTF-8 rather than Phase 3's ordinary UTF-8 string format.

`NbtString` stores `Vec<u16>`, preserving arbitrary Java UTF-16 code units. This is necessary because a Java string and Modified UTF-8 can preserve unpaired high or low surrogates, while a Rust `String` cannot. `NbtString::from(&str)` converts normal Rust text to UTF-16; `as_utf16_units` is lossless; `to_rust_string` succeeds only for valid paired UTF-16; and `to_string_lossy` explicitly requests replacement behavior.

Canonical encoding follows Java `DataOutput.writeUTF` rules per UTF-16 unit:

- U+0001 through U+007F use one byte;
- U+0000 and U+0080 through U+07FF use two bytes, so NUL is `C0 80`;
- all remaining UTF-16 units, including individual surrogate units, use three bytes;
- supplementary Rust scalar values first become a surrogate pair and therefore occupy six Modified UTF-8 bytes.

Decoding follows Java `DataInput.readUTF` compatibility: one-, two-, and three-byte groups are accepted when continuation bytes are structurally valid; four-byte standard UTF-8 groups and stray continuations are rejected. Java's described reader accepts a literal zero byte and non-shortest two/three-byte forms even though Cubic's encoder never produces them. Decoding preserves the resulting UTF-16 unit and canonical re-encoding may therefore use different bytes with the same Java string value. Encoded length is capped by both `NbtLimits` and the format's 65,535-byte prefix.

## Roots and packet integration

The API makes root representation explicit:

- `decode_named_root` / `encode_named_root` use `TAG_Compound + Modified-UTF-8 name + compound payload`.
- `decode_unnamed_network_root` / `encode_unnamed_network_root` use `TAG_Compound + compound payload`.

Both forms require a compound root. The decoder variants accepting `&mut CodecReader` consume exactly one document and leave subsequent bytes available for later packet fields. Separate `*_complete` slice helpers reject trailing bytes for standalone raw documents.

No Minecraft version number is embedded here. Future version/packet code must choose the appropriate root API. Older network formats can use a named root with an empty name; modern network formats use the unnamed form.

## Lists

A positive TAG_List count requires a recognized non-End element type, and every element payload has that exact type with no individual ID or name. `NbtList::new` rejects heterogeneous construction and non-empty TAG_End lists; encoding validates the invariant again.

For a zero or negative wire count, decoding returns an empty list and accepts/preserves any raw element-type byte, including unknown IDs. A negative count is normalized to zero if re-encoded. `NbtList::empty()` chooses TAG_End as the canonical type when no type was supplied. This follows documented Java Edition compatibility behavior, where different implementations historically wrote different empty-list element IDs.

## Compounds and duplicate names

`NbtCompound` uses a standard-library `BTreeMap<NbtString, NbtTag>`. Names are compared losslessly as UTF-16 sequences. This provides lookup, order-independent equality, and deterministic encoding sorted by name without an additional dependency. `get_str`, `get_int`, and `get_string` provide small conveniences for ordinary valid Rust names.

Although the historical format describes compound names as unique, Mojang's compound representation is map-like and `put` replaces an existing value. Cubic therefore accepts duplicate wire names with the last value winning. All encountered entries still count against compound, tag, and cumulative resource limits, so duplicates cannot evade budgets.

## Default safety limits

`NbtLimits::default()` applies:

| Limit | Default |
| --- | ---: |
| Maximum nesting depth | 64 |
| Maximum total payload tags, including root | 65,536 |
| Maximum wire entries in one compound | 4,096 |
| Maximum elements in one positive list | 65,536 |
| Maximum elements in one byte/int/long array | 1,048,576 |
| Maximum encoded bytes in one name/string | 65,535 |
| Maximum cumulative allocation/resource budget | 16 MiB |

Callers may choose stricter limits with `NbtLimits::with_*`. Collection lengths must additionally fit signed `i32`; string lengths must fit unsigned `u16`.

The cumulative budget is shared through the entire recursive decode. It charges owned string UTF-16 capacity, array storage, list element storage, and a conservative structural allowance for every encountered compound entry. Total-tag accounting separately counts the root, compound children, and list elements. Checked arithmetic is used before reserve or read, and arrays verify that all declared payload bytes exist before reserving output storage. The budget is a deterministic safety accounting model rather than a claim to reproduce allocator-specific overhead exactly.

Encoding performs a complete bounded structural validation pass before constructing its private output buffer. It applies the same depth, tag, collection, string, and cumulative resource policies and emits compounds in deterministic key order.

## Explicit exclusions

Phase 4 does not implement SNBT, Bedrock NBT, gzip/zlib wrappers, Minecraft packet compression, packet schemas, version checks, networking, status ping, login, authentication, encryption, world/chunk interpretation, or Mojang data files. Those concerns must remain separate layers or later phases.
