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

Phase 3's protocol reader uses checked slice access and borrowed results where ownership is unnecessary. Length-prefixed strings, byte arrays, BitSets, frame bodies, and aggregate fragmented input are bounded before allocation or copying. Fallible reservation reports a structured error. Incremental framing never trusts a declared frame size and does not allocate the frame body until all bounded bytes are present. Malformed or truncated data returns `CodecError`; production decode paths contain no unsafe code or intentional panic path.

Phase 4 NBT decoding carries one resource context through the entire nested document. It enforces depth, total-tag, per-compound, per-list, per-array, per-string, and cumulative allocation/resource limits. Array byte sizes and structural budget additions use checked arithmetic. Declared array bytes must be present before output storage is reserved. Modified UTF-8 is decoded into bounded UTF-16 storage without silently repairing unpaired surrogates. Production NBT parsing uses no unsafe code, unchecked external indexing, or panic-based error handling.

Phase 5 treats DNS results, TCP bytes, frames, JSON, MOTD components, player samples, favicons, and unknown server fields as untrusted. The query has separate connect and per-operation I/O timeouts plus an overall deadline. Status frames are limited to 128 KiB and the incremental receive buffer to 256 KiB. The JSON string, player sample, individual text fields, and favicon text have additional bounds. Lengths are validated by `cubic-protocol` before body ownership or JSON parsing. Pong packet ID, shape, and nonce are validated. Server-supplied text is displayed as text; favicon content is retained but not decoded, interpreted, fetched, or rendered. Cubic does not follow URLs or execute content supplied by a Status response.

Limits are explicit inputs to codecs so future packet schemas can apply field-appropriate bounds. The default frame body limit is the current protocol maximum of 2,097,151 bytes, while aggregate buffered input has a separate 8 MiB default. Callers may configure stricter bounds. These protections do not replace future per-connection budgets, timeouts, decompression limits, or transport backpressure.

Only the outbound server-list Status query is implemented. Login/play networking, downloading, archive processing, file or packet compression, encryption, authentication, and credential storage remain unimplemented. Phase 2 rendering still performs no filesystem or network operations. Resource and archive protections remain future requirements.
