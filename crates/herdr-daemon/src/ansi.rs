//! ANSI stripping: converts raw PTY bytes into plain text for the state machine.
//!
//! A tiny VT parser — enough to skip CSI sequences, OSC sequences and other escapes
//! without pulling a heavyweight terminal emulator dependency. Text payload bytes are
//! passed through untouched.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum ParseState {
    #[default]
    Ground,
    Escape,
    Csi,
    Osc,
    /// Escape sequences of the form `ESC ( B`, `ESC # 8`, etc.
    EscIntermediates,
}

/// Incremental ANSI stripper. Feed raw bytes, get clean text out.
#[derive(Debug, Default)]
pub struct AnsiStripper {
    state: ParseState,
    /// Bytes collected inside the current escape sequence (for OSC termination checks).
    osc_seen_bel: bool,
}

impl AnsiStripper {
    pub fn new() -> Self {
        Self::default()
    }

    /// Process raw PTY bytes; returns the printable text they contained.
    pub fn feed(&mut self, bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len());
        for &b in bytes {
            self.step(b, &mut out);
        }
        out
    }

    fn step(&mut self, b: u8, out: &mut String) {
        match self.state {
            ParseState::Ground => match b {
                0x1b => self.state = ParseState::Escape,
                // Treat other C0 controls (except \n, \r, \t) as ignorable.
                b'\n' | b'\r' | b'\t' => out.push(b as char),
                0x00..=0x08 | 0x0b | 0x0c | 0x0e..=0x1a | 0x1c..=0x1f => {}
                _ => {
                    // Pass UTF-8 through byte by byte; the caller reassembles strings.
                    // We collect into a Vec-free path by pushing only valid single bytes
                    // and letting multi-byte sequences accumulate as raw chars via
                    // from_utf8 lossless reconstruction below.
                    out.push(b as char);
                }
            },
            ParseState::Escape => match b {
                b'[' => self.state = ParseState::Csi,
                b']' => {
                    self.state = ParseState::Osc;
                    self.osc_seen_bel = false;
                }
                b'(' | b')' | b'#' | b'%' => self.state = ParseState::EscIntermediates,
                0x1b => {} // ESC ESC — stay in Escape
                _ => self.state = ParseState::Ground,
            },
            ParseState::Csi => {
                // Final byte range 0x40..=0x7E terminates a CSI sequence.
                if (0x40..=0x7e).contains(&b) {
                    self.state = ParseState::Ground;
                }
            }
            ParseState::Osc => match b {
                0x07 => {
                    self.state = ParseState::Ground;
                }
                0x1b => self.state = ParseState::Escape,
                _ => {}
            },
            ParseState::EscIntermediates => {
                self.state = ParseState::Ground;
            }
        }
    }
}

/// Strip ANSI from a one-shot byte buffer. Multi-byte UTF-8 is preserved via
/// lossy reconstruction (the state machine never splits text output; it only skips
/// escape bytes, so we can re-decode the survivors lossily).
pub fn strip_ansi(bytes: &[u8]) -> String {
    let mut stripper = AnsiStripper::new();
    let raw = stripper.feed_raw(bytes);
    String::from_utf8_lossy(&raw).into_owned()
}

impl AnsiStripper {
    /// Like [`feed`](Self::feed) but byte-faithful: text bytes are preserved exactly
    /// (UTF-8 stays intact across chunk boundaries handled by the caller).
    pub fn feed_raw(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(bytes.len());
        for &b in bytes {
            match self.state {
                ParseState::Ground => match b {
                    0x1b => self.state = ParseState::Escape,
                    b'\n' | b'\r' | b'\t' => out.push(b),
                    0x00..=0x08 | 0x0b | 0x0c | 0x0e..=0x1a | 0x1c..=0x1f => {}
                    _ => out.push(b),
                },
                ParseState::Escape => match b {
                    b'[' => self.state = ParseState::Csi,
                    b']' => self.state = ParseState::Osc,
                    b'(' | b')' | b'#' | b'%' => self.state = ParseState::EscIntermediates,
                    0x1b => {}
                    _ => self.state = ParseState::Ground,
                },
                ParseState::Csi => {
                    if (0x40..=0x7e).contains(&b) {
                        self.state = ParseState::Ground;
                    }
                }
                ParseState::Osc => match b {
                    0x07 => self.state = ParseState::Ground,
                    0x1b => self.state = ParseState::Escape,
                    _ => {}
                },
                ParseState::EscIntermediates => self.state = ParseState::Ground,
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_color_codes() {
        assert_eq!(strip_ansi(b"\x1b[32mgreen\x1b[0m plain"), "green plain");
    }

    #[test]
    fn strips_cursor_and_clear_sequences() {
        assert_eq!(strip_ansi(b"\x1b[2J\x1b[H\x1b[1;31mhi\x1b[K"), "hi");
    }

    #[test]
    fn strips_osc_title_sequences() {
        assert_eq!(strip_ansi(b"\x1b]0;my title\x07text"), "text");
        assert_eq!(strip_ansi(b"\x1b]2;t\x1b\\text"), "text");
    }

    #[test]
    fn preserves_newlines_and_tabs() {
        assert_eq!(strip_ansi(b"a\r\nb\tc"), "a\r\nb\tc");
    }

    #[test]
    fn preserves_multibyte_utf8() {
        let input = "héllo ❯ 世界".as_bytes();
        assert_eq!(strip_ansi(input), "héllo ❯ 世界");
    }

    #[test]
    fn escape_split_across_feed_boundaries() {
        let mut s = AnsiStripper::new();
        let a = s.feed_raw(b"\x1b");
        assert!(a.is_empty());
        let b = s.feed_raw(b"[31mred\x1b[0m");
        assert_eq!(String::from_utf8_lossy(&b), "red");
    }

    #[test]
    fn unterminated_escape_at_eof_is_dropped() {
        assert_eq!(strip_ansi(b"ok\x1b[3"), "ok");
    }

    #[test]
    fn c0_controls_dropped_but_formatting_kept() {
        assert_eq!(strip_ansi(b"a\x07b\x08c"), "abc");
    }
}
