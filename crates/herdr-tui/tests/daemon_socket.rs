//! Integration test: boot the real daemon on a temp socket and assert the TUI
//! client flows (herdr-tui's actual `HerdrClient` + `App`) over the real socket.
//!
//! Covers the user-visible flows end to end, the same paths the terminal UI
//! drives: connect → initial sync (List + Events + log backfill) → spawn via
//! `AgentSpawned` event → `AgentOutput` streaming into the `LogBuffer` →
//! `SendInput` round-trip (the agent echoes our line back, proving the PTY
//! received it) → `Kill` → exit event → daemon shutdown ends the stream.
//!
//! Everything runs against the real daemon code path: PTY, ring buffer, state
//! machine, NDJSON IPC. No fakes.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use herdr_protocol::{AgentState, ClientCommand, DaemonEvent, Response};
use herdr_tui::app::ConnState;
use herdr_tui::client::HerdrClient;
use herdr_tui::App;

/// A daemon on a fresh temp-dir socket plus nothing else — clients dial it via
/// `HerdrClient::connect_to`, exactly like the TUI binary (with an explicit
/// path so tests never touch process-global env).
struct TestDaemon {
    rt: tokio::runtime::Runtime,
    /// JoinHandle of `herdr_daemon::run`; awaits the daemon's exit result.
    handle: tokio::task::JoinHandle<anyhow::Result<()>>,
    sock: PathBuf,
    #[allow(dead_code)] // keeps the socket dir alive for the daemon's lifetime
    tmp: tempfile::TempDir,
}

impl TestDaemon {
    fn boot() -> Self {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        Self::boot_in(tmp)
    }

    /// Boot a daemon on `tmp/test.sock` with a fresh runtime. Used for the
    /// initial boot and for reboots on the same path after a simulated crash.
    fn boot_in(tmp: tempfile::TempDir) -> Self {
        let sock = tmp.path().join("test.sock");
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("tokio runtime");
        let handle = {
            let sock = sock.clone();
            rt.spawn(async move { herdr_daemon::run(sock).await })
        };
        // Wait for the socket to appear (bind happens at daemon startup).
        let deadline = Instant::now() + Duration::from_secs(5);
        while !sock.exists() {
            assert!(Instant::now() < deadline, "daemon never bound the socket");
            std::thread::sleep(Duration::from_millis(10));
        }
        Self {
            rt,
            handle,
            sock,
            tmp,
        }
    }

    /// Simulate a crash mid-session: tear down the whole runtime without the
    /// graceful shutdown path, so client connections hit EOF and the stale
    /// socket file is left behind — exactly what a real `kill -9` leaves.
    /// Returns the tempdir so the test can reboot on the same socket path.
    fn crash(self) -> tempfile::TempDir {
        self.handle.abort();
        self.rt.shutdown_background();
        self.tmp
    }
}

/// Spawn an agent that echoes a marker, then echoes back one PTY input line
/// (`read` blocks on the real PTY stdin — proves input injection end to end).
fn spawn_echo_agent(client: &HerdrClient, marker: &str, cwd: &Path) -> String {
    let resp = client
        .request(ClientCommand::Spawn {
            profile: "bash".into(),
            cwd: cwd.display().to_string(),
            command: "bash".into(),
            args: vec![
                "-c".into(),
                format!("echo {marker}; read line; echo \"got: $line\""),
            ],
            host: None,
        })
        .expect("spawn request must succeed");
    match resp {
        Response::AgentCreated { agent_id } => agent_id,
        other => panic!("expected AgentCreated, got {other:?}"),
    }
}

/// Drain events until `pred` matches or `deadline` passes; returns the events
/// seen along the way (for `App` state building in callers).
fn wait_for_event(
    rx: &std::sync::mpsc::Receiver<DaemonEvent>,
    pred: impl Fn(&DaemonEvent) -> bool,
    deadline: Instant,
) -> Vec<DaemonEvent> {
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(ev) => {
                let hit = pred(&ev);
                seen.push(ev);
                if hit {
                    return seen;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    panic!("timed out waiting for matching event; saw {seen:?}");
}

#[test]
fn daemon_socket_tui_client_flows() {
    let daemon = TestDaemon::boot();
    let client = HerdrClient::connect_to(&daemon.sock).expect("client connects");
    let event_rx = client.start_reader();

    // -- initial sync (what the TUI main loop does on startup) ---------------
    let mut app = App::new();
    let resp = client
        .request(ClientCommand::List)
        .expect("List over the socket");
    app.apply_response(resp);
    assert!(app.agents.is_empty(), "fresh daemon has no agents");
    let resp = client.request(ClientCommand::Events).expect("Events ack");
    app.apply_response(resp);

    // -- spawn: AgentCreated reply, then AgentSpawned event ------------------
    let marker = format!("tui-it-{}", std::process::id());
    let reply_marker = format!("reply-{}", std::process::id());
    let cwd = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug");
    let agent_id = spawn_echo_agent(&client, &marker, &cwd);

    let events = wait_for_event(
        &event_rx,
        |ev| matches!(ev, DaemonEvent::AgentSpawned { info } if info.id == agent_id),
        Instant::now() + Duration::from_secs(5),
    );
    app.apply_event(events.last().unwrap().clone());
    assert!(
        app.agents.contains_key(&agent_id),
        "spawn event feeds the app"
    );
    assert_eq!(app.order, vec![agent_id.clone()]);

    // -- output streams into the TUI log buffer -------------------------------
    let events = wait_for_event(
        &event_rx,
        |ev| {
            matches!(ev, DaemonEvent::AgentOutput { agent_id: id, payload }
                if id == &agent_id && payload.contains(&marker))
        },
        Instant::now() + Duration::from_secs(5),
    );
    app.apply_event(events.last().unwrap().clone());
    let text = app.agents[&agent_id]
        .logs
        .lines
        .iter()
        .map(|l| l.text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains(&marker),
        "log buffer should contain the echoed marker, got: {text:?}"
    );

    // -- logs over the socket: ring buffer backfill lands in the selected view
    let resp = client
        .request(ClientCommand::Logs {
            agent_id: agent_id.clone(),
            max_bytes: 64 * 1024,
        })
        .expect("Logs over the socket");
    app.apply_response(resp); // routes to the selected agent (first in order)
    let text = app.agents[&agent_id]
        .logs
        .lines
        .iter()
        .map(|l| l.text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains(&marker), "backfilled logs contain marker");

    // -- send input round-trip: the agent's `read` consumes our line and its
    //    echo comes back as AgentOutput, proving the PTY received our text ---
    let resp = client
        .request(ClientCommand::SendInput {
            agent_id: agent_id.clone(),
            text: format!("echo {reply_marker}"),
            raw: false,
        })
        .expect("SendInput over the socket");
    assert!(matches!(resp, Response::Ok));
    let events = wait_for_event(
        &event_rx,
        |ev| {
            matches!(ev, DaemonEvent::AgentOutput { agent_id: id, payload }
                if id == &agent_id && payload.contains(&reply_marker))
        },
        Instant::now() + Duration::from_secs(5),
    );
    app.apply_event(events.last().unwrap().clone());

    // -- kill flow: Kill → AgentExited → TUI shows a terminal state -----------
    // (The exact exit code for SIGTERM is platform-dependent — portable-pty may
    // report 128+15 or the raw signal — so we assert "terminated, terminal
    // state reached", which is what the TUI actually renders.)
    let resp = client
        .request(ClientCommand::Kill {
            agent_id: agent_id.clone(),
        })
        .expect("Kill over the socket");
    assert!(matches!(resp, Response::Ok));
    let events = wait_for_event(
        &event_rx,
        |ev| matches!(ev, DaemonEvent::AgentExited { agent_id: id, .. } if id == &agent_id),
        Instant::now() + Duration::from_secs(5),
    );
    app.apply_event(events.last().unwrap().clone());
    assert!(
        matches!(
            app.agents[&agent_id].info.state,
            AgentState::Errored(_) | AgentState::Exited(_)
        ),
        "killed agent reaches a terminal state in the TUI"
    );

    // -- unknown agent is a clean daemon error, not a hang --------------------
    let err = client
        .request(ClientCommand::SendInput {
            agent_id: "ghost".into(),
            text: "x".into(),
            raw: false,
        })
        .expect_err("unknown agent must be an error");
    assert!(err.to_string().contains("unknown agent"));

    // -- shutdown: daemon exits cleanly and removes its socket ----------------
    let resp = client.request(ClientCommand::Shutdown).expect("Shutdown");
    assert!(matches!(resp, Response::Ok));
    let daemon_result = daemon
        .rt
        .block_on(async { daemon.handle.await.expect("join") });
    assert!(
        daemon_result.is_ok(),
        "daemon exits cleanly: {daemon_result:?}"
    );
    assert!(!daemon.sock.exists(), "daemon removes its socket file");
}

#[test]
fn client_reports_unreachable_cleanly() {
    let tmp = tempfile::TempDir::new().unwrap();
    let result = HerdrClient::connect_to(&tmp.path().join("nope.sock"));
    assert!(
        matches!(result, Err(ref e) if e.to_string().contains("unreachable")),
        "expected Unreachable, got an unexpected result"
    );
}

#[test]
fn connect_fails_cleanly_without_daemon() {
    // Default-path connect with no daemon anywhere: clean error, not a panic.
    // (Explicit-path unreachable coverage lives in
    // `client_reports_unreachable_cleanly`; this one exercises the env-derived
    // default path resolution end to end.)
    std::env::set_var("HERDR_SOCKDIR", "/tmp/herdr-tui-it-nonexistent");
    std::env::remove_var("XDG_RUNTIME_DIR");
    let result = HerdrClient::connect();
    assert!(result.is_err());
}

#[test]
fn tui_client_resync_restores_fleet_view_after_daemon_restart() {
    // Kill the daemon mid-session (crash, not graceful shutdown), reboot it on
    // the same socket path, and assert the TUI client's resync restores the
    // fleet view: no ghosts from the dead session, new fleet visible.
    // Mirrors the reconnect path in `herdr-tui`'s main loop step by step
    // (Reconnecting banner → `reconnect()` → `resync()` → List + Events).

    // -- live session on daemon A -------------------------------------------
    let daemon = TestDaemon::boot();
    let sock = daemon.sock.clone();
    let mut client = HerdrClient::connect_to(&sock).expect("client connects");
    let event_rx = client.start_reader();

    // Initial sync, exactly like the TUI main loop on startup.
    let mut app = App::new();
    let resp = client.request(ClientCommand::List).expect("List");
    app.apply_response(resp);
    assert!(app.agents.is_empty(), "fresh daemon has no agents");
    let resp = client.request(ClientCommand::Events).expect("Events ack");
    app.apply_response(resp);

    // One live agent in the fleet view.
    let marker_a = format!("reconnect-a-{}", std::process::id());
    let cwd = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug");
    let agent_a = spawn_echo_agent(&client, &marker_a, &cwd);
    let events = wait_for_event(
        &event_rx,
        |ev| matches!(ev, DaemonEvent::AgentSpawned { info } if info.id == agent_a),
        Instant::now() + Duration::from_secs(5),
    );
    app.apply_event(events.last().unwrap().clone());
    assert_eq!(app.order, vec![agent_a.clone()]);
    assert!(app.agents.contains_key(&agent_a));

    // -- crash daemon A mid-session (no graceful shutdown) -------------------
    let tmp = daemon.crash();

    // The TUI client must notice the dead socket (reader thread hits EOF).
    let deadline = Instant::now() + Duration::from_secs(5);
    while client.is_connected() {
        assert!(
            Instant::now() < deadline,
            "client never noticed the daemon died"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    // The main loop raises the reconnecting banner while disconnected.
    app.connection_state = ConnState::Reconnecting;
    assert_eq!(app.connection_state, ConnState::Reconnecting);

    // -- reboot daemon B on the same socket path ------------------------------
    // A crash leaves a stale socket file; the restart replaces it, mirroring
    // what `herdr daemon` does via `prepare_socket_path`.
    assert!(sock.exists(), "crash leaves a stale socket file behind");
    herdr_daemon::ipc::prepare_socket_path(&sock).expect("replace stale socket");
    assert!(!sock.exists(), "stale socket removed before reboot");
    let daemon = TestDaemon::boot_in(tmp);

    // Reconnect (retried; the rebooted daemon binds fast).
    let deadline = Instant::now() + Duration::from_secs(10);
    while !client.is_connected() {
        assert!(
            Instant::now() < deadline,
            "client never reconnected to the rebooted daemon"
        );
        client.reconnect();
    }

    // -- resync, exactly like the TUI main loop after reconnect ---------------
    app.connection_state = ConnState::Connected;
    app.resync();
    let resp = client
        .request(ClientCommand::List)
        .expect("List after restart");
    app.apply_response(resp);
    let resp = client.request(ClientCommand::Events).expect("re-subscribe");
    app.apply_response(resp);

    // Agents died with daemon A: resync must leave no ghosts behind.
    assert!(
        app.agents.is_empty(),
        "stale agents cleared by resync, got: {:?}",
        app.order
    );
    assert!(app.order.is_empty());
    assert_eq!(app.selected_id(), None);

    // The rebooted daemon serves a new fleet, visible after resync.
    let marker_b = format!("reconnect-b-{}", std::process::id());
    let agent_b = spawn_echo_agent(&client, &marker_b, &cwd);
    let events = wait_for_event(
        &event_rx,
        |ev| matches!(ev, DaemonEvent::AgentSpawned { info } if info.id == agent_b),
        Instant::now() + Duration::from_secs(5),
    );
    app.apply_event(events.last().unwrap().clone());
    let resp = client.request(ClientCommand::List).expect("List new fleet");
    app.apply_response(resp);
    assert_eq!(
        app.order,
        vec![agent_b.clone()],
        "resync restores the fleet view from List"
    );
    assert!(app.agents.contains_key(&agent_b));
    assert_eq!(app.selected_id(), Some(agent_b.as_str()));

    // -- clean shutdown of the rebooted daemon ---------------------------------
    let resp = client.request(ClientCommand::Shutdown).expect("Shutdown");
    assert!(matches!(resp, Response::Ok));
    let daemon_result = daemon
        .rt
        .block_on(async { daemon.handle.await.expect("join") });
    assert!(
        daemon_result.is_ok(),
        "rebooted daemon exits cleanly: {daemon_result:?}"
    );
    assert!(!daemon.sock.exists(), "daemon removes its socket file");
}
