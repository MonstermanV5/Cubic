# Phase 8 Chat Mode MVP

Chat Mode is Cubic's low-power, full-window Minecraft chat experience. The Phase 8 MVP targeted a controlled vanilla Java 26.1.2 / protocol 775 server. It is not a terminal logger. Phase 18 now lets a world-capable session select this presentation without drawing the retained live world.

## Architecture

`cubic-app chat` starts the persistent network task on a dedicated Tokio thread, then runs winit and wgpu on the native UI thread. Bounded commands flow from `cubic-ui` to the network task; bounded protocol-independent `ChatEvent` values flow back. UI code never owns TCP, and network progress does not depend on redraw frequency.

The UI uses egui 0.36.1 directly with `egui-winit` and `egui-wgpu`, rather than adopting `eframe` or a game engine. This release explicitly supports wgpu 30 and winit 0.30 and uses the same cross-platform wgpu backend, including Metal on iOS. `egui-winit` is built with its official `clipboard` feature so keyboard copy, cut, and paste use the native OS clipboard instead of its same-process fallback.

egui's built-in fonts come from the crates.io `epaint_default_fonts` dependency, whose package metadata declares `(MIT OR Apache-2.0) AND OFL-1.1 AND Ubuntu-font-1.0`. On Windows, the platform bootstrap appends one installed CJK font—Microsoft YaHei, Yu Gothic, or Malgun Gothic in deterministic preference order—after the existing proportional and monospace fallback lists. Latin, Cyrillic, symbols, and egui's existing Noto emoji font therefore keep their current priority, while common Han characters gain coverage. The selected system file is read once at Chat Mode startup with a 64 MiB bound. Cubic redistributes none of these Windows fonts; their use remains governed by the local Windows installation. The provider boundary is platform-specific so a future native iOS host can supply an installed iOS font without changing `cubic-ui`. Until that host exists, the ARM64 iOS compile path retains egui's built-in fonts. Phase 25 may replace or extend this presentation layer for complete Minecraft text semantics.

The native event loop waits. It wakes every 200 ms to drain bounded session state but only requests a GPU frame when state/input changed or a window event requires one. This is an architectural low-idle-power baseline, not a final battery benchmark.

## Text and history

The decoder supports NBT string components, compound `text`, nested `extra`, Unicode, and a conservative `translate` fallback. The original bounded tree is converted to a stable protocol-independent structure alongside its plain projection. Unknown styles and richer valid fields remain inert; URLs, commands, click events, and hover events are never executed.

Unsigned outgoing chat uses protocol 775's complete payload, including the VarInt last-seen offset, fixed three-byte/20-bit acknowledgement field, and trailing checksum byte. Only signed Player Chat enters the server's tracked last-seen window and receives a dedicated acknowledgement. Unsigned Player Chat—including Cubic's own vanilla-server echo—must not advance that window. Cubic therefore uses an empty embedded update with checksum zero; server-console System or Disguised Chat also does not alter that state.

History retains at most 500 messages and 256 KiB of projected sender/text bytes, evicting oldest entries deterministically. Input is limited to 256 Java UTF-16 units and rejects control characters. Empty lines do not send. Slash commands are explicitly unsupported because Cubic does not yet retain the server command graph needed to determine legitimate signed-argument behavior.

## Protocol-775 subset

Handled clientbound Play traffic: Keep Alive, Ping, Player Position, Chunk Batch Finished, Cookie Request, Player Chat, Disguised Chat, System Chat, Disconnect, Set Health (low-health warning), and Start Configuration. The matching minimum replies are sent. All packet IDs and shapes remain in `cubic_protocol::bootstrap::v775`; Phase 12 will replace or absorb them with generated packet data.

The standalone `chat` command still discards other complete bounded world frames. The Phase 18 `world` composition instead keeps its existing bounded authoritative world pipeline active while Chat presentation is selected; this does not turn the UI into an owner of world state.

## Manual acceptance

Configure a vanilla Java Edition 26.1.2 server at `localhost:25565`:

```text
online-mode=false
network-compression-threshold=-1
resource-pack=
require-resource-pack=false
enable-code-of-conduct=false
```

Then run:

```text
cargo run -p cubic-app -- chat localhost:25565 --username CubicTest
```

The final real-server and Windows UI acceptance passed: persistent bidirectional chat, Unicode transport, visible common CJK fallback glyphs, spam handling, bounded history eviction, scrolling, resizing, minimize/restore, Enter and button send actions, health/death warnings, long idle operation, and clean disconnect all worked. Native clipboard interoperability was also verified in both directions: external application → Cubic paste and Cubic copy/cut → Windows system clipboard. Release-mode idle use on the tested Windows machine was approximately 5% CPU with brief spikes near 10%, 115 MiB RAM, and 1.3% GPU; this is accepted for the MVP and is not a profiling baseline.

Phase 8 is complete. The limitations and ownership boundaries below remain in effect.

Phase 9 reuses this exact UI with `chat <address> --backend xal`: stored credentials refresh silently, the app obtains a short-lived Mojang player certificate, the shared encrypted/compressed bootstrap reaches Play, and the selected version profile establishes/signs a persistent player chat session. Incoming message provenance is retained as system/not-applicable, unsigned, signed-but-not-yet-cryptographically-verified, or modified without exposing raw signatures to the UI. The real 26.1.2 test passed with `online-mode=true`, `enforce-secure-profile=true`, compression, signed outgoing chat, System Chat reception, and clean disconnect.

Exploratory Autcraft testing showed that some independently decorated rank/pronoun/channel messages can retain their prefix while Cubic's current presentation loses the visible message body; translation keys can also remain untranslated. This is recorded as a Phase 25 presentation limitation rather than server-specific behavior. Cubic does not add proxy-specific rendering rules.

Phase 25 owns complete text/UI/server-presentation semantics.

## Phase 18 shared-session mode

The `world` development command now exposes a `CHAT` control over the live world and a prominent `PLAY` control in the existing Chat Mode header. Both views retain the same TCP connection and network task; changing mode sends no Login, Configuration, reconnect, respawn, or player-creation request. Incoming chat history remains bounded by the Phase 8 limits and survives both transitions.

Entering Chat clears all gameplay held/edge state, releases pointer capture, and routes native input only through egui. Typing movement-key letters cannot move the player. Returning to Play clears input once more, requires fresh gameplay key transitions, and leaves pointer recapture explicit.

World packets and authoritative state continue while Chat is visible. The renderer receives only latest coalesced chunk revisions and pose state; it submits event-driven UI frames but performs no terrain frame, mesh-result integration, or new mesh dispatch. Existing worker/result channels remain capped at 32 and the dirty map remains bounded by the loaded-chunk cap. On return, current-pose distance priority rebuilds nearby dirty chunks first. Cubic has no particles or audio subsystem yet, so neither performs hidden work.

Semantic safety alerts appear as conspicuous notices and use the priority event slot rather than the spam-droppable regular chat queue. Health packets generate damage, one low-health transition warning at six health or below, and one death warning. The exact 26.1.2 Set Entity Data packet (`0x63`) supplies the base Entity air field when sent as its standalone index-1/INT update; crossing 60 air units generates one drowning warning until air recovers. While Chat is active, an authoritative correction at least eight blocks from local prediction generates a displacement warning; smaller reconciliation jitter does not. These event kinds are platform-neutral so a later iOS host can map them to haptics without protocol logic in UI code.

The optimized Windows localhost acceptance passed: Play -> Chat -> Play retained the same live session, chat typing remained isolated from gameplay, world changes continued while hidden, returning to Play converged to the current world, and the safety-warning path behaved sufficiently for Phase 18 acceptance.
