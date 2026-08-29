# Official Version and Resource Bootstrap

Phase 10 implements the bounded official bootstrap chain:

```text
Mojang version manifest v2
  -> exact typed Minecraft version ID
  -> SHA-1-verified per-version metadata
  -> size/SHA-1-verified asset index
  -> immutable logical asset descriptors
  -> on-demand content-addressed asset objects
  -> optional verified client JAR
```

`cubic-version` owns the manifest and selected-version metadata model: release, snapshot, forward-compatible other kinds, exact opaque version IDs, timestamps, official URLs, SHA-1 descriptors, asset-index metadata, client-download metadata, and optional Java/main-class/inheritance fields. Protocol numbers are not used as cache or version identities.

`cubic-resources` owns HTTPS acquisition and the verified cache. It accepts only credential-free HTTPS URLs on reviewed Mojang/Minecraft hosts, disables redirects, applies finite connection/request deadlines, bounds metadata before parsing, checks declared sizes, and verifies every published SHA-1 before promotion. SHA-1 is used only because Mojang publishes it as the immutable artifact identifier. Cubic sends no account credentials to these public endpoints.

The platform-selected cache root contains:

```text
cache/minecraft/
  manifests/version_manifest_v2.json
  versions/<validated-version-id>/metadata.json
  versions/<validated-version-id>/asset-index.json
  versions/<validated-version-id>/client.jar       # only when requested
  objects/<first-two-hash-chars>/<full-sha1>        # on demand
```

Version IDs pass the Phase 6 path-component validation. Logical asset names are lookup keys only and never become local paths. Shared asset objects deduplicate by content hash. Downloads use unique `.part` files, stream large artifacts while hashing, synchronize them, and rename only after verification. A partial or corrupt file is never accepted merely because it exists.

Bootstrap is cache-first. A valid cached manifest, version metadata, and asset index allow offline reuse without network activity. Missing or corrupt entries are fetched again when possible; unavailable network plus missing valid cache returns a structured error. The current simple manifest policy does not refresh a valid cached global manifest automatically; a future stable/snapshot update pipeline owns freshness policy.

Metadata-only bootstrap does not download the asset universe or client JAR. `--client-jar` explicitly requests the official client artifact. Cubic never executes or redistributes that JAR, and downloaded Mojang metadata/assets/JARs remain runtime cache content excluded from source control. Phase 11 owns generated registries/game data, Phase 12 generated packet codecs, and Phase 16 resource/model application.

Phase 16 reuses this exact verified client artifact automatically in World Mode. `cubic-resources` exposes only validated relative archive paths and bounded reads; it does not parse blockstate/model JSON. `cubic-render` independently interprets those factual resources before window creation. No extracted Mojang resource is written into or committed from the repository. See `BLOCK_RESOURCES.md`.

## Phase 10 manual acceptance

The official-Mojang acceptance passed for exact version `26.1.2`. A fresh bootstrap resolved a Release dataset from the network, verified asset index `30`, exposed 4,750 logical asset descriptors, and left the client JAR optional. The published client descriptor was 38,113,927 bytes with SHA-1 `4e618f09a0c649dde3fdf829df443ce0b8831e65`.

A second metadata bootstrap used the verified cache. An invalid version ID returned a structured not-found error and exit code 1 without a panic or cache promotion. Explicit `--client-jar` acquisition streamed, size-checked, hash-checked, and promoted the official JAR without executing it; a repeated run reused that verified artifact. With the computer's internet connection disabled, the cached version metadata, asset index, and client-JAR state still bootstrapped successfully with source `Cache`.
