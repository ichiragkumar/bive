# PRD: Herdr — Unified Agent Runtime & Control Surface

> Status: **Living document** — the product-level source of truth. Phases are specified in
> `10-14-phase-*.md`; architecture in `01-architecture.md`. Last updated: Phase 1–4
> shipped, Phase 3 refinement landed (three-pane desktop, TypeScript UI, Void design
> system), Phase 5 stubbed.

## 1. Product Vision

Build a lightning-fast, persistent background daemon and multi-interface management
system (CLI + TUI + Desktop app) that acts as the command center for all autonomous
agents. Herdr standardizes how we monitor, control, and interact with coding agents,
AI bots, local LLMs (like Hermes), and remote SSH agents through a single, unified
protocol.

One-liner: **Herdr is a terminal-native AI fleet for long-running coding sessions,
remote SSH work, and controllable (sub-)agents.**

## 2. Problem We Solve (as of now)

Developers already run coding agents (Claude Code, Codex, shell tools) — but each
one lives in its own scattered terminal tab, and:

1. **No fleet view.** With 3+ agents running, nobody can see at a glance what is
   working, what is stuck, and what finished. State lives in scrollback.
2. **Blocked agents go unnoticed.** An agent waiting on a `y/n` prompt or approval
   sits silently until the developer happens to look at that tab.
3. **Closing a terminal kills context.** Output history, state, and the ability to
   reattach die with the window; remote sessions over SSH are worse.
4. **No uniform control.** Every tool has its own CLI flags for the same verbs
   (list, send input, view logs, kill). Nothing composes.
5. **No audit trail.** What did the agent run, what did it print, when did it stall?
   Answers require manual log-keeping.

Herdr's answer: one daemon per user owns every agent PTY, infers state from the
terminal stream, keeps ring-buffer history, and exposes one protocol to every
client — so monitoring and control are uniform, persistent, and OS-integrated
(tray, notifications), whether the developer drives from CLI, TUI, or desktop.

## 3. Target Users

* **Solo developer running 2–10 coding agents** across repos (primary today).
* **Remote-box developer** whose real work happens on SSH machines.
* **Team lead / power user** (future) auditing sub-agent fleets, approvals, costs.
* Non-goals for now: non-technical users, mobile, browser-only users, managed-cloud
  execution (agents run on the user's machines, not ours).

## 4. Product Journey (end to end)

### 4.1 First run (CLI-first onboarding)

1. Install + build: `cargo build --release` (future: one-line installer).
2. Start the daemon: `herdr daemon` (foreground) or `herdr install-service`
   (launchd/systemd login service). `herdr ping` confirms liveness.
3. Spawn the first agent: `herdr spawn --profile bash --cwd /tmp -- bash --norc -i`
   → prints an agent id (`782e4274a90a`).
4. Drive it: `herdr send <id> 'echo hello'` → `herdr logs <id>` shows the echo →
   `herdr list` shows `● working` → `herdr kill <id>`.
5. Graduate to the TUI (`herdr-tui`) or desktop app for the live fleet view.

### 4.2 Daily loop (fleet operations)

1. Morning: daemon already running (login service); `herdr list` / TUI / desktop
   shows yesterday's fleet state.
2. Spawn agents per task (local or `--host dev` for remote), each under a profile
   (`generic`, `claude-code`, `codex`, `bash`).
3. Monitor passively: state badges, tray dot, OS notifications on `Blocked`/`Errored`.
4. Intervene: `send` input to unblock, `attach` to watch live, `kill` the stuck.
5. Review: `logs` replay (256 KB ring buffer survives disconnects and restarts).
6. Evening: close every client freely — agents keep running; `herdr shutdown`
   tears everything down when truly done.

### 4.3 Remote journey (Phase 4)

`herdr remote add dev box.example.com` once → `herdr spawn --host dev -- …`
(first use auto-installs `herdr-agent` to `~/.herdr/bin`) → remote agents appear
in the same fleet with a HOST column / sidebar group → identical `send/logs/kill`.
SSH drops auto-reconnect with backoff and re-sync; no orphan processes.

## 5. Core Principles

* **Zero-Blocking UI** — The engine runs decoupled from clients. Close the TUI or
  desktop window and agents continue uninterrupted.
* **Ultra-Low Latency** — State change → any client badge in < 100 ms locally;
  tray reflects reality within 1 s.
* **Agnostic Execution** — The system doesn't care what the agent runs (Python,
  Node, Rust, any CLI) or where (local PTY or remote SSH box).
* **Hierarchical Visibility** — Total clarity into the fleet and (Phase 5) the
  agent tree: who spawned whom, and what each agent is blocked on.
* **Event-driven, never polling** — One socket per client; every UI update is a
  pushed `DaemonEvent` or a one-shot command reply.
* **Never trust the terminal** — Output is ANSI-stripped before state inference;
  malformed input degrades to plain text, never to a crash or blank screen.

## 6. System Overview (what exists today)

| Piece | State | Description |
|---|---|---|
| `herdr-protocol` | ✅ | UDS path, NDJSON codec, `ClientCommand`/`Response`/`DaemonEvent`, profiles |
| `herdr-daemon` | ✅ | PTY supervisor, state machine, event bus, 256 KB ring buffers, IPC server, media pipeline, remote routing |
| `herdr-cli` | ✅ | `daemon ping spawn list send attach logs kill events shutdown remote replay install-service` |
| `herdr-tui` | ✅ | Fleet list, live ANSI log pane, summary bar, reconnect+resync |
| `herdr-desktop` | ✅ refined | Tauri 2 app: sidebar → agent list → detail (Chat/Terminal/Info), tray, notifications; strict TS UI + esbuild bundle |
| `herdr-remote` + `herdr-agent` | ✅ | SSH bridge (one multiplexed channel/host), headless remote runner |
| `herdr-plugin` | 🚧 stub | Phase 5: plugins, sub-agents, intervention |
| Static preview | ✅ | `scripts/make_preview.py` → `target/preview/index.html` (real files + `__TAURI__` simulator), regression-tested |

### 6.1 State model (inferred, uniform everywhere)

`● working` (hot stream) · `■ blocked` (prompt regex at tail) · `◌ idle`
(silence past timeout) · `✖ errored` (panic signature / non-zero exit) ·
`· exited(n)`. Glyphs double as the product's visual language in every client.

### 6.2 Desktop IA (post-refinement)

Three panes, no routes: **host sidebar** (Local + remotes with counts, spawn +
profile picker, `+ remote`, filter) → **agent list** (selection-driven,
keyboard `j/k`) → **detail pane** (Chat turns from the Rust core, Terminal raw
log, Info metadata, one composer). Tray aggregate + deduped notifications.
Visual system: Void/Panel/Signal/Mist + state colors as accents, IBM Plex Mono
(headlines/data) + Plex Sans (body), committed local fonts.

## 7. Functional Requirements (current contract)

* Daemon: one per user; owns all agent PTYs; survives client death; stale socket
  replaced on restart; live instance refuses doubles; graceful `Shutdown`.
* Spawn/kill/send/logs/list/attach/events over the V0 protocol; unknown agents
  are clean errors (exit 3), unreachable daemon is exit 2.
* State inference identical for local and remote agents (remote ships raw output;
  classification stays local).
* Media: `HERDR-MEDIA <mime> <base64> [caption]` (≤ 8 MiB) → structured event,
  inline rendering in desktop Chat/Terminal, TUI placeholder line.
* Clients: TUI reconnect path (banner → reconnect → resync → List+Events+backfill);
  desktop reopen restores via snapshot; tray < 1 s; notifications for
  Blocked/Errored deduped per (agent, state).
* No polling loops in any client; 0600 per-user socket; per-user services only.

## 8. Success Metrics

* Daemon startup → first agent output displayed: **< 500 ms**
* State change → visible in any client: **< 100 ms** (local socket)
* TUI input latency < 16 ms with 20 agents streaming
* Desktop tray reflects reality within **1 s**
* Remote attach/inject perceived latency: **< 200 ms**

## 9. Roadmap & Current Status

| Phase | Deliverable | Spec | Status |
|---|---|---|---|
| 1 | Core daemon, PTY manager, event bus, IPC, CLI | `10-phase-1-daemon-ipc.md` | ✅ |
| 2 | Ratatui TUI client | `11-phase-2-tui.md` | ✅ |
| 3 | Tauri desktop client (+ refinement: 3-pane, TS, design system) | `12-phase-3-desktop.md` | ✅ |
| 4 | Remote agents / SSH bridge | `13-phase-4-remote-ssh.md` | ✅ |
| 5 | Plugins, sub-agents, intervention, file annotations | `14-phase-5-plugins-control.md` | 🚧 stub |

Explicitly deferred (not dropped): Workspace Explorer file tree, PTY resize,
live host-status events, raw keystroke passthrough, multi-select/bulk actions.

## 10. Open Product Questions (proposed, NOT committed)

A design plan has proposed provider management (`herdr provider add`, BYOK for
Claude/OpenRouter/OpenAI-compatible), OAuth device-flow login, license keys with
a 9-session trial, pricing tiers, one-line installers, and a marketing site.
These contradict nothing technically but assume product surface that does not
exist (the daemon wraps the user's own CLIs; there is no provider/key/auth
concept in the protocol today). Decision required before any build: adopt as a
new phase spec, or keep Herdr bring-your-own-CLI with no accounts or billing.

## 11. Acceptance (product-level)

* A developer can install, spawn, monitor, unblock, and kill local + remote
  agents from CLI, TUI, and desktop interchangeably against one daemon.
* Killing every client mid-session loses no agents and no ring-buffer history.
* `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --workspace`
  green on ubuntu + macOS, incl. reconnect and preview-regression suites.

## 12. Glossary

*Fleet*: all agents owned by one daemon. *Agent*: one PTY-supervised process +
its inferred state, ring buffer, and metadata. *Profile*: output/regex pack
(`generic/claude-code/codex/bash`). *Blocked*: awaiting input/approval.
*Snapshot*: full UI-store handoff (cards + chat turns) for (re)hydration.
*Bridge*: one multiplexed SSH channel per remote host.
