# Herdr

**Unified agent runtime & control surface** — a persistent background daemon that runs
coding agents (Claude Code, Codex, shell tools, anything with a CLI) under pseudo-terminals,
infers their state, and streams events to any number of clients over a local Unix socket.

```
claude ──PTY──▶ ┌──────────────────┐          ┌── herdr CLI ──┐
codex  ──PTY──▶ │   herdr daemon   │ ──NDJSON─┼── herdr-tui ──┤
bash   ──PTY──▶ │ (this workspace) │  over UDS└ herdr-desktop ┘
                └───────┬──────────┘
                        │ SSH (one multiplexed exec channel per host)
                        ▼
                ┌──────────────────┐
                │   herdr-agent    │ ──PTY──▶ claude / bash / …
                │ (remote runner)  │
                └──────────────────┘
```

## Status: Phases 1–4 complete ✅

- [x] **Phase 1** — Core daemon, PTY supervisor, state inference, IPC, CLI (`specs/10-phase-1-daemon-ipc.md`)
- [x] **Phase 2** — Ratatui TUI (`specs/11-phase-2-tui.md`) — fleet list, live ANSI log pane, state summary bar
- [x] **Phase 3** — Tauri desktop app (`specs/12-phase-3-desktop.md`) — event-driven webview UI, tray aggregate, notifications; build with `--features tauri`
- [x] **Phase 4** — Remote agents via SSH bridge (`specs/13-phase-4-remote-ssh.md`) — `herdr-remote` + `herdr-agent` (see below)
- [ ] **Phase 5** — Plugins, sub-agents, intervention (`specs/14-phase-5-plugins-control.md`) — stub present

All product and architecture decisions live in `specs/` — start at `specs/00-prd.md`.

## Quickstart

```bash
# 1. Build
cargo build --release

# 2. Start the daemon — either register it as a login service (recommended):
herdr install-service          # launchd user agent on macOS, systemd user unit on Linux
#    ... or run it in the foreground / under your own supervisor:
./target/release/herdr daemon

# 3. In another shell: spawn an agent under a PTY
herdr spawn --profile bash --cwd /tmp -- bash --norc -i
# → prints the agent id, e.g. 782e4274a90a

# 4. Drive it
herdr send 782e4274a90a 'echo hello'
herdr logs 782e4274a90a
herdr list            # live state: ● working / ◌ idle / ■ blocked / ✖ errored
herdr attach 782e4274a90a   # live tail + stdin forwarding (Ctrl-C detaches only)

# 5. Or drive everything from the TUI
herdr-tui                  # fleet list, live ANSI log pane, summary bar
                           # keys: j/k select · s send · K kill · f follow · q quit

# Remote hosts work the same way (see "Remote agents" below):
herdr remote add dev box.example.com && herdr spawn --host dev -- bash --norc -i

# 6. Tear down
herdr kill 782e4274a90a     # one agent
herdr shutdown              # whole daemon + all agents
```

The socket lives at `${XDG_RUNTIME_DIR:-/tmp}/herdr-$UID.sock` (0600, per-user).
A daemon that crashed leaves a stale socket — the next `herdr daemon` detects and
replaces it; a **live** daemon refuses a second instance.

## CLI reference

| Command | Effect |
| --- | --- |
| `herdr daemon` | Run the daemon in the foreground |
| `herdr ping` | Liveness + version + uptime + agent count |
| `herdr spawn [--profile P] [--cwd D] [--host NAME] -- CMD [ARGS…]` | Spawn an agent (local, or on registered host `NAME`); prints its id |
| `herdr list` | Table of agents with inferred states and a HOST column (`local` or host name) |
| `herdr send <id> <text> [--raw]` | Inject input (appends `\n` unless `--raw`) |
| `herdr attach <id>` | Live output; forward stdin; Ctrl-C detaches (never kills) |
| `herdr logs <id> [--bytes N]` | Replay the daemon's 256 KB ring buffer |
| `herdr kill <id>` | Kill one agent |
| `herdr events` | Raw NDJSON event tap |
| `herdr shutdown` | Stop daemon and all agents |
| `herdr remote add <name> <ssh-target> [--port P] [--user U]` | Register a remote host and start its bridge (see below) |
| `herdr remote list` | Registered hosts with bridge state (`● up` / `○ down`) |
| `herdr remote remove <name>` | Disconnect and forget a host; its agents leave the fleet |
| `herdr replay [FILE]` | Offline state-timeline inference over a capture (profile tuning; no daemon needed) |
| `herdr install-service` | Register the daemon as a per-user login service (see below) |
| `herdr uninstall-service` | Remove the service registration |
| `herdr-tui` | Terminal client: fleet overview + live logs (see TUI section) |

Exit codes: `2` daemon unreachable, `3` unknown agent, `1` other errors.

## Profiles & state inference

Agents speak terminal, not JSON. The daemon strips ANSI, keeps a 256-char sliding
window of the stripped stream, and classifies:

| State | Trigger |
| --- | --- |
| `● working` | Output stream is hot |
| `■ blocked` | Profile prompt regex matches the tail (`?`, `(y/n)`, `❯`, …) |
| `◌ idle` | No output for the profile's idle timeout |
| `✖ errored` | Panic/error signature, or non-zero exit |
| `· exited(n)` | Clean exit |

Built-in profiles: `generic` (default), `claude-code`, `codex`, `bash`. Profiles are data
(`crates/herdr-protocol/src/profile.rs`) — adding a tool is a regex list, not code.

## Remote agents (Phase 4)

Agents on remote machines are first-class fleet members: spawned over SSH, streamed
back through the same event bus, and controlled (`send`/`logs`/`kill`/`attach`) exactly
like local ones. Clients never special-case them — `herdr list` shows a HOST column,
and remote agent ids are tagged `host:agentid` (e.g. `dev:782e4274a90a`).

```bash
# one-time: register a host (plain ssh destination — your ~/.ssh config, agent, keys all apply)
herdr remote add dev box.example.com
herdr remote add build user@10.0.0.7 --port 2222

herdr remote list        # NAME  SSH TARGET  PORT  STATE (● up / ○ down)  USER

# spawn remotely — first use auto-installs the runner (see below)
herdr spawn --host dev --profile claude-code -- claude

# drive it like any local agent
herdr list                              # HOST column shows `dev`
herdr logs dev:782e4274a90a
herdr send dev:782e4274a90a 'continue'
herdr kill dev:782e4274a90a             # no orphan processes left on the remote

herdr remote remove dev                 # bridge torn down; the host's agents leave the fleet
```

**Architecture.** One `ssh` process per registered host (the user's `ssh` binary —
auth agent, known_hosts and ProxyJump come for free; a `russh` backend can replace
the transport behind a trait without touching anything else). All traffic is
multiplexed over that single exec channel with an internal frame layer:
control frames (commands → runner), reply frames, and event frames
(runner events → local bus, re-stamped with `host:agentid` global ids).

**`herdr-agent` runner.** A tiny headless binary on the remote host that runs agent
PTYs and speaks the herdr frame protocol. On first `spawn --host`, herdr probes
`herdr-agent --version` over ssh; if missing or stale it `scp`s the local binary to
`~/.herdr/bin/herdr-agent` and uses it. If a runner daemon is already live on the
remote host it is reused, so remote agents survive bridge and laptop-daemon restarts.

**Resilience.** SSH drops are expected: the bridge retries with exponential backoff,
re-syncs the remote agent list on reconnect, and republishes live agents into the
fleet. State inference stays **local** (the runner ships raw output; the local daemon
classifies with the same profiles), so remote and local agents behave identically.
Remote `logs` are served from the remote runner's ring buffer, which stays
authoritative across reconnects.

## Workspace layout

```
crates/
├── herdr-protocol/   # wire types, NDJSON codec, profiles, socket path (no heavy deps)
├── herdr-daemon/     # bus, registry, PTY supervisor, state machine, IPC server, remote routing
├── herdr-cli/        # the `herdr` binary
├── herdr-tui/        # Ratatui TUI — fleet list, ANSI log pane, summary bar
├── herdr-desktop/    # Tauri desktop app (headless lib + `--features tauri` shell)
├── herdr-remote/     # SSH bridge: framing, transport, reconnect, runner installer
├── herdr-agent/      # headless remote runner installed on remote hosts
└── herdr-plugin/     # Phase 5 stub
```

## Service registration

```bash
herdr install-service      # writes the definition + enables/starts it
herdr install-service --dry-run   # preview the plist/unit and commands
herdr uninstall-service    # stops, disables, removes
```

| Backend | File | Behavior |
| --- | --- | --- |
| launchd (macOS) | `~/Library/LaunchAgents/io.github.herdr.daemon.plist` | `RunAtLoad` (starts on login) + `KeepAlive {SuccessfulExit: false}` (respawns on crash, **stays down** after `herdr shutdown`) |
| systemd (Linux) | `~/.config/systemd/user/herdr-daemon.service` | `WantedBy=default.target` + `Restart=on-failure` with backoff |

Both are strictly per-user — no root anywhere. The registered binary is the absolute
path of the `herdr` executable that ran `install-service`; re-run install after
moving or rebuilding to a new location. Service logs land in
`$TMPDIR/herdr-daemon.log`. If a live daemon already owns the socket, `herdr daemon`
(exited via the service at login) prints `daemon already running … — nothing to do`
and exits 0, so the two start paths never fight.

## TUI keys

| Key | Action |
| --- | --- |
| `j` / `k`, `↑` / `↓` | Select agent (log pane backfills from the daemon's ring buffer) |
| `g` / `G` | Jump to top / follow tail |
| `f` | Toggle follow-tail |
| `s` | Send input to the selected agent (Enter submits, Esc cancels) |
| `K` then `y` | Kill the selected agent (confirm first) |
| `q` / Ctrl-C | Quit the TUI — agents keep running |

## Development

```bash
cargo build        # whole workspace incl. stubs
cargo test         # 167 tests incl. a daemon-socket integration suite driving the TUI client
cargo clippy       # clean
cargo fmt          # rustfmt (CI enforces `--check`)
```

CI (`.github/workflows/ci.yml`) runs `cargo fmt --check`, `cargo clippy -D warnings`,
and `cargo test --workspace` on both **ubuntu-latest** and **macos-latest** on every
push to `master`/`main` and every pull request.

The daemon logs to stderr; set `RUST_LOG=herdr_daemon=debug` for verbosity.

## Design in one paragraph

One daemon per user owns every agent PTY. Blocking PTY reads run on
`tokio::task::spawn_blocking` threads and are bridged into a `tokio::sync::broadcast`
event bus; every client connection subscribes and gets NDJSON-framed events
(`{"event":…}`) interleaved with request replies (`{"id":…,"resp":…}`). The daemon never
blocks on a client (lagged subscribers are dropped), never blocks an agent (clients can
die freely), and never trusts the terminal (output is ANSI-stripped before state
inference). Full rationale in `specs/01-architecture.md` §9 decisions log.
