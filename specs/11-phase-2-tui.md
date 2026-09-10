# Phase 2 Spec — TUI Client (Ratatui)

> Status: **Spec — not yet implemented**. Depends on: Phase 1 contract (`10-phase-1-daemon-ipc.md`) — no daemon changes required.

## Goal

A Ratatui terminal app that feels instant: the whole fleet at a glance, one-keystroke
deep-dive into any agent, live log tailing with full color. Proves the "TUI is just
another client" decoupling — closing the TUI never disturbs an agent.

## Interface Contract

* One UDS connection, same `ClientCommand`/`DaemonEvent` envelopes as the CLI.
* On start: `List` → populate; `Logs` (64 KB) per visible agent; subscribe `Events`.
* On daemon restart (socket error / `ping` failure): exponential-backoff reconnect, then
  re-sync (`List` + `Logs`), then resubscribe. Never crashes on daemon death.

## Layout

```
┌ Fleet ──────────────────────────────────────────────────────────┐
│ ▸ a1b2 claude-code   Working   2m 14s   ~/proj/api              │
│   c3d4 codex         Blocked   0m 08s   ~/proj/web  ← prompt    │
│ ▾ e5f6 claude-code   Working   0m 41s   ~/proj/api              │
│     └ g7h8 codex     Working   0m 40s   (sub-agent, Phase 5)    │
├ Log pane (selected agent) ──────────────────────────────────────┤
│ $ pytest tests/ -x                                              │
│ ............                                                    │
│ ✔ 12 passed in 1.2s                                             │
│ ? Allow network access? (y/n)                                   │
├─────────────────────────────────────────────────────────────────┤
│ [a]ttach [s]end [k]ill [f]ollow 3/4 agents working    q]uit     │
└─────────────────────────────────────────────────────────────────┘
```

* **Agent list (left / top):** state glyphs (● working, ◌ idle, ■ blocked, ✖ errored),
  colored per state; tree indentation reserved for sub-agents (Phase 5).
* **Log pane:** virtualized scrollback (ring of styled lines, 10k cap per agent);
  ANSI from `AgentOutput.payload` parsed into styled spans — colors preserved, no raw
  escape leakage. Follow mode tails; any scroll-up releases follow, `f` or end-of-buffer
  re-engages.
* **Summary bar:** counts per state, aggregate, plus daemon uptime from `ping`.

## Interactions

| Key | Action |
| --- | --- |
| ↑/↓, j/k | select agent |
| Enter/a | focus log pane full-height (attach view) |
| s | open send-input modal → `SendInput` |
| k | kill with y/n confirm |
| f | toggle follow |
| PgUp/PgDn, g/G | scroll log |
| q | quit (agents unaffected) |

## Deliverables

1. `herdr-tui` binary wired to the Phase 1 socket.
2. ANSI-to-spans renderer module (unit-tested against escaped sample streams).
3. Reconnect + resync state machine (unit-tested with a scripted daemon stub).

## Acceptance Criteria

1. Input latency < 16 ms with 20 agents streaming (log pane capped, list virtualized).
2. Attach/detach mid-stream without dropping output; ring replay fills the pane on select.
3. Colored/progress-bar output renders cleanly; no raw `\x1b[` visible.
4. Works over plain SSH sessions (no graphics protocol required at this phase).
5. Daemon killed under the TUI ⇒ visible "reconnecting…" banner, auto-resync on revive.

## Test Strategy

* Unit: ANSI span parser, virtualized scrollback math, reconnect/backoff state machine.
* Integration: scripted daemon fixture emitting fixed streams; assert rendered lines.
* Manual: tmux + 20 synthetic `bash` agents; measure input→render latency.

## Out of Scope (deferred to Phase 5)

Kitty/Sixel media rendering, mouse-driven file tree, sub-agent tree UI (Phase 5 data model lands first in the protocol).
