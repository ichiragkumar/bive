//! Agent registry: authoritative in-daemon state for every agent.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use herdr_protocol::{AgentId, AgentInfo, AgentProfile};

use crate::state::StateMachine;

/// Per-agent output ring buffer cap (bytes, line-aligned on trim).
const RING_CAP_BYTES: usize = 256 * 1024;

/// Everything the daemon knows about one agent.
pub struct AgentEntry {
    pub info: AgentInfo,
    /// PTY master writer (input injection). Mutex gives the plain `Write` object
    /// the `Sync` bound the registry requires.
    pub writer: Option<Mutex<Box<dyn std::io::Write + Send>>>,
    /// Kill handle; `None` once the child has exited.
    pub killer: Option<KillHandle>,
    pub ring: Mutex<VecDeque<String>>,
    /// State-inference engine for this agent.
    pub sm: Mutex<StateMachine>,
}

/// Opaque process-kill handle owned by the PTY module.
pub struct KillHandle(Box<dyn Fn() + Send + Sync>);

impl KillHandle {
    pub fn new(f: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Box::new(f))
    }

    pub fn kill(&self) {
        (self.0)();
    }
}

impl std::fmt::Debug for KillHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("KillHandle")
    }
}

pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl AgentEntry {
    pub fn new(info: AgentInfo, profile: &AgentProfile) -> Self {
        Self {
            info,
            writer: None,
            killer: None,
            ring: Mutex::new(VecDeque::new()),
            sm: Mutex::new(StateMachine::new(profile)),
        }
    }

    /// Append output to the ring, trimming to cap on line boundaries.
    pub fn push_ring(&self, text: &str) {
        let mut ring = self.ring.lock().expect("ring mutex poisoned");
        ring.push_back(text.to_string());
        let mut total: usize = ring.iter().map(|s| s.len()).sum();
        while total > RING_CAP_BYTES {
            match ring.pop_front() {
                Some(front) => total = total.saturating_sub(front.len()),
                None => break,
            }
        }
    }

    /// Concatenated tail of the ring, up to `max_bytes`.
    pub fn ring_tail(&self, max_bytes: usize) -> String {
        let ring = self.ring.lock().expect("ring mutex poisoned");
        let mut out = String::new();
        for chunk in ring.iter().rev() {
            if out.len() >= max_bytes {
                break;
            }
            out.insert_str(0, chunk);
        }
        if out.len() > max_bytes {
            let skip = out.len() - max_bytes;
            // Snap to a char boundary.
            let boundary = out
                .char_indices()
                .find(|(i, _)| *i >= skip)
                .map(|(i, _)| i)
                .unwrap_or(out.len());
            out.drain(..boundary);
        }
        out
    }
}

/// Thread-safe registry of all agents.
#[derive(Default)]
pub struct Registry {
    agents: std::sync::RwLock<HashMap<AgentId, AgentEntry>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, entry: AgentEntry) {
        self.agents
            .write()
            .expect("registry poisoned")
            .insert(entry.info.id.clone(), entry);
    }

    pub fn remove(&self, agent_id: &str) -> Option<AgentInfo> {
        self.agents
            .write()
            .expect("registry poisoned")
            .remove(agent_id)
            .map(|e| e.info)
    }

    pub fn contains(&self, agent_id: &str) -> bool {
        self.agents
            .read()
            .expect("registry poisoned")
            .contains_key(agent_id)
    }

    pub fn len(&self) -> usize {
        self.agents.read().expect("registry poisoned").len()
    }

    #[allow(clippy::len_without_is_empty)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Snapshot list sorted by start time.
    pub fn list(&self) -> Vec<AgentInfo> {
        let mut infos: Vec<AgentInfo> = self
            .agents
            .read()
            .expect("registry poisoned")
            .values()
            .map(|e| e.info.clone())
            .collect();
        infos.sort_by_key(|i| i.started_at_unix_ms);
        infos
    }

    /// Mutate one agent's info (state updates).
    pub fn update_info<F: FnOnce(&mut AgentInfo)>(
        &self,
        agent_id: &str,
        f: F,
    ) -> Option<AgentInfo> {
        let mut guard = self.agents.write().expect("registry poisoned");
        let entry = guard.get_mut(agent_id)?;
        f(&mut entry.info);
        Some(entry.info.clone())
    }

    /// Run a closure with mutable access to one agent (writer/killer/ring).
    pub fn with_agent<R>(&self, agent_id: &str, f: impl FnOnce(&mut AgentEntry) -> R) -> Option<R> {
        let mut guard = self.agents.write().expect("registry poisoned");
        guard.get_mut(agent_id).map(f)
    }

    /// Collect ids of all agents (for shutdown).
    pub fn all_ids(&self) -> Vec<AgentId> {
        self.agents
            .read()
            .expect("registry poisoned")
            .keys()
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use herdr_protocol::AgentState;

    fn entry(id: &str) -> AgentEntry {
        AgentEntry::new(
            AgentInfo {
                id: id.into(),
                profile: "generic".into(),
                command: "bash".into(),
                cwd: "/tmp".into(),
                state: AgentState::Starting,
                started_at_unix_ms: now_unix_ms(),
                last_output_unix_ms: now_unix_ms(),
                host: None,
                parent: None,
            },
            AgentProfile::generic(),
        )
    }

    #[test]
    fn insert_list_remove_roundtrip() {
        let reg = Registry::new();
        reg.insert(entry("a"));
        reg.insert(entry("b"));
        assert_eq!(reg.len(), 2);
        assert!(reg.contains("a"));
        let list = reg.list();
        assert_eq!(list.len(), 2);
        let removed = reg.remove("a");
        assert!(removed.is_some());
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn ring_tail_respects_max_bytes() {
        let e = entry("a");
        e.push_ring("0123456789");
        e.push_ring("abcdefghij");
        let tail = e.ring_tail(8);
        assert_eq!(tail, "cdefghij");
    }

    #[test]
    fn ring_trims_when_over_cap() {
        let e = entry("a");
        let big = "x".repeat(64 * 1024);
        for _ in 0..6 {
            e.push_ring(&big); // 6 * 64 KiB > 256 KiB cap
        }
        let ring = e.ring.lock().unwrap();
        let total: usize = ring.iter().map(|s| s.len()).sum();
        assert!(total <= RING_CAP_BYTES + 64 * 1024); // one chunk of slack
    }

    #[test]
    fn update_info_mutates_state() {
        let reg = Registry::new();
        reg.insert(entry("a"));
        let updated = reg.update_info("a", |i| i.state = AgentState::Blocked);
        assert_eq!(updated.unwrap().state, AgentState::Blocked);
    }
}
