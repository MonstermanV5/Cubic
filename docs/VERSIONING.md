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

The generator and runtime data format are not implemented in Phase 1. Their eventual design must be deterministic, versioned, validated, and tested before generated data is accepted by the engine.

