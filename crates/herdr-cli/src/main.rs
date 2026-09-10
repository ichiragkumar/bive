//! herdr — the unified agent runtime CLI.
//!
//! Every subcommand (except `daemon`) is a thin socket client speaking the
//! `herdr-protocol` NDJSON wire format.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::anyhow;
use clap::{Parser, Subcommand};

use herdr_protocol::{
    codec::{Encoder, RequestEnvelope, ResponseEnvelope},
    AgentInfo, ClientCommand, DaemonEvent, Response,
};

mod replay;

#[derive(Parser)]
#[command(
    name = "herdr",
    version,
    about = "Unified agent runtime & control surface"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the daemon in the foreground (use tmux/nohup to background it).
    Daemon,
    /// Liveness check.
    Ping,
    /// Spawn a new agent under a PTY.
    Spawn {
        /// Agent profile (generic, claude-code, codex, bash).
        #[arg(long, default_value = "generic")]
        profile: String,
        /// Working directory for the agent.
        #[arg(long)]
        cwd: Option<String>,
        /// Command and args after `--`.
        #[arg(last = true)]
        command: Vec<String>,
    },
    /// List agents and their states.
    List,
    /// Send text to an agent's PTY (appends \n unless --raw).
    Send {
        agent_id: String,
        text: String,
        #[arg(long)]
        raw: bool,
    },
    /// Live-tail an agent's output; forward stdin lines; Ctrl-C detaches only.
    Attach { agent_id: String },
    /// Replay an agent's recent output from the daemon's ring buffer.
    Logs {
        agent_id: String,
        #[arg(long, default_value_t = 4096)]
        bytes: u32,
    },
    /// Kill an agent.
    Kill { agent_id: String },
    /// Tap the raw NDJSON event stream.
    Events,
    /// Stop the daemon and kill all agents.
    Shutdown,
    /// Offline state-timeline inference over a captured PTY stream (no daemon).
    Replay {
        /// Profile whose regexes/timings drive inference (generic, claude-code, codex, bash).
        #[arg(long, default_value = "generic")]
        profile: String,
        /// Capture to replay (raw PTY bytes or `herdr events` NDJSON); stdin if omitted.
        file: Option<String>,
        /// Chunking granularity in ms — approximates the daemon's read cadence.
        #[arg(long, default_value_t = 50)]
        chunk_ms: u64,
        /// Interpolated idle checks per output gap (0 disables interpolation).
        #[arg(long, default_value_t = 1)]
        idle: u32,
        /// Show the matched tail text on each transition for debugging regexes.
        #[arg(long)]
        explain: bool,
    },
}

fn main() {
    let cli = Cli::parse();

    match run(cli.cmd) {
        Ok(()) => {}
        Err(HerdrError::DaemonUnreachable(path)) => {
            eprintln!("error: daemon not reachable at {path} (start it with 'herdr daemon')");
            std::process::exit(2);
        }
        Err(HerdrError::UnknownAgent(id)) => {
            eprintln!("error: unknown agent {id}");
            std::process::exit(3);
        }
        Err(HerdrError::Other(e)) => {
            eprintln!("error: {e:#}");
            std::process::exit(1);
        }
    }
}

enum HerdrError {
    DaemonUnreachable(String),
    UnknownAgent(String),
    Other(anyhow::Error),
}

impl From<anyhow::Error> for HerdrError {
    fn from(e: anyhow::Error) -> Self {
        HerdrError::Other(e)
    }
}

/// Convert any error into `HerdrError::Other`.
fn other_err<E: Into<anyhow::Error>>(e: E) -> HerdrError {
    HerdrError::Other(e.into())
}

static REQ_ID: AtomicU64 = AtomicU64::new(1);

fn next_req_id() -> u64 {
    REQ_ID.fetch_add(1, Ordering::Relaxed)
}

struct Connection {
    stream: UnixStream,
}

fn socket_path() -> PathBuf {
    herdr_protocol::default_socket_path()
}

fn connect() -> Result<Connection, HerdrError> {
    let sock = socket_path();
    let stream = match UnixStream::connect(&sock) {
        Ok(s) => s,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Err(HerdrError::DaemonUnreachable(sock.display().to_string()));
        }
        Err(e) => return Err(other_err(e)),
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(other_err)?;
    Ok(Connection { stream })
}

/// Send one request; read frames until the reply with our id arrives.
fn request(conn: &mut Connection, cmd: ClientCommand) -> Result<Response, HerdrError> {
    let id = next_req_id();
    let line = Encoder::encode_request(&RequestEnvelope { id, cmd });
    conn.stream.write_all(line.as_bytes()).map_err(other_err)?;

    let mut buf = [0u8; 8192];
    let mut pending: Vec<u8> = Vec::new();
    loop {
        let n = conn.stream.read(&mut buf).map_err(other_err)?;
        if n == 0 {
            return Err(other_err(anyhow!("daemon closed the connection")));
        }
        pending.extend_from_slice(&buf[..n]);
        while let Some(pos) = pending.iter().position(|&b| b == b'\n') {
            let line_bytes: Vec<u8> = pending.drain(..=pos).collect();
            let text = String::from_utf8_lossy(&line_bytes);
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(text.trim()) {
                if v.get("resp").is_some() {
                    let env: ResponseEnvelope = serde_json::from_value(v).map_err(other_err)?;
                    if env.id == id {
                        return Ok(env.resp);
                    }
                }
                // Events preceding the reply are dropped in one-shot mode.
            }
        }
    }
}

impl Connection {
    /// Read one line of whatever arrives next (reply or event), as raw JSON text.
    fn read_line_json(&mut self) -> Result<Option<String>, HerdrError> {
        let mut acc: Vec<u8> = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            if let Some(pos) = acc.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = acc.drain(..=pos).collect();
                return Ok(Some(String::from_utf8_lossy(&line).trim().to_string()));
            }
            let n = self.stream.read(&mut buf).map_err(other_err)?;
            if n == 0 {
                return Ok(None);
            }
            acc.extend_from_slice(&buf[..n]);
        }
    }
}

fn expect_ok(resp: Response, ctx: &str) -> Result<(), HerdrError> {
    match resp {
        Response::Ok | Response::AgentCreated { .. } => Ok(()),
        Response::Err(e) if e.contains("unknown agent") => {
            // Caller wraps with the id where available.
            Err(HerdrError::Other(anyhow!(e)))
        }
        Response::Err(e) => Err(HerdrError::Other(anyhow!("{ctx}: {e}"))),
        other => Err(HerdrError::Other(anyhow!(
            "{ctx}: unexpected reply {other:?}"
        ))),
    }
}

fn run(cmd: Cmd) -> Result<(), HerdrError> {
    match cmd {
        Cmd::Daemon => daemon(),
        Cmd::Ping => {
            let mut conn = connect()?;
            let resp = request(&mut conn, ClientCommand::Ping)?;
            match resp {
                Response::Pong {
                    version,
                    uptime_ms,
                    agents,
                } => {
                    println!("herdr daemon v{version} up {uptime_ms} ms, {agents} agent(s)");
                    Ok(())
                }
                Response::Err(e) => Err(HerdrError::Other(anyhow!("daemon error: {e}"))),
                _ => Err(HerdrError::Other(anyhow!("unexpected reply to ping"))),
            }
        }
        Cmd::Spawn {
            profile,
            cwd,
            command,
        } => {
            if command.is_empty() {
                return Err(HerdrError::Other(anyhow!(
                    "spawn requires a command: herdr spawn -- <command> [args...]"
                )));
            }
            let cwd = cwd.unwrap_or_else(|| {
                std::env::current_dir()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| ".".into())
            });
            let (cmd0, args) = command.split_first().expect("non-empty");
            let mut conn = connect()?;
            let resp = request(
                &mut conn,
                ClientCommand::Spawn {
                    profile,
                    cwd,
                    command: cmd0.clone(),
                    args: args.to_vec(),
                },
            )?;
            match resp {
                Response::AgentCreated { agent_id } => {
                    println!("{agent_id}");
                    Ok(())
                }
                Response::Err(e) => Err(HerdrError::Other(anyhow!("spawn failed: {e}"))),
                _ => Err(HerdrError::Other(anyhow!("unexpected reply to spawn"))),
            }
        }
        Cmd::List => {
            let mut conn = connect()?;
            let resp = request(&mut conn, ClientCommand::List)?;
            match resp {
                Response::AgentList(agents) => {
                    print_agent_table(&agents);
                    Ok(())
                }
                Response::Err(e) => Err(HerdrError::Other(anyhow!("daemon error: {e}"))),
                _ => Err(HerdrError::Other(anyhow!("unexpected reply to list"))),
            }
        }
        Cmd::Send {
            agent_id,
            text,
            raw,
        } => {
            let mut conn = connect()?;
            let resp = request(
                &mut conn,
                ClientCommand::SendInput {
                    agent_id: agent_id.clone(),
                    text,
                    raw,
                },
            )?;
            match resp {
                Response::Ok => Ok(()),
                Response::Err(e) if e.contains("unknown agent") => {
                    Err(HerdrError::UnknownAgent(agent_id))
                }
                Response::Err(e) => Err(HerdrError::Other(anyhow!(e))),
                _ => Err(HerdrError::Other(anyhow!("unexpected reply to send"))),
            }
        }
        Cmd::Logs { agent_id, bytes } => {
            let mut conn = connect()?;
            let resp = request(
                &mut conn,
                ClientCommand::Logs {
                    agent_id: agent_id.clone(),
                    max_bytes: bytes,
                },
            )?;
            match resp {
                Response::LogChunk { payload } => {
                    print!("{payload}");
                    let _ = std::io::stdout().flush();
                    Ok(())
                }
                Response::Err(e) if e.contains("unknown agent") => {
                    Err(HerdrError::UnknownAgent(agent_id))
                }
                Response::Err(e) => Err(HerdrError::Other(anyhow!(e))),
                _ => Err(HerdrError::Other(anyhow!("unexpected reply to logs"))),
            }
        }
        Cmd::Kill { agent_id } => {
            let mut conn = connect()?;
            let resp = request(
                &mut conn,
                ClientCommand::Kill {
                    agent_id: agent_id.clone(),
                },
            )?;
            match resp {
                Response::Ok => {
                    println!("killed {agent_id}");
                    Ok(())
                }
                Response::Err(e) if e.contains("unknown agent") => {
                    Err(HerdrError::UnknownAgent(agent_id))
                }
                Response::Err(e) => Err(HerdrError::Other(anyhow!(e))),
                _ => Err(HerdrError::Other(anyhow!("unexpected reply to kill"))),
            }
        }
        Cmd::Shutdown => {
            let mut conn = connect()?;
            let resp = request(&mut conn, ClientCommand::Shutdown)?;
            expect_ok(resp, "shutdown")?;
            println!("daemon shutting down");
            Ok(())
        }
        Cmd::Events => events_tap(),
        Cmd::Attach { agent_id } => attach(agent_id),
        Cmd::Replay {
            profile,
            file,
            chunk_ms,
            idle,
            explain,
        } => replay::run(replay::ReplayArgs {
            profile,
            file,
            chunk_ms,
            idle,
            explain,
        })
        .map_err(other_err),
    }
}

fn daemon() -> Result<(), HerdrError> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(other_err)?;
    let socket = socket_path();
    rt.block_on(async move {
        // Single-instance guard: if the socket answers a Ping, a daemon is alive.
        if socket.exists() && daemon_is_live(&socket) {
            return Err(HerdrError::Other(anyhow!(
                "daemon already running on {}",
                socket.display()
            )));
        }
        herdr_daemon::ipc::prepare_socket_path(&socket)
            .map_err(|e| HerdrError::Other(anyhow!("{e:#}")))?;
        println!(
            "herdr daemon v{} listening on {}",
            herdr_protocol::PROTOCOL_VERSION,
            socket.display()
        );
        herdr_daemon::run(socket)
            .await
            .map_err(|e| HerdrError::Other(anyhow!("{e:#}")))
    })
}

/// Probe a socket: does a live herdr daemon answer there?
fn daemon_is_live(sock: &PathBuf) -> bool {
    if let Ok(stream) = UnixStream::connect(sock) {
        if stream
            .set_read_timeout(Some(Duration::from_millis(500)))
            .is_ok()
        {
            let mut conn = Connection { stream };
            if request(&mut conn, ClientCommand::Ping).is_ok() {
                return true;
            }
        }
    }
    false
}

/// `herdr events`: print every event line until the user interrupts.
fn events_tap() -> Result<(), HerdrError> {
    let mut conn = connect()?;
    let resp = request(&mut conn, ClientCommand::Events)?;
    expect_ok(resp, "events")?;
    // Long read timeout: we just want the stream.
    conn.stream
        .set_read_timeout(Some(Duration::from_secs(600)))
        .map_err(other_err)?;
    loop {
        match conn.read_line_json() {
            Ok(Some(line)) => {
                if !line.is_empty() {
                    println!("{line}");
                    let _ = std::io::stdout().flush();
                }
            }
            Ok(None) => return Ok(()), // daemon hung up
            Err(e) => return Err(e),
        }
    }
}

/// `herdr attach <id>`: print the agent's output stream; forward stdin lines;
/// Ctrl-C detaches (SIGINT kills this process only — agents live in the daemon).
fn attach(agent_id: String) -> Result<(), HerdrError> {
    let mut conn = connect()?;
    let resp = request(
        &mut conn,
        ClientCommand::Attach {
            agent_id: agent_id.clone(),
        },
    )?;
    expect_ok(resp, "attach").map_err(|e| match e {
        HerdrError::Other(msg) if msg.to_string().contains("unknown agent") => {
            HerdrError::UnknownAgent(agent_id.clone())
        }
        other => other,
    })?;
    // Attach is long-lived; lift the 10s request timeout.
    conn.stream
        .set_read_timeout(Some(Duration::from_secs(600)))
        .map_err(other_err)?;
    eprintln!("── attached to {agent_id} — Ctrl-C detaches (agent keeps running) ──");

    // Reader thread: print output + state + exit lines.
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let mut reader_conn = Connection {
        stream: conn.stream.try_clone().map_err(other_err)?,
    };
    std::thread::spawn(move || loop {
        match reader_conn.read_line_json() {
            Ok(Some(line)) => {
                if tx.send(line).is_err() {
                    return;
                }
            }
            _ => return,
        }
    });

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in rx {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
            if let Some(ev_val) = v.get("event") {
                if let Ok(event) = serde_json::from_value::<DaemonEvent>(ev_val.clone()) {
                    match event {
                        DaemonEvent::AgentOutput {
                            agent_id: id,
                            payload,
                        } if id == agent_id => {
                            let _ = write!(out, "{payload}");
                            let _ = out.flush();
                        }
                        DaemonEvent::StateChange {
                            agent_id: id,
                            state,
                        } if id == agent_id => {
                            let _ = writeln!(out, "\n── state: {state} ──");
                            let _ = out.flush();
                        }
                        DaemonEvent::AgentExited { agent_id: id, code } if id == agent_id => {
                            let _ = writeln!(out, "\n── agent exited with code {code} ──");
                            return Ok(());
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    Ok(())
}

fn print_agent_table(agents: &[AgentInfo]) {
    if agents.is_empty() {
        println!("no agents — spawn one with: herdr spawn -- <command>");
        return;
    }
    println!(
        "{:<14} {:<12} {:<14} {:<28} COMMAND",
        "ID", "PROFILE", "STATE", "CWD"
    );
    for a in agents {
        println!(
            "{:<14} {:<12} {:<14} {:<28} {}",
            a.id,
            a.profile,
            format!("{} {}", a.state.glyph(), a.state.name()),
            truncate(&a.cwd, 28),
            truncate(&a.command, 48),
        );
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let cut: String = s.chars().take(n - 1).collect();
        format!("{cut}…")
    }
}
