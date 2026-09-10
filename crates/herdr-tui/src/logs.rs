//! Log pane data model: bounded ring of styled lines + wrap-aware render window.
//!
//! The pane stores up to [`MAX_LINES`] styled lines (the tail of the stream).
//! Render math supports both wrap mode (each logical line may occupy multiple
//! terminal rows) and no-wrap mode (one row per line, wide lines truncated by
//! the renderer). Scroll-up releases follow mode; bottom re-engages it.

use crate::ansi::{AnsiLineParser, StyledLine};

/// Kept lines per agent (log pane ring).
pub const MAX_LINES: usize = 10_000;

/// Complete log buffer for one agent.
#[derive(Debug, Default)]
pub struct LogBuffer {
    pub lines: Vec<StyledLine>,
    /// The parser owns line-splitting across chunk boundaries: `feed` only
    /// returns *completed* lines, so the buffer just stores them.
    parser: AnsiLineParser,
}

impl LogBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append raw PTY text (may contain many lines and partial escapes).
    /// Chunks split mid-line merge into one logical line inside the parser.
    pub fn feed(&mut self, raw: &str) {
        if raw.is_empty() {
            return;
        }
        for line in self.parser.feed(raw) {
            self.lines.push(line);
        }
        self.trim();
    }

    fn trim(&mut self) {
        if self.lines.len() > MAX_LINES {
            let excess = self.lines.len() - MAX_LINES;
            self.lines.drain(..excess);
        }
    }

    /// Replay a chunk of ring buffer from the daemon (used on select/resync).
    pub fn replay(&mut self, payload: &str) {
        self.clear();
        self.feed(payload);
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.parser = AnsiLineParser::new();
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    #[allow(clippy::len_without_is_empty)]
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

/// Rows needed to render a logical line at the given width (wrap mode).
pub fn rows_for(line: &StyledLine, width: u16) -> usize {
    if width == 0 {
        return 1;
    }
    let len = line.text().chars().count();
    len.div_ceil(width as usize).max(1)
}

/// A slice of the buffer to render: (line_index, start_row_within_line).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub start_line: usize,
    pub start_row: usize,
}

/// Compute the visible render window for the log pane.
///
/// * `follow=true` → anchored to the bottom (latest output).
/// * `follow=false` → anchored at `scroll_line` (top row of the viewport).
///
/// Returns the window plus the total row count (for scrollbar math).
pub fn render_window(
    lines: &[StyledLine],
    area_height: u16,
    width: u16,
    follow: bool,
    scroll_line: usize,
) -> (Window, usize) {
    let height = area_height.max(1) as usize;
    if lines.is_empty() {
        return (
            Window {
                start_line: 0,
                start_row: 0,
            },
            0,
        );
    }

    // Total rows in wrap mode.
    let total_rows: usize = lines.iter().map(|l| rows_for(l, width)).sum();

    if follow || scroll_line >= total_rows {
        // Walk from the end accumulating rows until we cover `height`.
        let mut start_line = lines.len();
        let mut start_row = 0usize;
        let mut covered = 0usize;
        for (idx, line) in lines.iter().enumerate().rev() {
            let rows = rows_for(line, width);
            if covered + rows >= height {
                // This line is (partially) the top of the window.
                let needed = height - covered;
                start_line = idx;
                start_row = rows.saturating_sub(needed);
                break;
            }
            covered += rows;
            start_line = idx;
            start_row = 0;
        }
        return (
            Window {
                start_line,
                start_row,
            },
            total_rows,
        );
    }

    // Scroll mode: top of viewport is `scroll_line` row offset 0 (logical-line
    // granularity; sub-line rows are approximated by clamping).
    let start_line = scroll_line.min(lines.len() - 1);
    (
        Window {
            start_line,
            start_row: 0,
        },
        total_rows,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Style;

    fn line(text: &str) -> StyledLine {
        StyledLine {
            spans: vec![crate::ansi::Span {
                content: text.into(),
                style: Style::default(),
            }],
        }
    }

    #[test]
    fn feed_splits_lines_and_preserves_text() {
        let mut buf = LogBuffer::new();
        buf.feed("alpha\nbeta\n");
        assert_eq!(buf.len(), 2);
        assert_eq!(buf.lines[0].text(), "alpha");
        assert_eq!(buf.lines[1].text(), "beta");
    }

    #[test]
    fn feed_chunk_boundaries_merge_open_line() {
        let mut buf = LogBuffer::new();
        buf.feed("hel");
        buf.feed("lo wor");
        buf.feed("ld\nnext\n");
        assert_eq!(buf.len(), 2);
        assert_eq!(buf.lines[0].text(), "hello world");
        assert_eq!(buf.lines[1].text(), "next");
    }

    #[test]
    fn replay_replaces_buffer() {
        let mut buf = LogBuffer::new();
        buf.feed("old\n");
        buf.replay("new1\nnew2\n");
        assert_eq!(buf.len(), 2);
        assert_eq!(buf.lines[0].text(), "new1");
    }

    #[test]
    fn ring_trims_to_max() {
        let mut buf = LogBuffer::new();
        // Feed 12,000 lines in newline-terminated batches of 100.
        for batch in (0..12_000).step_by(100) {
            let chunk: String = (batch..batch + 100).map(|i| format!("line{i}\n")).collect();
            buf.feed(&chunk);
        }
        assert_eq!(buf.len(), MAX_LINES);
        assert_eq!(buf.lines.last().unwrap().text(), "line11999");
        assert_eq!(buf.lines[0].text(), format!("line{}", 12_000 - MAX_LINES));
    }

    #[test]
    fn styles_survive_chunk_boundary() {
        let mut buf = LogBuffer::new();
        buf.feed("\x1b[32m");
        buf.feed("green\n");
        assert_eq!(
            buf.lines[0].spans[0].style.fg,
            Some(ratatui::style::Color::Green)
        );
        assert_eq!(buf.lines[0].text(), "green");
    }

    #[test]
    fn rows_for_handles_wrap_and_empty() {
        assert_eq!(rows_for(&line(""), 40), 1);
        assert_eq!(rows_for(&line("short"), 40), 1);
        assert_eq!(rows_for(&line(&"x".repeat(80)), 40), 2);
        assert_eq!(rows_for(&line(&"x".repeat(81)), 40), 3);
    }

    #[test]
    fn window_follow_anchors_bottom() {
        let lines: Vec<StyledLine> = (0..100).map(|i| line(&format!("L{i}"))).collect();
        let (win, total) = render_window(&lines, 10, 80, true, 0);
        assert_eq!(total, 100);
        assert_eq!(win.start_line, 90); // last 10 lines fit
        assert_eq!(win.start_row, 0);
    }

    #[test]
    fn window_scroll_uses_scroll_line() {
        let lines: Vec<StyledLine> = (0..100).map(|i| line(&format!("L{i}"))).collect();
        let (win, _) = render_window(&lines, 10, 80, false, 30);
        assert_eq!(win.start_line, 30);
    }

    #[test]
    fn window_scroll_past_end_shows_bottom() {
        let lines: Vec<StyledLine> = (0..10).map(|i| line(&format!("L{i}"))).collect();
        // Scrolling past the end lands on the bottom viewport (lines 5..10
        // fill a 5-row pane), not an empty tail.
        let (win, _) = render_window(&lines, 5, 80, false, 999);
        assert_eq!(win.start_line, 5);
    }

    #[test]
    fn window_wrapped_lines_count_rows() {
        // 3 lines of 80 chars at width 40 → 2 rows each → 6 rows total.
        let lines: Vec<StyledLine> = (0..3).map(|_| line(&"x".repeat(80))).collect();
        let (win, total) = render_window(&lines, 6, 40, true, 0);
        assert_eq!(total, 6);
        assert_eq!(win.start_line, 0);
        assert_eq!(win.start_row, 0);
    }

    #[test]
    fn window_wrapped_bottom_viewport_may_start_mid_line() {
        // 5 wrapped lines (2 rows each = 10 rows) with height 3: the last 3
        // rows are line3-row1, line4-row0, line4-row1 → window starts there.
        let lines: Vec<StyledLine> = (0..5).map(|_| line(&"x".repeat(80))).collect();
        let (win, _) = render_window(&lines, 3, 40, true, 0);
        assert_eq!(win.start_line, 3);
        assert_eq!(win.start_row, 1);
    }

    #[test]
    fn window_empty_buffer() {
        let (win, total) = render_window(&[], 10, 80, true, 0);
        assert_eq!(total, 0);
        assert_eq!(win.start_line, 0);
    }
}
