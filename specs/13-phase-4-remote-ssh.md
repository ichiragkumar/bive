# Phase 4 Spec — Remote Agents & SSH Bridge

> Status: **Spec — not yet implemented**. Depends on: Phase 1 daemon + protocol. Introduces `herdr-remote` crate.

## Goal

Remote agents become first-class fleet members: spawn on a remote box over SSH, stream
output and state back through the same event bus, control them (send/kill/logs) as if
local. One fleet view, one protocol — `AgentInfo` gains a `host` field.

## Design

```
laptop: herdr-daemon ── russh (SSH, multiplexed channels) ──▶ remote host
        │  local bus                                          └─ herdr-agent (headless runner)
        ▼                                                        └─ PTY ▶ claude / bash / ...
     TUI/Desktop/CLI   (see no difference; host: field distinguishes)
```

* **No external `ssh` binary dependency for the tunnel** — use `russh` (pure Rust) with
  the user's existing `~/.ssh` keys and agent. Config-file-based hosts are supported for
  batch auth (documented, opt-in).
* **herdr-agent runner:** a tiny static binary copied to the remote host on first use
  (self-update via same channel). It exposes the *same NDJSON protocol* over a remote UDS;
  the SSH bridge multiplexes: channel 1 = control (client commands → remote), channel 2 =
  event stream (remote events → local bus, retagged with `host`).
* **State inference stays local:** remote runner ships raw output + exit codes; the local
  daemon runs the same `StateMachine` with the same profiles. One code path, one behavior.
* **Reconnect:** SSH drops are expected. Bridge holds last-known state, retries with
  backoff, re-establishes channels, re-syncs remote agent list, and replays nothing
  (remote ring buffer is authoritative — `Logs` goes remote).

## Interface Contract (additive to Phase 1 protocol)

```rust
pub struct RemoteHost { pub name: String, pub ssh_target: String, pub port: u16, pub user: Option<String> }

// New ClientCommands:
RemoteAdd    { host: RemoteHost }          // store + connect
RemoteRemove { name: String }
RemoteList
// Spawn gains optional host: Option<String>  (None = local)
// AgentInfo gains host: Option<String>       (None = local)
// AgentOutput/StateChange/... carry agent_id already; remote agents get daemon-global ids.
```

CLI: `herdr remote add <name> <ssh-target>`, `herdr remote list`, `herdr remote remove <name>`,
`herdr spawn --host NAME -- bash`, `herdr list` (shows HOST column).

## Deliverables

1. `herdr-remote` crate: russh client, channel multiplexer, remote runner installer/updater.
2. `herdr-agent` minimal runner binary (reuses `herdr-daemon`'s pty/state/registry modules
   via a shared `herdr-core` extraction if needed).
3. Daemon: remote host registry, bridge lifecycle, id-space integration (global agent ids,
   `host` tagging), remote-aware `Spawn/Kill/SendInput/Logs`.
4. CLI: remote subcommands + `--host` on spawn; `list` gains HOST column.

## Acceptance Criteria

1. `herdr remote add dev box.example.com` ⇒ reachable; `herdr spawn --host dev -- bash`
   ⇒ output streams locally; state transitions identical to local agents.
2. `herdr attach`/`send` on remote agent: perceived latency < 200 ms on LAN.
3. Network drop mid-stream: TUI keeps running, shows `host unreachable` on that agent;
   bridge auto-reconnects and the agent list restores.
4. Daemon restart: bridge re-establishes; remote agents re-registered; local `Logs` of
   remote agents still work (remote ring authoritative).
5. Killing a remote agent from the local UI leaves no orphan processes on the remote.

## Test Strategy

* Unit: channel multiplexer framing, retagging logic, backoff state machine.
* Integration: container-based SSH fixture (or local `sshd`) in CI; spawn/bash/kill cycle
  over the bridge; reconnect forced by SIGSTOP-ing sshd.
* Manual: real VPS — full fleet mix (2 local + 1 remote Claude Code), one UI.

## Out of Scope

Windows remote hosts (runner is Unix-first), GPU/LLM inventory, remote daemon auto-start
via init systems (runner runs on demand).
