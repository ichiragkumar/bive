# Phase 1 Spec — Core Daemon & IPC

> Status: **Implemented in this build**. Depends on: `01-architecture.md`.

## Goal

A headless, persistent daemon that spawns agents under pseudo-terminals, infers their
state, broadcasts events at millisecond latency, and exposes control to any client over a
Unix domain socket. Plus the `herdr` CLI as the first client.

## Deliverables

1. `herdr-protocol` crate — wire types, NDJSON codec, socket path resolution, agent profiles.
2. `herdr-daemon` crate — bus, registry, PTY supervisor, state inference, IPC server.
3. `herdr-cli` crate — the `herdr` binary.

## Module Contracts

### herdr-protocol

```rust
pub type AgentId = String; // 12-char hex, daemon-assigned

pub enum AgentState { Starting, Working, Idle, Blocked, Errored(String), Exited(i32) }

pub struct AgentInfo {
    pub id: AgentId,
    pub profile: String,
    pub command: String,       // full argv string, for display
    pub cwd: String,
    pub state: AgentState,
    pub started_at_unix_ms: u64,
    pub last_output_unix_ms: u64,
}

pub enum DaemonEvent {
    AgentSpawned { info: AgentInfo },
    AgentOutput  { agent_id: AgentId, payload: String }, // UTF-8-lossy, ANSI intact
    StateChange  { agent_id: AgentId, state: AgentState },
    AgentExited  { agent_id: AgentId, code: i32 },
    AgentRemoved { agent_id: AgentId },
}

pub enum ClientCommand {
    Ping, List, Events, Shutdown,
    Spawn     { profile: String, cwd: String, command: String, args: Vec<String> },
    Kill      { agent_id: AgentId },
    SendInput { agent_id: AgentId, text: String, raw: bool },
    Attach    { agent_id: AgentId },              // stream one agent (output+state+exit)
    Logs      { agent_id: AgentId, max_bytes: u32 },
}

pub enum Response {
    Ok,
    Err(String),
    AgentList(Vec<AgentInfo>),
    LogChunk { payload: String },
    Pong { version: String, uptime_ms: u64, agents: usize },
}
```

**Codec rules**
* Requests: `{"id": u64, "cmd": ClientCommand}` — one per line.
* Replies:  `{"id": u64, "resp": Response}` — exactly one per request.
* Events:   `{"event": DaemonEvent}` — asynchronous, interleaved with replies.
* Line limit: 1 MiB. Oversized or malformed line ⇒ one `Err` reply; connection stays open.
* `Attach`/`Events` mark the connection as *streaming*: subsequent `SendInput`/`Kill`/`Ping`
  replies share the socket with the event stream (envelope `id` disambiguates).
* `Logs` and `List` replies go through the same envelope as everything else.

### herdr-daemon

| Module | Responsibility |
| --- | --- |
| `bus.rs` | `tokio::sync::broadcast<DaemonEvent>` (cap 1024). Lagged subscribers are dropped with a `warn!`. |
| `registry.rs` | `RwLock<HashMap<AgentId, AgentEntry>>`. `AgentEntry` = `AgentInfo` + PTY writer + child kill handle + 256 KB line-aligned ring buffer. |
| `pty.rs` | Spawn via `portable-pty` (`TERM=xterm-256color`, 24×80); blocking reader on `spawn_blocking` feeding ring + state machine + bus; `write_all` for input; resize API; kill via child handle; blocking waitpid thread emits `AgentExited`. |
| `state.rs` | Pure `StateMachine`: ANSI strip → sliding window (256 chars, char-boundary safe) → profile regexes. Emits transitions only. Idle detection via `check_idle(now)`. |
| `profiles.rs` | `AgentProfile { name, prompt_regexes, error_regexes, idle_timeout_secs }`. Built-ins: `claude-code`, `codex`, `generic` (default, matches any `?`, `>`, `(y/n)` tail). |
| `ipc.rs` | Bind UDS (stale socket recovered), accept loop, per-connection task: read command envelopes → dispatch → write reply envelopes + subscribed events, interleaved. Streaming clients get events; one-shot clients get only their reply. |
| `supervisor.rs` | Owns registry + bus; implements `handle_command`; spawns PTY tasks; graceful `Shutdown` (kill all agents, remove socket). |
| `main.rs` | Runtime init, socket path resolution, stale-socket recovery, `tracing` to stderr, SIGINT/SIGTERM → graceful shutdown. |

**Exit semantics**
* `Exited(0)` → agent stays listed, state `Exited(0)`, removed from registry after 5 min.
* `Errored(reason)` → non-zero exit or detected panic signature; stays listed 15 min.
* `Kill` / `Shutdown` → SIGKILL via portable-pty child handle.

## CLI Contract (`herdr`)

```
herdr daemon                          run the daemon in the foreground
herdr ping                            liveness check
herdr spawn [--profile P] [--cwd D] -- CMD [ARGS...]
herdr list                            table: ID PROFILE STATE COMMAND AGE
herdr send <id> <text> [--raw]        inject input (adds \n unless --raw)
herdr attach <id>                     live output; stdin forwarded; Ctrl-C detaches only
herdr logs <id> [--bytes N]           replay ring buffer (default 4096)
herdr kill <id>
herdr events                          raw NDJSON event tap
herdr shutdown                        stop daemon + all agents
herdr replay [--profile P] [--chunk-ms MS] [--idle N] [FILE]
                                      offline state-timeline inference (see below)
herdr install-service [--backend B] [--dry-run]
                                      register daemon as per-user login service
                                      (launchd on macOS, systemd user unit on Linux)
herdr uninstall-service [--backend B] [--dry-run]
                                      remove the service registration
```

### `herdr replay` — offline profile tuning

Runs the *same* `StateMachine` + `AnsiStripper` the daemon uses, over a captured
stream (raw PTY bytes on stdin or `FILE`), and prints the inferred state timeline
with per-transition offsets. Profile selection matches `spawn` (`--profile claude-code`,
etc.). `--chunk-ms` controls the chunking granularity (defaults to 50 ms, matching the
daemon's read cadence); `--idle` interpolates idle checks between chunks so gaps
longer than the profile's idle timeout show up in the timeline. Captures come from
`herdr events > capture.ndjson` (`AgentOutput` payloads concatenated) or any raw
PTY recording:

```bash
herdr events | jq -rj '.event.AgentOutput.payload' > session.raw
herdr replay --profile claude-code session.raw
# 0.000s  starting
# 0.050s  working
# 12.400s blocked    ← prompt regex matched here
```

### `herdr install-service` — login persistence + crash restart

Per-user only (no root): a launchd agent (`~/Library/LaunchAgents/…plist`,
`RunAtLoad` + `KeepAlive {SuccessfulExit: false}`) or a systemd user unit
(`Restart=on-failure`, `WantedBy=default.target`). The plist KeepAlive dict encodes
the crash-only contract: respawn after SIGKILL, stay down after `herdr shutdown`'s
clean exit. To make that safe, `herdr daemon` exits **0** (not an error) when a live
daemon already owns the socket, so launchd's login start and a manual start settle
instead of looping. The unit references the absolute binary path of the `herdr` that
ran the install; uninstall stops/disables best-effort and removes the file.

Errors are human-readable; missing daemon ⇒ `error: daemon not reachable at <path> (start it with 'herdr daemon')`, exit 2. Unknown agent ⇒ exit 3.

## Acceptance Criteria

1. `herdr daemon` binds `${XDG_RUNTIME_DIR:-/tmp}/herdr-$UID.sock` (0600); `herdr ping` ⇒ Pong with version + uptime + agent count.
2. `herdr spawn -- bash` starts; `herdr send <id> 'echo hi'` ⇒ `hi` visible in `herdr attach` and `herdr logs`.
3. `herdr list` shows accurate transitions: Working → Idle after idle timeout; prompt at stream tail ⇒ Blocked.
4. `herdr kill <id>` terminates the process; `AgentExited` fires; `herdr shutdown` kills all agents and removes the socket.
5. Daemon restart after a crash recovers from a stale socket file.
6. `cargo build`, `cargo test`, `cargo clippy` clean across the whole workspace (stubs included).

## Test Strategy

* **Unit:** state machine transitions (synthetic ANSI streams: prompt tails, panic signatures, idle timeouts), ANSI stripper (colors, cursor moves, unterminated escapes), protocol round-trips of every variant, ring-buffer cap + char-boundary handling, profile regex behavior, socket-path resolution.
* **Integration (in-sandbox):** run daemon on a test socket; spawn real `bash`; drive `send`/`logs`/`list`/`kill` through the codec end-to-end; assert event ordering.
* **Manual:** `herdr attach` against a colorful CLI (e.g. `htop`-like output) — no raw escapes in logs pane, ANSI preserved for renderers.

## Out of Scope (deferred)

Windows named pipes (`#[cfg(windows)]`), TUI, remote agents, plugins, pause/resume, file-touch tracking, binary media events.
