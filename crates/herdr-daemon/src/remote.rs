//! Daemon-side remote integration: bridge table + remote-aware command routing.
//!
//! The [`RemoteManager`] owns one [`herdr_remote::Bridge`] per registered host.
//! The supervisor consults it for every command: tagged agent ids (`dev:abc`)
//! are forwarded to that host's runner with the tag stripped, and replies are
//! re-stamped with the tagged id so clients only ever see daemon-global ids.
//! Local commands pass through untouched.

use std::sync::{Arc, Mutex};

use herdr_protocol::{ClientCommand, DaemonEvent, RemoteHost, Response};

use herdr_remote::{split_tagged_id, Bridge, EventSink, ProcessSshTransport};

/// One bridge per registered host.
#[derive(Default)]
pub struct RemoteManager {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    hosts: Vec<HostEntry>,
}

struct HostEntry {
    host: RemoteHost,
    bridge: Arc<Bridge>,
}

impl RemoteManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register (or replace) a host and start its bridge. Returns the previous
    /// registration with the same alias, if any.
    pub fn add_host(&self, host: RemoteHost, sink: EventSink) -> Option<RemoteHost> {
        let transport = ProcessSshTransport::new(host.port, host.user.as_deref());
        let bridge = Arc::new(Bridge::new(host.clone(), Box::new(transport)));
        let entry = HostEntry {
            host: host.clone(),
            bridge: bridge.clone(),
        };
        let prev = {
            let mut inner = self.inner.lock().unwrap();
            let prev = inner.hosts.iter().position(|e| e.host.name == host.name);
            let old = prev.map(|i| inner.hosts.remove(i));
            inner.hosts.push(entry);
            old.map(|e| e.host)
        };
        bridge.run(sink); // detached; aborts via RemoteRemove or process exit
        prev
    }

    /// Tear down a host's bridge and forget it. Returns false if unknown.
    pub fn remove_host(&self, name: &str) -> bool {
        let removed = {
            let mut inner = self.inner.lock().unwrap();
            let pos = inner.hosts.iter().position(|e| e.host.name == name);
            pos.map(|i| inner.hosts.remove(i))
        };
        if let Some(e) = removed {
            e.bridge.abort();
            true
        } else {
            false
        }
    }

    /// Is this host alias registered?
    pub fn is_known_host(&self, name: &str) -> bool {
        self.inner
            .lock()
            .unwrap()
            .hosts
            .iter()
            .any(|e| e.host.name == name)
    }

    /// Is this (tagged) agent id owned by one of the registered hosts?
    pub fn is_remote_id(&self, agent_id: &str) -> bool {
        let Some((host, _)) = split_tagged_id(agent_id) else {
            return false;
        };
        self.inner
            .lock()
            .unwrap()
            .hosts
            .iter()
            .any(|e| e.host.name == host)
    }

    /// Registered hosts with bridge status, sorted by alias.
    pub fn list(&self) -> Vec<(RemoteHost, bool)> {
        let inner = self.inner.lock().unwrap();
        let mut out: Vec<_> = inner
            .hosts
            .iter()
            .map(|e| (e.host.clone(), e.bridge.is_connected()))
            .collect();
        out.sort_by(|a, b| a.0.name.cmp(&b.0.name));
        out
    }

    /// Route a command to a remote host if it targets one; `None` means the
    /// command is local (or its host is unknown — the supervisor decides).
    pub fn forward_if_remote(&self, cmd: ClientCommand) -> Option<Response> {
        let (host_name, untagged) = split(&cmd)?;
        let bridge = {
            let inner = self.inner.lock().unwrap();
            inner
                .hosts
                .iter()
                .find(|e| e.host.name == host_name)
                .map(|e| e.bridge.clone())
        }?;
        Some(restamp(bridge.request(untagged), &host_name))
    }

    /// Handle the Phase 4 control commands (`RemoteAdd/Remove/List`); `None` if
    /// `cmd` is not one of them.
    pub fn handle_control(
        &self,
        cmd: ClientCommand,
        sink_for: impl Fn(&RemoteHost) -> EventSink,
    ) -> Option<Response> {
        match cmd {
            ClientCommand::RemoteAdd { host } => {
                let sink = sink_for(&host);
                self.add_host(host, sink);
                Some(Response::Ok)
            }
            ClientCommand::RemoteRemove { name } => {
                if self.remove_host(&name) {
                    Some(Response::Ok)
                } else {
                    Some(Response::Err(format!("unknown remote host {name}")))
                }
            }
            ClientCommand::RemoteList => Some(Response::RemoteHostList { hosts: self.list() }),
            _ => None,
        }
    }
}

/// Extract the (host, untagged command) pair for commands that target a host or
/// a tagged remote agent id.
fn split(cmd: &ClientCommand) -> Option<(String, ClientCommand)> {
    match cmd.clone() {
        ClientCommand::Spawn {
            host: Some(host),
            profile,
            cwd,
            command,
            args,
        } => Some((
            host,
            ClientCommand::Spawn {
                host: None,
                profile,
                cwd,
                command,
                args,
            },
        )),
        ClientCommand::Kill { agent_id } => {
            let (h, remote) = split_tagged_id(&agent_id)?;
            Some((h, ClientCommand::Kill { agent_id: remote }))
        }
        ClientCommand::SendInput {
            agent_id,
            text,
            raw,
        } => {
            let (h, remote) = split_tagged_id(&agent_id)?;
            Some((
                h,
                ClientCommand::SendInput {
                    agent_id: remote,
                    text,
                    raw,
                },
            ))
        }
        ClientCommand::Logs {
            agent_id,
            max_bytes,
        } => {
            let (h, remote) = split_tagged_id(&agent_id)?;
            Some((
                h,
                ClientCommand::Logs {
                    agent_id: remote,
                    max_bytes,
                },
            ))
        }
        ClientCommand::Attach { agent_id } => {
            let (h, remote) = split_tagged_id(&agent_id)?;
            Some((h, ClientCommand::Attach { agent_id: remote }))
        }
        _ => None,
    }
}

/// Re-stamp remote replies so clients see daemon-global (tagged) ids.
fn restamp(resp: Result<Response, String>, host: &str) -> Response {
    let tag = |id: &str| herdr_remote::tag_id(host, id);
    match resp {
        Ok(Response::AgentCreated { agent_id }) => Response::AgentCreated {
            agent_id: tag(&agent_id),
        },
        Ok(other) => other,
        Err(e) => Response::Err(e),
    }
}

/// Events the daemon should publish when a host is removed (its agents vanish
/// from the fleet).
pub fn removal_events(host: &str, agent_ids: Vec<String>) -> Vec<DaemonEvent> {
    agent_ids
        .into_iter()
        .map(|id| DaemonEvent::AgentRemoved {
            agent_id: herdr_remote::tag_id(host, &id),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Split/untag routing logic without any network.
    #[test]
    fn split_routes_tagged_commands_only() {
        let (h, cmd) = split(&ClientCommand::Kill {
            agent_id: "dev:abc".into(),
        })
        .unwrap();
        assert_eq!(h, "dev");
        assert_eq!(
            cmd,
            ClientCommand::Kill {
                agent_id: "abc".into()
            }
        );

        // Untagged (local) commands are not routed.
        assert!(split(&ClientCommand::Kill {
            agent_id: "abc".into()
        })
        .is_none());
        assert!(split(&ClientCommand::Ping).is_none());

        // Spawn with a host alias routes and clears the host field.
        let (h, cmd) = split(&ClientCommand::Spawn {
            host: Some("box".into()),
            profile: "generic".into(),
            cwd: "/tmp".into(),
            command: "bash".into(),
            args: vec![],
        })
        .unwrap();
        assert_eq!(h, "box");
        match cmd {
            ClientCommand::Spawn { host, .. } => assert_eq!(host, None),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn restamp_tags_spawned_ids_and_errors_pass_through() {
        let r = restamp(
            Ok(Response::AgentCreated {
                agent_id: "abc".into(),
            }),
            "dev",
        );
        assert_eq!(
            r,
            Response::AgentCreated {
                agent_id: "dev:abc".into()
            }
        );
        let r = restamp(Err("timed out".into()), "dev");
        assert_eq!(r, Response::Err("timed out".into()));
    }

    #[test]
    fn removal_events_tag_ids() {
        let evs = removal_events("dev", vec!["abc".into()]);
        match &evs[0] {
            DaemonEvent::AgentRemoved { agent_id } => assert_eq!(agent_id, "dev:abc"),
            other => panic!("{other:?}"),
        }
    }

    /// With a registered but never-connected bridge, forwarded commands fail
    /// with a clean `Err`; unknown hosts are simply not routed.
    /// (`#[tokio::test]`: `Bridge::run` uses `spawn_blocking`, which requires a
    /// runtime context.)
    #[tokio::test]
    async fn forward_on_unknown_host_or_down_bridge() {
        let mgr = RemoteManager::new();
        assert!(mgr
            .forward_if_remote(ClientCommand::Kill {
                agent_id: "ghost:abc".into()
            })
            .is_none());
        mgr.add_host(
            RemoteHost {
                name: "dev".into(),
                ssh_target: "dev.example.com".into(),
                port: 22,
                user: None,
            },
            Arc::new(|_| {}),
        );
        // The bridge starts disconnected (its ssh spawn fails in tests), so the
        // fast-fail path answers immediately.
        let resp = mgr.forward_if_remote(ClientCommand::Kill {
            agent_id: "dev:abc".into(),
        });
        assert!(matches!(resp, Some(Response::Err(_))));
        assert!(mgr.remove_host("dev"));
        assert!(!mgr.remove_host("dev"));
    }

    #[test]
    fn control_commands_route_through_manager() {
        let mgr = RemoteManager::new();
        let no_sink = |_: &RemoteHost| Arc::new(move |_: DaemonEvent| {}) as EventSink;
        assert!(matches!(
            mgr.handle_control(ClientCommand::RemoteList, no_sink),
            Some(Response::RemoteHostList { hosts }) if hosts.is_empty()
        ));
        assert!(matches!(
            mgr.handle_control(ClientCommand::RemoteRemove { name: "x".into() }, no_sink),
            Some(Response::Err(_))
        ));
    }
}
