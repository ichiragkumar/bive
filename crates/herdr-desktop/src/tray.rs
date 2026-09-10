//! System-tray aggregation: one overall health dot for the whole fleet plus the
//! counts behind the tray tooltip/menu. Pure logic; the Tauri shell (feature
//! `tauri`) maps the dot to a real tray icon.

use herdr_protocol::{AgentInfo, AgentState, DaemonEvent};
use std::collections::HashMap;

/// Traffic-light aggregate for the fleet.
///
/// Priority order (first match wins): daemon unreachable beats everything,
/// then errored > blocked > healthy > empty. Documented so tray, tooltip,
/// and webview header all agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dot {
    /// Daemon unreachable — distinct from "no agents" (Gray).
    Offline,
    /// Nothing running.
    Gray,
    /// All agents fine (working/idle/exited).
    Green,
    /// At least one agent blocked (needs attention).
    Amber,
    /// At least one agent errored.
    Red,
}

/// Current fleet picture as shown in the tray.
#[derive(Debug, Clone)]
pub struct TrayState {
    pub agents: HashMap<String, AgentState>,
    pub connected: bool,
}

impl Default for TrayState {
    fn default() -> Self {
        Self {
            agents: HashMap::new(),
            connected: true,
        }
    }
}

impl TrayState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_connected(&mut self, connected: bool) {
        self.connected = connected;
    }

    /// Overall dot: offline > red > amber > green > gray.
    pub fn dot(&self) -> Dot {
        if !self.connected {
            return Dot::Offline;
        }
        let mut dot = if self.agents.is_empty() {
            Dot::Gray
        } else {
            Dot::Green
        };
        for state in self.agents.values() {
            match state {
                AgentState::Errored(_) => return Dot::Red,
                AgentState::Blocked => dot = Dot::Amber,
                _ => {}
            }
        }
        dot
    }

    /// `(total, working, blocked, errored)` for the tooltip.
    pub fn counts(&self) -> (usize, usize, usize, usize) {
        let mut working = 0;
        let mut blocked = 0;
        let mut errored = 0;
        for s in self.agents.values() {
            match s {
                AgentState::Working | AgentState::Starting => working += 1,
                AgentState::Blocked => blocked += 1,
                AgentState::Errored(_) => errored += 1,
                _ => {}
            }
        }
        (self.agents.len(), working, blocked, errored)
    }

    /// Tooltip text, e.g. `herdr — 3 agents (1 working, 1 blocked)`.
    pub fn tooltip(&self) -> String {
        if !self.connected {
            return "herdr — daemon unreachable".into();
        }
        let (total, working, blocked, errored) = self.counts();
        if total == 0 {
            return "herdr — no agents".into();
        }
        let mut bits = Vec::new();
        if working > 0 {
            bits.push(format!("{working} working"));
        }
        if blocked > 0 {
            bits.push(format!("{blocked} blocked"));
        }
        if errored > 0 {
            bits.push(format!("{errored} errored"));
        }
        let idle = total - working - blocked - errored;
        if idle > 0 {
            bits.push(format!("{idle} idle"));
        }
        format!(
            "herdr — {total} agent{} ({})",
            if total == 1 { "" } else { "s" },
            bits.join(", ")
        )
    }

    /// Maintain state from the event stream (event-driven — the tray never polls).
    pub fn observe(&mut self, event: &DaemonEvent) {
        match event {
            DaemonEvent::AgentSpawned { info } => {
                self.agents.insert(info.id.clone(), info.state.clone());
            }
            DaemonEvent::StateChange { agent_id, state } => {
                if let Some(s) = self.agents.get_mut(agent_id) {
                    *s = state.clone();
                }
            }
            DaemonEvent::AgentExited { agent_id, code } => {
                if let Some(s) = self.agents.get_mut(agent_id) {
                    *s = if *code == 0 {
                        AgentState::Exited(0)
                    } else {
                        AgentState::Errored(format!("exit {code}"))
                    };
                }
            }
            DaemonEvent::AgentRemoved { agent_id } => {
                self.agents.remove(agent_id);
            }
            DaemonEvent::AgentOutput { .. } | DaemonEvent::AgentMedia { .. } => {}
        }
    }

    /// Rebuild from a `List` response (startup / reconnect resync).
    pub fn sync_from(&mut self, agents: &[AgentInfo]) {
        self.agents = agents
            .iter()
            .map(|a| (a.id.clone(), a.state.clone()))
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: &str, state: AgentState) -> AgentInfo {
        AgentInfo {
            id: id.into(),
            profile: "generic".into(),
            command: "bash".into(),
            cwd: "/tmp".into(),
            state,
            started_at_unix_ms: 0,
            last_output_unix_ms: 0,
            host: None,
            parent: None,
        }
    }

    #[test]
    fn empty_is_gray() {
        let t = TrayState::new();
        assert_eq!(t.dot(), Dot::Gray);
        assert_eq!(t.tooltip(), "herdr — no agents");
    }

    #[test]
    fn severity_order_red_over_amber_over_green() {
        let mut t = TrayState::new();
        t.sync_from(&[info("a", AgentState::Working)]);
        assert_eq!(t.dot(), Dot::Green);

        t.observe(&DaemonEvent::StateChange {
            agent_id: "a".into(),
            state: AgentState::Blocked,
        });
        assert_eq!(t.dot(), Dot::Amber);

        t.observe(&DaemonEvent::StateChange {
            agent_id: "a".into(),
            state: AgentState::Errored("boom".into()),
        });
        assert_eq!(t.dot(), Dot::Red);

        t.observe(&DaemonEvent::StateChange {
            agent_id: "a".into(),
            state: AgentState::Working,
        });
        assert_eq!(t.dot(), Dot::Green);
    }

    #[test]
    fn counts_and_tooltip_reflect_fleet() {
        let mut t = TrayState::new();
        t.sync_from(&[
            info("a", AgentState::Working),
            info("b", AgentState::Blocked),
            info("c", AgentState::Idle),
        ]);
        assert_eq!(t.counts(), (3, 1, 1, 0));
        assert_eq!(
            t.tooltip(),
            "herdr — 3 agents (1 working, 1 blocked, 1 idle)"
        );
    }

    #[test]
    fn removal_back_to_gray() {
        let mut t = TrayState::new();
        t.sync_from(&[info("a", AgentState::Idle)]);
        t.observe(&DaemonEvent::AgentRemoved {
            agent_id: "a".into(),
        });
        assert_eq!(t.dot(), Dot::Gray);
    }

    #[test]
    fn offline_beats_everything() {
        let mut t = TrayState::new();
        t.sync_from(&[
            info("a", AgentState::Working),
            info("b", AgentState::Errored("x".into())),
        ]);
        assert_eq!(t.dot(), Dot::Red);
        t.set_connected(false);
        assert_eq!(t.dot(), Dot::Offline);
        assert_eq!(t.tooltip(), "herdr — daemon unreachable");
        t.set_connected(true);
        assert_eq!(t.dot(), Dot::Red);
    }
}
