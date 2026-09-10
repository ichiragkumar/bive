//! The supervisor: owns the registry + bus and implements every `ClientCommand`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::watch;

use herdr_protocol::{
    AgentId, AgentInfo, AgentProfile, AgentState, ClientCommand, DaemonEvent, Response,
};

use crate::bus::EventBus;
use crate::pty::{generate_agent_id, spawn_agent, SpawnContext, SpawnSpec};
use crate::registry::{now_unix_ms, AgentEntry, Registry};

pub struct Supervisor {
    pub registry: Arc<Registry>,
    pub bus: EventBus,
    started: Instant,
    shutdown_tx: watch::Sender<bool>,
}

impl Supervisor {
    pub fn new() -> Self {
        let (shutdown_tx, _) = watch::channel(false);
        Self {
            registry: Arc::new(Registry::new()),
            bus: EventBus::new(),
            started: Instant::now(),
            shutdown_tx,
        }
    }

    /// Watch channel that fires `true` when the daemon should exit
    /// (via `Shutdown` command or SIGINT/SIGTERM).
    pub fn subscribe_shutdown(&self) -> watch::Receiver<bool> {
        self.shutdown_tx.subscribe()
    }

    /// Signal the daemon main loop to exit.
    pub fn signal_shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }

    pub fn uptime_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    /// Central command dispatch. Infallible at the type level: every failure is a
    /// `Response::Err` so clients always get exactly one reply per request.
    pub async fn handle(&self, cmd: ClientCommand) -> Response {
        match cmd {
            ClientCommand::Ping => Response::Pong {
                version: herdr_protocol::PROTOCOL_VERSION.into(),
                uptime_ms: self.uptime_ms(),
                agents: self.registry.len(),
            },
            ClientCommand::List => Response::AgentList(self.registry.list()),
            ClientCommand::Shutdown => {
                self.shutdown_all().await;
                self.signal_shutdown();
                Response::Ok
            }
            ClientCommand::Spawn {
                profile,
                cwd,
                command,
                args,
            } => self.spawn(profile, cwd, command, args).await,
            ClientCommand::Kill { agent_id } => self.kill(&agent_id).await,
            ClientCommand::SendInput {
                agent_id,
                text,
                raw,
            } => self.send_input(&agent_id, &text, raw),
            ClientCommand::Attach { agent_id } => {
                if self.registry.contains(&agent_id) {
                    Response::Ok
                } else {
                    Response::Err(format!("unknown agent {agent_id}"))
                }
            }
            ClientCommand::Logs { agent_id, max_bytes } => self.logs(&agent_id, max_bytes),
            ClientCommand::Events => Response::Ok, // connection is now streaming
        }
    }

    async fn spawn(
        &self,
        profile_name: String,
        cwd: String,
        command: String,
        args: Vec<String>,
    ) -> Response {
        let profile = AgentProfile::builtin(&profile_name)
            .unwrap_or_else(AgentProfile::generic)
            .clone();

        let cwd_path = PathBuf::from(&cwd);
        if !cwd_path.is_dir() {
            return Response::Err(format!("cwd does not exist or is not a directory: {cwd}"));
        }

        let agent_id: AgentId = generate_agent_id();
        let info = AgentInfo {
            id: agent_id.clone(),
            profile: profile.name.clone(),
            command: format!("{command} {}", args.join(" ")).trim().to_string(),
            cwd: cwd.clone(),
            state: AgentState::Starting,
            started_at_unix_ms: now_unix_ms(),
            last_output_unix_ms: now_unix_ms(),
            host: None,
            parent: None,
        };

        let entry = AgentEntry::new(info.clone(), &profile);
        self.registry.insert(entry);

        let ctx = SpawnContext {
            registry: self.registry.clone(),
            bus: self.bus.clone(),
        };
        let spec = SpawnSpec {
            agent_id: agent_id.clone(),
            cwd: cwd_path,
            command,
            args,
        };

        match spawn_agent(spec, ctx) {
            Ok(handles) => {
                self.registry.with_agent(&agent_id, |e| {
                    e.writer = Some(std::sync::Mutex::new(handles.writer));
                    e.killer = Some(handles.killer);
                });
                self.bus.publish(DaemonEvent::AgentSpawned { info });
                Response::AgentCreated { agent_id }
            }
            Err(e) => {
                self.registry.remove(&agent_id);
                Response::Err(format!("spawn failed: {e:#}"))
            }
        }
    }

    async fn kill(&self, agent_id: &str) -> Response {
        let killer = self
            .registry
            .with_agent(agent_id, |e| e.killer.take())
            .flatten();
        match killer {
            Some(handle) => {
                handle.kill();
                Response::Ok
            }
            None => {
                if self.registry.contains(agent_id) {
                    Response::Err(format!("agent {agent_id} has already exited"))
                } else {
                    Response::Err(format!("unknown agent {agent_id}"))
                }
            }
        }
    }

    fn send_input(&self, agent_id: &str, text: &str, raw: bool) -> Response {
        if !self.registry.contains(agent_id) {
            return Response::Err(format!("unknown agent {agent_id}"));
        }
        let payload = if raw { text.to_string() } else { format!("{text}\n") };
        let bytes = payload.into_bytes();

        let written = self.registry.with_agent(agent_id, |e| match &e.writer {
            Some(w) => {
                let mut w = w.lock().expect("writer mutex poisoned");
                std::io::Write::write_all(&mut *w, &bytes)
                    .and_then(|_| std::io::Write::flush(&mut *w))
                    .is_ok()
            }
            None => false,
        });

        match written {
            Some(true) => Response::Ok,
            Some(false) => Response::Err(format!("agent {agent_id} cannot accept input (exited?)")),
            None => Response::Err(format!("unknown agent {agent_id}")),
        }
    }

    fn logs(&self, agent_id: &str, max_bytes: u32) -> Response {
        let tail = self.registry.with_agent(agent_id, |e| e.ring_tail(max_bytes as usize));
        match tail {
            Some(payload) => Response::LogChunk { payload },
            None => Response::Err(format!("unknown agent {agent_id}")),
        }
    }

    /// Kill every agent and publish removals. Used by `Shutdown` and signal handlers.
    pub async fn shutdown_all(&self) {
        let ids = self.registry.all_ids();
        for id in ids {
            if let Some(killer) = self
                .registry
                .with_agent(&id, |e| e.killer.take())
                .flatten()
            {
                killer.kill();
            }
        }
        // Give children a moment to die before the process exits.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        for id in self.registry.all_ids() {
            if let Some(_info) = self.registry.remove(&id) {
                self.bus.publish(DaemonEvent::AgentRemoved { agent_id: id });
            }
        }
    }

    /// Periodic idle check for all agents; emits StateChange events for transitions.
    pub fn tick_idle(&self) {
        let ids = self.registry.all_ids();
        for id in ids {
            let transition = self
                .registry
                .with_agent(&id, |e| e.sm.lock().expect("state mutex").check_idle())
                .flatten();
            if let Some(state) = transition {
                self.registry.update_info(&id, |i| i.state = state.clone());
                self.bus.publish(DaemonEvent::StateChange {
                    agent_id: id,
                    state,
                });
            }
        }
    }
}

impl Default for Supervisor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ping_pong() {
        let sup = Supervisor::new();
        match sup.handle(ClientCommand::Ping).await {
            Response::Pong { agents, .. } => assert_eq!(agents, 0),
            other => panic!("expected Pong, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn spawn_rejects_bad_cwd() {
        let sup = Supervisor::new();
        let resp = sup
            .handle(ClientCommand::Spawn {
                profile: "generic".into(),
                cwd: "/nonexistent-herdr-test".into(),
                command: "bash".into(),
                args: vec![],
            })
            .await;
        assert!(matches!(resp, Response::Err(_)));
    }

    #[tokio::test]
    async fn send_input_unknown_agent() {
        let sup = Supervisor::new();
        let resp = sup
            .handle(ClientCommand::SendInput {
                agent_id: "nope".into(),
                text: "hi".into(),
                raw: false,
            })
            .await;
        assert!(matches!(resp, Response::Err(_)));
    }

    #[tokio::test]
    async fn logs_unknown_agent() {
        let sup = Supervisor::new();
        let resp = sup
            .handle(ClientCommand::Logs {
                agent_id: "nope".into(),
                max_bytes: 100,
            })
            .await;
        assert!(matches!(resp, Response::Err(_)));
    }
}
