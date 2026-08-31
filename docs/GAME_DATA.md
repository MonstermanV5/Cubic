# Generated Game Data

Phase 11 transforms version-scoped factual registry reports into a compact, validated Cubic artifact. It does not generate packet codecs, implement world state, decode chunks, or apply Minecraft resources.

## Source and provenance

The selected source is Mojang Data Generator report output from the exact official version artifact already verified by Phase 10:

```text
verified official client descriptor and client.jar
  + reports/registries.json
  + reports/blocks.json
  -> version-generator
  -> game-data.json
```

Read-only inspection confirmed the official 26.1.2 client contains `net.minecraft.data.Main` and its `--reports` and `--output` options, but the client JAR does not ship ready-made registry reports. Cubic deliberately does not resolve or execute Mojang's Java classpath automatically in normal runtime or tests. A developer supplies report output produced with the official version's resolved classpath. The Cubic generator then verifies the cached client's published size/SHA-1, exact metadata version, and both report hashes before generation.

The generated artifact records schema version 1, exact Minecraft version ID, source kind, published client SHA-1, report SHA-1s, registries, and block-state data. It contains factual names and numeric relationships only. Neither source reports, Minecraft JARs, assets, nor decompiled source belong in Git.

## Model and runtime API

Each generic registry has a namespaced identifier and deterministically ordered entries containing a namespaced entry identifier and sparse version-scoped raw ID. Unknown registry names and non-`minecraft` namespaces remain representable. Items and entity types use the generic registry model.

Blocks additionally contain their block raw ID, exactly one default global state ID, ordered property domains, and ordered concrete states. Each state records its global version-specific state ID and complete property assignment. `GameData` exposes exact registry, block, raw-ID, state-ID, property-set, item, and entity-type lookups without exposing report parsing to callers.

The official block report does **not** contain physical collision voxel shapes or the executable block-class behavior that produces them. Phase 17B therefore does not extend schema 1 with invented or incomplete shape fields. `cubic-world` consumes generated identifiers/properties through a separately selected exact-version collision-rule adapter. This keeps factual version data immutable, keeps behavior code reviewable and replaceable, and avoids conflating physical collision with visual, outline, targeting, or occlusion shapes. A future authoritative shape generator may evolve the schema, but must replace rather than compete with this boundary.

Artifacts live beneath an explicit caller-selected root:

```text
<generated-root>/
  <exact-version-id>/
    game-data.json
```

`GameData::load` requires the root and exact typed version ID, checks size and symlink boundaries, validates schema/invariants, and rejects a declared-version mismatch. It performs no network access and has no working-directory default.

## Baseline versus server authority

The artifact is an immutable generated vanilla baseline. Modern Configuration can provide server-authoritative dynamic registry content; future runtime registry state must remain a separate overlay and must not mutate or blindly defer to baseline raw IDs where the server controls the mapping. Phase 11 establishes this boundary but does not implement Configuration registry synchronization.

## Determinism and validation

Input maps are canonicalized by identifier, properties and values are ordered, block states are ordered by state ID, serialization is pretty JSON with one trailing LF, and no timestamps, usernames, absolute paths, or random values enter the artifact. Identical inputs produce byte-identical output.

Generation rejects duplicate JSON keys, registries, entries, raw IDs, state IDs, missing or ambiguous default states, unknown block references, invalid identifiers, impossible property values, unsupported schema versions, malformed provenance hashes, oversized inputs, symlinks, and a cached client that fails its published size/hash.

## Manual 26.1.2 acceptance

First produce reports using Mojang's official 26.1.2 Data Generator with its official launcher-resolved Java classpath:

```text
java -cp <official-26.1.2-classpath> net.minecraft.data.Main --reports --output <report-output>
```

The Cubic command itself is exact and does not rely on the current directory. In PowerShell:

```powershell
$cache = Join-Path $env:LOCALAPPDATA 'Cubic\cache\minecraft'
$reports = 'C:\path\to\26.1.2-datagen-output\reports'
$generated = Join-Path $env:LOCALAPPDATA 'Cubic\generated\game-data'
cargo run -p version-generator -- game-data $cache 26.1.2 $reports $generated
```

Run the same command a second time, then verify the stable content hash:

```powershell
cargo run -p version-generator -- game-data $cache 26.1.2 $reports $generated
Get-FileHash (Join-Path $generated '26.1.2\game-data.json') -Algorithm SHA1
```

The second run should report the same content SHA-1 and leave identical bytes. The summary reports real registry, block, state, item, and entity counts plus artifact and approximate loaded sizes.

Narrow spot checks:

```powershell
$data = Join-Path $generated '26.1.2\game-data.json'
cargo run -p version-generator -- validate-game-data $data
cargo run -p version-generator -- inspect-game-data $data minecraft:air
cargo run -p version-generator -- inspect-game-data $data minecraft:stone
cargo run -p version-generator -- inspect-game-data $data minecraft:oak_log
cargo run -p version-generator -- inspect-game-data $data minecraft:diamond
cargo run -p version-generator -- inspect-game-data $data minecraft:pig
```

These checks must use the actual generated data; Cubic does not predeclare 26.1.2 counts or raw IDs. Future release/snapshot pipelines can invoke the same generator once their verified report production is automated.

## Accepted 26.1.2 result

Real manual acceptance used the official installed 26.1.2 client and launcher metadata with its 76-entry launcher-resolved classpath. Mojang's `net.minecraft.data.Main --reports` completed successfully, producing a 516,149-byte `registries.json` and a 6,239,720-byte `blocks.json`.

The Cubic generator produced schema 1 data from those reports and the Phase 10-verified client SHA-1 `4e618f09a0c649dde3fdf829df443ce0b8831e65`:

- 95 registries;
- 1,168 blocks;
- 29,873 block states;
- 1,506 items;
- 157 entity types;
- 7,763,125 artifact bytes;
- approximately 12,633,830 loaded bytes; and
- content SHA-1 `936dcc94a71fc8006807819a88f45ec6bfd23f2c`.

An identical second generation produced the same byte count, loaded-size estimate, and SHA-1; an independent filesystem hash comparison passed. Independent artifact validation reported the same registry/block/state counts. Spot checks confirmed `minecraft:air` raw/default state 0, `minecraft:stone` raw/default state 1, `minecraft:oak_log` raw ID 49 with default state 137 and three states, `minecraft:diamond` item raw ID 899, and `minecraft:pig` entity-type raw ID 100.

The measured JSON artifact size is acceptable for the current architecture. A more compact representation requires profiling evidence and is not part of Phase 11. The official reports, launcher classpath, client JAR, and generated real artifact remain runtime/development data and are not committed.
