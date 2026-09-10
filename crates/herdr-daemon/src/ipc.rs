//! IPC server: accepts client connections on the daemon's Unix domain socket and
//! multiplexes replies + broadcast events onto each connection.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;

use herdr_protocol::{
    codec::{Decoder, EventEnvelope, ResponseEnvelope},
    ClientCommand, DaemonEvent,
};

use crate::supervisor::Supervisor;

/// Remove a stale socket file left by a crashed daemon (verified dead by the caller).
pub fn prepare_socket_path(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        std::fs::remove_file(path).with_context(|| format!("removing stale socket {}", path.display()))?;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    Ok(path.to_path_buf())
}

/// Bind the socket with 0600 permissions and start the accept loop.
/// Exits when `shutdown` fires (Shutdown command or SIGINT/SIGTERM).
pub async fn serve(
    socket_path: PathBuf,
    supervisor: Arc<Supervisor>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("binding {}", socket_path.display()))?;
    // Owner-only: same-user clients only.
    restrict_permissions(&socket_path);
    tracing::info!(socket = %socket_path.display(), "IPC server listening");

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _addr)) => {
                        let supervisor = supervisor.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(stream, supervisor).await {
                                tracing::debug!(error = %e, "client connection ended");
                            }
                        });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "accept failed");
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
            }
            _ = shutdown.changed(), if *shutdown.borrow() == false => {
                tracing::info!("IPC server shutting down");
                break;
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        tracing::warn!(error = %e, "could not restrict socket permissions");
    }
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) {}

/// Per-connection task: read request lines, dispatch, write replies and events.
async fn handle_connection(stream: UnixStream, supervisor: Arc<Supervisor>) -> Result<()> {
    let (mut reader, mut writer) = stream.into_split();
    let mut dec = Decoder::new();
    let mut rx = supervisor.bus.subscribe();
    let mut buf = [0u8; 8192];
    // `None` until an `Events`/`Attach` command marks this connection as streaming;
    // streaming connections stay open until the client hangs up.
    let mut streaming = false;
    // Agent id filter for `Attach`; `None` (with streaming=true) means all events.
    let mut attached_to: Option<String> = None;

    loop {
        tokio::select! {
            read = reader.read(&mut buf) => {
                match read {
                    Ok(0) => break, // client hung up
                    Ok(n) => {
                        let results = dec.push(&buf[..n]);
                        for result in results {
                            let req = match result {
                                Ok(req) => req,
                                Err(e) => {
                                    let env = ResponseEnvelope {
                                        id: 0,
                                        resp: e.to_response(),
                                    };
                                    writer
                                        .write_all(herdr_protocol::codec::Encoder::encode_response(&env).as_bytes())
                                        .await?;
                                    continue;
                                }
                            };
                            let is_stream_marker = matches!(
                                req.cmd,
                                ClientCommand::Events | ClientCommand::Attach { .. }
                            );
                            let attach_target = match &req.cmd {
                                ClientCommand::Attach { agent_id } => Some(agent_id.clone()),
                                _ => None,
                            };
                            let resp = supervisor.handle(req.cmd).await;

                            // After Attach/Events, this connection receives events.
                            if is_stream_marker {
                                streaming = true;
                                if let Some(agent_id) = attach_target {
                                    attached_to = Some(agent_id);
                                }
                            }
                            let env = ResponseEnvelope { id: req.id, resp };
                            writer
                                .write_all(herdr_protocol::codec::Encoder::encode_response(&env).as_bytes())
                                .await?;
                        }
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            ev = rx.recv(), if streaming => {
                match ev {
                    Ok(event) => {
                        if let Some(agent) = &attached_to {
                            if !event_concerns(&event, agent) {
                                continue;
                            }
                        }
                        let env = EventEnvelope { event };
                        writer
                            .write_all(herdr_protocol::codec::Encoder::encode_event(&env).as_bytes())
                            .await?;
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(lagged = n, "client too slow; dropping connection");
                        return Err(anyhow::anyhow!("lagged"));
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
    Ok(())
}

fn event_concerns(event: &DaemonEvent, agent_id: &str) -> bool {
    match event {
        DaemonEvent::AgentSpawned { info } => info.id == agent_id,
        DaemonEvent::AgentOutput { agent_id: ev_id, .. }
        | DaemonEvent::StateChange { agent_id: ev_id, .. }
        | DaemonEvent::AgentExited { agent_id: ev_id, .. }
        | DaemonEvent::AgentRemoved { agent_id: ev_id } => ev_id == agent_id,
    }
}
