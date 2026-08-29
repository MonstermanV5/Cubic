# Generated Packet Schemas

Phase 12 introduces an exact-version packet registry and a bounded data-driven wire-codec model. Mojang's report is the identity authority; independent structural data is supplemental and cannot override it.

## Official report findings

For Java Edition 26.1.2, Mojang's Data Generator writes `reports/packets.json`. Its actual structure is:

```text
protocol state
  -> direction (serverbound/clientbound)
    -> namespaced packet identity
      -> protocol_id
```

The report covers Handshake, Status, Login, Configuration, and Play. IDs are sparse and are scoped by exact version, state, and direction. It does **not** describe ordered fields, wire types, bounds, optional/conditional relationships, enum discriminants, nested structures, or `StreamCodec` composition. Cubic never invents those missing layouts.

## Supplemental structural source

The selected source is PrismarineJS `minecraft-data`'s ProtoDef `protocol.json`, under its declared MIT license. Cubic pins commit `8a80816cbfb3fe2b609f2cde4e57796c8033af61`, protocol content SHA-256 `2dd1dcde27d5a48e8658ae3333179370a589fdbb69e6c78aadf64f7485e4723f`, and version-index SHA-256 `ce7bd7523c8e3a2b27f7e84cf961ab86519426db1fcfa4c82d82cfa61eb85913`. The pinned index maps exact `26.1.2` and protocol `775` to the `26.1` major schema. The source provides ordered containers, named types, primitive types, arrays, optionals, mappings, switches, and nested definitions. It does not supply Cubic-grade security bounds, Mojang namespaced identities, or a representation Cubic supports for every ProtoDef construct.

Prismarine's repository notes that some historical data originated from other public documentation whose terms may differ; Cubic uses only the narrow factual packet schema under the repository's declared MIT terms and records that provenance. A separately investigated Go redistribution was rejected as the authority because its manifest checksum did not match its bundled/upstream protocol bytes. No third-party implementation code or raw source dataset is committed.

The parser is strict and bounded. It rejects duplicate JSON keys, malformed dispatch tables, duplicate packet names/IDs, recursive named codecs, invalid field names, unsupported source shapes, and oversized inputs. Exact namespaced suffix matches are preferred. A small state/direction/version-scoped alias table covers reviewed differences such as `set_protocol` → `minecraft:intention` and `chat_message` → `minecraft:chat`; no fuzzy matching occurs. A mapped source ID that differs from Mojang's ID fails generation.

The review also considered VoidMC's human-facing packet pages, unmerged Prismarine 26.1.2 pull-request snapshots, and the generated Go package in `go-theft-craft/minecraft-protocol`. The pages were not a pinned comprehensive machine-readable schema; the early pull-request data explicitly carried older layouts and documented mismatches; and the Go release's source manifest checksum was internally inconsistent. Direct pinned Prismarine data was selected only after its current version index, structural coverage, upstream bytes, declared license, and Mojang-ID overlap were checked independently.

## Artifact and runtime model

`cubic-protocol::packet_schema` owns schema format 1. Each deterministic JSON artifact records:

- exact `MinecraftVersionId` and typed `ProtocolVersion`;
- the official `packets.json` SHA-1;
- separately identified supplemental source, revision, source schema, SHA-256, and license;
- state, direction, namespaced identity, non-negative VarInt ID, and layout availability for every packet.

`PacketRegistry` builds immutable indexes by `(state, direction, ID)` and `(state, direction, identity)`. Exact version remains part of the artifact identity; protocol number is metadata and is not assumed unique. Multiple registries can coexist without global state.

The public result model distinguishes a completely unknown ID, a known identity whose layout is unsupported, a malformed known payload, unsupported schema constructs, and trailing bytes. Whether a caller may skip a complete packet remains an explicit network/session policy above this layer.

## Layout and codec model

For translated layouts, ordered `PacketField` definitions compose the existing Phase 3/4 primitives: fixed integers and float bits, Boolean, VarInt/VarLong, bounded strings and identifiers, UUID, Position, bounded byte arrays and fixed bytes, bounded BitSet, bounded VarInt lists, presence-prefixed optionals, validated VarInt enums, nested structures, preceding-Boolean conditionals, bounded NBT, and explicitly bounded remaining bytes. Unsupported source constructs remain identity-only with one of four deterministic categories: no source entry, ambiguous identity, unsupported codec construct, or unsupported conditional construct.

Every dynamic length has a declared cap. Where ProtoDef omits a normative bound, Cubic uses a documented implementation safety limit: 32,767 UTF-16 units / 131,068 UTF-8 bytes for general strings, 65,535 list items, and 2 MiB for opaque byte payloads. These broad safe defaults are not claims about narrower packet-specific protocol limits and must be reviewed before a generated codec replaces a live semantic codec. Schemas reject duplicate fields, invalid names, empty/impossible enums, unbounded or pathological limits, excessive nesting, and invalid Boolean conditionals. Decoding consumes exactly the declared payload and reports structured errors; encoding checks value type, order, enum membership, and bounds.

This is a hybrid architecture: a compact runtime identity/schema artifact provides multi-version selection and inspection, while stable Cubic semantic events/commands remain typed Rust APIs. Runtime schema values are confined to the wire boundary and do not replace long-lived engine types. Future generated Rust adapters may be added for hot or widely consumed packets after real structural sources exist; an opaque binary format is not justified yet.

## Bootstrap coexistence

The proven hand-written `bootstrap::v775` codecs remain active for Status, Login, Configuration, authenticated transport, and Chat Mode. The Phase 12 generator cross-checks 34 overlapping protocol-775 packet identities/IDs against the official report and 14 representative generated layouts against real-tested manual semantics. It does not replace working codecs. Migration can proceed packet-by-packet after bounds and semantic review.

The real 26.1.2 report contains 256 definitions: Handshake 1/0, Status 2/2, Login 5/6, Configuration 10/20, and Play 69/141 (serverbound/clientbound). Its report SHA-1 is `5931622151abf29f41596f4515c2669fcfebc44c`. The merged deterministic artifact is 114,441 bytes with content SHA-1 `c43e6035f08d250cf3f0e91a558fe105e3b6d040`: 96 layouts are generated and 160 remain identity-only (150 ambiguous source identities, 7 unsupported codec constructs, and 3 unsupported conditional constructs).

This exact-version generation and inspection flow passed Phase 12 real manual acceptance. Acceptance confirms the deterministic registry architecture and its explicit unsupported classifications; it does not imply that identity-only definitions have executable codecs or that live networking has migrated away from the reviewed bootstrap codecs.

## Offline commands

Acquire the reviewed source deliberately at the pinned commit; never substitute `latest`:

```powershell
$source = 'C:\path\to\minecraft-data-at-8a80816'
git clone --filter=blob:none --no-checkout https://github.com/PrismarineJS/minecraft-data.git $source
git -C $source fetch --depth 1 origin 8a80816cbfb3fe2b609f2cde4e57796c8033af61
git -C $source checkout --detach FETCH_HEAD
if ((Get-FileHash (Join-Path $source 'data\pc\26.1\protocol.json') -Algorithm SHA256).Hash -ne '2DD1DCDE27D5A48E8658AE3333179370A589FDBB69E6C78AADF64F7485E4723F') { throw 'protocol source hash mismatch' }
if ((Get-FileHash (Join-Path $source 'data\pc\common\protocolVersions.json') -Algorithm SHA256).Hash -ne 'CE7BD7523C8E3A2B27F7E84CF961AB86519426DB1FCFA4C82D82CFA61EB85913') { throw 'version index hash mismatch' }
```

Generation verifies the exact cached Phase 10 client descriptor before accepting the report and performs no network access:

```powershell
$cache = Join-Path $env:LOCALAPPDATA 'Cubic\cache\minecraft'
$reports = 'C:\path\to\26.1.2-datagen-output\reports'
$source = 'C:\path\to\minecraft-data-at-8a80816cbfb3fe2b609f2cde4e57796c8033af61'
$generated = Join-Path $env:LOCALAPPDATA 'Cubic\generated\packet-schema'
cargo run -p version-generator -- packet-schema $cache 26.1.2 775 $reports $source $generated
```

Repeat the same command and compare the content hash:

```powershell
cargo run -p version-generator -- packet-schema $cache 26.1.2 775 $reports $source $generated
$schema = Join-Path $generated '26.1.2\packet-schema.json'
$before = (Get-FileHash $schema -Algorithm SHA1).Hash
cargo run -p version-generator -- packet-schema $cache 26.1.2 775 $reports $source $generated
$after = (Get-FileHash $schema -Algorithm SHA1).Hash
if ($before -ne $after) { throw "packet schema is not deterministic: $before != $after" }
cargo run -p version-generator -- validate-packet-schema $schema
```

Representative official identity checks:

```powershell
cargo run -p version-generator -- inspect-packet-schema $schema handshake serverbound minecraft:intention
cargo run -p version-generator -- inspect-packet-schema $schema status serverbound minecraft:status_request
cargo run -p version-generator -- inspect-packet-schema $schema login serverbound minecraft:hello
cargo run -p version-generator -- inspect-packet-schema $schema configuration clientbound minecraft:finish_configuration
cargo run -p version-generator -- inspect-packet-schema $schema play clientbound minecraft:custom_payload
cargo run -p version-generator -- inspect-packet-schema $schema play serverbound minecraft:chat
cargo run -p version-generator -- inspect-packet-schema $schema play serverbound minecraft:chat_session_update
cargo run -p version-generator -- inspect-packet-schema $schema play clientbound minecraft:system_chat
cargo run -p version-generator -- inspect-packet-schema $schema play clientbound minecraft:keep_alive
cargo run -p version-generator -- inspect-packet-schema $schema login serverbound minecraft:key
```

The final command deliberately demonstrates an identity-only packet and its bounded reason. Inspection prints ordered fields, codec types, optionals/conditions, and bounds for generated layouts.

No localhost regression is required for the current Phase 12 slice because no live network path was migrated. Phase 13 world state, Phase 14 chunk decoding, and Phase 31–33 automated version pipelines remain separate work.
