# Phase 5 Spec — Plugins, Sub-Agents & Advanced Control

> Status: **Spec — not yet implemented**. Depends on: Phases 1–4. Introduces `herdr-plugin` crate.

## Goal

Open the runtime to third parties and unlock deep intervention: plugins register synthetic
agents, sub-agent hierarchies become visible and controllable, agents can be paused/resumed,
and the TUI renders media inline via Kitty/Sixel.

## A. Plugin System

**Transport:** JSON-RPC 2.0 over a dedicated UDS per plugin (`herdr.<plugin>.sock`).
Daemon acts as the registry and permission broker.

```rust
// New ClientCommands (daemon-internal, from plugin connections):
PluginRegister { name: String, version: String, capabilities: Vec<PluginCap> }
PluginProvideAgent { plugin: String, spec: SyntheticAgentSpec } // agent without a PTY
PluginEmit        { agent_id: AgentId, event: DaemonEvent }     // constrained set
// Capabilities: ProvideAgent | ObserveEvents | InjectInput | ControlLifecycle
```

* **Synthetic agents** are first-class: they appear in `list`/TUI/desktop with
  `source: plugin`, have ring buffers the plugin fills via `PluginEmit`, and can receive
  `SendInput` routed back to the plugin.
* **Permission model:** registration declares capabilities; daemon enforces (e.g.
  `ObserveEvents` subscriptions are filtered to declared scopes; `ControlLifecycle`
  requires per-agent grant). Phase 5 ships an allowlist file `~/.config/herdr/plugins.toml`.
* **Lifecycle:** plugins connect → register → declare agents; daemon watchdog kills
  synthetic agents whose plugin socket closes (marked `Errored("plugin disconnected")`).

## B. Sub-Agent Hierarchy

* **Explicit first:** any agent can emit the marker line
  `HERDR-SUBAGENT <parent_id>` on stdout (similar magic-line mechanism as media), or call
  the plugin API `DeclareChild { parent, child }`.
* **Heuristic fallback:** for tools that don't emit markers, a `profile.subagent_hints`
  regex (e.g. Claude Code's "Task" tool output) attributes children best-effort, clearly
  marked `inferred: true`.
* **Protocol:** `AgentInfo` gains `parent: Option<AgentId>`, `inferred: bool`;
  `List` returns the flat map — clients build trees.
* **Control:** `Kill { subtree: bool }` extension — killing a master kills descendants
  (topological order, leaves first). SendInput always targets exactly one agent.

## C. Intervention Mechanics

* **Pause/Resume:** `Pause { agent_id }` → POSIX `SIGSTOP` to the PTY child (plus stop
  reading), `Resume` → `SIGCONT`. State shows `Paused` (new `AgentState` variant).
  Windows: documented limitation — no pause, kill-only (spec note in code).
* **Approval queue hook:** `Blocked` agents with a profile-declared `approve_pattern`
  (e.g. `(y/n)`) surface a one-key approve/deny action in TUI/desktop that maps to
  `SendInput("y\n")` / `SendInput("n\n")`. No new protocol — a client-side affordance on
  top of Phase 1 primitives.

## D. Media in the TUI

* Kitty graphics protocol first (chunked base64 transmission in-band), Sixel fallback for
  terminals advertising it, plain-text placeholder otherwise.
* Images come from the Phase 3 `AgentMedia` event — same source, terminal-native rendering.
* Escaping rules: images render only in the log pane's dedicated media gutter so they
  never corrupt scrollback text.

## Deliverables

1. `herdr-plugin` crate: JSON-RPC server side, capability enforcement, synthetic-agent
   registry integration.
2. Daemon: hierarchy fields + `Kill{subtree}`, Pause/Resume, `HERDR-SUBAGENT` scanner,
   plugin watchdog.
3. Demo plugin (Rust or Python) registering a synthetic "hermes-status" agent.
4. TUI: tree view, approve/deny keybinds, Kitty/Sixel image gutter.

## Acceptance Criteria

1. Demo plugin registers, provides a synthetic agent, streams output to `herdr attach`;
   killing its socket marks the agent `Errored("plugin disconnected")`.
2. Claude Code multi-agent session renders as a tree (parent/child) in TUI; killing the
   master with subtree=true kills all descendants.
3. Pause/Resume verified on a long-running local agent (`Paused` glyph, frozen output,
   clean resume).
4. A test script emitting `HERDR-MEDIA image/png ...` renders an actual image in Kitty
   and Sixel-capable terminals; harmless placeholder elsewhere.
5. Permission enforcement: a plugin without `InjectInput` attempting `SendInput` gets a
   JSON-RPC permission error; allowlist file is honored.

## Test Strategy

* Unit: capability enforcement matrix, subtree topological kill order, marker scanner
  (split across chunks), pause state transitions.
* Integration: scripted plugin speaks full JSON-RPC lifecycle against daemon fixture.
* Manual: demo plugin + Claude Code hierarchy + Kitty terminal media walkthrough.

## Out of Scope

Plugin marketplace/signing, remote plugin execution, mouse file-tree annotations
(workspace explorer annotations move to a follow-up spec if needed).
