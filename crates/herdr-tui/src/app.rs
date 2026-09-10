//! TUI application state and event handling.
//!
//! Pure state — no I/O — so it is unit-testable: `apply_event` / `apply_response`
//! / `handle_key` mutate state; the renderer reads it.

use std::collections::BTreeMap;

use crossterm::event::{KeyCode, KeyEvent};

use herdr_protocol::{AgentInfo, AgentState, ClientCommand, DaemonEvent, Response};

use crate::logs::LogBuffer;

/// Connection state for the banner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnState {
    Connected,
    Reconnecting,
}

/// UI mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    SendInput,
    ConfirmKill,
    ConfirmQuit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modal {
    None,
    Quit,
}

/// Everything the UI needs for one agent.
#[derive(Debug)]
pub struct AgentView {
    pub info: AgentInfo,
    pub logs: LogBuffer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Fleet,
    Log,
}

pub struct App {
    /// Agents by id (BTreeMap → stable display order alongside list order).
    pub agents: BTreeMap<String, AgentView>,
    /// Display order (daemon list order = start time).
    pub order: Vec<String>,
    pub selected: usize,
    pub mode: Mode,
    pub modal: Modal,
    pub focus: Focus,
    pub follow: bool,
    pub scroll_line: usize,
    /// Typed text in SendInput mode.
    pub input: String,
    pub connection_state: ConnState,
    pub should_quit: bool,
    pub exit_message: Option<String>,
    /// A client command the main loop should issue (set by handle_key).
    pub request_needed: Option<ClientCommand>,
    /// Status line flash message (e.g. "killed abc").
    pub flash: Option<String>,
    /// Selection to restore after the next List lands (set by resync).
    pub pending_selection: Option<String>,
    /// Set when the selection changed so the main loop can backfill logs.
    pub selection_dirty: bool,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        Self {
            agents: BTreeMap::new(),
            order: Vec::new(),
            selected: 0,
            mode: Mode::Normal,
            modal: Modal::None,
            focus: Focus::Fleet,
            follow: true,
            scroll_line: 0,
            input: String::new(),
            connection_state: ConnState::Connected,
            should_quit: false,
            exit_message: None,
            request_needed: None,
            flash: None,
            pending_selection: None,
            selection_dirty: false,
        }
    }

    pub fn selected_id(&self) -> Option<&str> {
        self.order.get(self.selected).map(String::as_str)
    }

    pub fn select_agent(&mut self, id: &str) {
        if let Some(idx) = self.order.iter().position(|x| x == id) {
            self.selected = idx;
            self.on_selection_changed();
        }
    }

    fn on_selection_changed(&mut self) {
        self.follow = true;
        self.scroll_line = 0;
        self.selection_dirty = true;
    }

    /// Counts per coarse state for the summary bar.
    pub fn counts(&self) -> (usize, usize, usize, usize, usize) {
        let mut working = 0;
        let mut idle = 0;
        let mut blocked = 0;
        let mut errored = 0;
        let mut exited = 0;
        for a in self.agents.values() {
            match a.info.state {
                AgentState::Starting | AgentState::Working => working += 1,
                AgentState::Idle => idle += 1,
                AgentState::Blocked => blocked += 1,
                AgentState::Errored(_) => errored += 1,
                AgentState::Exited(_) => exited += 1,
            }
        }
        (working, idle, blocked, errored, exited)
    }

    // ---- event application -------------------------------------------------

    pub fn apply_event(&mut self, event: DaemonEvent) {
        match event {
            DaemonEvent::AgentSpawned { info } => {
                let id = info.id.clone();
                self.agents.entry(id.clone()).or_insert_with(|| AgentView {
                    info,
                    logs: LogBuffer::new(),
                });
                if !self.order.contains(&id) {
                    self.order.push(id);
                }
            }
            DaemonEvent::AgentOutput { agent_id, payload } => {
                if let Some(view) = self.agents.get_mut(&agent_id) {
                    view.logs.feed(&payload);
                }
            }
            DaemonEvent::StateChange { agent_id, state } => {
                if let Some(view) = self.agents.get_mut(&agent_id) {
                    view.info.state = state;
                }
            }
            DaemonEvent::AgentExited { agent_id, code } => {
                if let Some(view) = self.agents.get_mut(&agent_id) {
                    view.info.state = if code == 0 {
                        AgentState::Exited(0)
                    } else {
                        AgentState::Errored(format!("exit code {code}"))
                    };
                }
            }
            DaemonEvent::AgentRemoved { agent_id } => {
                self.agents.remove(&agent_id);
                self.order.retain(|x| x != &agent_id);
                if self.selected >= self.order.len() && !self.order.is_empty() {
                    self.selected = self.order.len() - 1;
                }
            }
        }
    }

    pub fn apply_response(&mut self, resp: Response) {
        match resp {
            Response::AgentList(agents) => self.apply_list(agents),
            Response::LogChunk { payload } => {
                if let Some(id) = self.selected_id().map(str::to_string) {
                    if let Some(view) = self.agents.get_mut(&id) {
                        view.logs.replay(&payload);
                    }
                }
            }
            Response::Ok | Response::AgentCreated { .. } | Response::Pong { .. } => {}
            Response::Err(e) => self.flash = Some(format!("error: {e}")),
        }
    }

    fn apply_list(&mut self, agents: Vec<AgentInfo>) {
        // Reconcile: keep log buffers, update info, preserve selection by id.
        let selected_id = self.selected_id().map(str::to_string);
        self.order = agents.iter().map(|a| a.id.clone()).collect();
        for info in agents {
            let id = info.id.clone();
            match self.agents.get_mut(&id) {
                Some(view) => {
                    // Never let a stale list downgrade a fresher live event.
                    if is_state_upgrade(&view.info.state, &info.state) {
                        view.info.state = info.state;
                    }
                    view.info.last_output_unix_ms = info.last_output_unix_ms;
                }
                None => {
                    self.agents.insert(id.clone(), AgentView {
                        info,
                        logs: LogBuffer::new(),
                    });
                }
            }
        }
        // Drop agents the daemon no longer knows.
        self.agents.retain(|id, _| self.order.contains(id));
        if let Some(sel) = self.pending_selection.take() {
            self.select_agent(&sel);
        } else if let Some(sel) = selected_id {
            self.select_agent(&sel);
        }
        if self.selected >= self.order.len() {
            self.selected = self.order.len().saturating_sub(1);
        }
    }

    /// Full resync after reconnect: forget everything, start clean.
    pub fn resync(&mut self) {
        let selected_id = self.selected_id().map(str::to_string);
        self.agents.clear();
        self.order.clear();
        self.selected = 0;
        // Caller re-issues List + Logs; selection restored after.
        if let Some(sel) = selected_id {
            self.pending_selection = Some(sel);
        }
    }

    pub fn flash_now(&mut self, msg: impl Into<String>) {
        self.flash = Some(msg.into());
    }

    // ---- key handling --------------------------------------------------------

    /// Handle one key; returns stdin lines to forward via SendInput.
    pub fn handle_key(&mut self, key: KeyEvent) -> Vec<PendingLine> {
        let mut pending = Vec::new();

        // Modal-less modes first.
        match self.mode {
            Mode::SendInput => {
                match key.code {
                    KeyCode::Esc => self.mode = Mode::Normal,
                    KeyCode::Enter => {
                        let text = self.input.clone();
                        self.input.clear();
                        self.mode = Mode::Normal;
                        if let Some(id) = self.selected_id().map(str::to_string) {
                            pending.push(PendingLine {
                                agent_id: id,
                                text,
                                raw: false,
                            });
                        }
                    }
                    KeyCode::Backspace => {
                        self.input.pop();
                    }
                    KeyCode::Char(c) => self.input.push(c),
                    _ => {}
                }
                return pending;
            }
            Mode::ConfirmKill => {
                self.mode = Mode::Normal;
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                        if let Some(id) = self.selected_id().map(str::to_string) {
                            self.request_needed = Some(ClientCommand::Kill { agent_id: id.clone() });
                            self.flash_now(format!("killing {id}…"));
                        }
                    }
                    _ => {}
                }
                return pending;
            }
            Mode::ConfirmQuit => {
                self.mode = Mode::Normal;
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                        self.should_quit = true;
                    }
                    _ => {}
                }
                return pending;
            }
            Mode::Normal => {}
        }

        // Normal mode.
        match key.code {
            KeyCode::Char('q') => {
                // If stdin forwarding is meaningful, confirm; else quit directly.
                self.should_quit = true;
            }
            KeyCode::Esc => {
                self.focus = Focus::Fleet;
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::PageUp => self.page_scroll(-1),
            KeyCode::PageDown => self.page_scroll(1),
            KeyCode::Char('g') => {
                self.follow = false;
                self.scroll_line = 0;
            }
            KeyCode::Char('G') => {
                self.follow = true;
                self.scroll_line = 0;
            }
            KeyCode::Char('f') => {
                self.follow = !self.follow;
            }
            KeyCode::Char('s') => {
                if self.selected_id().is_some() {
                    self.mode = Mode::SendInput;
                    self.input.clear();
                }
            }
            KeyCode::Char('K') => {
                if self.selected_id().is_some() {
                    self.mode = Mode::ConfirmKill;
                }
            }
            KeyCode::Enter | KeyCode::Char('a') => {
                self.focus = Focus::Log;
            }
            _ => {}
        }
        pending
    }

    fn move_selection(&mut self, delta: i32) {
        if self.order.is_empty() {
            return;
        }
        let len = self.order.len() as i32;
        let next = (self.selected as i32 + delta).clamp(0, len - 1);
        if next != self.selected as i32 {
            self.selected = next as usize;
            self.on_selection_changed();
        }
    }

    fn page_scroll(&mut self, direction: i32) {
        // Pages over logical lines; exact rows depend on pane height (render-time).
        self.follow = false;
        self.scroll_line = (self.scroll_line as i64 + direction as i64 * 20)
            .clamp(0, usize::MAX as i64) as usize;
    }
}

/// A line of stdin the main loop must forward via `SendInput`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingLine {
    pub agent_id: String,
    pub text: String,
    pub raw: bool,
}

/// Does `new_state` carry more information than `old_state` for reconciliation?
fn is_state_upgrade(old: &AgentState, new: &AgentState) -> bool {
    use AgentState::*;
    match (old, new) {
        // Terminal states always win.
        (Exited(_) | Errored(_), _) => false,
        (_, Exited(_) | Errored(_)) => true,
        // Blocked is more specific than Working.
        (Blocked, Starting | Working) => false,
        (Starting | Working, Blocked) => true,
        (Idle, Starting | Working) => false,
        (Starting | Working, Idle) => true,
        _ => false,
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
            started_at_unix_ms: 1,
            last_output_unix_ms: 1,
            host: None,
            parent: None,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    #[test]
    fn spawn_then_output_then_state() {
        let mut app = App::new();
        app.apply_event(DaemonEvent::AgentSpawned { info: info("a", AgentState::Starting) });
        assert_eq!(app.order, vec!["a"]);
        app.apply_event(DaemonEvent::AgentOutput {
            agent_id: "a".into(),
            payload: "hi\n".into(),
        });
        app.apply_event(DaemonEvent::StateChange {
            agent_id: "a".into(),
            state: AgentState::Blocked,
        });
        assert_eq!(app.agents["a"].info.state, AgentState::Blocked);
        assert_eq!(app.agents["a"].logs.len(), 1);
        assert_eq!(app.agents["a"].logs.lines[0].text(), "hi");
    }

    #[test]
    fn output_for_unknown_agent_is_ignored() {
        let mut app = App::new();
        app.apply_event(DaemonEvent::AgentOutput {
            agent_id: "ghost".into(),
            payload: "x".into(),
        });
        assert!(app.agents.is_empty());
    }

    #[test]
    fn list_reconcile_preserves_fresher_state_and_logs() {
        let mut app = App::new();
        app.apply_event(DaemonEvent::AgentSpawned { info: info("a", AgentState::Starting) });
        app.apply_event(DaemonEvent::AgentOutput {
            agent_id: "a".into(),
            payload: "log line\n".into(),
        });
        app.apply_event(DaemonEvent::StateChange {
            agent_id: "a".into(),
            state: AgentState::Working,
        });
        // Daemon list (stale snapshot) says Starting; must not downgrade.
        app.apply_response(Response::AgentList(vec![info("a", AgentState::Starting)]));
        assert_eq!(app.agents["a"].info.state, AgentState::Working);
        assert_eq!(app.agents["a"].logs.len(), 1);
    }

    #[test]
    fn list_reconcile_drops_removed_agents() {
        let mut app = App::new();
        app.apply_event(DaemonEvent::AgentSpawned { info: info("a", AgentState::Working) });
        app.apply_event(DaemonEvent::AgentSpawned { info: info("b", AgentState::Working) });
        app.apply_response(Response::AgentList(vec![info("a", AgentState::Working)]));
        assert_eq!(app.order, vec!["a"]);
        assert!(app.agents.contains_key("a"));
        assert!(!app.agents.contains_key("b"));
    }

    #[test]
    fn selection_survives_list_refresh() {
        let mut app = App::new();
        app.apply_event(DaemonEvent::AgentSpawned { info: info("a", AgentState::Working) });
        app.apply_event(DaemonEvent::AgentSpawned { info: info("b", AgentState::Working) });
        app.select_agent("b");
        assert_eq!(app.selected, 1);
        app.apply_response(Response::AgentList(vec![
            info("a", AgentState::Working),
            info("b", AgentState::Working),
        ]));
        assert_eq!(app.selected_id(), Some("b"));
    }

    #[test]
    fn removal_clamps_selection() {
        let mut app = App::new();
        app.apply_event(DaemonEvent::AgentSpawned { info: info("a", AgentState::Working) });
        app.apply_event(DaemonEvent::AgentSpawned { info: info("b", AgentState::Working) });
        app.select_agent("b");
        app.apply_event(DaemonEvent::AgentRemoved { agent_id: "b".into() });
        assert_eq!(app.order, vec!["a"]);
        assert_eq!(app.selected, 0);
        assert_eq!(app.selected_id(), Some("a"));
    }

    #[test]
    fn counts_categorize_states() {
        let mut app = App::new();
        app.apply_event(DaemonEvent::AgentSpawned { info: info("a", AgentState::Working) });
        app.apply_event(DaemonEvent::AgentSpawned { info: info("b", AgentState::Blocked) });
        app.apply_event(DaemonEvent::AgentSpawned { info: info("c", AgentState::Idle) });
        app.apply_event(DaemonEvent::AgentSpawned {
            info: info("d", AgentState::Errored("x".into())),
        });
        assert_eq!(app.counts(), (1, 1, 1, 1, 0));
    }

    #[test]
    fn keys_navigate_selection() {
        let mut app = App::new();
        for id in ["a", "b", "c"] {
            app.apply_event(DaemonEvent::AgentSpawned { info: info(id, AgentState::Working) });
        }
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.selected_id(), Some("b"));
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.selected_id(), Some("c"));
        app.handle_key(key(KeyCode::Char('j'))); // clamped
        assert_eq!(app.selected_id(), Some("c"));
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.selected_id(), Some("b"));
    }

    #[test]
    fn selection_resets_follow() {
        let mut app = App::new();
        app.apply_event(DaemonEvent::AgentSpawned { info: info("a", AgentState::Working) });
        app.apply_event(DaemonEvent::AgentSpawned { info: info("b", AgentState::Working) });
        app.follow = false;
        app.scroll_line = 40;
        app.handle_key(key(KeyCode::Down));
        assert!(app.follow);
        assert_eq!(app.scroll_line, 0);
    }

    #[test]
    fn send_mode_collects_input_and_submits() {
        let mut app = App::new();
        app.apply_event(DaemonEvent::AgentSpawned { info: info("a", AgentState::Working) });
        app.handle_key(key(KeyCode::Char('s')));
        assert_eq!(app.mode, Mode::SendInput);
        for ch in "echo hi".chars() {
            app.handle_key(key(KeyCode::Char(ch)));
        }
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.mode, Mode::Normal);
        // The request is routed via request_needed.
        match app.request_needed.take() {
            Some(ClientCommand::SendInput { agent_id, text, raw }) => {
                assert_eq!(agent_id, "a");
                assert_eq!(text, "echo hi");
                assert!(!raw);
            }
            other => panic!("expected SendInput, got {other:?}"),
        }
    }

    #[test]
    fn send_mode_esc_cancels() {
        let mut app = App::new();
        app.apply_event(DaemonEvent::AgentSpawned { info: info("a", AgentState::Working) });
        app.handle_key(key(KeyCode::Char('s')));
        app.handle_key(key(KeyCode::Char('x')));
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.input, "");
        assert!(app.request_needed.is_none());
    }

    #[test]
    fn kill_requires_confirmation() {
        let mut app = App::new();
        app.apply_event(DaemonEvent::AgentSpawned { info: info("a", AgentState::Working) });
        app.handle_key(key(KeyCode::Char('K')));
        assert_eq!(app.mode, Mode::ConfirmKill);
        app.handle_key(key(KeyCode::Char('n')));
        assert!(app.request_needed.is_none());
        app.handle_key(key(KeyCode::Char('K')));
        app.handle_key(key(KeyCode::Char('y')));
        match app.request_needed.take() {
            Some(ClientCommand::Kill { agent_id }) => assert_eq!(agent_id, "a"),
            other => panic!("expected Kill, got {other:?}"),
        }
    }

    #[test]
    fn quit_sets_flag() {
        let mut app = App::new();
        app.handle_key(key(KeyCode::Char('q')));
        assert!(app.should_quit);
    }

    #[test]
    fn follow_toggle_and_scroll_keys() {
        let mut app = App::new();
        app.handle_key(key(KeyCode::Char('f')));
        assert!(!app.follow);
        app.handle_key(key(KeyCode::Char('f')));
        assert!(app.follow);
        app.handle_key(key(KeyCode::PageDown));
        assert!(!app.follow);
        assert_eq!(app.scroll_line, 20);
        app.handle_key(key(KeyCode::Char('G')));
        assert!(app.follow);
    }

    #[test]
    fn state_upgrade_matrix() {
        assert!(is_state_upgrade(&AgentState::Working, &AgentState::Blocked));
        assert!(!is_state_upgrade(&AgentState::Blocked, &AgentState::Working));
        assert!(is_state_upgrade(&AgentState::Working, &AgentState::Errored("x".into())));
        assert!(!is_state_upgrade(&AgentState::Errored("x".into()), &AgentState::Working));
        assert!(!is_state_upgrade(&AgentState::Exited(0), &AgentState::Working));
    }

    #[test]
    fn resync_clears_and_remembers_selection() {
        let mut app = App::new();
        app.apply_event(DaemonEvent::AgentSpawned { info: info("a", AgentState::Working) });
        app.select_agent("a");
        app.resync();
        assert!(app.agents.is_empty());
        assert_eq!(app.pending_selection, Some("a".to_string()));
    }
}
