//! Per-agent state inference.
//!
//! Pure state machine over the ANSI-stripped output stream:
//! * Working — stream is hot (default while alive)
//! * Blocked — profile prompt regex matches the stream tail
//! * Idle    — no output for `idle_timeout_secs` while not Blocked
//! * Errored — panic/error signature matched, or non-zero exit (set by supervisor)
//! * Exited  — clean exit (set by supervisor)
//!
//! Emits a value only when the state *changes*, so the bus stays quiet.

use std::time::{Duration, Instant};

use regex::Regex;

use herdr_protocol::{AgentProfile, AgentState};

/// Length of the sliding window (chars) scanned for prompt/error matches.
const WINDOW_CHARS: usize = 256;

pub struct StateMachine {
    current: AgentState,
    window: String,
    last_output: Instant,
    prompt_regexes: Vec<Regex>,
    error_regexes: Vec<Regex>,
    idle_timeout: Duration,
}

impl StateMachine {
    pub fn new(profile: &AgentProfile) -> Self {
        Self {
            current: AgentState::Starting,
            window: String::with_capacity(WINDOW_CHARS * 2),
            last_output: Instant::now(),
            prompt_regexes: compile(&profile.prompt_regexes),
            error_regexes: compile(&profile.error_regexes),
            idle_timeout: Duration::from_secs(profile.idle_timeout_secs),
        }
    }

    pub fn current(&self) -> &AgentState {
        &self.current
    }

    /// Feed a chunk of ANSI-stripped text. Returns the new state if it changed.
    /// Late output after a terminal state (Exited/Errored) is ignored.
    pub fn process_chunk(&mut self, text: &str) -> Option<AgentState> {
        if matches!(self.current, AgentState::Exited(_) | AgentState::Errored(_)) {
            return None;
        }
        self.last_output = Instant::now();
        self.window.push_str(text);

        // Keep only the last WINDOW_CHARS chars, respecting char boundaries.
        if self.window.chars().count() > WINDOW_CHARS {
            let skip = self.window.chars().count() - WINDOW_CHARS;
            let offset = self
                .window
                .char_indices()
                .nth(skip)
                .map(|(i, _)| i)
                .unwrap_or(self.window.len());
            self.window.drain(..offset);
        }

        let next = self.classify();
        self.transition_to(next)
    }

    /// Called on a periodic tick; returns the new state if idle fired.
    pub fn check_idle(&mut self) -> Option<AgentState> {
        if matches!(self.current, AgentState::Working | AgentState::Starting)
            && self.last_output.elapsed() >= self.idle_timeout
        {
            return self.transition_to(AgentState::Idle);
        }
        None
    }

    /// Force a state (exit handling). Returns Some if it changed.
    pub fn set_terminal(&mut self, state: AgentState) -> Option<AgentState> {
        debug_assert!(matches!(state, AgentState::Exited(_) | AgentState::Errored(_)));
        self.transition_to(state)
    }

    fn classify(&self) -> AgentState {
        let tail = self.window.as_str();

        if self.error_regexes.iter().any(|re| re.is_match(tail)) {
            return AgentState::Errored("error signature in output".into());
        }

        if self.prompt_regexes.iter().any(|re| re.is_match(tail)) {
            return AgentState::Blocked;
        }

        AgentState::Working
    }

    fn transition_to(&mut self, next: AgentState) -> Option<AgentState> {
        if self.current == next {
            None
        } else {
            self.current = next.clone();
            Some(next)
        }
    }
}

fn compile(patterns: &[String]) -> Vec<Regex> {
    patterns
        .iter()
        .filter_map(|p| match Regex::new(p) {
            Ok(re) => Some(re),
            Err(e) => {
                // A bad built-in regex would be a programming error; user-supplied
                // profiles (Phase 5) just lose that pattern.
                tracing::warn!(pattern = %p, error = %e, "dropping invalid regex");
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn sm_with(profile_name: &str) -> StateMachine {
        StateMachine::new(AgentProfile::builtin(profile_name).unwrap())
    }

    fn sm_generic() -> StateMachine {
        sm_with("generic")
    }

    #[test]
    fn starts_starting_then_working_on_output() {
        let mut sm = sm_generic();
        assert_eq!(*sm.current(), AgentState::Starting);
        assert_eq!(sm.process_chunk("hello"), Some(AgentState::Working));
        assert_eq!(sm.process_chunk("more output"), None); // no change
    }

    #[test]
    fn prompt_tail_transitions_to_blocked() {
        let mut sm = sm_generic();
        sm.process_chunk("building...\n");
        assert_eq!(
            sm.process_chunk("Allow this action? (y/n): "),
            Some(AgentState::Blocked)
        );
    }

    #[test]
    fn new_output_after_prompt_returns_to_working() {
        let mut sm = sm_generic();
        sm.process_chunk("? (y/n): ");
        assert_eq!(*sm.current(), AgentState::Blocked);
        assert_eq!(sm.process_chunk("ok, proceeding\n"), Some(AgentState::Working));
    }

    #[test]
    fn dollar_shell_prompt_is_blocked_for_bash_profile() {
        let mut sm = sm_with("bash");
        sm.process_chunk("echo done\n");
        // A bare trailing `$ ` (typical interactive shell prompt) means waiting.
        assert_eq!(sm.process_chunk("$ "), Some(AgentState::Blocked));
    }

    #[test]
    fn panic_signature_is_errored() {
        let mut sm = sm_generic();
        assert!(matches!(
            sm.process_chunk("thread 'main' panicked at src/main.rs:3:5:"),
            Some(AgentState::Errored(_))
        ));
    }

    #[test]
    fn python_traceback_is_errored() {
        let mut sm = sm_generic();
        sm.process_chunk("Traceback (most recent call last):\n  File ...");
        assert!(matches!(sm.current(), AgentState::Errored(_)));
    }

    #[test]
    fn idle_fires_after_timeout() {
        let mut sm = sm_generic();
        sm.process_chunk("working hard\n");
        assert_eq!(*sm.current(), AgentState::Working);
        // Can't wait 30s in a test; shrink the timeout by constructing a custom profile.
        let profile = AgentProfile {
            name: "test".into(),
            prompt_regexes: vec![],
            error_regexes: vec![],
            idle_timeout_secs: 0, // fires immediately on next check
        };
        let mut fast = StateMachine::new(&profile);
        fast.process_chunk("x");
        // Elapsed >= 0s timeout — may or may not fire depending on timing; force it:
        fast.last_output = Instant::now() - Duration::from_secs(1);
        assert_eq!(fast.check_idle(), Some(AgentState::Idle));
        let _ = sm; // silence unused in this branch
    }

    #[test]
    fn idle_does_not_override_blocked() {
        let profile = AgentProfile {
            name: "test".into(),
            prompt_regexes: vec![r"\?\s*$".into()],
            error_regexes: vec![],
            idle_timeout_secs: 0,
        };
        let mut sm = StateMachine::new(&profile);
        sm.process_chunk("? ");
        assert_eq!(*sm.current(), AgentState::Blocked);
        sm.last_output = Instant::now() - Duration::from_secs(5);
        assert_eq!(sm.check_idle(), None); // Blocked stays Blocked
    }

    #[test]
    fn terminal_states_are_final() {
        let mut sm = sm_generic();
        sm.process_chunk("run\n");
        assert_eq!(
            sm.set_terminal(AgentState::Exited(0)),
            Some(AgentState::Exited(0))
        );
        // Further output does not resurrect a dead agent.
        assert_eq!(sm.process_chunk("late bytes"), None);
        assert_eq!(sm.check_idle(), None);
    }

    #[test]
    fn window_is_bounded_on_char_boundaries() {
        let mut sm = sm_generic();
        let chunk = "世".repeat(400); // 3-byte chars, way over 256 chars
        sm.process_chunk(&chunk);
        sm.process_chunk("tail?");
        // No panic + still functional is the contract here.
        assert!(matches!(*sm.current(), AgentState::Blocked | AgentState::Working));
    }

    #[test]
    fn errored_from_regex_wins_over_prompt() {
        let profile = AgentProfile {
            name: "test".into(),
            prompt_regexes: vec![r"\?\s*$".into()],
            error_regexes: vec![r"^Error: ".into()],
            idle_timeout_secs: 30,
        };
        let mut sm = StateMachine::new(&profile);
        sm.process_chunk("Error: disk full?");
        assert!(matches!(sm.current(), AgentState::Errored(_)));
    }
}
