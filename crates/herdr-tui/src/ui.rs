//! Frame renderer: summary bar / fleet list / log pane / status bar (+ overlays).

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::app::{App, ConnState, Focus, Mode};
use crate::logs::rows_for;
use herdr_protocol::AgentState;

/// Draw one frame. Layout:
/// ┌ summary bar (1 row) ───────────────┐
/// ├ fleet (30%) ┊ log pane (70%) ──────┤
/// ├ status bar (1 row) ────────────────┘
pub fn draw(f: &mut Frame, app: &App) {
    let [summary, body, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(f.area());

    draw_summary(f, app, summary);
    draw_body(f, app, body);
    draw_status(f, app, status);

    match (app.mode, app.connection_state) {
        (_, ConnState::Reconnecting) => draw_reconnect_overlay(f, f.area()),
        (Mode::SendInput, _) => draw_send_modal(f, f.area(), app),
        (Mode::ConfirmKill, _) => draw_confirm_modal(f, f.area(), "Kill selected agent? (y/n)"),
        _ => {}
    }
}

fn draw_summary(f: &mut Frame, app: &App, area: Rect) {
    let (w, i, b, e, x) = app.counts();
    let conn = match app.connection_state {
        ConnState::Connected => Span::from("● connected ").green(),
        ConnState::Reconnecting => Span::from("◌ reconnecting… ").yellow(),
    };
    let line = Line::from(vec![
        conn,
        Span::from("│ ").dark_gray(),
        Span::from(format!("working {w} ")).green(),
        Span::from(format!("idle {i} ")).dark_gray(),
        Span::from(format!("blocked {b} ")).yellow(),
        Span::from(format!("errored {e} ")).red(),
        Span::from(format!("exited {x} ")).dark_gray(),
        Span::from("│ ").dark_gray(),
        Span::from(format!("{} agents", app.order.len())),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn draw_body(f: &mut Frame, app: &App, area: Rect) {
    let [fleet, logs] = Layout::horizontal([
        Constraint::Percentage(30),
        Constraint::Percentage(70),
    ])
    .areas(area);

    draw_fleet(f, app, fleet);
    draw_log_pane(f, app, logs);
}

fn draw_fleet(f: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app
        .order
        .iter()
        .map(|id| {
            let view = &app.agents[id];
            ListItem::new(agent_line(view, id == app.selected_id().unwrap_or("")))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::RIGHT)
                .title(Span::from(" Fleet ").bold()),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    // We implement highlight manually via reversed style above on selection;
    // keep ListState aligned with selection index.
    let mut state = ListState::default();
    state.select(Some(app.selected.min(app.order.len().saturating_sub(1))));
    f.render_stateful_widget(list, area, &mut state);
}

fn agent_line(view: &crate::app::AgentView, selected: bool) -> Line<'_> {
    let info = &view.info;
    let mut spans = vec![
        Span::from(format!("{} ", state_glyph(&info.state))),
        Span::from(format!("{:<12} ", info.id)).style(Style::default().add_modifier(Modifier::BOLD)),
        Span::from(format!("{:<11} ", info.profile)).style(Style::default().dim()),
        Span::from(state_label(&info.state)),
    ];
    if selected {
        spans.insert(0, Span::from("▸ "));
    } else {
        spans.insert(0, Span::from("  "));
    }
    Line::from(spans)
}

fn state_glyph(state: &AgentState) -> &'static str {
    match state {
        AgentState::Starting => "◔",
        AgentState::Working => "●",
        AgentState::Idle => "◌",
        AgentState::Blocked => "■",
        AgentState::Errored(_) => "✖",
        AgentState::Exited(_) => "·",
    }
}

fn state_label(state: &AgentState) -> &'static str {
    match state {
        AgentState::Starting => "starting",
        AgentState::Working => "working",
        AgentState::Idle => "idle",
        AgentState::Blocked => "blocked",
        AgentState::Errored(_) => "errored",
        AgentState::Exited(_) => "exited",
    }
}

fn state_color(state: &AgentState) -> Style {
    match state {
        AgentState::Starting | AgentState::Working => Style::default().green(),
        AgentState::Idle => Style::default().dark_gray(),
        AgentState::Blocked => Style::default().yellow(),
        AgentState::Errored(_) => Style::default().red(),
        AgentState::Exited(_) => Style::default().dark_gray(),
    }
}

fn draw_log_pane(f: &mut Frame, app: &App, area: Rect) {
    let title = match app.selected_id() {
        Some(id) => {
            let view = &app.agents[id];
            Span::from(format!(
                " {} · {} · {} ",
                id,
                view.info.profile,
                view.info.command
            ))
            .style(state_color(&view.info.state))
        }
        None => Span::from(" Log "),
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_bottom(Span::from(if app.follow { " follow ⏬ " } else { " scrolled ⏸ " }).dim());

    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(id) = app.selected_id() else {
        f.render_widget(
            Paragraph::new("No agent selected — spawn one with `herdr spawn`"),
            inner,
        );
        return;
    };
    let view = &app.agents[id];
    let width = inner.width.saturating_sub(1).max(1);
    let (window, total_rows) = crate::logs::render_window(
        &view.logs.lines,
        inner.height,
        width,
        app.follow,
        app.scroll_line,
    );
    if total_rows == 0 {
        f.render_widget(Paragraph::new("(no output yet)"), inner);
        return;
    }

    // Render rows from the window, honoring per-line wrap offsets.
    let mut rows_left = inner.height as usize;
    let mut line_idx = window.start_line;
    let mut skip_rows = window.start_row;
    let mut rendered: Vec<Line> = Vec::with_capacity(inner.height as usize);

    while line_idx < view.logs.lines.len() && rows_left > 0 {
        let line = &view.logs.lines[line_idx];
        let text = line.text();
        let chars: Vec<char> = text.chars().collect();
        let total = rows_for(line, width);
        let mut row = 0usize;
        while row < total && rows_left > 0 {
            if skip_rows > 0 {
                skip_rows -= 1;
                row += 1;
                continue;
            }
            let start = row * width as usize;
            let slice: String = chars
                .iter()
                .skip(start)
                .take(width as usize)
                .collect();
            if slice.is_empty() {
                rendered.push(Line::from(""));
            } else {
                // Style: take the span covering this slice's start (approximation
                // is acceptable for MVP; spans are contiguous runs).
                let style = span_style_at(line, start);
                rendered.push(Line::from(Span::styled(slice, style)));
            }
            rows_left -= 1;
            row += 1;
        }
        line_idx += 1;
    }

    f.render_widget(Paragraph::new(rendered), inner);
}

/// Style for the span covering char offset `at` (best effort).
fn span_style_at(line: &crate::ansi::StyledLine, at: usize) -> Style {
    let mut offset = 0usize;
    for span in &line.spans {
        let len = span.content.chars().count();
        if at < offset + len {
            return span.style;
        }
        offset += len;
    }
    line.spans
        .last()
        .map(|s| s.style)
        .unwrap_or_default()
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let sel = app.selected_id().unwrap_or("-");
    let focus_label = match app.focus {
        Focus::Fleet => "fleet",
        Focus::Log => "log",
    };
    let flash = app.flash.clone().unwrap_or_default();
    let line = Line::from(vec![
        Span::from(" q quit ").bold(),
        Span::from(" j/k select ").dim(),
        Span::from(" s send ").dim(),
        Span::from(" K kill ").dim(),
        Span::from(" f follow ").dim(),
        Span::from(" g/G top/bottom ").dim(),
        Span::from("│ ").dark_gray(),
        Span::from(format!("sel {sel} · focus {focus_label} ")).dim(),
        Span::from(flash).yellow(),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

// ---- overlays ---------------------------------------------------------------

fn draw_reconnect_overlay(f: &mut Frame, area: Rect) {
    let text = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  ◌ connection to daemon lost — reconnecting…  ".to_string(),
            Style::default().yellow().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "  agents keep running; this view resyncs on reattach.  ",
            Style::default().dim(),
        )),
    ];
    let block = Paragraph::new(text).centered();
    let popup = centered_rect(area, 60, 5);
    f.render_widget(ratatui::widgets::Clear, popup);
    f.render_widget(
        block.block(Block::default().borders(Borders::ALL).style(Style::default().yellow())),
        popup,
    );
}

fn draw_send_modal(f: &mut Frame, area: Rect, app: &App) {
    let sel = app.selected_id().unwrap_or("-");
    let text = vec![
        Line::from(vec![
            Span::from(" send to "),
            Span::from(sel.to_string()).bold(),
            Span::from("  (Enter submits, Esc cancels)"),
        ]),
        Line::from(Span::from(format!(" > {}", app.input)).style(Style::default().green())),
    ];
    let popup = centered_rect(area, 70, 4);
    f.render_widget(ratatui::widgets::Clear, popup);
    f.render_widget(
        Paragraph::new(text).block(Block::default().borders(Borders::ALL).title(" Send input ")),
        popup,
    );
}

fn draw_confirm_modal(f: &mut Frame, area: Rect, message: &str) {
    let text = vec![Line::from(Span::styled(
        message.to_string(),
        Style::default().red().add_modifier(Modifier::BOLD),
    ))];
    let popup = centered_rect(area, 50, 4);
    f.render_widget(ratatui::widgets::Clear, popup);
    f.render_widget(
        Paragraph::new(text).block(Block::default().borders(Borders::ALL).title(" Confirm ")),
        popup,
    );
}

fn centered_rect(area: Rect, percent_x: u16, height: u16) -> Rect {
    let w = area.width * percent_x / 100;
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect { x, y, width: w.min(area.width), height: height.min(area.height) }
}
