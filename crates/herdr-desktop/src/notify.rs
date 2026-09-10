//! Notification logic: fire an OS notification when an agent enters `Blocked`
//! (needs attention) or `Errored`, suppressing duplicates per (agent, state
//! bucket) until the agent's state leaves that bucket.

use herdr_protocol::{AgentId, AgentState, DaemonEvent};
use std::collections::HashSet;

/// The desktop-facing presentation of an attention-worthy state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub agent_id: AgentId,
    pub title: String,
    pub body: String,
}

/// Deduplicating filter between the daemon's event stream and the OS notifier.
#[derive(Debug, Default)]
pub struct NotificationFilter {
    /// (agent, bucket) pairs already notified and still current.
    active: HashSet<(AgentId, Bucket)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Bucket {
    Blocked,
    Errored,
}

impl NotificationFilter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Update the filter with a daemon event; returns the notification to show,
    /// if any. Non-attention events just clear the relevant dedup entries.
    pub fn observe(&mut self, event: &DaemonEvent) -> Option<Notification> {
        match event {
            DaemonEvent::StateChange { agent_id, state } => self.observe_state(agent_id, state),
            DaemonEvent::AgentExited { agent_id, code } => {
                // Exiting with an error is worth surfacing once.
                if *code != 0 {
                    self.observe_state(agent_id, &AgentState::Errored(format!("exit {code}")))
                } else {
                    self.clear_agent(agent_id);
                    None
                }
            }
            DaemonEvent::AgentRemoved { agent_id } => {
                self.clear_agent(agent_id);
                None
            }
            _ => None,
        }
    }

    fn observe_state(&mut self, agent_id: &AgentId, state: &AgentState) -> Option<Notification> {
        let bucket = match state {
            AgentState::Blocked => Bucket::Blocked,
            AgentState::Errored(_) => Bucket::Errored,
            _ => {
                self.clear_agent(agent_id);
                return None;
            }
        };
        if self.active.insert((agent_id.clone(), bucket)) {
            return Some(Notification {
                agent_id: agent_id.clone(),
                title: match bucket {
                    Bucket::Blocked => "Agent needs input".into(),
                    Bucket::Errored => "Agent errored".into(),
                },
                body: match state {
                    AgentState::Errored(reason) => format!("{agent_id}: {reason}"),
                    _ => format!("{agent_id} is waiting for your input"),
                },
            });
        }
        None
    }

    fn clear_agent(&mut self, agent_id: &AgentId) {
        self.active.retain(|(a, _)| a != agent_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocked_notifies_once_until_leaving_bucket() {
        let mut f = NotificationFilter::new();
        assert!(f.observe(&ev_blocked("a")).is_some());
        assert!(
            f.observe(&ev_blocked("a")).is_none(),
            "duplicate suppressed"
        );
        // Leaving the bucket re-arms it.
        assert!(f.observe(&ev_state("a", AgentState::Working)).is_none());
        assert!(f.observe(&ev_blocked("a")).is_some());
    }

    #[test]
    fn errored_and_blocked_are_separate_buckets() {
        let mut f = NotificationFilter::new();
        assert!(f.observe(&ev_blocked("a")).is_some());
        assert!(f
            .observe(&ev_state("a", AgentState::Errored("boom".into())))
            .is_some());
        // Both active: neither re-fires.
        assert!(f.observe(&ev_blocked("a")).is_none());
        assert!(f
            .observe(&ev_state("a", AgentState::Errored("boom".into())))
            .is_none());
    }

    #[test]
    fn removal_clears_dedup() {
        let mut f = NotificationFilter::new();
        assert!(f.observe(&ev_blocked("a")).is_some());
        f.observe(&DaemonEvent::AgentRemoved {
            agent_id: "a".into(),
        });
        assert!(
            f.observe(&ev_blocked("a")).is_some(),
            "re-armed after removal"
        );
    }

    #[test]
    fn error_exit_notifies_once_clean_exit_does_not() {
        let mut f = NotificationFilter::new();
        assert!(f
            .observe(&DaemonEvent::AgentExited {
                agent_id: "a".into(),
                code: 137
            })
            .is_some());
        assert!(f
            .observe(&DaemonEvent::AgentExited {
                agent_id: "a".into(),
                code: 137
            })
            .is_none());
        assert!(f
            .observe(&DaemonEvent::AgentExited {
                agent_id: "b".into(),
                code: 0
            })
            .is_none());
    }

    #[test]
    fn unrelated_events_are_ignored() {
        let mut f = NotificationFilter::new();
        assert!(f
            .observe(&DaemonEvent::AgentOutput {
                agent_id: "a".into(),
                payload: "x".into()
            })
            .is_none());
        assert!(f
            .observe(&DaemonEvent::AgentSpawned {
                info: herdr_protocol::AgentInfo {
                    id: "a".into(),
                    profile: "generic".into(),
                    command: "bash".into(),
                    cwd: "/tmp".into(),
                    state: AgentState::Starting,
                    started_at_unix_ms: 0,
                    last_output_unix_ms: 0,
                    host: None,
                    parent: None,
                }
            })
            .is_none());
    }

    fn ev_state(id: &str, state: AgentState) -> DaemonEvent {
        DaemonEvent::StateChange {
            agent_id: id.into(),
            state,
        }
    }

    fn ev_blocked(id: &str) -> DaemonEvent {
        ev_state(id, AgentState::Blocked)
    }
}
