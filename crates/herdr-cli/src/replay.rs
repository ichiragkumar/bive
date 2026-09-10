//! `herdr replay` — offline state-timeline inference for profile tuning.
//!
//! Feeds a captured PTY stream (raw bytes, or NDJSON from `herdr events`) through
//! the *same* `StateMachine` + `AnsiStripper` the daemon runs, then prints the
//! inferred state timeline. This keeps the tuning tool incapable of drifting from
//! production behavior: same crate, same code path, same regex semantics.

use std::io::Read;

use anyhow::{anyhow, bail, Context, Result};

use herdr_daemon::ansi::AnsiStripper;
use herdr_daemon::state::StateMachine;
use herdr_protocol::{AgentProfile, AgentState};
/// One row of the printed timeline.
#[derive(Debug)]
struct Transition {
    offset: f64,
    state: AgentState,
    /// Window tail at transition time (populated with --explain).
    tail: Option<String>,
}

/// Split the capture into evenly spaced chunks, like the daemon's PTY reader.
fn split_chunks(bytes: &[u8], chunk_bytes: usize) -> Vec<&[u8]> {
    if bytes.is_empty() {
        return Vec::new();
    }
    let mut chunks = Vec::with_capacity(bytes.len() / chunk_bytes + 1);
    let mut rest = bytes;
    while !rest.is_empty() {
        let take = rest.len().min(chunk_bytes);
        let (head, tail) = rest.split_at(take);
        chunks.push(head);
        rest = tail;
    }
    chunks
}

/// Resolve the capture source to bytes: a file path, or stdin.
fn read_capture(file: Option<&str>) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    match file {
        Some(path) => {
            std::fs::File::open(path)
                .with_context(|| format!("cannot open capture file {path}"))?
                .read_to_end(&mut buf)?;
        }
        None => {
            std::io::stdin()
                .read_to_end(&mut buf)
                .context("reading capture from stdin")?;
        }
    }
    Ok(buf)
}

/// Heuristic loader for NDJSON captures from `herdr events`: if any of the first
/// few lines *looks* like a herdr event envelope, concatenate `AgentOutput`
/// payloads; otherwise treat the input as a raw byte capture.
fn maybe_extract_ndjson(bytes: &[u8]) -> Option<Vec<u8>> {
    let text = String::from_utf8_lossy(bytes);
    // Captures may start with a reply line (e.g. the `Events` Ok), so probe the
    // first few lines rather than only the first.
    let looks_like_events = text
        .lines()
        .take(5)
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l.trim()).ok())
        .any(|v| v.get("event").is_some());
    if !looks_like_events {
        return None;
    }
    let mut out = Vec::new();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        if let Some(ev) = v.get("event") {
            if let Some(payload) = ev.get("AgentOutput").and_then(|o| o.get("payload")) {
                if let Some(s) = payload.as_str() {
                    out.extend_from_slice(s.as_bytes());
                }
            }
        }
    }
    Some(out)
}

/// Compute the inferred state timeline for a raw (ANSI-ful) byte capture.
///
/// Deterministic: identical input and flags always produce an identical timeline.
/// Offsets are simulated, never wall-clock.
fn compute_timeline(
    profile: &AgentProfile,
    raw: &[u8],
    chunk_ms: u64,
    idle_checks: u32,
    want_tail: bool,
) -> Vec<Transition> {
    let mut sm = StateMachine::new_replay(profile);
    let mut stripper = AnsiStripper::new();
    let step = chunk_ms as f64 / 1000.0;
    let mut timeline = Vec::new();

    // No bytes: the profile's idle timeout alone decides the outcome.
    if raw.is_empty() {
        if let Some(state) = sm.check_idle_at(profile.idle_timeout_secs as f64) {
            timeline.push(Transition {
                offset: profile.idle_timeout_secs as f64,
                state,
                tail: None,
            });
        }
        return timeline;
    }

    let chunks = split_chunks(raw, 256);
    let mut offset = 0.0f64;
    for chunk in chunks {
        let stripped = String::from_utf8_lossy(&stripper.feed_raw(chunk)).into_owned();
        if !stripped.is_empty() {
            if let Some(next) = sm.process_chunk_at(&stripped, offset) {
                timeline.push(Transition {
                    offset,
                    tail: want_tail.then(|| sm.window_tail().to_string()),
                    state: next,
                });
            }
        }
        // Interpolate idle checks across the gap to the next chunk: catches
        // Working → Idle inside long silences rather than only at chunk edges.
        if idle_checks > 0 {
            for k in 1..=idle_checks {
                let frac = k as f64 / (idle_checks as f64 + 1.0);
                if let Some(next) = sm.check_idle_at(offset + step * frac) {
                    timeline.push(Transition {
                        offset: offset + step * frac,
                        tail: want_tail.then(|| sm.window_tail().to_string()),
                        state: next,
                    });
                }
            }
        }
        offset += step;
    }

    // Tail idle check: if the capture ends Working, the daemon would eventually
    // mark it Idle — show when (profile.idle_timeout_secs after the last output).
    if let Some(next) = sm.check_idle_at(offset + profile.idle_timeout_secs as f64) {
        timeline.push(Transition {
            offset: offset + profile.idle_timeout_secs as f64,
            tail: want_tail.then(|| sm.window_tail().to_string()),
            state: next,
        });
    }
    timeline
}

/// Print the timeline in a stable, grep-friendly format.
fn print_timeline(timeline: &[Transition], profile_name: &str, raw_len: usize, explain: bool) {
    println!(
        "replay: profile={profile_name} bytes={raw_len} transitions={}",
        timeline.len()
    );
    for t in timeline {
        print!("{:8.3}s  {:<8}", t.offset, t.state.name());
        match &t.state {
            AgentState::Errored(reason) => print!(" {reason}"),
            AgentState::Exited(code) => print!(" (code {code})"),
            _ => {}
        }
        println!();
        if explain {
            if let Some(tail) = &t.tail {
                let compact: String = tail
                    .chars()
                    .map(|c| if c == '\n' || c == '\r' { '⏎' } else { c })
                    .collect();
                let start = compact
                    .char_indices()
                    .rev()
                    .nth(79)
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                println!("           └─ window tail: {}", &compact[start..]);
            }
        }
    }
    if timeline.is_empty() {
        println!("  (no transitions — state stayed as classified by the initial chunk)");
    }
}

/// Entry point for the `replay` subcommand.
pub fn run(args: ReplayArgs) -> Result<()> {
    let ReplayArgs {
        profile,
        file,
        chunk_ms,
        idle,
        explain,
    } = args;

    let profile = AgentProfile::builtin(&profile)
        .ok_or_else(|| {
            anyhow!(
                "unknown profile {:?} — built-ins: {}",
                profile,
                AgentProfile::builtin_names().join(", ")
            )
        })?
        .clone();

    let raw = read_capture(file.as_deref())?;
    let (raw, source_note) = match maybe_extract_ndjson(&raw) {
        Some(payload) => (payload, " (ndjson events)"),
        None => (raw, ""),
    };
    if raw.is_empty() {
        bail!("empty capture — pipe `herdr events > capture.ndjson` or pass --file");
    }

    let timeline = compute_timeline(&profile, &raw, chunk_ms, idle, explain);
    print_timeline(&timeline, &profile.name, raw.len(), explain);
    let _ = source_note;
    Ok(())
}

/// Arguments passed from the clap parser in `main.rs`.
pub struct ReplayArgs {
    pub profile: String,
    pub file: Option<String>,
    pub chunk_ms: u64,
    pub idle: u32,
    pub explain: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(name: &str) -> AgentProfile {
        AgentProfile::builtin(name).unwrap().clone()
    }

    #[test]
    fn chunks_split_evenly_with_remainder() {
        let chunks = split_chunks(&[0u8; 700], 256);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].len(), 256);
        assert_eq!(chunks[1].len(), 256);
        assert_eq!(chunks[2].len(), 188);
        assert!(split_chunks(b"", 256).is_empty());
    }

    #[test]
    fn timeline_starts_working_and_blocks_on_prompt() {
        // Two full chunks of text so the stream spans multiple reads like a real
        // capture (the chunker splits at 256 bytes).
        let mut raw = format!("building the thing...\n{}\n", "work ".repeat(60)).into_bytes();
        raw.extend_from_slice(b"Allow this action? (y/n): ");
        let tl = compute_timeline(&profile("generic"), &raw, 50, 1, false);
        assert!(matches!(tl[0].state, AgentState::Working), "got {tl:?}");
        assert!(
            matches!(tl.last().unwrap().state, AgentState::Blocked),
            "got {tl:?}"
        );
    }

    #[test]
    fn idle_interpolation_fires_inside_gap() {
        // Working stream (>1 chunk), then silence: the trailing idle check fires.
        let raw = format!("crunching numbers...\n{}\n", "busy ".repeat(60)).into_bytes();
        let p = profile("generic"); // idle_timeout 30s
        let tl = compute_timeline(&p, &raw, 50, 1, false);
        assert!(
            matches!(tl.last().unwrap().state, AgentState::Idle),
            "expected trailing Idle, got {tl:?}"
        );
        assert!(tl.last().unwrap().offset > 0.05);
    }

    #[test]
    fn deterministic_across_runs() {
        let mut raw = b"out \x1b[32mgreen\x1b[0m more\n".to_vec();
        raw.extend_from_slice(b"? ");
        let a = compute_timeline(&profile("generic"), &raw, 50, 2, false);
        let b = compute_timeline(&profile("generic"), &raw, 50, 2, false);
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(b.iter()) {
            assert_eq!(x.offset, y.offset);
            assert_eq!(x.state, y.state);
        }
    }

    #[test]
    fn errored_beats_blocked_and_stays_terminal() {
        let mut raw = b"? (y/n): \n".to_vec();
        raw.extend_from_slice(b"thread 'main' panicked at src/x.rs:1:1:\n");
        let tl = compute_timeline(&profile("generic"), &raw, 50, 0, false);
        let last = tl.last().unwrap();
        assert!(matches!(last.state, AgentState::Errored(_)), "got {tl:?}");
    }

    #[test]
    fn ndjson_events_capture_is_detected_and_concatenated() {
        // Captures typically start with the reply to `herdr events`, then events.
        let ndjson = concat!(
            r#"{"id":1,"resp":{"Ok":null}}"#,
            "\n",
            r#"{"event":{"AgentOutput":{"agent_id":"a","payload":"hello "}}}"#,
            "\n",
            r#"{"event":{"AgentOutput":{"agent_id":"a","payload":"world\n"}}}"#,
            "\n"
        );
        let extracted = maybe_extract_ndjson(ndjson.as_bytes()).expect("should detect ndjson");
        assert_eq!(extracted, b"hello world\n");
        // Pure event stream (no leading reply) is detected too.
        let pure = r#"{"event":{"AgentOutput":{"agent_id":"a","payload":"x"}}}"#.as_bytes();
        assert_eq!(maybe_extract_ndjson(pure).as_deref(), Some(&b"x"[..]));
        // Raw capture: passthrough.
        assert!(maybe_extract_ndjson(b"plain raw bytes \x1b[32m").is_none());
        // Non-event JSON: ignored.
        assert!(maybe_extract_ndjson(b"{\"unrelated\":true}\n").is_none());
    }

    #[test]
    fn profile_replay_of_bash_prompt_matches_daemon_classification() {
        // The same bytes the daemon's bash profile sees at a shell prompt must
        // replay to Blocked — the engine-parity guarantee.
        let raw = b"echo hi\nhi\nbash-3.2$ ".to_vec();
        let tl = compute_timeline(&profile("bash"), &raw, 50, 0, false);
        assert!(
            matches!(tl.last().unwrap().state, AgentState::Blocked),
            "got {tl:?}"
        );
    }
}
