# PRD: Herdr — Unified Agent Runtime & Control Surface

> Status: **Living document** — the product-level source of truth. Phases are specified in
> `10-14-phase-*.md`; architecture in `01-architecture.md`.

## 1. Product Vision

Build a lightning-fast, persistent background daemon and dual-interface management system
(TUI + Desktop App) that acts as the command center for all autonomous agents. Herdr
standardizes how we monitor, control, and interact with coding agents, AI bots, local LLMs
(like Hermes), and remote SSH agents through a single, unified protocol.

## 2. Core Principles

* **Zero-Blocking UI** — The core engine runs completely decoupled from the UI. If the TUI
  or Desktop app is closed, agents continue working uninterrupted.
* **Ultra-Low Latency** — Millisecond-level state synchronization between running agents and
  control surfaces.
* **Agnostic Execution** — The system doesn't care what the agent is written in (Python,
  Node, Rust) or where it lives (local machine or remote SSH server).
* **Hierarchical Visibility** — Total clarity into the "agent tree": which master agent
  spawned which sub-agent, and exactly what file or network call they are blocked on.

## 3. Technical Architecture

* **Core Daemon (Rust / Tokio)** — Background service managing all process spawning, SSH
  tunneling, and socket connections. One daemon per user; clients come and go freely.
* **State Sync (Event Bus)** — Internal pub/sub. As agents emit logs or change state, the
  daemon broadcasts events over a local Unix domain socket (NDJSON), letting clients consume
  updates with near-zero overhead.
* **TUI Client (Ratatui)** — Highly responsive terminal interface. Uses modern terminal
  image protocols (Kitty graphics / Sixel) to render media directly in the terminal,
  alongside a navigable file tree.
* **Desktop Client (Tauri 2)** — Lightweight native app wrapping the Rust engine, with
  native OS file-system integration, rich media rendering, and deep visual control.

## 4. Key Features & Requirements

### 4.1 Universal Agent Management
* **Multi-Source Support** — Run Claude Code, Pi-based bots, T3 coding assistants, and local
  Hermes instances side-by-side.
* **Remote Bridging** — Add a remote machine via SSH; the daemon tunnels the remote agent's
  stdout and state back to your local UI as if it were running natively.
* **Plugin System** — Standardized IPC protocol allowing custom plugins and third-party bots
  to register themselves with the runtime.

### 4.2 Deep State & Sub-Agent Control
* **Live Status Indicators** — Agents and sub-agents are strictly categorized: `Working`,
  `Idle`, `Blocked (Awaiting Approval/Input)`, or `Errored`.
* **Intervention Mechanics** — Pause, force-kill, or inject a direct prompt into a specific
  sub-agent without disrupting the master agent's context.

### 4.3 Rich Media & File Navigation
* **Workspace Explorer** — Unified file tree showing exactly which files agents are
  currently reading or modifying.
* **Media Preview** — Native rendering of output images, charts, and web links directly
  within both TUI and Desktop app.

## 5. Success Metrics

* Daemon startup → first agent output displayed: **< 500 ms**
* State change → visible in any client: **< 100 ms** (local socket)
* TUI stays responsive (input latency < 16 ms) with 20 agents streaming
* Desktop tray state reflects reality within **1 s** of a state change
* Remote attach/inject perceived latency: **< 200 ms**

## 6. Roadmap

| Phase | Weeks | Deliverable | Spec |
| --- | --- | --- | --- |
| 1 | 1–2 | Core daemon, PTY manager, event bus, IPC, CLI | `10-phase-1-daemon-ipc.md` |
| 2 | 3–4 | Ratatui TUI client (MVP) | `11-phase-2-tui.md` |
| 3 | 5–6 | Tauri desktop client | `12-phase-3-desktop.md` |
| 4 | 7–8 | Remote agents / SSH bridge | `13-phase-4-remote-ssh.md` |
| 5 | 9–10 | Plugins, sub-agents, intervention, media | `14-phase-5-plugins-control.md` |

Each phase spec defines goal, interface contract, deliverables, acceptance criteria, and
test strategy. Phases build strictly on the previous phase's contract — no phase reopens
the wire protocol except via additive, versioned changes.
