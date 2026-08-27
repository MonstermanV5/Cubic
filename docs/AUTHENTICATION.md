# Phase 9 Authentication

Phase 9 is an in-progress public-native-client authentication implementation. Cubic never accepts a Microsoft password and has no client secret. It exposes two explicit providers that produce the same bounded `AuthenticatedMinecraftAccount` model:

- `CubicEntra` is the intended long-term production path and uses Cubic's own Entra application ID.
- `XalInterop` is an experimental development/interoperability path using the public first-party Minecraft/Xbox launcher XAL/SISU protocol shape. Its identifiers are not Cubic's identity and their use is not assumed authorized for public or store distribution.

Neither provider requires a Cubic-hosted backend, embedded login form, borrowed independent-launcher credential, or disabled TLS verification.

## Cubic Entra application registration

Cubic's preferred backend is a public mobile/desktop Entra application supporting personal Microsoft accounts. Configure its application ID through `CUBIC_MSA_CLIENT_ID`, enable public-client flows, and register the matching loopback redirect. Cubic rejects missing or malformed IDs and never substitutes another launcher's ID.

Interactive Cubic-Entra sign-in uses the system browser, Microsoft Authorization Code flow, an S256 PKCE challenge, random state, and a one-use IPv4-loopback callback on an ephemeral port. The Windows callback is `http://127.0.0.1:<ephemeral>/cubic/oauth/callback`; a future iOS host must provide its own registered native callback/system authentication session.

Microsoft documents Authorization Code + PKCE for desktop/mobile applications at `https://learn.microsoft.com/entra/identity-platform/v2-oauth2-auth-code-flow` and loopback redirect rules at `https://learn.microsoft.com/entra/identity-platform/reply-url`.

The real Cubic-Entra test completed Microsoft Authorization Code + PKCE, Xbox User Authentication, and XSTS. Minecraft Services then returned HTTP 403 `Invalid app registration`. Cubic maps that response to a dedicated external-registration error. It must be resolved through legitimate Microsoft/Minecraft approval; borrowing or spoofing Prism, ATLauncher, MultiMC, or another independent launcher's identity is forbidden.

The implemented preferred chain remains:

```text
Microsoft /consumers Authorization Code + PKCE
  -> Microsoft access/refresh token
  -> Xbox User Token (RPS, user.auth.xboxlive.com)
  -> XSTS (RETAIL, rp://api.minecraftservices.com/)
  -> Minecraft login_with_xbox
  -> Java entitlement check
  -> authoritative Minecraft profile
```

## Experimental XAL/SISU interoperability

The experimental backend is selected only with `--backend xal`; it never changes the default meaning of `auth login`. Its independently implemented flow is:

```text
persistent P-256 device identity
  -> signed Xbox device/authenticate
  -> signed SISU authenticate (PKCE challenge + random state)
  -> dedicated private WebView2 Microsoft login window on Windows
  -> state-validated desktop redirect + authorization-code exchange
  -> signed SISU authorize (device + user + title context)
  -> signed XSTS (user + device + title tokens,
                  rp://api.minecraftservices.com/)
  -> Minecraft launcher/login (platform PC_LAUNCHER)
  -> Java entitlement check
  -> authoritative Minecraft profile
```

The experimental constants are the observed first-party Minecraft/Xbox launcher application ID `00000000402b5328` and title ID `1794566092`. They are named and scoped as XAL interoperability constants, not `CUBIC_MSA_CLIENT_ID`. Cubic makes no claim that using this first-party identity is authorized for eventual public distribution; that policy question must be resolved before a production or store release.

The protocol shape was cross-checked against the current [Modrinth launcher authentication implementation](https://github.com/modrinth/code/blob/main/packages/app-lib/src/state/minecraft_auth.rs). Cubic's implementation is independent and does not copy that source. The proof-of-possession canonical byte layout and signature envelope follow Microsoft's [Xbox service-call signing documentation](https://learn.microsoft.com/en-us/gaming/gdk/docs/services/fundamentals/s2s-auth-calls/s2s-calls/live-title-service-calls-xbox-live). These sources describe interoperability facts; they do not grant Cubic permission to distribute under Microsoft's first-party identity.

Xbox device, SISU, and XSTS requests are signed with a persistent P-256 key. Public coordinates are sent as an ES256 JWK. The signature input is the policy version, Windows FILETIME timestamp, uppercase HTTP method, absolute path/query, Authorization value, and exact transmitted body, with required NUL separators. The `Signature` header contains version + timestamp + fixed-width raw ECDSA `r || s`, encoded with standard Base64. The key and device UUID are generated with a cryptographically secure RNG.

The first-party identity is registered to `https://login.live.com/oauth20_desktop.srf`, not Cubic's loopback callback. The initial real validation used a temporary manual redirect copy/paste handoff. Windows now opens a dedicated private WebView2 navigation host and observes only top-level navigation. It accepts the SISU authorization URL only at Microsoft's exact HTTPS `login.live.com/oauth20_authorize.srf` endpoint. When top-level navigation reaches the exact desktop redirect with one code and the matching random state, Cubic cancels that navigation, closes the window immediately, wraps the code in its redacted secret type, and continues the unchanged PKCE exchange automatically. It never injects JavaScript, inspects the DOM or fields, captures keys, or receives the Microsoft password.

The sign-in host requires the Microsoft Edge WebView2 Runtime. It requests WebView2 private mode and also uses a uniquely created temporary browser-profile directory that is discarded after the window closes, avoiding reliance on persistent browser state. It disables downloads, page permissions, popups, developer tools, general autofill, context menus, and browser accelerator keys. It permits top-level HTTPS navigation only to the reviewed exact identity hosts `login.live.com`, `account.live.com`, `login.microsoftonline.com`, and `account.microsoft.com`; ports, user information, misleading suffixes, and subdomains are rejected. Subresources are not inspected. Closing the window returns cancellation, and a five-minute deadline returns a distinct timeout. Cubic-Entra continues to use the external system browser plus loopback callback and does not use this window.

Refresh uses the stored Microsoft refresh token, the same secure device identity, a fresh Xbox device token, SISU authorize without an interactive session ID, XSTS, and Minecraft launcher login. Rotated refresh tokens replace older values; if a successful response omits a replacement, Cubic retains the previous token.

## HTTP and secret handling

Both clients use compiled HTTPS production origins, finite connect/request timeouts, disabled redirects, a Cubic user agent, normal TLS verification, bounded 256 KiB response bodies, and structured transport/status/JSON errors. Tokens, authorization codes, PKCE verifiers, SISU session IDs, device identity material, and shared secrets are redacted from formatting and never traced.

XAL endpoints are limited to Xbox device authentication, SISU authenticate/authorize, XSTS, Microsoft's desktop OAuth token endpoint, Minecraft launcher login, entitlements, and profile. Cubic does not accept user-configurable production service URLs.

## Secure storage

Windows uses Credential Manager through `keyring` with no plaintext fallback. Cubic-Entra retains its original `default` credential key for compatibility. XAL account credentials use a separate `xal-interop-account` record, and the XAL device UUID/private key use a separate `xal-interop-device` record. One backend cannot overwrite or delete the other ambiguously.

XAL logout removes only the selected account/refresh credential and intentionally retains the device identity for stable future device authentication. Tests use an in-memory implementation. The trait remains suitable for a future iOS Keychain implementation, but iOS secure persistence is explicitly unavailable instead of falling back to a file.

## Online-mode Login

`cubic-network` uses a provider-neutral `MinecraftSessionJoiner`; it does not know which authentication provider produced the Minecraft access token. Both providers feed the existing protocol-775 online path: authenticated profile, Mojang session-server join, RSA PKCS#1 v1.5 Encryption Response, continuous AES-128/CFB8, bounded zlib compression, Login, Configuration, and Play.

The transport ordering remains:

```text
TCP <-> AES/CFB8 <-> Minecraft outer frame <-> zlib packet body <-> packet codec
```

## Development commands

```text
# Preferred Cubic-owned registration (unchanged defaults)
cargo run -p cubic-app -- auth login
cargo run -p cubic-app -- auth logout
cargo run -p cubic-app -- online-login localhost:25565

# Experimental XAL interoperability
cargo run -p cubic-app -- auth login --backend xal
cargo run -p cubic-app -- auth status --backend xal
cargo run -p cubic-app -- auth logout --backend xal
cargo run -p cubic-app -- online-login localhost:25565 --backend xal

# Reports both backend slots without exposing tokens
cargo run -p cubic-app -- auth status
```

Without `--backend`, login, logout, and online-login retain their original Cubic-Entra meaning. Status prints only backend, profile, UUID, and refresh requirement.

## Current acceptance boundary

Automated tests never use real credentials. The Cubic-Entra backend's real test is externally blocked at Minecraft Services `Invalid app registration` after successful Microsoft/Xbox/XSTS stages. The experimental XAL backend has now passed a real end-to-end account test: Xbox device authentication, SISU authenticate, Microsoft OAuth with PKCE, SISU authorize, XSTS, Minecraft `/launcher/login`, entitlement verification, profile retrieval, and secure credential persistence all succeeded. That interoperability result does not resolve Cubic's own registration approval or establish authorization to distribute under the first-party identity. The new automatic WebView2 redirect capture still requires a short real UX retest.

The Mojang player-certificate endpoint, protocol-775 Player Session establishment, authenticated secure-chat signing, persistent authenticated Chat Mode, native iOS Keychain/browser callback host, and a full deterministic mock HTTP sequence remain incomplete. Cubic does not claim compatibility with `enforce-secure-profile=true` Chat Mode. These limitations are not permission to disable or bypass server security. Phase 9 remains `[~]`, and Phase 10 has not begun.
