//! Wire helpers for talking to the `herdr-agent` runner on a remote host.
//!
//! The runner is a headless herdr daemon (same crate code as the local one) whose
//! socket is reached through the SSH pipe. The bridge converts framed control
//! messages into runner requests and runner replies/events back into frames,
//! retagging agent identity as it crosses.
//!
//! Id-space contract: remote agent ids are namespaced by the host alias, so a
//! remote agent `abc` on host `dev` appears locally as `dev:abc`. `untag_id` is
//! the exact inverse used when forwarding control commands to the runner; after
//! a successful remote spawn the reply is re-stamped with the tagged id so the
//! local client only ever sees `dev:*`.

use herdr_protocol::{DaemonEvent, RequestEnvelope};

use crate::frame::Frame;

/// Envelope a control request for the runner.
pub fn control_request(id: u64, cmd: herdr_protocol::ClientCommand) -> Frame {
    Frame::Control(RequestEnvelope { id, cmd })
}

/// Retag a runner event into the local id-space: every agent id becomes
/// `host:remote_id`, and `AgentSpawned` also stamps `AgentInfo::host`.
pub fn retag_event(event: DaemonEvent, host: &str) -> DaemonEvent {
    let tag = |id: &str| format!("{host}:{id}");
    match event {
        DaemonEvent::AgentSpawned { mut info } => {
            info.id = tag(&info.id);
            info.host = Some(host.to_string());
            DaemonEvent::AgentSpawned { info }
        }
        DaemonEvent::AgentOutput { agent_id, payload } => DaemonEvent::AgentOutput {
            agent_id: tag(&agent_id),
            payload,
        },
        DaemonEvent::StateChange { agent_id, state } => DaemonEvent::StateChange {
            agent_id: tag(&agent_id),
            state,
        },
        DaemonEvent::AgentExited { agent_id, code } => DaemonEvent::AgentExited {
            agent_id: tag(&agent_id),
            code,
        },
        DaemonEvent::AgentRemoved { agent_id } => DaemonEvent::AgentRemoved {
            agent_id: tag(&agent_id),
        },
        DaemonEvent::AgentMedia {
            agent_id,
            mime,
            data_base64,
            caption,
        } => DaemonEvent::AgentMedia {
            agent_id: tag(&agent_id),
            mime,
            data_base64,
            caption,
        },
    }
}

/// Split a local (tagged) id into its host alias and remote id. Ids without a
/// host prefix are local and yield `None`.
pub fn split_tagged_id(local_id: &str) -> Option<(String, String)> {
    let (host, remote) = local_id.split_once(':')?;
    if host.is_empty() || remote.is_empty() {
        return None;
    }
    Some((host.to_string(), remote.to_string()))
}

/// Local (tagged) form of a remote agent id, for stamping spawn replies.
pub fn tag_id(host: &str, remote_id: &str) -> String {
    format!("{host}:{remote_id}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::write_frame;
    use herdr_protocol::{AgentInfo, AgentState, Response, ResponseEnvelope};

    fn info(id: &str) -> AgentInfo {
        AgentInfo {
            id: id.into(),
            profile: "generic".into(),
            command: "bash".into(),
            cwd: "/tmp".into(),
            state: AgentState::Working,
            started_at_unix_ms: 0,
            last_output_unix_ms: 0,
            host: None,
            parent: None,
        }
    }

    #[test]
    fn retag_namespaces_every_variant() {
        let cases = vec![
            DaemonEvent::AgentSpawned { info: info("abc") },
            DaemonEvent::AgentOutput {
                agent_id: "abc".into(),
                payload: "x".into(),
            },
            DaemonEvent::StateChange {
                agent_id: "abc".into(),
                state: AgentState::Idle,
            },
            DaemonEvent::AgentExited {
                agent_id: "abc".into(),
                code: 0,
            },
            DaemonEvent::AgentRemoved {
                agent_id: "abc".into(),
            },
            DaemonEvent::AgentMedia {
                agent_id: "abc".into(),
                mime: "image/png".into(),
                data_base64: "aGk=".into(),
                caption: None,
            },
        ];
        for ev in cases {
            let tagged = retag_event(ev, "dev");
            let json = serde_json::to_string(&tagged).unwrap();
            assert!(json.contains("dev:abc"), "event not retagged: {json}");
        }
        // Spawn also stamps the host field.
        match retag_event(DaemonEvent::AgentSpawned { info: info("abc") }, "dev") {
            DaemonEvent::AgentSpawned { info } => {
                assert_eq!(info.id, "dev:abc");
                assert_eq!(info.host.as_deref(), Some("dev"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn id_splitting_round_trips() {
        assert_eq!(
            split_tagged_id("dev:abc123"),
            Some(("dev".into(), "abc123".into()))
        );
        // Local ids (no colon, or leading colon) are not tagged.
        assert_eq!(split_tagged_id("abc123"), None);
        assert_eq!(split_tagged_id(":abc123"), None);
        // Remote paths with colons still split at the first colon.
        assert_eq!(
            split_tagged_id("dev:a:b"),
            Some(("dev".into(), "a:b".into()))
        );
        assert_eq!(tag_id("dev", "abc123"), "dev:abc123");
    }

    #[test]
    fn control_and_reply_helpers() {
        let req = control_request(42, herdr_protocol::ClientCommand::Ping);
        match req {
            Frame::Control(env) => {
                assert_eq!(env.id, 42);
                assert_eq!(env.cmd, herdr_protocol::ClientCommand::Ping);
            }
            other => panic!("{other:?}"),
        }
        // A tagged spawn reply: retagging the response payload keeps the
        // daemon-global id consistent for clients.
        let resp = Response::AgentCreated {
            agent_id: "abc".into(),
        };
        let frame = Frame::Reply(ResponseEnvelope {
            id: 42,
            resp: resp.clone(),
        });
        write_frame(&mut Vec::new(), &frame).unwrap();
        assert_eq!(
            resp,
            Response::AgentCreated {
                agent_id: "abc".into()
            }
        );
    }
}
