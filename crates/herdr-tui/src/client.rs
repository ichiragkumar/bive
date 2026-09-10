//! Daemon connection for the TUI: one UDS with a split read/write fd
//! (`try_clone`), a reader thread demultiplexing frames (replies → pending map,
//! events → queue), and request/reply matching by envelope id. Reconnects with
//! exponential backoff when the socket dies; the event receiver survives
//! reconnects.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use herdr_protocol::{
    codec::{Encoder, RequestEnvelope, ResponseEnvelope},
    ClientCommand, DaemonEvent, Response,
};

const REQUEST_TIMEOUT_MS: u64 = 5000;
const READ_SLICE_MS: u64 = 200;
const MAX_BACKOFF_MS: u64 = 2000;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("daemon unreachable at {0}")]
    Unreachable(String),
    #[error("daemon error: {0}")]
    Daemon(String),
    #[error("request timed out")]
    Timeout,
    #[error("connection closed")]
    Closed,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol error: {0}")]
    Protocol(String),
}

enum Frame {
    Reply(ResponseEnvelope),
    Event(DaemonEvent),
}

fn parse_frame(line: &str) -> Result<Frame, ClientError> {
    let v: serde_json::Value =
        serde_json::from_str(line).map_err(|e| ClientError::Protocol(e.to_string()))?;
    if let Some(ev) = v.get("event") {
        let event: DaemonEvent =
            serde_json::from_value(ev.clone()).map_err(|e| ClientError::Protocol(e.to_string()))?;
        Ok(Frame::Event(event))
    } else if v.get("resp").is_some() {
        let env: ResponseEnvelope =
            serde_json::from_value(v).map_err(|e| ClientError::Protocol(e.to_string()))?;
        Ok(Frame::Reply(env))
    } else {
        Err(ClientError::Protocol("unrecognized frame".into()))
    }
}

/// Interior state shared between the client handle and its reader thread.
struct Inner {
    /// Write-side fd clone; the reader owns the read-side clone.
    writer: Mutex<Option<UnixStream>>,
    connected: AtomicBool,
    req_id: AtomicU64,
    pending: Mutex<HashMap<u64, Response>>,
    events_tx: Mutex<Option<Sender<DaemonEvent>>>,
    /// Whether a reader thread should be (re)spawned on reconnect.
    reader_wanted: AtomicBool,
}

/// TUI-side client. `start_reader` spawns the demux thread; it runs until
/// disconnect and is respawned by `reconnect` when the daemon comes back.
#[derive(Clone)]
pub struct HerdrClient {
    inner: Arc<Inner>,
}

impl HerdrClient {
    pub fn connect() -> Result<Self, ClientError> {
        let sock = herdr_protocol::default_socket_path();
        let stream = Self::dial(&sock)?;
        Ok(Self {
            inner: Arc::new(Inner {
                writer: Mutex::new(Some(stream)),
                connected: AtomicBool::new(true),
                req_id: AtomicU64::new(1),
                pending: Mutex::new(HashMap::new()),
                events_tx: Mutex::new(None),
                reader_wanted: AtomicBool::new(false),
            }),
        })
    }

    fn dial(sock: &std::path::Path) -> Result<UnixStream, ClientError> {
        let stream = UnixStream::connect(sock)
            .map_err(|_| ClientError::Unreachable(sock.display().to_string()))?;
        let _ = stream.set_read_timeout(Some(Duration::from_millis(READ_SLICE_MS)));
        let _ = stream.set_write_timeout(Some(Duration::from_millis(REQUEST_TIMEOUT_MS)));
        Ok(stream)
    }

    /// Spawn the reader thread; returns the event receiver. Call once; the
    /// receiver keeps receiving across reconnects.
    pub fn start_reader(&self) -> Receiver<DaemonEvent> {
        let (tx, rx) = mpsc::channel();
        *self.inner.events_tx.lock().unwrap() = Some(tx);
        self.inner.reader_wanted.store(true, Ordering::SeqCst);
        self.spawn_reader();
        rx
    }

    fn spawn_reader(&self) {
        let read_fd = {
            let guard = self.inner.writer.lock().unwrap();
            match guard.as_ref() {
                // Independent fd for the reader: same socket, own read timeout.
                Some(s) => s.try_clone().ok(),
                None => None,
            }
        };
        let Some(read_fd) = read_fd else {
            return;
        };
        let clone = self.clone();
        std::thread::spawn(move || reader_loop(clone, read_fd));
    }

    pub fn is_connected(&self) -> bool {
        self.inner.connected.load(Ordering::SeqCst)
    }

    /// Send one request and wait for its reply (reader thread demuxes).
    pub fn request(&self, cmd: ClientCommand) -> Result<Response, ClientError> {
        if !self.is_connected() {
            return Err(ClientError::Closed);
        }
        let id = self.inner.req_id.fetch_add(1, Ordering::Relaxed);
        let line = Encoder::encode_request(&RequestEnvelope { id, cmd });
        {
            let guard = self.inner.writer.lock().unwrap();
            let stream = guard.as_ref().ok_or(ClientError::Closed)?;
            let mut w = stream;
            w.write_all(line.as_bytes())?;
            w.flush()?;
        }
        self.wait_reply(id)
    }

    fn wait_reply(&self, id: u64) -> Result<Response, ClientError> {
        let deadline = std::time::Instant::now() + Duration::from_millis(REQUEST_TIMEOUT_MS);
        loop {
            if let Some(resp) = self.inner.pending.lock().unwrap().remove(&id) {
                if let Response::Err(e) = &resp {
                    return Err(ClientError::Daemon(e.clone()));
                }
                return Ok(resp);
            }
            if !self.is_connected() {
                return Err(ClientError::Closed);
            }
            if std::time::Instant::now() > deadline {
                return Err(ClientError::Timeout);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Reconnect with exponential backoff (a few attempts per call); respawns
    /// the reader thread on success so events keep flowing to the same receiver.
    pub fn reconnect(&mut self) {
        let sock = herdr_protocol::default_socket_path();
        let mut backoff = 100u64;
        for _ in 0..4 {
            match Self::dial(&sock) {
                Ok(stream) => {
                    *self.inner.writer.lock().unwrap() = Some(stream);
                    self.inner.connected.store(true, Ordering::SeqCst);
                    if self.inner.reader_wanted.load(Ordering::SeqCst) {
                        self.spawn_reader();
                    }
                    return;
                }
                Err(_) => {
                    std::thread::sleep(Duration::from_millis(backoff));
                    backoff = (backoff * 2).min(MAX_BACKOFF_MS);
                }
            }
        }
    }

    fn mark_disconnected(&self) {
        self.inner.connected.store(false, Ordering::SeqCst);
    }
}

fn reader_loop(client: HerdrClient, mut stream: UnixStream) {
    let mut buf = [0u8; 16384];
    let mut acc: Vec<u8> = Vec::new();
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                acc.extend_from_slice(&buf[..n]);
                while let Some(pos) = acc.iter().position(|&b| b == b'\n') {
                    let line_bytes: Vec<u8> = acc.drain(..=pos).collect();
                    let line = String::from_utf8_lossy(&line_bytes).trim().to_string();
                    if line.is_empty() {
                        continue;
                    }
                    match parse_frame(&line) {
                        Ok(Frame::Reply(env)) => {
                            client
                                .inner
                                .pending
                                .lock()
                                .unwrap()
                                .insert(env.id, env.resp);
                        }
                        Ok(Frame::Event(ev)) => {
                            if let Some(tx) = client.inner.events_tx.lock().unwrap().as_ref() {
                                let _ = tx.send(ev);
                            }
                        }
                        Err(_) => { /* skip malformed frame */ }
                    }
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
    client.mark_disconnected();
}

#[cfg(test)]
mod tests {
    use super::*;
    use herdr_protocol::AgentState;

    #[test]
    fn parse_reply_frame() {
        let line = r#"{"id":5,"resp":{"Pong":{"version":"0.1.0","uptime_ms":1,"agents":0}}}"#;
        match parse_frame(line).unwrap() {
            Frame::Reply(env) => {
                assert_eq!(env.id, 5);
                assert!(matches!(env.resp, Response::Pong { .. }));
            }
            _ => panic!("expected reply"),
        }
    }

    #[test]
    fn parse_event_frame() {
        let line = r#"{"event":{"StateChange":{"agent_id":"abc","state":"Blocked"}}}"#;
        match parse_frame(line).unwrap() {
            Frame::Event(DaemonEvent::StateChange { agent_id, state }) => {
                assert_eq!(agent_id, "abc");
                assert_eq!(state, AgentState::Blocked);
            }
            _ => panic!("expected event"),
        }
    }

    #[test]
    fn parse_garbage_is_error() {
        assert!(parse_frame("hello").is_err());
        assert!(parse_frame("{}").is_err());
    }

    #[test]
    fn connect_fails_cleanly_without_daemon() {
        std::env::set_var("HERDR_SOCKDIR", "/tmp/herdr-tui-test-nonexistent");
        std::env::remove_var("XDG_RUNTIME_DIR");
        let result = HerdrClient::connect();
        assert!(matches!(result, Err(ClientError::Unreachable(_))));
    }
}
