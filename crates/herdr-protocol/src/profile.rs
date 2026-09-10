//! Agent profiles: per-tool tuning for state inference.

use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

/// Tuning for the state-inference engine, selected at spawn time via
/// `ClientCommand::Spawn { profile }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProfile {
    /// Profile key, e.g. `"claude-code"`, `"codex"`, `"generic"`.
    pub name: String,
    /// Regexes matched against the tail of the ANSI-stripped output stream;
    /// a match means the agent is waiting on user input.
    #[serde(default)]
    pub prompt_regexes: Vec<String>,
    /// Regexes indicating an error signature in the stream tail.
    #[serde(default)]
    pub error_regexes: Vec<String>,
    /// Seconds without output before an agent is considered Idle.
    #[serde(default = "default_idle")]
    pub idle_timeout_secs: u64,
}

fn default_idle() -> u64 {
    30
}

impl AgentProfile {
    /// Built-in profile registry. Unknown names fall back to `generic`.
    pub fn builtin(name: &str) -> Option<&'static AgentProfile> {
        BUILTIN.iter().find(|p| p.name == name)
    }

    /// The `generic` fallback profile.
    pub fn generic() -> &'static AgentProfile {
        &BUILTIN[0]
    }

    /// All built-in profile names (for CLI completions/help).
    pub fn builtin_names() -> Vec<&'static str> {
        BUILTIN.iter().map(|p| p.name.as_str()).collect()
    }
}

/// Regexes are compiled once by the daemon's state machine, not here — this crate
/// stays dependency-light on purpose.
static BUILTIN: LazyLock<Vec<AgentProfile>> = LazyLock::new(|| {
    vec![
        AgentProfile {
            name: "generic".into(),
            prompt_regexes: vec![
                r"(?m)(❯|\$|>|#)\s*$".into(),
                r"(?m)\?\s*$".into(),
                r"(?i)\(y/n\)[?:]?\s*$".into(),
                r"(?i)press enter to continue\s*$".into(),
                r"(?i)\[y/n\]\s*$".into(),
            ],
            error_regexes: vec![
                r"thread '[^']+' panicked at".into(),
                r"(?m)^Traceback \(most recent call last\):".into(),
                r"(?m)^Error: ".into(),
            ],
            idle_timeout_secs: 30,
        },
        AgentProfile {
            name: "claude-code".into(),
            prompt_regexes: vec![
                r"❯\s*$".into(),
                r"\?\s*$".into(),
                r"(?i)\(y/n\)[?:]?\s*$".into(),
                r"(?i)press enter to continue\s*$".into(),
                r"(?i)would you like to (proceed|continue|run)[^?]*\?\s*$".into(),
                r"─+$".into(), // Claude Code draws a prompt box bottom border when waiting
            ],
            error_regexes: vec![
                r"thread '[^']+' panicked at".into(),
                r"(?m)^Error: ".into(),
                r"API Error".into(),
            ],
            idle_timeout_secs: 60,
        },
        AgentProfile {
            name: "codex".into(),
            prompt_regexes: vec![
                r"(?m)^[│▌▏>❯].{0,120}$".into(), // codex draws an input widget when waiting
                r"\?\s*$".into(),
                r"(?i)\(y/n\)[?:]?\s*$".into(),
            ],
            error_regexes: vec![
                r"thread '[^']+' panicked at".into(),
                r"(?m)^Error: ".into(),
            ],
            idle_timeout_secs: 45,
        },
        AgentProfile {
            name: "bash".into(),
            prompt_regexes: vec![
                r"(?m)(\$|#)\s*$".into(),
                r"(?m)❯\s*$".into(),
            ],
            error_regexes: vec![r"(?m)^bash: line \d+: .*".into()],
            idle_timeout_secs: 15,
        },
    ]
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_lookup_and_fallback() {
        assert!(AgentProfile::builtin("claude-code").is_some());
        assert!(AgentProfile::builtin("nope").is_none());
        assert_eq!(AgentProfile::generic().name, "generic");
        assert!(AgentProfile::builtin_names().contains(&"codex"));
    }

    #[test]
    fn profiles_roundtrip_through_json() {
        for p in BUILTIN.iter() {
            let json = serde_json::to_string(p).unwrap();
            let back: AgentProfile = serde_json::from_str(&json).unwrap();
            assert_eq!(&back, p);
        }
    }

    #[test]
    fn defaults_apply_on_deserialize() {
        let p: AgentProfile =
            serde_json::from_str(r#"{"name":"custom","prompt_regexes":[],"error_regexes":[]}"#)
                .unwrap();
        assert_eq!(p.idle_timeout_secs, 30);
    }

    #[test]
    fn generic_prompt_regexes_match_common_tails() {
        let p = AgentProfile::generic();
        let compiled: Vec<_> = p
            .prompt_regexes
            .iter()
            .map(|r| regex::Regex::new(r).unwrap())
            .collect();
        // NOTE: this test compiles regexes with the `regex` crate, which is a
        // dev-only dependency.
        let tails = ["Allow? (y/n): ", "? ", "$ ", "❯ ", "Press Enter to continue"];
        for tail in tails {
            assert!(
                compiled.iter().any(|re| re.is_match(tail)),
                "no prompt regex matched tail: {tail:?}"
            );
        }
    }
}
