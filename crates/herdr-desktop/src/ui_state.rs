//! Webview-facing state store: what the React/vanilla UI renders. Maintained
//! purely from daemon events plus explicit `List`/`Logs` replies (no polling).
//!
//! The Tauri shell (feature `tauri`) serializes snapshots of this store into the
//! webview after every change; the frontend never talks to the daemon directly.

use herdr_protocol::{AgentInfo, DaemonEvent};
use std::collections::HashMap;

/// One inline media block in an agent's detail view.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MediaBlock {
    pub mime: String,
    pub data_base64: String,
    pub caption: Option<String>,
}

/// Everything the UI shows for one agent.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentCard {
    pub info: AgentInfo,
    /// Rolling log text (ANSI-stripped by the UI renderer, which re-styles it).
    pub log_tail: String,
    pub media: Vec<MediaBlock>,
}

/// Bound on log text kept per agent in the UI store.
const LOG_TAIL_CHARS: usize = 64 * 1024;

/// The whole store.
#[derive(Debug, Default, Clone)]
pub struct UiStore {
    /// Insertion-ordered fleet.
    pub order: Vec<String>,
    pub agents: HashMap<String, AgentCard>,
    pub connected: bool,
}

impl UiStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply a daemon event; returns `true` if something changed visibly.
    pub fn observe(&mut self, event: &DaemonEvent) -> bool {
        match event {
            DaemonEvent::AgentSpawned { info } => {
                let id = info.id.clone();
                if !self.agents.contains_key(&id) {
                    self.order.push(id.clone());
                    self.agents.insert(
                        id,
                        AgentCard {
                            info: info.clone(),
                            log_tail: String::new(),
                            media: Vec::new(),
                        },
                    );
                } else if let Some(card) = self.agents.get_mut(&id) {
                    card.info = info.clone();
                }
                true
            }
            DaemonEvent::AgentOutput { agent_id, payload } => self.append_log(agent_id, payload),
            DaemonEvent::AgentMedia {
                agent_id,
                mime,
                data_base64,
                caption,
            } => {
                if let Some(card) = self.agents.get_mut(agent_id) {
                    card.media.push(MediaBlock {
                        mime: mime.clone(),
                        data_base64: data_base64.clone(),
                        caption: caption.clone(),
                    });
                    self.append_log(
                        agent_id,
                        &format!(
                            "[media: {mime}{}]\n",
                            caption
                                .as_deref()
                                .map(|c| format!(" — {c}"))
                                .unwrap_or_default()
                        ),
                    );
                    return true;
                }
                false
            }
            DaemonEvent::StateChange { agent_id, state } => {
                if let Some(card) = self.agents.get_mut(agent_id) {
                    if card.info.state != *state {
                        card.info.state = state.clone();
                        return true;
                    }
                }
                false
            }
            DaemonEvent::AgentExited { agent_id, code } => {
                let state = if *code == 0 {
                    herdr_protocol::AgentState::Exited(0)
                } else {
                    herdr_protocol::AgentState::Errored(format!("exit {code}"))
                };
                if let Some(card) = self.agents.get_mut(agent_id) {
                    card.info.state = state;
                    true
                } else {
                    false
                }
            }
            DaemonEvent::AgentRemoved { agent_id } => {
                self.agents.remove(agent_id);
                let before = self.order.len();
                self.order.retain(|x| x != agent_id);
                before != self.order.len()
            }
        }
    }

    fn append_log(&mut self, agent_id: &str, text: &str) -> bool {
        if let Some(card) = self.agents.get_mut(agent_id) {
            card.log_tail.push_str(text);
            let len = card.log_tail.chars().count();
            if len > LOG_TAIL_CHARS {
                let skip = len - LOG_TAIL_CHARS;
                let offset = card
                    .log_tail
                    .char_indices()
                    .nth(skip)
                    .map(|(i, _)| i)
                    .unwrap_or(card.log_tail.len());
                card.log_tail.drain(..offset);
            }
            return true;
        }
        false
    }

    /// Replace the fleet from a `List` reply (startup, reconnect). Log tails are
    /// preserved; unknown agents are added, stale ones dropped.
    pub fn sync_from(&mut self, agents: Vec<AgentInfo>) {
        let selected_ids: Vec<String> = agents.iter().map(|a| a.id.clone()).collect();
        for info in agents {
            let id = info.id.clone();
            match self.agents.get_mut(&id) {
                Some(card) => card.info = info,
                None => {
                    self.order.push(id.clone());
                    self.agents.insert(
                        id,
                        AgentCard {
                            info,
                            log_tail: String::new(),
                            media: Vec::new(),
                        },
                    );
                }
            }
        }
        self.order.retain(|id| selected_ids.contains(id));
        self.agents.retain(|id, _| selected_ids.contains(id));
    }

    /// Latest state snapshot (cloned) for serialization into the webview.
    pub fn cards(&self) -> Vec<AgentCard> {
        self.order
            .iter()
            .filter_map(|id| self.agents.get(id))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use herdr_protocol::AgentState;

    fn info(id: &str, state: AgentState) -> AgentInfo {
        AgentInfo {
            id: id.into(),
            profile: "generic".into(),
            command: "bash".into(),
            cwd: "/tmp".into(),
            state,
            started_at_unix_ms: 1,
            last_output_unix_ms: 2,
            host: None,
            parent: None,
        }
    }

    #[test]
    fn spawn_output_state_media_removed_lifecycle() {
        let mut s = UiStore::new();
        assert!(s.observe(&DaemonEvent::AgentSpawned {
            info: info("a", AgentState::Starting)
        }));
        assert!(s.observe(&DaemonEvent::AgentOutput {
            agent_id: "a".into(),
            payload: "line one\n".into(),
        }));
        assert!(s.observe(&DaemonEvent::StateChange {
            agent_id: "a".into(),
            state: AgentState::Working,
        }));
        assert!(s.observe(&DaemonEvent::AgentMedia {
            agent_id: "a".into(),
            mime: "image/png".into(),
            data_base64: "aGk=".into(),
            caption: Some("chart".into()),
        }));
        assert!(s.observe(&DaemonEvent::AgentRemoved {
            agent_id: "a".into()
        }));

        let cards = s.cards();
        assert!(cards.is_empty());
    }

    #[test]
    fn media_blocks_collect_in_order() {
        let mut s = UiStore::new();
        s.observe(&DaemonEvent::AgentSpawned {
            info: info("a", AgentState::Working),
        });
        for mime in ["image/png", "image/svg+xml"] {
            s.observe(&DaemonEvent::AgentMedia {
                agent_id: "a".into(),
                mime: mime.into(),
                data_base64: "aGk=".into(),
                caption: None,
            });
        }
        let card = &s.cards()[0];
        assert_eq!(card.media.len(), 2);
        assert_eq!(card.media[0].mime, "image/png");
        assert_eq!(card.media[1].mime, "image/svg+xml");
    }

    #[test]
    fn log_tail_is_bounded() {
        let mut s = UiStore::new();
        s.observe(&DaemonEvent::AgentSpawned {
            info: info("a", AgentState::Working),
        });
        let big = "x".repeat(200_000);
        s.observe(&DaemonEvent::AgentOutput {
            agent_id: "a".into(),
            payload: big,
        });
        let card = &s.cards()[0];
        assert!(card.log_tail.chars().count() <= LOG_TAIL_CHARS);
    }

    #[test]
    fn sync_preserves_logs_and_drops_stale() {
        let mut s = UiStore::new();
        s.observe(&DaemonEvent::AgentSpawned {
            info: info("a", AgentState::Working),
        });
        s.observe(&DaemonEvent::AgentSpawned {
            info: info("b", AgentState::Working),
        });
        s.observe(&DaemonEvent::AgentOutput {
            agent_id: "a".into(),
            payload: "keepme\n".into(),
        });

        // Daemon now reports only `b` (a vanished).
        s.sync_from(vec![info("b", AgentState::Blocked)]);
        let ids: Vec<&str> = s.order.iter().map(String::as_str).collect();
        assert_eq!(ids, ["b"]);
        // b's state was refreshed by the list.
        assert_eq!(s.agents["b"].info.state, AgentState::Blocked);
        // Re-adding `a` starts a fresh card (no stale log resurrection).
        s.sync_from(vec![
            info("a", AgentState::Working),
            info("b", AgentState::Blocked),
        ]);
        assert_eq!(s.agents["a"].log_tail, "");
    }

    #[test]
    fn duplicate_spawn_does_not_duplicate_card() {
        let mut s = UiStore::new();
        s.observe(&DaemonEvent::AgentSpawned {
            info: info("a", AgentState::Starting),
        });
        s.observe(&DaemonEvent::AgentSpawned {
            info: info("a", AgentState::Working),
        });
        assert_eq!(s.order.len(), 1);
        assert_eq!(s.agents["a"].info.state, AgentState::Working);
    }
}
