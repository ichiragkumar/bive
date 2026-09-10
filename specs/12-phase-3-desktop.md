# Phase 3 Spec — Desktop Client (Tauri 2)

> Status: **Spec — not yet implemented**. Depends on: Phase 1 contract — no daemon changes required except one additive protocol extension defined below.

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

1. **Fleet Dashboard** — card grid: agent id, profile, state badge, uptime, sparkline of
   output activity, last line of output. Filter/sort by state.
2. **Agent Detail** — rich log view (ANSI rendered as HTML), media blocks inline (images,
   charts), composer to `SendInput`, buttons: Kill, Resize PTY, Copy ID.
3. **Workspace Explorer** — file tree of the agent's `cwd`; Phase 5 adds per-file
   annotations from file-touch tracking; double-click opens in system editor.
4. **System Tray** — aggregate state dot (green/amber/red), agent count, quick actions:
   Spawn generic shell, Kill all, Open dashboard. Tray must reflect state within 1 s
   (event-driven, no polling).
5. **Notifications** — OS notification when any agent enters `Blocked` (attention) or
   `Errored`; suppress duplicates per (agent, state) until state leaves that bucket.

## Deliverables

1. Tauri 2 project in `crates/herdr-desktop` (src-tauri + webview UI).
2. Rust bridge crate: socket client → Tauri events (`event://`, `resp://`), command
   handlers (`spawn`, `send`, `kill`, `attach`, `logs`, `list`, `ping`).
3. Media event pipeline (magic-line → structured event) in daemon + protocol.
4. Tray + notification module with state dedup.

## Acceptance Criteria

1. State latency identical to CLI/TUI (same bus): state change → UI badge < 100 ms local.
2. Test agent emitting `HERDR-MEDIA image/png <base64>` renders inline in Agent Detail.
3. Tray dot changes < 1 s after any agent state change; kill-all works from tray.
4. Closing the window (tray mode) leaves agents running; reopening restores full state
   via `List` + `Logs` replay.
5. No polling loops: all UI updates are event-driven over the single socket.

## Test Strategy

* Unit: media magic-line parser (valid, malformed, oversized, split across chunks),
  notification dedup logic.
* Integration: scripted daemon fixture emits media events; bridge forwards typed events
  to webview harness.
* Manual: two-agent demo fleet (one image-producing Python script, one bash); verify
  tray, notifications, media inline, and daemon-restart recovery.

## Out of Scope

Workspace file annotations (Phase 5), sub-agent tree view (Phase 5), remote hosts (Phase 4 provides the data, Phase 3 UI already renders `host` field when present).
