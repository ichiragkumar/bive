# Herdr — System Architecture

> Status: **Living document**. Companion to the PRD (`00-prd.md`). Concrete per-phase
> requirements live in `10-14-phase-*.md`. This file records the durable design and the
> decisions log.

## 1. System Overview

```
                    ┌────────────────────────────────────────────────┐
                    │                herdr-daemon                    │
                    │                                                │
  claude ──PTY──▶   │  pty.rs ── spawn_blocking reader ──┐           │
  codex  ──PTY──▶   │  (portable-pty)                    ▼           │
  bash   ──PTY──▶   │                                bus.rs          │
                    │              registry.rs ◀────broadcast────┐   │
                    │            (agents, rings)                 │   │
                    │                                    ▲       │   │
                    │                                state.rs    │   │
                    │                            (per-agent FSM) │   │
                    │                                                │
                    │  ipc.rs ◀── NDJSON over UDS ──▶ clients        │
                    └────────────────────────────────────────────────┘
                                   ▲            ▲            ▲
                        herdr (CLI)│      herdr-tui      herdr-desktop
                                   │        (Phase 2)     (Phase 3)
                              ┌────┴─────┐
                              │  YOU     │
                              └──────────┘
```

Every client is equal: the CLI, TUI, and Desktop app all speak the same
`herdr-protocol` over the same socket. The daemon is fully headless — closing any client
never touches a running agent.

## 2. Process & Threading Model

* **One daemon per user.** Single-instance via socket existence + ping.
* Tokio multi-threaded runtime. No blocking calls on the async executors:
  * PTY reads → `tokio::task::spawn_blocking` threads (one per agent).
  * Child waitpid → blocking thread per agent.
* `portable-pty` for PTY allocation. `TERM=xterm-256color` is forced so agents emit rich
  output.
* Per-agent **ring buffer** (256 KB, line-aligned) lets any client replay recent output
  without the agent ever re-running it.

## 3. Crate Graph

```
                ┌────────────────┐
                │ herdr-protocol │  types, codec, socket path, profiles
                └───────┬────────┘
        ┌───────────────┼───────────────────┐
        ▼               ▼                   ▼
┌──────────────┐ ┌─────────────┐  ┌──────────────────┐
│ herdr-daemon │ │ herdr-cli   │  │ herdr-tui (P2)   │
└──────────────┘ └─────────────┘  │ herdr-desktop(P3)│
                                  │ herdr-remote(P4) │
                                  │ herdr-plugin(P5) │
                                  └──────────────────┘
```

* **herdr-protocol** — `AgentId`, `AgentState`, `DaemonEvent`, `ClientCommand`,
  `Response`, `AgentInfo`, `AgentProfile` + NDJSON line codec + default socket path.
  Depends only on `serde`/`serde_json`. Clients compile against this alone — the daemon
  never leaks internals.
* **herdr-daemon** — bus, registry, PTY supervisor, state inference, IPC server.
* **herdr-cli** — `herdr` binary: `daemon`, `spawn`, `list`, `send`, `attach`, `logs`,
  `kill`, `events`, `ping`, `shutdown`.

## 4. The Event Bus

* `tokio::sync::broadcast<DaemonEvent>` with capacity 1024.
* Every IPC client subscribes on connect; `Senders` are internal-only.
* **Lag policy:** a client that stops reading gets `RecvError::Lagged(n)` — the connection
  is dropped with a warning. Clients are expected to reconnect and re-sync via
  `List` + `Logs`. This keeps the daemon immune to stuck UIs.

## 5. Wire Protocol — NDJSON over Unix Domain Socket

* Socket: `${XDG_RUNTIME_DIR:-/tmp}/herdr-$UID.sock`. Permissions `0600`.
* **Framing:** one JSON value per `\n`. Requests are `ClientCommand` (envelope: `{"id":..,"cmd":{...}}`);
  replies and events are `ServerFrame` (envelope: `{"id":..,"resp":{...}}` or `{"event":{...}}`).
  Envelopes carry a client-generated `id` so a client can multiplex replies and events on
  one connection (this is what makes `attach` + `send` share a socket).
* PTY payload is transported as **UTF-8-lossy strings**. Agents are text-first; binary
  media gets a dedicated event in Phase 3 (see `12-phase-3-desktop.md`) rather than
  overloading the output channel.
* Windows support: `#[cfg(windows)]` named-pipe path with identical framing — spec'd in
  `10-phase-1-daemon-ipc.md`, implemented later.

### 4.1 Example exchange

```
C: {"id":1,"cmd":{"Spawn":{"profile":"generic","cwd":"/tmp/x","command":"bash","args":[]}}}
S: {"id":1,"resp":{"Ok":{"agent_id":"a1b2..."}}}
S: {"event":{"AgentSpawned":{"info":{...}}}}
S: {"event":{"AgentOutput":{"agent_id":"a1b2...","payload":"\u001b[32m$\u001b[0m "}}}
C: {"id":2,"cmd":{"SendInput":{"agent_id":"a1b2...","text":"echo hi","raw":false}}}
S: {"id":2,"resp":"Ok"}
S: {"event":{"AgentOutput":{"agent_id":"a1b2...","payload":"hi\r\n"}}}
```

## 6. State Inference Engine

Agents speak terminal, not JSON. The daemon runs a per-agent **state machine** over the
ANSI-stripped output stream:

| State | Trigger |
| --- | --- |
| `Working` | Stream is hot (bytes flowing). Default while alive. |
| `Blocked` | Profile prompt regex matches the tail of the stripped stream. |
| `Idle` | No output for `profile.idle_timeout_secs` while not Blocked. |
| `Errored(reason)` | Exit code non-zero, or panic/error signature in stream tail. |
| `Exited(code)` | Clean exit (code 0). |

* Sliding window of the last **256 chars** (char boundaries respected) — bounded scan cost.
* Profiles (`claude-code`, `codex`, `generic`) tune prompt + error regexes and idle
  timeouts. ANSI stripping via a small hand-rolled VT parser (no heavyweight dep).
* The state machine is **pure** — `&mut self` + `process_chunk(&str) -> Option<AgentState>`
  + `check_idle(&mut self) -> Option<AgentState>` — so it is unit-testable with synthetic
  streams and reusable by the remote bridge (Phase 4) on the daemon side.

## 7. Directory Layout (repo)

```
herdr/
├── Cargo.toml               # workspace
├── README.md
├── specs/                   # this documentation
└── crates/
    ├── herdr-protocol/
    ├── herdr-daemon/
    ├── herdr-cli/
    ├── herdr-tui/           # Phase 2 stub
    ├── herdr-desktop/       # Phase 3 stub
    ├── herdr-remote/       # Phase 4 stub
    └── herdr-plugin/       # Phase 5 stub
```

## 8. Security Model (Phase 1)

* UDS at `0600` — same-user access only. No TLS needed locally.
* Commands are executed as the daemon's user; `Spawn.cwd` must exist and be a directory.
* Phase 5 adds a plugin permission model (see `14-phase-5-plugins-control.md`); Phase 4
  remote hosts authenticate via the user's SSH config/agent.

## 9. Decisions Log

| # | Decision | Rationale | Date |
| --- | --- | --- | --- |
| D1 | Rust + Tokio for the daemon | Memory safety, concurrency, low latency | 2026-09-10 |
| D2 | `portable-pty` over raw `openpty` | Cross-platform, maintained, PTY tricks agents into interactive mode | 2026-09-10 |
| D3 | UDS + NDJSON, not gRPC | Zero-dep debuggability (`socat`/`nc`), trivial clients, fast enough locally; can add gRPC later without breaking NDJSON | 2026-09-10 |
| D4 | Output as UTF-8-lossy strings | Agents are text-first; binary gets a dedicated event in Phase 3 | 2026-09-10 |
| D5 | Lagged broadcast clients are dropped | Daemon must never block on a stuck UI | 2026-10-10 |
| D6 | State inference is heuristic + profile-driven | Agents don't emit structured state; regexes are tuned per tool, fallback `generic` profile always works | 2026-09-10 |
| D7 | Ring buffer in daemon, not clients | Clients can replay after reconnect without agents re-emitting | 2026-09-10 |
| D8 | macOS/Linux first | UDS + PTY semantics are cleanest there; Windows named pipes are a `#[cfg]` layer later | 2026-09-10 |
| D9 | CLI subcommands for agent registration | Flexible ad-hoc mixed fleets; config-file autostart can layer on later | 2026-09-10 |
| D10 | Socket in `$XDG_RUNTIME_DIR` or `/tmp` | XDG-aware, per-user, predictable | 2026-09-10 |
