# Runtime Logging

Cubic uses one `tracing` subscriber for terminal and persistent diagnostics. The platform layer selects Cubic's persistent data directory; the application writes `logs/latest.log` beneath it. On Windows this is under the user's local application-data directory, while a future native iOS host uses its application-support container. No code depends on the repository or current working directory.

At launch, an existing `latest.log` moves to `logs/previous/previous-1.log`. Older files shift upward and Cubic retains five previous launches. Logs are plain UTF-8 and are not compressed. Rotation occurs before a new file is created; initialization failure falls back to terminal diagnostics and normally does not prevent startup.

The default level is INFO. Set `CUBIC_LOG_LEVEL=debug` for bounded packet classifications, component structures, cache decisions, and safe sizes. TRACE and broad raw-packet logging are intentionally unavailable. Records include a UTC time-of-day, thread name, level, target/component, message, and structured fields.

Minecraft chat is intentionally logged before UI projection. INFO records the category, sender identity where known, signed/plain content, trust classification, and safe indices. DEBUG additionally records bounded decoded text-component structures and optional decorated content. Outgoing plaintext is logged before signing. This preserves the evidence needed to distinguish protocol extraction from presentation loss without dumping arbitrary decrypted packet bytes or player-chat signatures.

Logs can therefore contain public chat, private/server messages delivered through Minecraft chat systems, outgoing user text, usernames, UUIDs, server text, and decoded components. Logs stay local: Cubic adds no telemetry, analytics, automatic upload, crash upload, or sharing feature. OAuth codes, tokens, passwords, device/player private keys, private PEM/DER, AES secrets, session credentials, authentication URLs carrying codes, and cryptographic signatures must never be logged.

The Phase 10 logging acceptance captured real authentication, encryption, compression, Login/Configuration/Play transitions, chat-session establishment, outgoing plaintext, decoded Player/System/Disguised Chat, and resource-bootstrap cache activity.

Those logs conclusively classified the earlier Autcraft missing-message observation as a presentation limitation rather than a secure-chat transport failure. The signed body and complete decoded unsigned/decorated component—including player name and message—were present before presentation, while the current simple plain-text projection omitted child component forms. Raw translation components such as `multiplayer.player.joined` are likewise decoded correctly but not translated yet. These are concrete Phase 25 regression cases; Phase 10 does not change text-component rendering. Autcraft must not be contacted again for Cubic testing.
