//! PTY process manager: spawns agents under pseudo-terminals and bridges their
//! blocking I/O into the async world.
//!
//! Layout per agent:
//! * **reader task** (`spawn_blocking`): blocking loop on the PTY master reader →
//!   ANSI-stripped text feeds the agent's [`StateMachine`]; raw text goes to the
//!   ring buffer; both broadcast as `AgentOutput` events.
//! * **wait task** (`spawn_blocking`): `child.wait()` → `AgentExited` + terminal
//!   state + scheduled removal. Owns the PTY master so the session stays alive.
//! * **writer/killer**: stored in the registry for input injection and kill.

use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, PtySize};

use herdr_protocol::{AgentId, DaemonEvent};

use crate::ansi::AnsiStripper;
use crate::bus::EventBus;
use crate::registry::{now_unix_ms, KillHandle, Registry};

/// Reader buffer size per agent.
const READ_BUF: usize = 4096;

/// Everything a spawned PTY task needs to reach shared daemon state.
#[derive(Clone)]
pub struct SpawnContext {
    pub registry: Arc<Registry>,
    pub bus: EventBus,
}

/// Parameters for one agent spawn.
pub struct SpawnSpec {
    pub agent_id: AgentId,
    pub cwd: PathBuf,
    pub command: String,
    pub args: Vec<String>,
}

/// Handles the supervisor stores into the registry after spawn.
pub struct PtyHandles {
    pub writer: Box<dyn std::io::Write + Send>,
    pub killer: KillHandle,
}

/// Spawn the agent under a PTY and wire its tasks into the bus/registry.
///
/// The registry entry for `spec.agent_id` must already exist (reader/wait tasks
/// write into it).
pub fn spawn_agent(spec: SpawnSpec, ctx: SpawnContext) -> Result<PtyHandles> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("openpty failed")?;

    let mut cmd = CommandBuilder::new(&spec.command);
    cmd.args(spec.args.iter());
    cmd.cwd(&spec.cwd);
    // Force rich output so agents behave as if attached to a real terminal.
    cmd.env("TERM", "xterm-256color");

    let child = pair
        .slave
        .spawn_command(cmd)
        .with_context(|| format!("spawning {}", spec.command))?;
    // The slave fd must be released so reader EOF works once the child exits.
    drop(pair.slave);

    let mut reader = pair
        .master
        .try_clone_reader()
        .context("cloning pty reader")?;
    let writer = pair.master.take_writer().context("taking pty writer")?;
    let killer_box: Box<dyn ChildKiller + Send + Sync> = child.clone_killer();

    // --- reader task: blocking loop bridged into the async world -------------
    let (registry, bus, agent_id) = (ctx.registry.clone(), ctx.bus.clone(), spec.agent_id.clone());
    tokio::task::spawn_blocking(move || {
        let mut buf = [0u8; READ_BUF];
        let mut stripper = AnsiStripper::new();
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break, // EOF — agent exited
                Ok(n) => {
                    let chunk = &buf[..n];
                    let raw = String::from_utf8_lossy(chunk).into_owned();
                    let stripped = String::from_utf8_lossy(&stripper.feed_raw(chunk)).into_owned();

                    registry.with_agent(&agent_id, |e| e.push_ring(&raw));
                    registry.update_info(&agent_id, |i| i.last_output_unix_ms = now_unix_ms());

                    let transition = registry
                        .with_agent(&agent_id, |e| {
                            e.sm.lock().expect("state mutex").process_chunk(&stripped)
                        })
                        .flatten();
                    if let Some(new_state) = transition {
                        registry.update_info(&agent_id, |i| i.state = new_state.clone());
                        bus.publish(DaemonEvent::StateChange {
                            agent_id: agent_id.clone(),
                            state: new_state,
                        });
                    }

                    bus.publish(DaemonEvent::AgentOutput {
                        agent_id: agent_id.clone(),
                        payload: raw,
                    });
                }
                Err(_) => break,
            }
        }
    });

    // --- wait task: blocking waitpid, owns the master to keep the session up -
    let (registry, bus, agent_id) = (ctx.registry.clone(), ctx.bus.clone(), spec.agent_id.clone());
    tokio::task::spawn_blocking(move || {
        let _master_keepalive = pair.master; // session stays open until child exits
        let mut child = child;
        let code = match child.wait() {
            Ok(status) => status.exit_code() as i32,
            Err(e) => {
                tracing::warn!(agent = %agent_id, error = %e, "wait failed");
                -1
            }
        };

        let final_state = if code == 0 {
            herdr_protocol::AgentState::Exited(0)
        } else {
            herdr_protocol::AgentState::Errored(format!("exit code {code}"))
        };

        let transition = registry
            .with_agent(&agent_id, |e| {
                e.sm.lock().expect("state mutex").set_terminal(final_state.clone())
            })
            .flatten();
        if let Some(s) = transition {
            registry.update_info(&agent_id, |i| i.state = s.clone());
            bus.publish(DaemonEvent::StateChange {
                agent_id: agent_id.clone(),
                state: s,
            });
        }
        registry.update_info(&agent_id, |i| i.state = final_state);

        bus.publish(DaemonEvent::AgentExited {
            agent_id: agent_id.clone(),
            code,
        });

        // Schedule auto-removal: 5 min for clean exits, 15 min otherwise.
        let (registry, bus) = (registry.clone(), bus.clone());
        let delay = if code == 0 {
            Duration::from_secs(5 * 60)
        } else {
            Duration::from_secs(15 * 60)
        };
        let handle = tokio::runtime::Handle::current();
        handle.spawn(async move {
            tokio::time::sleep(delay).await;
            if let Some(_info) = registry.remove(&agent_id) {
                bus.publish(DaemonEvent::AgentRemoved { agent_id });
            }
        });
    });

    // --- kill handle ---------------------------------------------------------
    let killer_cell = Mutex::new(killer_box);
    let killer = KillHandle::new(move || {
        if let Ok(mut k) = killer_cell.lock() {
            let _ = k.kill();
        }
    });

    Ok(PtyHandles { writer, killer })
}

/// Generate a 12-hex-char agent id from the OS RNG with a time fallback.
pub fn generate_agent_id() -> String {
    use std::io::Read as _;
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let mut buf = [0u8; 6];
        if f.read_exact(&mut buf).is_ok() {
            return buf.iter().map(|b| format!("{b:02x}")).collect();
        }
    }
    // Fallback: nanotime + address entropy.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let addr = &nanos as *const u128 as usize as u128;
    let mixed = nanos ^ addr;
    format!("{mixed:024x}")[..12].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_ids_are_12_hex() {
        let a = generate_agent_id();
        assert_eq!(a.len(), 12);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        let b = generate_agent_id();
        assert_ne!(a, b, "ids should be unique");
    }
}
