# Phase 3 Spec — Desktop Client (Tauri 2)

> Status: **Implemented, V2 landed** — three-pane fleet dashboard (sidebar →
> agent list → detail Thread/Terminal/Events/Media/Info), spawn modal with
> presets + command preview, command palette (⌘K), search + attention-first
> sort, kill confirmations, connection states (connected/reconnecting/
> disconnected with supervisor-driven resubscribe + resync), tray priority
> (offline > errored > blocked > working > idle > empty), preview scenarios
> (empty/busy/blocked/errored/media/disconnected), keyboard shortcuts, empty
> states, a11y labels + focus trap + visible focus. No `herdr-daemon`/
> `herdr-protocol` changes. Prior: protocol `AgentMedia` extension + daemon
> media pipeline (10 tests), TUI media placeholders, Tauri-free desktop core,
> Tauri 2 shell + static UI behind `--features tauri`.

## Goal

A lightweight native desktop app (Tauri 2, Rust backend + web frontend) that is the
richest client: fleet dashboard, rich-media log rendering, workspace explorer, system
tray, and OS notifications. The daemon stays headless; the desktop app is just another
socket client with a prettier face.

## Interface Contract

* Same UDS + NDJSON protocol as CLI/TUI. The Tauri Rust side owns the socket connection
  (no node-side socket), exposing typed events/commands to the webview via Tauri events
  and commands.
* **Protocol extension (additive, versioned):** `DaemonEvent::AgentMedia { agent_id, mime, data_base64, caption }`
  — agents signal media by writing a herdr magic line to stdout:
  `HERDR-MEDIA <mime> <base64> [<caption>]` (max 8 MiB). The daemon recognizes the prefix,
  strips it from the text stream, and emits the structured event. Falls back to plain text
  if malformed.

## Views

1. **Sidebar** — hosts (`Local` + one entry per `RemoteAdd`-ed host, live agent
   count per host, offline grey-out from a one-shot `RemoteList` per
   snapshot/resync), `+ spawn` with profile picker
   (`generic/claude-code/codex/bash`, passed through to `Spawn` unchanged,
   incl. `--host`), `+ remote` entry point to `RemoteAdd` (✕ per row for
   `RemoteRemove`), state filter. Collapses to a drawer under ~1080 px
   (toggle in the topbar).
2. **Agent list** — selection-driven rows (id, profile, state badge, last
   line); replaces the card grid, which doesn't scale past ~12 agents.
   Keyboard-navigable (`j/k`, arrows, Enter → composer).
3. **Detail pane** — persistent panel (no modal), three tabs with per-agent
   memory: **Chat** (turn-by-turn human/agent/tool segments from the Rust
   core; "raw output only" notice for `bash`/`generic`), **Terminal** (raw
   ANSI log, ground truth), **Info** (id, host, profile, cwd, command, state,
   uptime, media count). Single composer below all tabs (line-send only).
4. **System Tray** — aggregate state dot (green/amber/red), agent count, quick actions:
   Spawn generic shell, Kill all, Open dashboard. Tray must reflect state within 1 s
   (event-driven, no polling).
5. **Notifications** — OS notification when any agent enters `Blocked` (attention) or
   `Errored`; suppress duplicates per (agent, state) until state leaves that bucket.

Deferred (explicitly, not dropped): **Workspace Explorer** (no webview fs
access without a new plugin) and **PTY resize** (no `Resize` variant in
`ClientCommand` — needs a protocol addition). Remote hosts are *in* scope
now that Phase 4 has landed (this supersedes the old out-of-scope line).

## Deliverables

1. Tauri 2 project in `crates/herdr-desktop` (src-tauri + webview UI).
2. Rust bridge crate: socket client → Tauri events (`herdr://event`,
   `herdr://conn`), command handlers (`spawn`, `send`, `kill`, `kill_all`,
   `logs`, `list`, `ping`, `snapshot`, `remote_add`, `remote_list`,
   `remote_remove`; `spawn` takes an optional `host`).
3. Media event pipeline (magic-line → structured event) in daemon + protocol.
4. Tray + notification module with state dedup.
5. Modular frontend (`ui/`: strict TypeScript sources under `src/`, one module
   per surface, split styles), esbuild bundle (`ui/dist/bundle.js`, committed,
   rebuilt with `sh scripts/build_ui.sh` which runs `tsc --noEmit` first),
   Chat segmentation in the Rust core shipped via `snapshot`, static preview
   generated from the real files.
6. Visual system: Void/Panel/Signal/Mist base with agent state colors as the
   accent system, IBM Plex Mono (headlines/data) + IBM Plex Sans (body) from
   committed local font files (offline-first; `font-src 'self'` in the CSP),
   state glyphs (● ■ ◌ ✖ ·) as the visual language.

## Acceptance Criteria

1. State latency identical to CLI/TUI (same bus): state change → UI badge < 100 ms local.
2. Test agent emitting `HERDR-MEDIA image/png <base64>` renders inline in Agent Detail.
3. Tray dot changes < 1 s after any agent state change; kill-all works from tray.
4. Closing the window (tray mode) leaves agents running; reopening restores full state
   via `List` + `Logs` replay.
5. No polling loops: all UI updates are event-driven over the single socket.

## Test Strategy

* Unit: media magic-line parser (valid, malformed, oversized, split across chunks),
  notification dedup logic, Chat segmentation goldens (`segment_chat`:
  human/agent/tool grouping, FIFO consume-once, short-send rule, bounds).
* Integration: scripted daemon fixture emits media events; bridge forwards typed events
  to webview harness.
* Structural (`preview_html.rs`, 6 tests): skeleton mount points, bundle↔Rust
  surface match (incl. remote commands), stylesheet coverage, harness contract,
  generated-preview validation, balance-checker negative control. The preview
  is generated FROM the real `ui/` files so the skeleton cannot drift.
* Manual: two-agent demo fleet (one image-producing Python script, one bash); verify
  tray, notifications, media inline, and daemon-restart recovery.

## Out of Scope

Workspace file annotations (Phase 5), sub-agent tree view (Phase 5),
Workspace Explorer file tree (no webview fs access without a new plugin),
PTY resize (needs a protocol addition), live host-status events (needs a
protocol addition — sidebar uses one-shot `RemoteList`), raw keystroke
passthrough in the Terminal tab, multi-select/bulk actions beyond global
kill-all. (Remote hosts themselves are in scope since Phase 4 landed.)
## Phase 3.5 implementation record (UX/journey revamp)

Built against the Phase 3.5 prompt. No daemon/protocol changes; one thin Tauri
wrapper added (`shutdown_cmd` over the existing `Shutdown` command — same class
as the earlier `remote_*` wrappers).

* **Found + fixed live:** the app never sent `ClientCommand::Events`, so the
  real shell was snapshot-only. Added a supervisor thread (reconnect →
  resubscribe → resync → `connected`; `reconnecting`/`disconnected` states),
  cold-start-disconnected launch, and `Bridge::disconnected()`.
* **Shell states (§3):** pill + strip + dimmed last-known fleet with stale
  markers; guided empty/disconnected states; detail empty state hides
  tabs/actions/composer (guard-tested).
* **Journeys:** optimistic spawn rows reconciled + auto-selected on
  `AgentSpawned` with toast; blocked→emphasized composer→send→flip; errored
  bar (copy logs / new-like-this / kill); kill confirmations everywhere;
  clear-exited (view-hide until daemon auto-prune); shutdown with strong
  confirm; palette (spawn presets, jump, hosts, follow, tabs, reconnect,
  shutdown, diagnostics).
* **Fleet:** search + 4 sorts (attention default) + 200-row cap + sparklines +
  relative-time tick (30 s view clock, no daemon traffic) + hover copy/kill.
* **Detail:** Thread/Terminal (follow+pause pill, full-buffer load, copy,
  byte count)/Events timeline/Media gallery/Info; per-agent tab memory.
* **Feedback:** toasts on async actions, copy morphs, inline spawn/modal
  errors, retry + copy-diagnostics in the strip.
* **Preview:** 10 scenarios via `?scenario=` + banner links, from the real UI.
* **Known limitations (documented deviations):** no label field (no protocol
  slot); no respawn-with-identical-args ("new like this" prefills
  profile/host/cwd/command); notification click-through unsupported by
  tauri-plugin-notification v2 (no click callback); tray menu actions can't
  confirm (noted); row "reconnecting" markers need host-level events (no
  protocol support); `~`/relative cwd passed through unchecked (webview has
  no fs access); list capped at 200 rows instead of virtualized.
