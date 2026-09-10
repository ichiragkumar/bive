//! The bridge: one SSH connection per remote host, multiplexing the runner
//! protocol over a single exec pipe.
//!
//! Lifecycle: [`Bridge::run`] drives the connect/re-sync/pump/reconnect loop on a
//! blocking task. On every successful connect the bridge re-syncs first — it
//! sends `List`, drives the read loop until the reply arrives (routing any
//! events it sees), and republishes the live remote agents retagged into the
//! sink — then hands the reader to [`Bridge::pump`] for the long-lived session.
//!
//! The write half of the tunnel sits behind a mutex shared between the resync/
//! pump and `request` callers; pending waiters fail fast with a clear error when
//! the tunnel drops. `RemoteRemove` calls [`Bridge::abort`] so the reconnect
//! loop exits and the ssh child is reaped instead of retrying forever.
//!
//! Blocking I/O throughout: the loop runs on `spawn_blocking`, matching the
//! daemon's PTY reader design.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use herdr_protocol::{ClientCommand, DaemonEvent, RemoteHost, RequestEnvelope, Response};

use crate::frame::{write_frame, Frame, FrameError, FrameReader};
use crate::runner_proto::retag_event;
use crate::Backoff;

/// Where retagged remote events go. The daemon passes a closure that publishes
/// into its bus; tests pass a channel. Kept as a callback so `herdr-remote`
/// never depends on `herdr-daemon` (the dependency runs the other way).
pub type EventSink = Arc<dyn Fn(DaemonEvent) + Send + Sync>;

/// Abstraction over the SSH tunnel so tests can use in-memory pipes and a
/// different backend (russh) can replace the default without touching the bridge.
pub trait SshTransport: Send + 'static {
    /// Open the tunnel: returns (write-half, read-half) carrying framed
    /// [`Frame`]s to/from the remote runner.
    fn open(
        &mut self,
        host: &RemoteHost,
    ) -> std::io::Result<(Box<dyn Write + Send>, Box<dyn Read + Send>)>;

    /// Force-terminate any tunnel process held by this transport. Called on
    /// [`Bridge::abort`]; must be safe to call when no tunnel is open.
    fn abort(&mut self);
}

/// Default transport: the user's `ssh` binary running `herdr-agent pipe` on the
/// remote host. Auth, keys, agent, known_hosts and ProxyJump behavior are exactly
/// the user's own `ssh` — nothing is re-implemented.
pub struct ProcessSshTransport {
    /// Extra args before the destination (e.g. `-p 2222`).
    ssh_args: Vec<String>,
    remote_runner: String,
    children: Arc<Mutex<Vec<Child>>>,
}

impl ProcessSshTransport {
    pub fn new(port: u16, user: Option<&str>) -> Self {
        let mut ssh_args = Vec::new();
        if port != 22 {
            ssh_args.push("-p".into());
            ssh_args.push(port.to_string());
        }
        if let Some(u) = user {
            ssh_args.push("-l".into());
            ssh_args.push(u.to_string());
        }
        // Sensible defaults for a long-lived multiplexed session: dead-peer
        // detection every ~45 s (15s × 3 misses), no interactive prompts (fail
        // fast so the backoff loop owns retries).
        ssh_args.extend([
            "-o".into(),
            "ServerAliveInterval=15".into(),
            "-o".into(),
            "ServerAliveCountMax=3".into(),
            "-o".into(),
            "BatchMode=yes".into(),
        ]);
        Self {
            ssh_args,
            remote_runner: "herdr-agent".into(),
            children: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Override the remote runner command (used when the runner is installed at
    /// a non-PATH location).
    pub fn with_remote_runner(mut self, path: impl Into<String>) -> Self {
        self.remote_runner = path.into();
        self
    }
}

impl Drop for ProcessSshTransport {
    fn drop(&mut self) {
        self.abort();
    }
}

impl SshTransport for ProcessSshTransport {
    fn open(
        &mut self,
        host: &RemoteHost,
    ) -> std::io::Result<(Box<dyn Write + Send>, Box<dyn Read + Send>)> {
        let mut cmd = Command::new("ssh");
        cmd.args(&self.ssh_args).arg(&host.ssh_target);
        cmd.arg("--").arg(&self.remote_runner).arg("pipe");
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = cmd.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "no stdout"))?;
        self.children.lock().unwrap().push(child);
        Ok((Box::new(stdin), Box::new(stdout)))
    }

    fn abort(&mut self) {
        for mut child in self.children.lock().unwrap().drain(..) {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[derive(Default)]
struct BridgeStatus {
    connected: AtomicBool,
    req_id: AtomicU64,
    /// Set by `abort`; the reconnect loop exits instead of retrying.
    cancelled: AtomicBool,
}

pub struct Bridge {
    pub host: RemoteHost,
    status: Arc<BridgeStatus>,
    transport: Mutex<Box<dyn SshTransport>>,
    /// Write half of the current tunnel; `None` while disconnected.
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    pending: Arc<Mutex<HashMap<u64, std::sync::mpsc::SyncSender<Response>>>>,
    connected_tx: tokio::sync::watch::Sender<bool>,
}

impl Bridge {
    pub fn new(host: RemoteHost, transport: Box<dyn SshTransport>) -> Self {
        let (connected_tx, _) = tokio::sync::watch::channel(false);
        Self {
            host,
            status: Arc::new(BridgeStatus::default()),
            transport: Mutex::new(transport),
            writer: Mutex::new(None),
            pending: Arc::new(Mutex::new(HashMap::new())),
            connected_tx,
        }
    }

    pub fn is_connected(&self) -> bool {
        self.status.connected.load(Ordering::SeqCst)
    }

    pub fn subscribe_connected(&self) -> tokio::sync::watch::Receiver<bool> {
        self.connected_tx.subscribe()
    }

    /// Stop the reconnect loop and kill the current ssh process (`RemoteRemove`).
    pub fn abort(&self) {
        self.status.cancelled.store(true, Ordering::SeqCst);
        self.fail_pending();
        self.transport.lock().unwrap().abort();
    }

    /// Run the connect/re-sync/pump/reconnect loop until aborted. Spawned by the
    /// daemon on a blocking task; events are retagged and handed to `sink`.
    pub fn run(self: Arc<Self>, sink: EventSink) -> tokio::task::JoinHandle<()> {
        tokio::task::spawn_blocking(move || {
            let mut backoff = Backoff::new();
            while !self.status.cancelled.load(Ordering::SeqCst) {
                let opened = self.transport.lock().unwrap().open(&self.host);
                match opened {
                    Ok((w, mut r)) => {
                        tracing::info!(host = %self.host.name, "bridge connected");
                        *self.writer.lock().unwrap() = Some(w);
                        self.status.connected.store(true, Ordering::SeqCst);
                        let _ = self.connected_tx.send(true);
                        backoff.reset();

                        let result = self
                            .resync(&mut *r, &sink)
                            .and_then(|_| self.pump(&mut *r, &sink));

                        self.status.connected.store(false, Ordering::SeqCst);
                        let _ = self.connected_tx.send(false);
                        *self.writer.lock().unwrap() = None;
                        if let Err(e) = result {
                            tracing::warn!(host = %self.host.name, error = %e, "bridge ended");
                        }
                    }
                    Err(e) => {
                        tracing::warn!(host = %self.host.name, error = %e, "ssh open failed");
                    }
                }
                if self.status.cancelled.load(Ordering::SeqCst) {
                    break;
                }
                // Disconnect handling: fail waiters fast, wait, retry.
                self.fail_pending();
                let wait = backoff.next_delay();
                std::thread::sleep(wait);
            }
            tracing::info!(host = %self.host.name, "bridge loop exiting");
        })
    }

    /// Re-establish local knowledge of remote agents after (re)connect: send
    /// `List`, drive the read loop until its reply arrives, and republish the
    /// live remote agents retagged into the sink. Events seen along the way are
    /// forwarded — nothing is lost during resync.
    fn resync(&self, r: &mut (dyn Read + Send), sink: &EventSink) -> Result<(), FrameError> {
        let resp = self.request_inner(r, ClientCommand::List, sink)?;
        if let Response::AgentList(agents) = resp {
            for mut info in agents {
                if matches!(
                    info.state,
                    herdr_protocol::AgentState::Exited(_) | herdr_protocol::AgentState::Errored(_)
                ) {
                    // Reconnect republishes live agents only.
                    continue;
                }
                info.state = herdr_protocol::AgentState::Starting;
                sink(retag_event(
                    DaemonEvent::AgentSpawned { info },
                    &self.host.name,
                ));
            }
        }
        Ok(())
    }

    /// Pump the read half to completion: replies to waiters, events retagged
    /// into the sink.
    fn pump(&self, r: &mut (dyn Read + Send), sink: &EventSink) -> Result<(), FrameError> {
        let mut reader = FrameReader::new();
        let mut buf = [0u8; 16384];
        loop {
            let n = r.read(&mut buf)?;
            if n == 0 {
                return Err(FrameError::Eof);
            }
            for frame in reader.feed(&buf[..n])? {
                match frame {
                    Frame::Reply(env) => {
                        if let Some(tx) = self.pending.lock().unwrap().remove(&env.id) {
                            let _ = tx.send(env.resp);
                        }
                    }
                    Frame::Event(ev) => {
                        sink(retag_event(ev, &self.host.name));
                    }
                    Frame::Control(_) => { /* runner never sends control frames */ }
                }
            }
        }
    }

    fn fail_pending(&self) {
        let mut pending = self.pending.lock().unwrap();
        for (_, tx) in pending.drain() {
            let _ = tx.send(Response::Err("remote host unreachable".into()));
        }
    }

    /// Multiplex one control request onto the tunnel. Fails fast when down.
    pub fn request(&self, cmd: ClientCommand) -> Result<Response, String> {
        if !self.is_connected() {
            return Err("remote host unreachable".into());
        }
        self.request_inner_no_read(cmd).map_err(|e| e.to_string())
    }

    /// Request path used by IPC handlers: writes the frame and waits for the
    /// reply to be routed by whichever read loop is active (resync or pump).
    fn request_inner_no_read(&self, cmd: ClientCommand) -> Result<Response, FrameError> {
        let id = self.status.req_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        self.pending.lock().unwrap().insert(id, tx);
        let frame = Frame::Control(RequestEnvelope { id, cmd });
        let write_result = {
            let mut guard = self.writer.lock().unwrap();
            match guard.as_mut() {
                Some(w) => write_frame(w, &frame),
                None => Err(FrameError::Eof),
            }
        };
        if let Err(e) = write_result {
            self.pending.lock().unwrap().remove(&id);
            return Err(e);
        }
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(resp) => Ok(resp),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(FrameError::Eof)
            }
        }
    }

    /// Resync's request: writes the frame and then drives the read loop itself
    /// until the reply arrives (the pump is not running yet at that point).
    fn request_inner(
        &self,
        r: &mut (dyn Read + Send),
        cmd: ClientCommand,
        sink: &EventSink,
    ) -> Result<Response, FrameError> {
        let id = self.status.req_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        self.pending.lock().unwrap().insert(id, tx);
        let frame = Frame::Control(RequestEnvelope { id, cmd });
        {
            let mut guard = self.writer.lock().unwrap();
            match guard.as_mut() {
                Some(w) => write_frame(w, &frame)?,
                None => return Err(FrameError::Eof),
            }
        }
        // Drive reads until our reply shows up; forward anything else (events
        // from agents that produced output while we were reconnecting).
        let mut reader = FrameReader::new();
        let mut buf = [0u8; 16384];
        loop {
            if let Ok(resp) = rx.try_recv() {
                return Ok(resp);
            }
            let n = r.read(&mut buf)?;
            if n == 0 {
                return Err(FrameError::Eof);
            }
            for frame in reader.feed(&buf[..n])? {
                match frame {
                    Frame::Reply(env) if env.id == id => return Ok(env.resp),
                    Frame::Reply(env) => {
                        if let Some(tx) = self.pending.lock().unwrap().remove(&env.id) {
                            let _ = tx.send(env.resp);
                        }
                    }
                    Frame::Event(ev) => sink(retag_event(ev, &self.host.name)),
                    Frame::Control(_) => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use herdr_protocol::{AgentInfo, AgentState, ResponseEnvelope};
    use std::io::Cursor;
    use std::sync::mpsc;

    /// In-memory transport: a fixed server-side byte script plays on each open.
    struct ScriptedTransport {
        script: Vec<u8>,
    }

    impl SshTransport for ScriptedTransport {
        fn open(
            &mut self,
            _host: &RemoteHost,
        ) -> std::io::Result<(Box<dyn Write + Send>, Box<dyn Read + Send>)> {
            Ok((
                Box::new(SinkWriter),
                Box::new(Cursor::new(self.script.clone())),
            ))
        }

        fn abort(&mut self) {}
    }

    /// Discard everything written (the tests drive reads directly).
    struct SinkWriter;

    impl Write for SinkWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn host() -> RemoteHost {
        RemoteHost {
            name: "dev".into(),
            ssh_target: "dev.example.com".into(),
            port: 22,
            user: None,
        }
    }

    fn agent_info(id: &str, state: AgentState) -> AgentInfo {
        AgentInfo {
            id: id.into(),
            profile: "generic".into(),
            command: "bash".into(),
            cwd: "/tmp".into(),
            state,
            started_at_unix_ms: 0,
            last_output_unix_ms: 0,
            host: None,
            parent: None,
        }
    }

    #[test]
    fn request_fails_fast_when_disconnected() {
        let b = Bridge::new(host(), Box::new(ScriptedTransport { script: vec![] }));
        assert!(matches!(
            b.request(ClientCommand::Ping),
            Err(e) if e.contains("unreachable")
        ));
    }

    #[test]
    fn resync_republishes_live_agents_retagged() {
        // Server script: a List reply (id 0 = the first resync request id) with
        // one live and one already-exited agent.
        let mut script = Vec::new();
        write_frame(
            &mut script,
            &Frame::Reply(ResponseEnvelope {
                id: 0,
                resp: Response::AgentList(vec![
                    agent_info("abc", AgentState::Working),
                    agent_info("dead", AgentState::Exited(0)),
                ]),
            }),
        )
        .unwrap();

        let b = Bridge::new(host(), Box::new(ScriptedTransport { script: vec![] }));
        b.status.connected.store(true, Ordering::SeqCst);
        *b.writer.lock().unwrap() = Some(Box::new(SinkWriter));

        let (tx, rx) = mpsc::channel::<DaemonEvent>();
        let sink: EventSink = Arc::new(move |ev| {
            let _ = tx.send(ev);
        });

        // resync drives its own reads: feed it the scripted reply.
        let mut r: Box<dyn Read + Send> = Box::new(Cursor::new(script));
        b.resync(&mut *r, &sink).expect("resync succeeds");

        let events: Vec<DaemonEvent> = rx.try_iter().collect();
        assert_eq!(events.len(), 1, "live agent republished, exited skipped");
        match &events[0] {
            DaemonEvent::AgentSpawned { info } => {
                assert_eq!(info.id, "dev:abc");
                assert_eq!(info.host.as_deref(), Some("dev"));
                assert_eq!(info.state, AgentState::Starting);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn pump_routes_replies_and_events() {
        let mut script = Vec::new();
        write_frame(
            &mut script,
            &Frame::Reply(ResponseEnvelope {
                id: 5,
                resp: Response::Ok,
            }),
        )
        .unwrap();
        write_frame(
            &mut script,
            &Frame::Event(DaemonEvent::StateChange {
                agent_id: "abc".into(),
                state: AgentState::Blocked,
            }),
        )
        .unwrap();

        let b = Bridge::new(host(), Box::new(ScriptedTransport { script: vec![] }));

        let (tx, rx) = mpsc::channel::<DaemonEvent>();
        let sink: EventSink = Arc::new(move |ev| {
            let _ = tx.send(ev);
        });

        let (wtx, wrx) = mpsc::sync_channel(1);
        b.pending.lock().unwrap().insert(5, wtx);

        let mut r: Box<dyn Read + Send> = Box::new(Cursor::new(script));
        let _ = b.pump(&mut *r, &sink);

        assert!(matches!(
            wrx.recv_timeout(Duration::from_secs(1)),
            Ok(Response::Ok)
        ));
        let ev = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        match ev {
            DaemonEvent::StateChange { agent_id, .. } => assert_eq!(agent_id, "dev:abc"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn abort_cancels_and_fails_waiters() {
        let b = Bridge::new(host(), Box::new(ScriptedTransport { script: vec![] }));
        b.status.connected.store(true, Ordering::SeqCst);
        let (wtx, wrx) = mpsc::sync_channel(1);
        b.pending.lock().unwrap().insert(9, wtx);
        b.abort();
        assert!(
            matches!(wrx.try_recv(), Ok(Response::Err(_))),
            "waiters get a clear error on abort"
        );
    }

    #[test]
    fn process_transport_builds_ssh_argv() {
        let t = ProcessSshTransport::new(2222, Some("ops"));
        assert!(t.ssh_args.contains(&"-p".to_string()));
        assert!(t.ssh_args.contains(&"2222".to_string()));
        assert!(t.ssh_args.contains(&"-l".to_string()));
        assert!(t.ssh_args.contains(&"ops".to_string()));
        assert!(t
            .ssh_args
            .windows(2)
            .any(|w| w[0] == "-o" && w[1].starts_with("ServerAlive")));
    }
}
