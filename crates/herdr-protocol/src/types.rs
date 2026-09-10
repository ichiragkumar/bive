//! Wire types for the herdr protocol.

use serde::{Deserialize, Serialize};

/// Daemon-assigned agent identifier (12-char hex).
pub type AgentId = String;

/// Lifecycle/state of an agent as inferred by the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentState {
    Starting,
    Working,
    Idle,
    /// Waiting on user input (prompt detected at stream tail).
    Blocked,
    /// Non-zero exit or detected panic/error signature.
    Errored(String),
    /// Clean exit with the given code.
    Exited(i32),
}

impl AgentState {
    /// Short glyph for TUI/list rendering.
    pub fn glyph(&self) -> &'static str {
        match self {
            AgentState::Starting => "◔",
            AgentState::Working => "●",
            AgentState::Idle => "◌",
            AgentState::Blocked => "■",
            AgentState::Errored(_) => "✖",
            AgentState::Exited(_) => "·",
        }
    }

    /// Stable name for filtering/serialization-friendly displays.
    pub fn name(&self) -> &'static str {
        match self {
            AgentState::Starting => "starting",
            AgentState::Working => "working",
            AgentState::Idle => "idle",
            AgentState::Blocked => "blocked",
            AgentState::Errored(_) => "errored",
            AgentState::Exited(_) => "exited",
        }
    }
}

impl std::fmt::Display for AgentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentState::Errored(e) => write!(f, "errored({e})"),
            AgentState::Exited(c) => write!(f, "exited({c})"),
            other => write!(f, "{}", other.name()),
        }
    }
}

/// Public snapshot of an agent, as returned by `List` and broadcast on spawn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInfo {
    pub id: AgentId,
    pub profile: String,
    /// Full command line, for display only.
    pub command: String,
    pub cwd: String,
    pub state: AgentState,
    pub started_at_unix_ms: u64,
    pub last_output_unix_ms: u64,
    /// Remote host (Phase 4); `None`/empty = local.
    #[serde(default)]
    pub host: Option<String>,
    /// Parent agent id (Phase 5 sub-agents).
    #[serde(default)]
    pub parent: Option<AgentId>,
}

/// Asynchronous notifications broadcast by the daemon to every streaming client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DaemonEvent {
    AgentSpawned {
        info: AgentInfo,
    },
    /// Raw PTY output, UTF-8-lossy, ANSI escape sequences intact.
    AgentOutput {
        agent_id: AgentId,
        payload: String,
    },
    StateChange {
        agent_id: AgentId,
        state: AgentState,
    },
    AgentExited {
        agent_id: AgentId,
        code: i32,
    },
    AgentRemoved {
        agent_id: AgentId,
    },
    /// Structured media the agent signaled via the `HERDR-MEDIA` magic line.
    /// Additive in v0.1 of the protocol (Phase 3); clients that don't render media
    /// can ignore it.
    AgentMedia {
        agent_id: AgentId,
        /// IANA type, e.g. `image/png`.
        mime: String,
        /// Base64-encoded payload (≤ 8 MiB raw after the daemon's size check).
        data_base64: String,
        caption: Option<String>,
    },
}

/// A registered remote host (Phase 4). Agents spawned there look identical to
/// local agents except `AgentInfo::host` is `Some(name)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteHost {
    /// Local alias used in `spawn --host <name>`.
    pub name: String,
    /// SSH destination (`host` or `user@host`).
    pub ssh_target: String,
    pub port: u16,
    /// Overrides the SSH username when set.
    #[serde(default)]
    pub user: Option<String>,
}

/// Client → daemon commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientCommand {
    Ping,
    List,
    /// Subscribe to the global event stream for the lifetime of this connection.
    Events,
    /// Shut the daemon down (kills all agents).
    Shutdown,
    Spawn {
        profile: String,
        cwd: String,
        command: String,
        args: Vec<String>,
        /// Remote host alias (Phase 4); `None` = spawn locally.
        #[serde(default)]
        host: Option<String>,
    },
    Kill {
        agent_id: AgentId,
    },
    /// Inject text into the agent's PTY. `raw=false` appends `\n`.
    SendInput {
        agent_id: AgentId,
        text: String,
        raw: bool,
    },
    /// Store a remote host and connect to it (Phase 4).
    RemoteAdd {
        host: RemoteHost,
    },
    /// Forget a remote host and tear down its bridge.
    RemoteRemove {
        name: String,
    },
    /// List registered remote hosts.
    RemoteList,
    /// Stream one agent's output + state changes + exit on this connection.
    Attach {
        agent_id: AgentId,
    },
    /// Replay up to `max_bytes` of the agent's ring buffer.
    Logs {
        agent_id: AgentId,
        max_bytes: u32,
    },
}

/// Daemon → client replies. Exactly one per request, wrapped in `ResponseEnvelope`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    Ok,
    Err(String),
    /// Spawn succeeded; carries the daemon-assigned agent id.
    AgentCreated {
        agent_id: AgentId,
    },
    AgentList(Vec<AgentInfo>),
    LogChunk {
        payload: String,
    },
    Pong {
        version: String,
        uptime_ms: u64,
        agents: usize,
    },
    /// Registered remote hosts with bridge status (Phase 4).
    RemoteHostList {
        hosts: Vec<(RemoteHost, bool)>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip<
        T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
    >(
        value: T,
    ) {
        let json = serde_json::to_string(&value).expect("serialize");
        let back: T = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, value, "round-trip failed for json: {json}");
    }

    #[test]
    fn roundtrip_agent_states() {
        roundtrip(AgentState::Starting);
        roundtrip(AgentState::Working);
        roundtrip(AgentState::Idle);
        roundtrip(AgentState::Blocked);
        roundtrip(AgentState::Errored("boom".into()));
        roundtrip(AgentState::Exited(0));
    }

    #[test]
    fn roundtrip_daemon_events() {
        let info = AgentInfo {
            id: "abc123def456".into(),
            profile: "claude-code".into(),
            command: "claude --resume".into(),
            cwd: "/tmp".into(),
            state: AgentState::Starting,
            started_at_unix_ms: 1,
            last_output_unix_ms: 2,
            host: None,
            parent: None,
        };
        roundtrip(DaemonEvent::AgentSpawned { info: info.clone() });
        roundtrip(DaemonEvent::AgentOutput {
            agent_id: info.id.clone(),
            payload: "\x1b[32mhello\x1b[0m".into(),
        });
        roundtrip(DaemonEvent::StateChange {
            agent_id: info.id.clone(),
            state: AgentState::Blocked,
        });
        roundtrip(DaemonEvent::AgentExited {
            agent_id: info.id.clone(),
            code: 1,
        });
        roundtrip(DaemonEvent::AgentRemoved {
            agent_id: info.id.clone(),
        });
        roundtrip(DaemonEvent::AgentMedia {
            agent_id: info.id,
            mime: "image/png".into(),
            data_base64: "aGVsbG8=".into(),
            caption: Some("build chart".into()),
        });
    }

    #[test]
    fn roundtrip_client_commands() {
        roundtrip(ClientCommand::Ping);
        roundtrip(ClientCommand::List);
        roundtrip(ClientCommand::Events);
        roundtrip(ClientCommand::Shutdown);
        roundtrip(ClientCommand::Spawn {
            profile: "generic".into(),
            cwd: "/tmp".into(),
            command: "bash".into(),
            args: vec!["-lc".into(), "echo hi".into()],
            host: None,
        });
        roundtrip(ClientCommand::RemoteAdd {
            host: RemoteHost {
                name: "dev".into(),
                ssh_target: "box.example.com".into(),
                port: 22,
                user: Some("ops".into()),
            },
        });
        roundtrip(ClientCommand::RemoteRemove { name: "dev".into() });
        roundtrip(ClientCommand::RemoteList);
        roundtrip(ClientCommand::Kill {
            agent_id: "x".into(),
        });
        roundtrip(ClientCommand::SendInput {
            agent_id: "x".into(),
            text: "echo hi".into(),
            raw: false,
        });
        roundtrip(ClientCommand::Attach {
            agent_id: "x".into(),
        });
        roundtrip(ClientCommand::Logs {
            agent_id: "x".into(),
            max_bytes: 4096,
        });
    }

    #[test]
    fn roundtrip_responses() {
        roundtrip(Response::Ok);
        roundtrip(Response::Err("nope".into()));
        roundtrip(Response::AgentCreated {
            agent_id: "abc123".into(),
        });
        roundtrip(Response::AgentList(vec![]));
        roundtrip(Response::LogChunk {
            payload: "abc".into(),
        });
        roundtrip(Response::RemoteHostList { hosts: vec![] });
        roundtrip(Response::Pong {
            version: "0.1.0".into(),
            uptime_ms: 5,
            agents: 1,
        });
    }

    #[test]
    fn agent_state_glyphs_and_names() {
        assert_eq!(AgentState::Working.glyph(), "●");
        assert_eq!(AgentState::Blocked.name(), "blocked");
        assert_eq!(AgentState::Errored("x".into()).to_string(), "errored(x)");
    }
}
