//! Socket bridge: the desktop app's single daemon connection.
//!
//! Mirrors the TUI client design (one UDS, split read/write, reader thread
//! demultiplexing `{"id":…,"resp":…}` replies and `{"event":…}` frames) but is
//! Tauri-free: UI layers register interest and receive clones of every frame.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use herdr_protocol::{codec::Encoder, ClientCommand, DaemonEvent, RequestEnvelope, Response};

const REQUEST_TIMEOUT_MS: u64 = 5000;
const READ_SLICE_MS: u64 = 200;

/// What UI components can receive. Replies are delivered to the waiting caller
/// directly; broadcasts are events from the daemon's bus.
#[derive(Debug, Clone)]
pub enum Frame {
    Event(DaemonEvent),
}

#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("daemon unreachable at {0}")]
    Unreachable(String),
    #[error("request timed out")]
    Timeout,
    #[error("connection closed")]
    Closed,
    #[error("daemon error: {0}")]
    Daemon(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol error: {0}")]
    Protocol(String),
}

struct Inner {
    writer: Mutex<Option<UnixStream>>,
    connected: AtomicBool,
    req_id: AtomicU64,
    pending: Mutex<HashMap<u64, mpsc::SyncSender<Response>>>,
    listeners: Mutex<Vec<mpsc::Sender<Frame>>>,
}

/// Cloneable handle to the daemon connection. Drop the last handle to stop the
/// reader thread.
#[derive(Clone)]
pub struct Bridge {
    inner: Arc<Inner>,
}

impl Bridge {
    /// Connect to the daemon's default socket.
    pub fn connect() -> Result<Self, BridgeError> {
        let sock = herdr_protocol::default_socket_path();
        let stream = Self::dial(&sock)?;
        Ok(Self {
            inner: Arc::new(Inner {
                writer: Mutex::new(Some(stream)),
                connected: AtomicBool::new(true),
                req_id: AtomicU64::new(1),
                pending: Mutex::new(HashMap::new()),
                listeners: Mutex::new(Vec::new()),
            }),
        })
    }

    fn dial(sock: &std::path::Path) -> Result<UnixStream, BridgeError> {
        let stream = UnixStream::connect(sock)
            .map_err(|_| BridgeError::Unreachable(sock.display().to_string()))?;
        let _ = stream.set_read_timeout(Some(Duration::from_millis(READ_SLICE_MS)));
        let _ = stream.set_write_timeout(Some(Duration::from_millis(REQUEST_TIMEOUT_MS)));
        Ok(stream)
    }

    /// Register a listener for daemon events. Survives reconnects; drop the
    /// returned [`mpsc::Receiver`] (or let it disconnect) to unregister.
    pub fn listen(&self) -> mpsc::Receiver<Frame> {
        let (tx, rx) = mpsc::channel();
        self.inner.listeners.lock().unwrap().push(tx);
        rx
    }

    pub fn is_connected(&self) -> bool {
        self.inner.connected.load(Ordering::SeqCst)
    }

    /// Send one command and await its reply. The reader thread resolves the
    /// matching `ResponseEnvelope` by id.
    pub fn request(&self, cmd: ClientCommand) -> Result<Response, BridgeError> {
        if !self.is_connected() {
            return Err(BridgeError::Closed);
        }
        let id = self.inner.req_id.fetch_add(1, Ordering::Relaxed);
        let line = Encoder::encode_request(&RequestEnvelope { id, cmd });

        // Register our oneshot before writing so no reply can race us.
        let (tx, rx) = mpsc::sync_channel::<Response>(1);
        self.inner.pending.lock().unwrap().insert(id, tx);

        {
            let mut guard = self.inner.writer.lock().unwrap();
            let Some(stream) = guard.as_mut() else {
                self.inner.pending.lock().unwrap().remove(&id);
                return Err(BridgeError::Closed);
            };
            if let Err(e) = stream
                .write_all(line.as_bytes())
                .and_then(|_| stream.flush())
            {
                self.inner.pending.lock().unwrap().remove(&id);
                return Err(e.into());
            }
        }

        match rx.recv_timeout(Duration::from_millis(REQUEST_TIMEOUT_MS)) {
            Ok(resp) => match resp {
                Response::Err(e) => Err(BridgeError::Daemon(e)),
                ok => Ok(ok),
            },
            Err(_) => {
                self.inner.pending.lock().unwrap().remove(&id);
                if !self.is_connected() {
                    Err(BridgeError::Closed)
                } else {
                    Err(BridgeError::Timeout)
                }
            }
        }
    }

    /// Spawn the reader thread. Called once at startup.
    pub fn start_reader(&self) {
        let Some(read_fd) = self
            .inner
            .writer
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|s| s.try_clone().ok())
        else {
            return;
        };
        let inner = self.inner.clone();
        std::thread::spawn(move || reader_loop(inner, read_fd));
    }

    /// Reconnect after a disconnect (a few attempts, exponential backoff).
    pub fn reconnect(&self) -> bool {
        let sock = herdr_protocol::default_socket_path();
        let mut backoff = 100u64;
        for _ in 0..4 {
            match Self::dial(&sock) {
                Ok(stream) => {
                    *self.inner.writer.lock().unwrap() = Some(stream);
                    self.inner.connected.store(true, Ordering::SeqCst);
                    self.start_reader();
                    return true;
                }
                Err(_) => {
                    std::thread::sleep(Duration::from_millis(backoff));
                    backoff = (backoff * 2).min(2000);
                }
            }
        }
        false
    }
}

fn reader_loop(inner: Arc<Inner>, mut stream: UnixStream) {
    let mut buf = [0u8; 16384];
    let mut acc: Vec<u8> = Vec::new();
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                acc.extend_from_slice(&buf[..n]);
                while let Some(pos) = acc.iter().position(|&b| b == b'\n') {
                    let line_bytes: Vec<u8> = acc.drain(..=pos).collect();
                    let line = String::from_utf8_lossy(&line_bytes);
                    handle_line(&inner, line.trim());
                }
            }
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(_) => break,
        }
    }
    inner.connected.store(false, Ordering::SeqCst);
}

fn handle_line(inner: &Inner, line: &str) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
        return; // skip malformed frame
    };
    if let Some(ev_val) = v.get("event") {
        if let Ok(event) = serde_json::from_value::<DaemonEvent>(ev_val.clone()) {
            let mut listeners = inner.listeners.lock().unwrap();
            listeners.retain(|tx| tx.send(Frame::Event(event.clone())).is_ok());
            return;
        }
    }
    if let Some(id) = v.get("id").and_then(|i| i.as_u64()) {
        if let Some(resp_val) = v.get("resp") {
            if let Ok(resp) = serde_json::from_value::<Response>(resp_val.clone()) {
                if let Some(tx) = inner.pending.lock().unwrap().remove(&id) {
                    let _ = tx.send(resp);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_lines_route_to_listeners_and_pending() {
        let inner = Inner {
            writer: Mutex::new(None),
            connected: AtomicBool::new(true),
            req_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            listeners: Mutex::new(Vec::new()),
        };
        let (tx, rx) = mpsc::channel();
        inner.listeners.lock().unwrap().push(tx);

        // Event frame reaches listeners.
        handle_line(
            &inner,
            r#"{"event":{"StateChange":{"agent_id":"a","state":"Blocked"}}}"#,
        );
        match rx.recv_timeout(Duration::from_millis(100)).unwrap() {
            Frame::Event(DaemonEvent::StateChange { agent_id, state }) => {
                assert_eq!(agent_id, "a");
                assert_eq!(state, herdr_protocol::AgentState::Blocked);
            }
            other => panic!("unexpected {other:?}"),
        }

        // Reply frame resolves the pending oneshot.
        let (ptx, prx) = mpsc::sync_channel(1);
        inner.pending.lock().unwrap().insert(7, ptx);
        handle_line(&inner, r#"{"id":7,"resp":{"Ok":null}}"#);
        assert!(matches!(
            prx.recv_timeout(Duration::from_millis(100)),
            Ok(Response::Ok)
        ));

        // Malformed lines are ignored.
        handle_line(&inner, "not json");
        handle_line(&inner, r#"{"id":99,"resp":{"Ok":null}}"#); // no pending: dropped
    }

    #[test]
    fn listeners_prune_on_disconnect() {
        let inner = Inner {
            writer: Mutex::new(None),
            connected: AtomicBool::new(true),
            req_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            listeners: Mutex::new(Vec::new()),
        };
        let (tx, rx) = mpsc::channel::<Frame>();
        inner.listeners.lock().unwrap().push(tx);
        drop(rx); // simulate a UI surface closing
        handle_line(&inner, r#"{"event":{"AgentRemoved":{"agent_id":"x"}}}"#);
        assert!(
            inner.listeners.lock().unwrap().is_empty(),
            "dead listener pruned"
        );
    }

    #[test]
    fn connect_fails_cleanly_without_daemon() {
        std::env::set_var("HERDR_SOCKDIR", "/tmp/herdr-desktop-test-nonexistent");
        std::env::remove_var("XDG_RUNTIME_DIR");
        let result = Bridge::connect();
        assert!(matches!(result, Err(BridgeError::Unreachable(_))));
    }
}
