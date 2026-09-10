//! herdr-agent — the headless remote runner (Phase 4).
//!
//! One process, two roles, selected by argv:
//!
//! * `herdr-agent --version` — installer probe target.
//! * `herdr-agent pipe` — the SSH entry point. Speaks the herdr frame protocol
//!   (varint + kind + NDJSON, see `herdr-remote::frame`) on stdin/stdout and
//!   proxies every control frame to a local runner daemon over a Unix socket.
//!
//! Runner daemon lifecycle: if a live runner already owns the socket it is
//! **reused** — remote agents survive bridge drops and local-daemon restarts
//! (spec acceptance criterion 4), because the next `pipe` process rediscovers
//! them via `List`. Otherwise this process boots one (same Supervisor/PTY/state
//! code as the local daemon — one behavior everywhere) and shuts it down when
//! the pipe closes.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use herdr_protocol::{
    codec::{Encoder, RequestEnvelope, ResponseEnvelope},
    ClientCommand,
};

fn main() {
    let arg = std::env::args().nth(1);
    match arg.as_deref() {
        Some("--version") | Some("version") => {
            println!("herdr-agent {}", herdr_protocol::PROTOCOL_VERSION);
        }
        Some("pipe") => {
            if let Err(e) = pipe() {
                eprintln!("herdr-agent: {e:#}");
                std::process::exit(1);
            }
        }
        other => {
            eprintln!("usage: herdr-agent pipe | herdr-agent --version (got {other:?})");
            std::process::exit(2);
        }
    }
}

/// Per-user socket path for the runner daemon (mode 0700 dir, same-user only).
fn runner_socket_path() -> std::path::PathBuf {
    let uid = nix_uid().unwrap_or_else(|| "shared".to_string());
    std::env::temp_dir()
        .join(format!("herdr-agent-{uid}"))
        .join("agent.sock")
}

fn nix_uid() -> Option<String> {
    // No libc dependency: read /proc on Linux, fall back to `id -u` elsewhere.
    if let Ok(text) = std::fs::read_to_string("/proc/self/status") {
        for line in text.lines() {
            if let Some(uid) = line.strip_prefix("Uid:") {
                if let Some(first) = uid.split_whitespace().next() {
                    return Some(first.to_string());
                }
            }
        }
    }
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Is a live runner daemon answering on `path`?
fn runner_is_live(path: &std::path::Path) -> bool {
    let Ok(stream) = UnixStream::connect(path) else {
        return false;
    };
    let mut stream = stream;
    if stream
        .set_read_timeout(Some(Duration::from_millis(750)))
        .is_err()
    {
        return false;
    }
    let line = Encoder::encode_request(&RequestEnvelope {
        id: 1,
        cmd: ClientCommand::Ping,
    });
    if stream.write_all(line.as_bytes()).is_err() {
        return false;
    }
    let mut reader = BufReader::new(stream);
    let mut buf = String::new();
    reader.read_line(&mut buf).is_ok() && buf.contains("\"Pong\"")
}

/// Ensure a runner daemon is up on the socket; returns whether we own it.
fn ensure_runner_daemon(socket: &std::path::Path) -> anyhow::Result<bool> {
    if runner_is_live(socket) {
        return Ok(false); // reuse
    }
    herdr_daemon::ipc::prepare_socket_path(socket)?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let supervisor = Arc::new(herdr_daemon::Supervisor::new());

    // Idle-state ticker (Working → Idle), same cadence as the local daemon.
    let tick_supervisor = supervisor.clone();
    rt.spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(500));
        loop {
            interval.tick().await;
            tick_supervisor.tick_idle();
        }
    });

    let shutdown_rx = supervisor.subscribe_shutdown();
    let serve_socket = socket.to_path_buf();
    let serve_supervisor = supervisor.clone();
    std::thread::spawn(move || {
        let result = rt.block_on(herdr_daemon::ipc::serve(
            serve_socket,
            serve_supervisor,
            shutdown_rx,
        ));
        if let Err(e) = result {
            tracing::error!(error = %e, "runner daemon serve loop failed");
        }
    });
    // Give the bind a moment; the bridge sends its first request immediately.
    for _ in 0..50 {
        if socket.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(true)
}

/// `herdr-agent pipe`: frame ⇄ NDJSON proxy, booting/reusing the runner daemon.
fn pipe() -> anyhow::Result<()> {
    let socket = runner_socket_path();
    let owned = ensure_runner_daemon(&socket)?;

    let stream = UnixStream::connect(&socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut sock_reader = BufReader::new(stream.try_clone()?);
    let mut sock_writer = stream;

    // Thread A: stdin frames → NDJSON request lines on the socket.
    let writer_close = Arc::new(AtomicBool::new(false));
    let close_flag = writer_close.clone();
    let stdin_thread = std::thread::spawn(move || -> Result<(), std::io::Error> {
        let mut stdin = std::io::stdin().lock();
        let mut frame_reader = herdr_remote_frame_reader();
        let mut buf = [0u8; 8192];
        loop {
            let n = stdin.read(&mut buf)?;
            if n == 0 {
                break; // pipe closed
            }
            for frame in frame_reader
                .feed(&buf[..n])
                .map_err(std::io::Error::other)?
            {
                let herdr_remote::frame::Frame::Control(env) = frame else {
                    continue; // bridge never sends replies/events to the runner
                };
                sock_writer.write_all(Encoder::encode_request(&env).as_bytes())?;
                sock_writer.flush()?;
            }
        }
        close_flag.store(true, Ordering::SeqCst);
        Ok(())
    });

    // Thread B (current): socket NDJSON lines → frames on stdout.
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut line = String::new();
    loop {
        if writer_close.load(Ordering::SeqCst) {
            // stdin EOF: shut down a daemon we own; leave a reused one running.
            if owned {
                let _ = shutdown_runner(&socket);
            }
            break;
        }
        line.clear();
        match sock_reader.read_line(&mut line) {
            Ok(0) => break, // runner daemon died
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) else {
                    continue;
                };
                let frame = if v.get("event").is_some() {
                    serde_json::from_value::<herdr_protocol::DaemonEvent>(v["event"].clone())
                        .ok()
                        .map(herdr_remote::frame::Frame::Event)
                } else if v.get("resp").is_some() {
                    serde_json::from_value::<ResponseEnvelope>(v.clone())
                        .ok()
                        .map(herdr_remote::frame::Frame::Reply)
                } else {
                    None
                };
                if let Some(frame) = frame {
                    if herdr_remote::frame::write_frame(&mut out, &frame).is_err() {
                        break; // bridge hung up
                    }
                }
            }
            Err(e) => {
                // Read timeout is survivable — loop re-checks the close flag.
                if e.kind() != std::io::ErrorKind::WouldBlock
                    && e.kind() != std::io::ErrorKind::TimedOut
                {
                    break;
                }
            }
        }
    }
    let _ = stdin_thread.join();
    Ok(())
}

/// Tell the owned runner daemon to shut down cleanly (kills its agents — the
/// whole point: no orphans left on the remote when the pipe dies... for agents
/// killed via the bridge; an owned daemon exit also reaps its own children).
fn shutdown_runner(socket: &std::path::Path) -> anyhow::Result<()> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let line = Encoder::encode_request(&RequestEnvelope {
        id: 2,
        cmd: ClientCommand::Shutdown,
    });
    stream.write_all(line.as_bytes())?;
    Ok(())
}

/// Build the frame reader. In a helper so the dependency is exercised in tests.
fn herdr_remote_frame_reader() -> herdr_remote::frame::FrameReader {
    herdr_remote::frame::FrameReader::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_is_per_user() {
        let p = runner_socket_path();
        assert!(p.starts_with(std::env::temp_dir()));
        assert!(p.to_string_lossy().contains("herdr-agent-"));
        assert!(p.ends_with("agent.sock"));
    }

    #[test]
    fn live_probe_false_for_missing_socket() {
        let p =
            std::env::temp_dir().join(format!("herdr-agent-test-absent-{}", std::process::id()));
        assert!(!runner_is_live(&p));
    }
}
