//! ANSI-to-spans parser for the TUI log pane.
//!
//! Decodes SGR sequences (colors, bold, italic, underline, dim, reverse) into
//! `ratatui::style::Style`, skips cursor-movement/other escapes, and splits text
//! into lines of styled spans. Handles escape sequences split across feed
//! boundaries via the incremental [`AnsiLineParser`].

use ratatui::style::{Color, Modifier, Style};

/// A run of text sharing one style within a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub content: String,
    pub style: Style,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyledLine {
    pub spans: Vec<Span>,
}

impl StyledLine {
    pub fn plain(content: impl Into<String>) -> Self {
        Self {
            spans: vec![Span { content: content.into(), style: Style::default() }],
        }
    }

    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.content.as_str()).collect()
    }
}

/// Interior mutable parser that tolerates split escape sequences.
#[derive(Debug, Default)]
pub struct AnsiLineParser {
    /// Carry: bytes of an unfinished escape sequence.
    carry: Vec<u8>,
}

impl AnsiLineParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a raw chunk (possibly containing many lines and partial escapes);
    /// returns completed styled lines. Call [`finish_line`](Self::finish_line)
    /// when the stream ends to flush the trailing partial line.
    pub fn feed(&mut self, raw: &str) -> Vec<StyledLine> {
        let mut bytes = std::mem::take(&mut self.carry);
        bytes.extend_from_slice(raw.as_bytes());

        let mut lines = Vec::new();
        let mut cur = LineBuilder::default();
        let mut i = 0usize;
        while i < bytes.len() {
            match bytes[i] {
                0x1b => {
                    // Try to consume one escape sequence starting at i.
                    match consume_escape(&bytes[i..]) {
                        Some((sgr, used)) => {
                            if let Some(style_delta) = sgr {
                                cur.apply_sgr(&style_delta);
                            }
                            i += used;
                            continue;
                        }
                        None => {
                            // Incomplete sequence: carry the rest for next feed.
                            self.carry = bytes[i..].to_vec();
                            break;
                        }
                    }
                }
                b'\n' => {
                    lines.push(cur.finish());
                    i += 1;
                }
                b'\r' => {
                    // Simple strategy: carriage return overwrites from col 0.
                    cur.cr();
                    i += 1;
                }
                _ => {
                    // Decode one UTF-8 scalar (may be multiple bytes).
                    let s = decode_char(&bytes[i..]);
                    match s {
                        Some((ch, used)) => {
                            cur.push_char(ch);
                            i += used;
                        }
                        None => {
                            // Incomplete UTF-8: carry remainder.
                            self.carry = bytes[i..].to_vec();
                            break;
                        }
                    }
                }
            }
        }
        if self.carry.is_empty() {
            lines.push(cur.finish_partial());
        } else {
            lines.push(cur.finish_partial());
        }
        lines
    }

    /// Flush the trailing partial line (without newline) at end of stream.
    pub fn finish_line(&mut self) -> Option<StyledLine> {
        if self.carry.is_empty() {
            return None;
        }
        let bytes = std::mem::take(&mut self.carry);
        let mut parser = AnsiLineParser::default();
        let mut lines = parser.feed(&String::from_utf8_lossy(&bytes));
        lines.pop()
    }
}

/// Decode a single UTF-8 char from bytes; returns (char, bytes_consumed).
fn decode_char(bytes: &[u8]) -> Option<(char, usize)> {
    let need = match bytes.first()? {
        b if *b < 0x80 => 1,
        b if *b < 0xC0 => return Some((char::REPLACEMENT_CHARACTER, 1)),
        b if *b < 0xE0 => 2,
        b if *b < 0xF0 => 3,
        _ => 4,
    };
    if bytes.len() < need {
        return None; // incomplete
    }
    match std::str::from_utf8(&bytes[..need]) {
        Ok(s) => s.chars().next().map(|c| (c, need)),
        Err(_) => Some((char::REPLACEMENT_CHARACTER, 1)),
    }
}

/// One SGR "style delta": the list of params from a `ESC[...m` sequence.
type Sgr = Vec<u16>;

/// Try to consume one escape sequence at the start of `bytes`.
/// Returns `Some((Some(sgr_params), used))` for SGR, `Some((None, used))` for
/// other consumed sequences, `None` if the sequence is incomplete.
fn consume_escape(bytes: &[u8]) -> Option<(Option<Sgr>, usize)> {
    if bytes.len() < 2 {
        return None;
    }
    match bytes[1] {
        b'[' => {
            // CSI: params 0x30-0x3F, intermediates 0x20-0x2F, final 0x40-0x7E.
            let mut i = 2usize;
            loop {
                let Some(&b) = bytes.get(i) else {
                    return None; // incomplete
                };
                if (0x40..=0x7e).contains(&b) {
                    let params: Vec<u16> = String::from_utf8_lossy(&bytes[2..i])
                        .split(|c: char| c == ';')
                        .map(|p| p.parse::<u16>().unwrap_or(0))
                        .collect();
                    let is_sgr = b == b'm';
                    return Some((if is_sgr { Some(params) } else { None }, i + 1));
                }
                if !(0x20..=0x3f).contains(&b) {
                    return Some((None, i + 1)); // malformed; skip
                }
                i += 1;
            }
        }
        b']' => {
            // OSC: terminated by BEL (0x07) or ST (ESC \).
            let mut i = 2usize;
            loop {
                let Some(&b) = bytes.get(i) else {
                    return None;
                };
                if b == 0x07 {
                    return Some((None, i + 1));
                }
                if b == 0x1b && bytes.get(i + 1) == Some(&b'\\') {
                    return Some((None, i + 2));
                }
                i += 1;
            }
        }
        // Two-byte escapes: ESC ( B, ESC # 8, ESC >, etc.
        0x1b => Some((None, 2)), // ESC ESC — treat as skipped pair
        _ => Some((None, 2)),
    }
}

/// Accumulates styled spans for one line, tracking the current SGR state.
#[derive(Default)]
struct LineBuilder {
    spans: Vec<Span>,
    cur: String,
    fg: Option<Color>,
    bg: Option<Color>,
    mods: Modifier,
}

impl LineBuilder {
    fn current_style(&self) -> Style {
        let mut s = Style::default();
        if let Some(fg) = self.fg {
            s = s.fg(fg);
        }
        if let Some(bg) = self.bg {
            s = s.bg(bg);
        }
        s.add_modifier(self.mods)
    }

    fn push_char(&mut self, c: char) {
        self.cur.push(c);
    }

    /// `\r` — restart the line (best-effort progress-bar handling).
    fn cr(&mut self) {
        self.spans.clear();
        self.cur.clear();
    }

    fn apply_sgr(&mut self, params: &[u16]) {
        if params.is_empty() {
            params_sgr(self, &[0]);
            return;
        }
        params_sgr(self, params);
    }

    fn finish(mut self) -> StyledLine {
        self.flush();
        StyledLine { spans: self.spans }
    }

    fn finish_partial(mut self) -> StyledLine {
        self.flush();
        StyledLine { spans: self.spans }
    }

    fn flush(&mut self) {
        if !self.cur.is_empty() {
            self.spans.push(Span {
                content: std::mem::take(&mut self.cur),
                style: self.current_style(),
            });
        }
    }
}

fn params_sgr(b: &mut LineBuilder, params: &[u16]) {
    let mut i = 0usize;
    while i < params.len() {
        match params[i] {
            0 => {
                b.fg = None;
                b.bg = None;
                b.mods = Modifier::empty();
            }
            1 => b.mods |= Modifier::BOLD,
            2 => b.mods |= Modifier::DIM,
            3 => b.mods |= Modifier::ITALIC,
            4 => b.mods |= Modifier::UNDERLINED,
            7 => b.mods |= Modifier::REVERSED,
            9 => b.mods |= Modifier::CROSSED_OUT,
            22 => b.mods &= !(Modifier::BOLD | Modifier::DIM),
            23 => b.mods &= !Modifier::ITALIC,
            24 => b.mods &= !Modifier::UNDERLINED,
            27 => b.mods &= !Modifier::REVERSED,
            29 => b.mods &= !Modifier::CROSSED_OUT,
            30..=37 => b.fg = Some(Color::Indexed((params[i] - 30) as u8)),
            38 => {
                // extended fg: 38;5;n or 38;2;r;g;b
                if let Some((color, consumed)) = extended_color(&params[i..]) {
                    b.fg = Some(color);
                    i += consumed;
                }
            }
            39 => b.fg = None,
            40..=47 => b.bg = Some(Color::Indexed((params[i] - 40) as u8)),
            48 => {
                if let Some((color, consumed)) = extended_color(&params[i..]) {
                    b.bg = Some(color);
                    i += consumed;
                }
            }
            49 => b.bg = None,
            90..=97 => b.fg = Some(Color::Indexed((params[i] - 90 + 8) as u8)),
            100..=107 => b.bg = Some(Color::Indexed((params[i] - 100 + 8) as u8)),
            _ => {}
        }
        i += 1;
    }
}

/// Parse `38;5;n` / `48;2;r;g;b` (also colon-separated variants collapsed by the
/// caller into semicolons). Returns the color and how many params were consumed.
fn extended_color(params: &[u16]) -> Option<(Color, usize)> {
    match params.get(1)? {
        5 => {
            let n = *params.get(2)?;
            Some((Color::Indexed(n as u8), 3))
        }
        2 => {
            let r = *params.get(2)?;
            let g = *params.get(3)?;
            let b = *params.get(4)?;
            Some((Color::Rgb(r as u8, g as u8, b as u8), 5))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(input: &str) -> Vec<StyledLine> {
        let mut p = AnsiLineParser::new();
        p.feed(input)
    }

    #[test]
    fn plain_text_is_unstyled() {
        let lines = feed_all("hello\nworld\n");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text(), "hello");
        assert_eq!(lines[0].spans[0].style, Style::default());
    }

    #[test]
    fn color_codes_become_span_styles() {
        let lines = feed_all("\x1b[32mgreen\x1b[0m plain\n");
        assert_eq!(lines.len(), 1);
        let spans = &lines[0].spans;
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].content, "green");
        assert_eq!(spans[0].style.fg, Some(Color::Green));
        assert_eq!(spans[1].content, " plain");
        assert_eq!(spans[1].style.fg, None);
    }

    #[test]
    fn bold_and_colors_combine() {
        let lines = feed_all("\x1b[1;31mred bold\x1b[0m\n");
        let s = &lines[0].spans[0].style;
        assert_eq!(s.fg, Some(Color::Red));
        assert!(s.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn bright_colors_map_to_indexed_8_to_15() {
        let lines = feed_all("\x1b[91mbright red\n");
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::Indexed(9)));
    }

    #[test]
    fn truecolor_supported() {
        let lines = feed_all("\x1b[38;2;255;128;0morange\n");
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::Rgb(255, 128, 0)));
    }

    #[test]
    fn cursor_and_clear_sequences_are_skipped() {
        let lines = feed_all("\x1b[2J\x1b[H\x1b[1;31mhi\x1b[K\n");
        assert_eq!(lines[0].text(), "hi");
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::Red));
    }

    #[test]
    fn osc_title_sequences_are_skipped() {
        let lines = feed_all("\x1b]0;my title\x07text\n");
        assert_eq!(lines[0].text(), "text");
        let lines = feed_all("\x1b]2;t\x1b\\more\n");
        assert_eq!(lines[0].text(), "more");
    }

    #[test]
    fn escape_split_across_feeds_is_handled() {
        let mut p = AnsiLineParser::new();
        let first = p.feed("abc\x1b");
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].text(), "abc");
        let second = p.feed("[31mred\n");
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].text(), "red");
        assert_eq!(second[0].spans[0].style.fg, Some(Color::Red));
    }

    #[test]
    fn utf8_multibyte_preserved_and_split_safe() {
        let mut p = AnsiLineParser::new();
        let a = p.feed("hél");
        assert_eq!(a[0].text(), "hél");
        let b = p.feed("lo ❯\n");
        assert_eq!(b[0].text(), "lo ❯");
    }

    #[test]
    fn progress_bar_cr_restarts_line() {
        // " 10%\r 50%\r done" → one line " done"
        let lines = feed_all(" 10%\r 50%\r done\n");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text(), " done");
    }

    #[test]
    fn multiple_codes_reuse_window() {
        let lines = feed_all("\x1b[31mA\x1b[32mB\x1b[0mC\n");
        let spans = &lines[0].spans;
        assert_eq!(spans[0].style.fg, Some(Color::Red));
        assert_eq!(spans[1].style.fg, Some(Color::Green));
        assert_eq!(spans[2].style.fg, None);
    }

    #[test]
    fn trailing_partial_line_flushes_without_newline() {
        let mut p = AnsiLineParser::new();
        let lines = p.feed("no newline");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text(), "no newline");
    }
}
