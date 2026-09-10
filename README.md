# Herdr

**Unified agent runtime & control surface** — a persistent background daemon that runs
coding agents (Claude Code, Codex, shell tools, anything with a CLI) under pseudo-terminals,
infers their state, and streams events to any number of clients over a local Unix socket.

```
claude ──PTY──▶ ┌──────────────────┐          ┌── herdr CLI ──┐
codex  ──PTY──▶ │   herdr daemon   │ ──NDJSON─┼── herdr-tui ──┤ (Phase 2)
bash   ──PTY──▶ │ (this workspace) │  over UDS└ herdr-desktop ┘ (Phase 3)
                └──────────────────┘
```

## Status: Phases 1–2 complete ✅

- [x] **Phase 1** — Core daemon, PTY supervisor, state inference, IPC, CLI (`specs/10-phase-1-daemon-ipc.md`)
- [x] **Phase 2** — Ratatui TUI (`specs/11-phase-2-tui.md`) — implemented: fleet list, live ANSI log pane, state summary bar
- [ ] **Phase 3** — Tauri desktop app (`specs/12-phase-3-desktop.md`) — stub present
- [ ] **Phase 4** — Remote agents via SSH bridge (`specs/13-phase-4-remote-ssh.md`) — stub present
- [ ] **Phase 5** — Plugins, sub-agents, intervention (`specs/14-phase-5-plugins-control.md`) — stub present

All product and architecture decisions live in `specs/` — start at `specs/00-prd.md`.

## Quickstart

```bash
# 1. Build
cargo build --release

# 2. Start the daemon (foreground, or background it with tmux/nohup)
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
| `herdr spawn [--profile P] [--cwd D] -- CMD [ARGS…]` | Spawn an agent; prints its id |
| `herdr list` | Table of agents with inferred states |
| `herdr send <id> <text> [--raw]` | Inject input (appends `\n` unless `--raw`) |
| `herdr attach <id>` | Live output; forward stdin; Ctrl-C detaches (never kills) |
| `herdr logs <id> [--bytes N]` | Replay the daemon's 256 KB ring buffer |
| `herdr kill <id>` | Kill one agent |
| `herdr events` | Raw NDJSON event tap |
| `herdr shutdown` | Stop daemon and all agents |
| `herdr replay [FILE]` | Offline state-timeline inference over a capture (profile tuning; no daemon needed) |
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

## Workspace layout

```
crates/
├── herdr-protocol/   # wire types, NDJSON codec, profiles, socket path (no heavy deps)
├── herdr-daemon/     # bus, registry, PTY supervisor, state machine, IPC server
├── herdr-cli/        # the `herdr` binary
├── herdr-tui/        # Ratatui TUI — fleet list, ANSI log pane, summary bar
├── herdr-desktop/    # Phase 3 stub
├── herdr-remote/     # Phase 4 stub
└── herdr-plugin/     # Phase 5 stub
```

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
cargo test         # 103 unit tests (daemon 31, protocol 20, TUI 45, cli/replay 7)
cargo clippy       # clean
cargo fmt          # rustfmt (CI enforces `--check`)
```

CI (`.github/workflows/ci.yml`) runs `cargo fmt --check`, `cargo clippy -D warnings`,
and `cargo test --workspace` on both **ubuntu-latest** and **macos-latest** on every
push to `main` and every pull request.

The daemon logs to stderr; set `RUST_LOG=herdr_daemon=debug` for verbosity.

## Design in one paragraph

One daemon per user owns every agent PTY. Blocking PTY reads run on
`tokio::task::spawn_blocking` threads and are bridged into a `tokio::sync::broadcast`
event bus; every client connection subscribes and gets NDJSON-framed events
(`{"event":…}`) interleaved with request replies (`{"id":…,"resp":…}`). The daemon never
blocks on a client (lagged subscribers are dropped), never blocks an agent (clients can
die freely), and never trusts the terminal (output is ANSI-stripped before state
inference). Full rationale in `specs/01-architecture.md` §9 decisions log.
