# Security Principles

Cubic will process hostile or malformed data. All network input, server data, resource packs, downloaded metadata, and archives must be treated as untrusted.

- Validate lengths and enforce bounded allocations before reserving or reading externally specified sizes.
- Parsers must return structured errors for malformed input rather than panic.
- Prevent archive and path traversal by validating paths and extraction destinations.
- Apply compressed-size, expanded-size, nesting, and resource limits to defend against decompression bombs.
- Never log credentials, passwords, access tokens, refresh tokens, private keys, or signing material.
- Use platform-appropriate secure credential storage when authentication is implemented later.
- Verify Mojang-provided resource hashes before accepting resources when resource downloading is implemented later.
- Never download or execute arbitrary executable code.
- Treat resource packs as untrusted data, not trusted code.

No networking, downloading, archive processing, authentication, or credential storage is implemented. Phase 2 adds only native windowing, GPU initialization, and clear-frame presentation; rendering performs no filesystem or network operations.
