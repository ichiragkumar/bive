//! herdr-tui — Ratatui terminal client for herdr (Phase 2).
//!
//! See `specs/11-phase-2-tui.md`. The TUI is just another socket client: it
//! subscribes to the daemon's global event stream and issues the same
//! `ClientCommand`s as the CLI. Closing the TUI never disturbs an agent.
//!
//! The reusable parts (client, app state, ANSI parsing, rendering) live in the
//! library root (`src/lib.rs`) so integration tests exercise the real code.

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use herdr_protocol::{ClientCommand, DaemonEvent};
use herdr_tui::app::{App, ConnState, Mode};
use herdr_tui::client::HerdrClient;
use herdr_tui::ui;

fn main() -> Result<()> {
    let mut client = HerdrClient::connect()
        .map_err(|e| anyhow::anyhow!("{e} — start the daemon with `herdr daemon` first"))?;
    let event_rx = client.start_reader();

    let mut terminal = ratatui::init();
    let mut app = App::new();
    let result = run(&mut terminal, &mut app, &mut client, &event_rx);

    ratatui::restore();

    result
}

fn run(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    client: &mut HerdrClient,
    event_rx: &Receiver<DaemonEvent>,
) -> Result<()> {
    // Initial sync: agent list, global event subscription, log backfill.
    match client.request(ClientCommand::List) {
        Ok(resp) => app.apply_response(resp),
        Err(_) => app.connection_state = ConnState::Reconnecting,
    }
    let _ = client.request(ClientCommand::Events);
    backfill_selected(app, client);

    loop {
        // 1. Drain socket events (non-blocking).
        for event in drain_events(event_rx) {
            app.apply_event(event);
        }
        // 2. Drain user keys.
        for key in poll_keys()? {
            let pending = app.handle_key(key);
            if let Some(req) = app.request_needed.take() {
                match client.request(req) {
                    Ok(resp) => app.apply_response(resp),
                    Err(e) => app.flash_now(format!("{e}")),
                }
            }
            for line in pending {
                let _ = client.request(ClientCommand::SendInput {
                    agent_id: line.agent_id,
                    text: line.text,
                    raw: line.raw,
                });
            }
            if app.selection_dirty {
                app.selection_dirty = false;
                backfill_selected(app, client);
            }
            if app.should_quit {
                return Ok(());
            }
        }
        // 3. Reconnect if the socket died.
        if !client.is_connected() {
            app.connection_state = ConnState::Reconnecting;
            client.reconnect();
            if client.is_connected() {
                app.connection_state = ConnState::Connected;
                app.resync();
                if let Ok(resp) = client.request(ClientCommand::List) {
                    app.apply_response(resp);
                }
                let _ = client.request(ClientCommand::Events);
                backfill_selected(app, client);
            }
        }
        // 4. Redraw (50 ms tick from poll_keys paces the loop).
        terminal
            .draw(|f| ui::draw(f, app))
            .context("drawing frame")?;
    }
}

fn drain_events(rx: &Receiver<DaemonEvent>) -> Vec<DaemonEvent> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        out.push(ev);
    }
    out
}

/// Request ring-buffer replay (64 KB) for the selected agent so the log pane
/// has history before live events arrive.
fn backfill_selected(app: &mut App, client: &HerdrClient) {
    let Some(id) = app.selected_id().map(str::to_string) else {
        return;
    };
    if let Ok(resp) = client.request(ClientCommand::Logs {
        agent_id: id,
        max_bytes: 64 * 1024,
    }) {
        app.apply_response(resp);
    }
    // A missing agent between list and logs is harmless.
}

/// Poll crossterm for key events with a 50 ms timeout; returns all keys seen.
/// Ctrl-C is translated to `q` (quit TUI only — agents are never killed).
fn poll_keys() -> Result<Vec<KeyEvent>> {
    let mut keys = Vec::new();
    if !event::poll(Duration::from_millis(50))? {
        return Ok(keys);
    }
    loop {
        match event::read()? {
            Event::Key(k) => {
                if matches!(k.code, KeyCode::Char('c'))
                    && k.modifiers.contains(KeyModifiers::CONTROL)
                {
                    keys.push(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::empty()));
                } else {
                    keys.push(k);
                }
            }
            Event::Resize(_, _) => {}
            _ => {}
        }
        if !event::poll(Duration::from_millis(0))? {
            break;
        }
    }
    Ok(keys)
}

// Keep DaemonEvent in scope for the doc example above.
#[allow(dead_code)]
fn _event_type_check(_e: &DaemonEvent) {}
// Mode is referenced in app; silence unused if configurations change.
#[allow(dead_code)]
fn _mode_type_check(_m: Mode) {}
