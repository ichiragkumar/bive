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

/// One turn in the Chat-tab view: who produced this block of output.
///
/// Segments are a *documented heuristic* over the existing `AgentOutput`
/// stream (no new inference layer, no protocol change): lines echoing a
/// composer submission read as human turns, `claude-code` tool markers
/// (`⏺` call / `⎿` result) and `[media: …]` placeholders read as tool calls,
/// everything else reads as agent output. The Terminal tab stays the ground
/// truth; a per-line mirror of this rule lives in `ui/components/detail-chat.js`
/// for live appends, and `snapshot` (computed here) is authoritative.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum ChatKind {
    Human,
    Agent,
    Tool,
}

/// One Chat-tab block.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ChatSegment {
    pub kind: ChatKind,
    pub text: String,
}

/// Everything the UI shows for one agent.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentCard {
    pub info: AgentInfo,
    /// Rolling log text (ANSI-stripped by the UI renderer, which re-styles it).
    pub log_tail: String,
    pub media: Vec<MediaBlock>,
    /// Recent composer submissions (bounded), used only for Chat segmentation.
    /// Never serialized: segments are recomputed at snapshot time instead.
    #[serde(skip_serializing)]
    pub sent: Vec<String>,
}

/// Bound on log text kept per agent in the UI store.
const LOG_TAIL_CHARS: usize = 64 * 1024;

/// Bound on remembered composer submissions per agent.
const SENT_LINES_KEPT: usize = 20;

/// Bound on Chat segments per agent (snapshot payload stays small).
const MAX_CHAT_SEGMENTS: usize = 500;

/// Minimum composer-text length that can claim a log line as a human turn.
/// Single characters (e.g. `y` answers) occur inside unrelated output, so
/// they never match — they stay agent output rather than mis-splitting.
const MIN_SENT_MATCH_CHARS: usize = 3;

/// Does this log line look like a tool call/result rather than plain output?
/// Markers come from `claude-code` (`⏺` call, `⎿` result); `[media: …]` lines
/// are daemon-rendered tool outputs. Extensible per profile, deliberately small.
fn is_tool_line(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with('⏺') || t.starts_with('⎿') || line.contains("[media:")
}

/// Group rolling log text into Chat turns. Pure function so the rule is
/// golden-testable here and mirrored 1:1 by the JS live-append path.
///
/// `sent` is FIFO: each submission claims the *first* later line containing
/// it, then is consumed. Consecutive same-kind lines coalesce.
pub fn segment_chat(log_tail: &str, sent: &[String]) -> Vec<ChatSegment> {
    let mut pending: std::collections::VecDeque<&str> = sent.iter().map(String::as_str).collect();
    let mut segments: Vec<ChatSegment> = Vec::new();

    let mut push = |kind: ChatKind, line: &str| {
        if let Some(last) = segments.last_mut() {
            if last.kind == kind {
                last.text.push('\n');
                last.text.push_str(line);
                return;
            }
        }
        segments.push(ChatSegment {
            kind,
            text: line.to_string(),
        });
    };

    for line in log_tail.lines() {
        // Human turn: earliest pending submission contained in this line.
        // (Scan all pending, not just the head: an un-echoed early send must
        // not block later sends from matching.)
        let hit = pending
            .iter()
            .position(|s| s.chars().count() >= MIN_SENT_MATCH_CHARS && line.contains(*s));
        if let Some(idx) = hit {
            push(ChatKind::Human, line);
            pending.remove(idx);
            continue;
        }
        if is_tool_line(line) {
            push(ChatKind::Tool, line);
        } else {
            push(ChatKind::Agent, line);
        }
    }

    // Keep the tail: newest turns matter, snapshot payload stays bounded.
    if segments.len() > MAX_CHAT_SEGMENTS {
        segments.drain(..segments.len() - MAX_CHAT_SEGMENTS);
    }
    segments
}

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
                            sent: Vec::new(),
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
                            sent: Vec::new(),
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

    /// Record a composer submission for Chat segmentation. Called by the shell
    /// after a successful `SendInput`; the echoed text then reads as a human
    /// turn when it arrives back in the output stream.
    pub fn note_sent(&mut self, agent_id: &str, text: String) {
        if let Some(card) = self.agents.get_mut(agent_id) {
            if !text.is_empty() {
                card.sent.push(text);
                if card.sent.len() > SENT_LINES_KEPT {
                    card.sent.drain(..card.sent.len() - SENT_LINES_KEPT);
                }
            }
        }
    }

    /// Chat turns for one agent, computed on demand from its log tail plus
    /// remembered submissions. Empty for unknown agents.
    pub fn chat_segments(&self, agent_id: &str) -> Vec<ChatSegment> {
        self.agents
            .get(agent_id)
            .map(|card| segment_chat(&card.log_tail, &card.sent))
            .unwrap_or_default()
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

    fn kinds(segments: &[ChatSegment]) -> Vec<ChatKind> {
        segments.iter().map(|s| s.kind.clone()).collect()
    }

    #[test]
    fn chat_groups_human_agent_and_tool_turns() {
        let log = "Planning the refactor across 3 crates\n\
                   ⏺ Read src/main.rs\n\
                   ⎿ 142 lines read\n\
                   bash-3.2$ echo done\n\
                   done\n\
                   [media: image/png — chart]\n";
        let segs = segment_chat(log, &["echo done".to_string()]);
        assert_eq!(
            kinds(&segs),
            vec![
                ChatKind::Agent,
                ChatKind::Tool,
                ChatKind::Human,
                ChatKind::Agent,
                ChatKind::Tool,
            ]
        );
        assert!(segs[0].text.contains("Planning the refactor"));
        // Consecutive tool lines coalesce into one segment.
        assert!(segs[1].text.contains("Read src/main.rs"));
        assert!(segs[1].text.contains("142 lines read"));
        assert!(segs[2].text.contains("echo done"));
    }

    #[test]
    fn chat_sent_lines_are_consumed_once_in_order() {
        let log = "first request\noutput\nsecond request\nmore output\nfirst request\n";
        let segs = segment_chat(
            log,
            &["first request".to_string(), "second request".to_string()],
        );
        assert_eq!(
            kinds(&segs),
            vec![
                ChatKind::Human,
                ChatKind::Agent,
                ChatKind::Human,
                // Repeat of an already-consumed send reads as agent output
                // and coalesces with the preceding agent lines.
                ChatKind::Agent,
            ]
        );
        assert!(segs[3].text.contains("more output"));
        assert!(segs[3].text.contains("first request"));
    }

    #[test]
    fn chat_short_sends_never_match() {
        // `y` occurs inside "yes" — must not split the turn.
        let segs = segment_chat("yes, promoting\n", &["y".to_string()]);
        assert_eq!(kinds(&segs), vec![ChatKind::Agent]);
    }

    #[test]
    fn chat_segments_are_bounded() {
        let log = (0..2000)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let segs = segment_chat(&log, &[]);
        assert!(segs.len() <= MAX_CHAT_SEGMENTS);
        assert!(segs.last().unwrap().text.contains("line 1999"));
    }

    #[test]
    fn chat_segments_flow_through_the_store() {
        let mut s = UiStore::new();
        s.observe(&DaemonEvent::AgentSpawned {
            info: info("a", AgentState::Working),
        });
        s.observe(&DaemonEvent::AgentOutput {
            agent_id: "a".into(),
            payload: "working on it\nbash$ refactor auth\nrefactored\n".into(),
        });
        s.note_sent("a", "refactor auth".into());
        let segs = s.chat_segments("a");
        assert_eq!(
            kinds(&segs),
            vec![ChatKind::Agent, ChatKind::Human, ChatKind::Agent]
        );
        // Unknown agents yield nothing, not a panic.
        assert!(s.chat_segments("ghost").is_empty());
        // note_sent remembers only a bounded tail.
        for i in 0..30 {
            s.note_sent("a", format!("send number {i}"));
        }
        assert_eq!(s.agents["a"].sent.len(), SENT_LINES_KEPT);
    }
}
