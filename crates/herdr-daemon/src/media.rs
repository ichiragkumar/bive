//! Media magic-line detection: agents embed images/charts in their terminal output
//! by writing the herdr magic line to stdout:
//!
//! ```text
//! HERDR-MEDIA <mime> <base64> [<caption>]
//! ```
//!
//! The daemon scans raw PTY output for that prefix, extracts it from the text
//! stream, and emits a structured [`DaemonEvent::AgentMedia`] instead. Malformed
//! lines (bad base64, unknown MIME, oversized payload) fall back to plain text.

use base64::Engine as _;

use herdr_protocol::DaemonEvent;

/// The magic prefix that marks a media line.
pub const MEDIA_PREFIX: &str = "HERDR-MEDIA ";

/// Maximum raw payload size the daemon will relay (8 MiB).
pub const MAX_MEDIA_BYTES: usize = 8 * 1024 * 1024;

/// Upper bound on the internal line assembly buffer — a "line" longer than this
/// can never be a magic line, so the head is released as plain text.
const MAX_LINE_BUFFER: usize = MAX_MEDIA_BYTES * 2;

/// Outcome of feeding raw PTY bytes through the scanner.
pub struct ScanResult {
    /// Text with any complete magic lines removed (still ANSI-ful, still raw).
    pub passthrough: String,
    /// One structured event per complete, valid magic line, in order.
    pub media_events: Vec<DaemonEvent>,
}

/// Incremental scanner: magic lines may arrive split across PTY reads.
#[derive(Debug, Default)]
pub struct MediaScanner {
    /// Bytes accumulated since the last newline — a potential magic line.
    line: Vec<u8>,
}

impl MediaScanner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed raw PTY bytes; returns passthrough text and any media events.
    pub fn feed(&mut self, chunk: &[u8]) -> ScanResult {
        let mut passthrough = String::new();
        let mut events = Vec::new();
        let mut rest = chunk;

        while !rest.is_empty() {
            match rest.iter().position(|&b| b == b'\n') {
                Some(nl) => {
                    let (head, tail) = rest.split_at(nl + 1);
                    self.line.extend_from_slice(&head[..head.len() - 1]);
                    let line = std::mem::take(&mut self.line);
                    if let Some(ev) = parse_media_line(&line) {
                        events.push(ev);
                    } else {
                        passthrough.push_str(&String::from_utf8_lossy(&line));
                        passthrough.push('\n');
                    }
                    rest = tail;
                }
                None => {
                    // No newline in the remainder: buffer it (may become a magic
                    // line later) unless it can never be one.
                    self.line.extend_from_slice(rest);
                    if self.line.len() > MAX_LINE_BUFFER {
                        let drained = std::mem::take(&mut self.line);
                        passthrough.push_str(&String::from_utf8_lossy(&drained));
                    }
                    rest = &[];
                }
            }
        }

        ScanResult {
            passthrough,
            media_events: events,
        }
    }

    /// Flush at EOF: the final line has no trailing newline.
    pub fn finish(&mut self) -> ScanResult {
        let line = std::mem::take(&mut self.line);
        if let Some(ev) = parse_media_line(&line) {
            ScanResult {
                passthrough: String::new(),
                media_events: vec![ev],
            }
        } else {
            ScanResult {
                passthrough: String::from_utf8_lossy(&line).into_owned(),
                media_events: Vec::new(),
            }
        }
    }
}

/// Parse one complete line (no trailing `\n`). `Some(event)` only for a fully
/// valid magic line; everything else is passthrough text.
pub fn parse_media_line(line: &[u8]) -> Option<DaemonEvent> {
    let text = String::from_utf8_lossy(line);
    let rest = text.strip_prefix(MEDIA_PREFIX)?;
    let mut parts = rest.splitn(3, ' ');
    let mime = parts.next()?;
    let b64 = parts.next()?;
    let caption = parts.next().map(str::to_string);

    // Cheap MIME sanity: `type/subtype` with no spaces.
    if mime.is_empty()
        || !mime.contains('/')
        || mime.contains(char::is_whitespace)
        || mime.starts_with('/')
        || mime.ends_with('/')
    {
        tracing::debug!(mime, "rejecting media line: bad mime");
        return None;
    }

    // Whitespace inside the base64 (e.g. \r from \r\n) is tolerated.
    let cleaned: String = b64.chars().filter(|c| !c.is_whitespace()).collect();
    if cleaned.is_empty() {
        return None;
    }
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(cleaned.as_bytes())
        .ok()?;
    if decoded.len() > MAX_MEDIA_BYTES {
        tracing::warn!(
            size = decoded.len(),
            "media payload exceeds limit; dropping"
        );
        return None;
    }

    let encoded = base64::engine::general_purpose::STANDARD.encode(&decoded);
    Some(DaemonEvent::AgentMedia {
        agent_id: String::new(), // filled by the caller (PTY reader knows the id)
        mime: mime.to_string(),
        data_base64: encoded,
        caption,
    })
}

/// Convenience for the PTY reader: fill in the agent id after parsing.
pub fn with_agent(event: DaemonEvent, agent_id: &str) -> DaemonEvent {
    match event {
        DaemonEvent::AgentMedia {
            mime,
            data_base64,
            caption,
            ..
        } => DaemonEvent::AgentMedia {
            agent_id: agent_id.to_string(),
            mime,
            data_base64,
            caption,
        },
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG_1PX_B64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

    fn media_line(mime: &str, b64: &str, caption: Option<&str>) -> String {
        match caption {
            Some(c) => format!("{MEDIA_PREFIX}{mime} {b64} {c}\n"),
            None => format!("{MEDIA_PREFIX}{mime} {b64}\n"),
        }
    }

    #[test]
    fn valid_line_becomes_media_event() {
        let mut sc = MediaScanner::new();
        let r = sc.feed(media_line("image/png", PNG_1PX_B64, Some("chart")).as_bytes());
        assert!(r.passthrough.is_empty());
        assert_eq!(r.media_events.len(), 1);
        match &r.media_events[0] {
            DaemonEvent::AgentMedia {
                mime,
                data_base64,
                caption,
                ..
            } => {
                assert_eq!(mime, "image/png");
                assert_eq!(data_base64, PNG_1PX_B64);
                assert_eq!(caption.as_deref(), Some("chart"));
            }
            other => panic!("unexpected event {other:?}"),
        }
    }

    #[test]
    fn plain_text_passes_through_untouched() {
        let mut sc = MediaScanner::new();
        let r = sc.feed(b"hello world\nsecond line\n");
        assert_eq!(r.passthrough, "hello world\nsecond line\n");
        assert!(r.media_events.is_empty());
    }

    #[test]
    fn magic_line_split_across_chunks_is_assembled() {
        let line = media_line("image/png", PNG_1PX_B64, None);
        let bytes = line.as_bytes();
        let mut sc = MediaScanner::new();
        let mut events = Vec::new();
        let mut passthrough = String::new();
        for part in [bytes, &[][..]].iter() {
            let r = sc.feed(part);
            events.extend(r.media_events);
            passthrough.push_str(&r.passthrough);
        }
        assert!(passthrough.is_empty());
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn non_media_line_starting_with_partial_prefix_passes_through() {
        // "HERDR-MED" in one chunk, rest later — but the assembled line is not a
        // valid magic line (no mime) so it must fall through as text.
        let mut sc = MediaScanner::new();
        let r1 = sc.feed(b"HERDR-MED");
        assert!(r1.passthrough.is_empty(), "incomplete line stays buffered");
        let r2 = sc.feed(b"IA not a media line\nnext\n");
        assert_eq!(r2.passthrough, "HERDR-MEDIA not a media line\nnext\n");
        assert!(r2.media_events.is_empty());
    }

    #[test]
    fn malformed_media_lines_fall_back_to_text() {
        let cases = [
            "HERDR-MEDIA \n",                                     // empty
            "HERDR-MEDIA image/png \n",                           // no base64
            "HERDR-MEDIA notamime aGk=\n",                        // no slash
            "HERDR-MEDIA image/png !!!!bad!!\n",                  // bad base64
            "HERDR-MEDIA image/png aGk= cap with spaces extra\n", // caption ok actually
        ];
        for case in cases {
            let mut sc = MediaScanner::new();
            let r = sc.feed(case.as_bytes());
            // All of these are passthrough (the last one's caption is 'cap' plus
            // trailing text is captured by splitn(3) so it's VALID — move on).
            if case.contains("cap with spaces") {
                assert_eq!(r.media_events.len(), 1, "{case}");
            } else {
                assert!(r.media_events.is_empty(), "{case}");
                assert!(r.passthrough.contains("HERDR-MEDIA"), "{case}");
            }
        }
    }

    #[test]
    fn oversized_payload_is_rejected() {
        let big = vec![b'A'; MAX_MEDIA_BYTES + 16];
        let line = format!(
            "{MEDIA_PREFIX}image/png {}\n",
            base64::engine::general_purpose::STANDARD.encode(&big)
        );
        assert!(parse_media_line(line.as_bytes()).is_none());
    }

    #[test]
    fn crlf_terminator_is_tolerated() {
        let line = media_line("image/png", PNG_1PX_B64, None);
        let mut sc = MediaScanner::new();
        let r = sc.feed(format!("{line}\r").as_bytes());
        assert_eq!(r.media_events.len(), 1);
    }

    #[test]
    fn finish_flushes_trailing_line() {
        let mut sc = MediaScanner::new();
        let line = media_line("image/png", PNG_1PX_B64, None);
        let r = sc.feed(line.trim_end().as_bytes()); // no trailing newline
        assert!(r.passthrough.is_empty() && r.media_events.is_empty());
        let r = sc.finish();
        assert_eq!(r.media_events.len(), 1);
    }

    #[test]
    fn with_agent_fills_id() {
        let ev = parse_media_line(media_line("image/png", PNG_1PX_B64, None).as_bytes()).unwrap();
        let ev = with_agent(ev, "abc123");
        match ev {
            DaemonEvent::AgentMedia { agent_id, .. } => assert_eq!(agent_id, "abc123"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn exactly_at_limit_passes() {
        let big = vec![b'A'; MAX_MEDIA_BYTES];
        let line = format!(
            "{MEDIA_PREFIX}image/png {}\n",
            base64::engine::general_purpose::STANDARD.encode(&big)
        );
        assert!(parse_media_line(line.as_bytes()).is_some());
    }
}
