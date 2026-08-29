# Versioning Model

The intended future composition is:

```text
Cubic Engine
    +
Generated Minecraft Version Data
    +
Small, isolated compatibility adapters where behavior genuinely differs
```

The engine should operate on stable internal concepts instead of embedding Minecraft numeric IDs or widespread version checks. Ordinary registry, packet-shape, or metadata changes should ideally be handled by generated data without engine changes. Compatibility adapters are reserved for differences that cannot be represented cleanly as data.

Stable releases and snapshots should eventually coexist as separately identified version-data sets. Selecting one version must not overwrite or invalidate another installed version.

Phase 6 implements the runtime foundation: opaque validated `MinecraftVersionId` values, typed non-unique `ProtocolVersion` values, explicit release/snapshot classification, schema-versioned deterministic JSON, a caller-rooted filesystem store, a cross-validated deterministic catalog, and compatibility-profile identifiers. The offline `version-generator` validates local datasets and rebuilds catalogs; it performs no downloads. Real registries, packet mappings, resources, and automatic stable/snapshot pipelines remain deferred to Phases 10-12 and 31-32.

Phase 7 does not bypass this design. Its sole real-server target is represented by a small `DevLoginProtocolProfile` holding typed version `26.1.2` and protocol `775`. All temporary packet IDs and shapes are isolated in `cubic_protocol::bootstrap::v775`; no runtime version string checks are spread through networking or engine code. Phase 12 will replace or absorb this manual bootstrap with generated packet data. The development login does not require an installed dataset and does not imply that protocol 775 uniquely identifies version 26.1.2.

Phase 10 adds a separate official-discovery input. Mojang's manifest selects exact textual version IDs and supplies immutable metadata descriptors; it does not replace Cubic's schema-versioned generated datasets or infer identity from protocol numbers. Releases, snapshots, and forward-compatible other official kinds can coexist in the verified cache. Later generation phases consume these official artifacts through typed APIs rather than embedding 26.1.2 globally.

Phase 11 game-data artifacts have their own schema version and exact Minecraft version identity. Their provenance records the published client-JAR SHA-1 and the hashes of both Mojang Data Generator reports. Two versions may assign different raw IDs and state layouts to the same identifier without collision. Release and snapshot automation in Phases 31–32 can invoke the same deterministic generator; protocol number is never the artifact key.

Phase 12 packet-schema artifacts are likewise keyed by exact `MinecraftVersionId`, not protocol number. Packet identity additionally includes state and direction, so sparse/reused IDs are safe. The artifact records the official packet-report hash separately from the supplemental source name, pinned revision, source schema, SHA-256, and license. The pinned source index explicitly maps exact `26.1.2` / protocol `775` to its `26.1` major schema. Synthetic tests prove two exact versions can change IDs and layouts without shared global state; a future version receives its own independently pinned inputs and aliases.
