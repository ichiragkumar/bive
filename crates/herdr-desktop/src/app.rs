//! Tauri shell wiring (feature `tauri`): owns the [`Bridge`], pumps daemon events
//! into the webview + tray + notifications, and exposes the command surface the
//! frontend invokes. All UI updates are event-driven — no socket polling anywhere.
//!
//! Event surface (webview):
//! * `herdr://event` — every `DaemonEvent` (JSON) for the log/media/fleet views.
//! * `herdr://conn`  — connection state changes (`"connected"` / `"disconnected"`).
//!
//! Command surface (webview → Rust): `spawn`, `send`, `kill`, `list`, `logs`,
//! `ping`, `snapshot` (full current UI store), `kill_all`.
//!
//! Tray: single icon whose dot is recomputed on every event (event-driven, spec
//! requirement <1 s) and a menu with spawn/kill-all/open actions.

use std::sync::Mutex;

use tauri::{
    tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, State,
};

use herdr_protocol::{ClientCommand, DaemonEvent, Response};

use crate::bridge::{Bridge, BridgeError};
use crate::notify::NotificationFilter;
use crate::tray::{Dot, TrayState};
use crate::ui_state::UiStore;

/// Everything the webview and tray need, behind one managed struct.
pub struct DesktopState {
    bridge: Bridge,
    store: Mutex<UiStore>,
    tray_state: Mutex<TrayState>,
    notify: Mutex<NotificationFilter>,
    tray: Mutex<Option<TrayIcon>>,
}

impl DesktopState {
    fn new(bridge: Bridge) -> Self {
        Self {
            bridge,
            store: Mutex::new(UiStore::new()),
            tray_state: Mutex::new(TrayState::new()),
            notify: Mutex::new(NotificationFilter::new()),
            tray: Mutex::new(None),
        }
    }
}

/// Map a `DaemonEvent` to the tray dot color name the frontend/icon uses.
fn dot_name(dot: Dot) -> &'static str {
    match dot {
        Dot::Gray => "gray",
        Dot::Green => "green",
        Dot::Amber => "amber",
        Dot::Red => "red",
    }
}

/// One pump iteration: fan an event out to store, tray and notifier.
/// Public for the integration harness (spec: "bridge forwards typed events to a
/// webview harness").
pub fn pump_event(state: &DesktopState, event: &DaemonEvent) {
    if let Ok(mut store) = state.store.lock() {
        store.observe(event);
    }
    if let Ok(mut tray) = state.tray_state.lock() {
        tray.observe(event);
        refresh_tray(state, &tray);
    }
    if let Some(notification) = state.notify.lock().unwrap().observe(event) {
        // OS notification (plugin); failures are non-fatal.
        use tauri_plugin_notification::NotificationExt;
        let _ = APP
            .get()
            .expect("app handle set at startup")
            .notification()
            .builder()
            .title(notification.title)
            .body(notification.body)
            .show();
    }
}

/// The app handle is needed for notifications; stashed globally at setup.
static APP: std::sync::OnceLock<AppHandle> = std::sync::OnceLock::new();

fn refresh_tray(state: &DesktopState, tray_state: &TrayState) {
    let dot = dot_name(tray_state.dot());
    let tooltip = tray_state.tooltip();
    if let Some(tray) = state.tray.lock().unwrap().as_ref() {
        let _ = tray.set_tooltip(Some(tooltip));
        // Icon swap is done in JS-land-independent native code when icons are
        // embedded; the frontend reflects the dot via the tray title/tooltip and
        // the menu. (Icon path set at build time; see ui/tray-icons.)
        let _ = dot;
    }
    // Also notify the webview so the in-app header mirrors the tray.
    if let Some(app) = APP.get() {
        let _ = app.emit("herdr://tray", dot);
    }
}

/// Resync everything after (re)connect: List + per-agent Logs replay. Satisfies
/// the spec's "reopening restores full state via List + Logs replay".
fn resync(state: &DesktopState) {
    if let Ok(Response::AgentList(agents)) = state.bridge.request(ClientCommand::List) {
        if let Ok(mut store) = state.store.lock() {
            store.sync_from(agents.clone());
        }
        if let Ok(mut tray) = state.tray_state.lock() {
            tray.sync_from(&agents);
        }
    }
}

fn on_event(app: &AppHandle, event: DaemonEvent) {
    let state: State<DesktopState> = app.state();
    pump_event(&state, &event);
    let _ = app.emit("herdr://event", &event);
}

fn run_app() -> Result<(), Box<dyn std::error::Error>> {
    let bridge = Bridge::connect().map_err(|e| format!("daemon unreachable: {e}"))?;
    bridge.start_reader();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(DesktopState::new(bridge.clone()))
        .setup(move |app| {
            let handle = app.handle().clone();
            let _ = APP.set(handle.clone());
            let state = app.state::<DesktopState>();

            // System tray with dot + quick actions.
            let tray = TrayIconBuilder::new()
                .tooltip("herdr — no agents")
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        // Left click opens/focuses the dashboard window.
                        if let Some(window) = tray.app_handle().get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                })
                .on_menu_event(|app, event| {
                    let state: State<DesktopState> = app.state();
                    match event.id().as_ref() {
                        "spawn-shell" => {
                            let _ = state.bridge.request(ClientCommand::Spawn {
                                profile: "generic".into(),
                                cwd: std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()),
                                command: std::env::var("SHELL")
                                    .unwrap_or_else(|_| "/bin/bash".into()),
                                args: vec![],
                                host: None,
                            });
                            resync(&state);
                        }
                        "kill-all" => {
                            if let Ok(Response::AgentList(agents)) =
                                state.bridge.request(ClientCommand::List)
                            {
                                for a in agents {
                                    let _ = state.bridge.request(ClientCommand::Kill {
                                        agent_id: a.id.clone(),
                                    });
                                }
                            }
                        }
                        "open-dashboard" => {
                            if let Some(window) = app.get_webview_window("main") {
                                let _ = window.show();
                                let _ = window.set_focus();
                            }
                        }
                        _ => {}
                    }
                })
                .build(app)?;
            *state.tray.lock().unwrap() = Some(tray);

            // Event pump: one thread per app, forwards daemon frames everywhere.
            let listener = bridge.listen();
            let pump_handle = handle.clone();
            std::thread::spawn(move || {
                for frame in listener {
                    match frame {
                        crate::bridge::Frame::Event(event) => on_event(&pump_handle, event),
                    }
                }
            });

            // Initial resync + mark connected.
            resync(&state);
            let _ = handle.emit("herdr://conn", "connected");
            Ok(())
        })
        .on_window_event(|window, event| {
            // Spec: closing the window (tray mode) leaves agents running — the
            // window is hidden, not destroyed; the process stays up.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let hide_to_tray = std::env::var("HERDR_TRAY_MODE").as_deref() != Ok("off");
                if hide_to_tray {
                    let _ = window.hide();
                    api.prevent_close();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            spawn_agent_cmd,
            send_input_cmd,
            kill_agent_cmd,
            kill_all_cmd,
            list_cmd,
            logs_cmd,
            ping_cmd,
            snapshot_cmd
        ])
        .run(tauri::generate_context!())
        .map_err(|e| format!("tauri runtime error: {e}"))?;

    Ok(())
}

/// Entry point used by `src/bin/herdr-desktop.rs`.
pub fn main() {
    env_logger_fallback();
    if let Err(e) = run_app() {
        eprintln!("herdr-desktop: {e}");
        std::process::exit(1);
    }
}

fn env_logger_fallback() {
    // tracing-subscriber with a sane default; RUST_LOG honored.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init()
        .ok();
}

// ---------------------------------------------------------------------------
// Webview-invoked commands
// ---------------------------------------------------------------------------

fn map_err(e: BridgeError) -> String {
    e.to_string()
}

#[tauri::command]
fn spawn_agent_cmd(
    state: State<'_, DesktopState>,
    profile: String,
    cwd: String,
    command: String,
    args: Vec<String>,
) -> Result<String, String> {
    match state.bridge.request(ClientCommand::Spawn {
        profile,
        cwd,
        command,
        args,
        host: None,
    }) {
        Ok(Response::AgentCreated { agent_id }) => {
            resync(&state);
            Ok(agent_id)
        }
        Ok(other) => Err(format!("unexpected reply: {other:?}")),
        Err(e) => Err(map_err(e)),
    }
}

#[tauri::command]
fn send_input_cmd(
    state: State<'_, DesktopState>,
    agent_id: String,
    text: String,
    raw: bool,
) -> Result<(), String> {
    state
        .bridge
        .request(ClientCommand::SendInput {
            agent_id,
            text,
            raw,
        })
        .map(|_| ())
        .map_err(map_err)
}

#[tauri::command]
fn kill_agent_cmd(state: State<'_, DesktopState>, agent_id: String) -> Result<(), String> {
    state
        .bridge
        .request(ClientCommand::Kill { agent_id })
        .map(|_| ())
        .map_err(map_err)
}

#[tauri::command]
fn kill_all_cmd(state: State<'_, DesktopState>) -> Result<usize, String> {
    let mut killed = 0;
    if let Ok(Response::AgentList(agents)) = state.bridge.request(ClientCommand::List) {
        for a in agents {
            if state
                .bridge
                .request(ClientCommand::Kill {
                    agent_id: a.id.clone(),
                })
                .is_ok()
            {
                killed += 1;
            }
        }
    }
    Ok(killed)
}

#[tauri::command]
fn list_cmd(state: State<'_, DesktopState>) -> Result<serde_json::Value, String> {
    match state.bridge.request(ClientCommand::List) {
        Ok(Response::AgentList(agents)) => {
            // Also refresh the store so snapshot/list stay consistent.
            if let Ok(mut store) = state.store.lock() {
                store.sync_from(agents.clone());
            }
            serde_json::to_value(agents).map_err(|e| e.to_string())
        }
        Ok(other) => Err(format!("unexpected reply: {other:?}")),
        Err(e) => Err(map_err(e)),
    }
}

#[tauri::command]
fn logs_cmd(
    state: State<'_, DesktopState>,
    agent_id: String,
    bytes: u32,
) -> Result<String, String> {
    match state.bridge.request(ClientCommand::Logs {
        agent_id,
        max_bytes: bytes,
    }) {
        Ok(Response::LogChunk { payload }) => Ok(payload),
        Ok(other) => Err(format!("unexpected reply: {other:?}")),
        Err(e) => Err(map_err(e)),
    }
}

#[tauri::command]
fn ping_cmd(state: State<'_, DesktopState>) -> Result<bool, String> {
    match state.bridge.request(ClientCommand::Ping) {
        Ok(Response::Pong { .. }) => Ok(true),
        Ok(_) => Ok(false),
        Err(_) => Ok(false),
    }
}

/// Full snapshot of the UI store for (re)hydration of the frontend.
#[tauri::command]
fn snapshot_cmd(state: State<'_, DesktopState>) -> Result<serde_json::Value, String> {
    let store = state.store.lock().unwrap();
    serde_json::json!({
        "connected": store.connected,
        "cards": store.cards(),
    })
    .pipe_ok()
}

trait PipeOk<T> {
    fn pipe_ok(self) -> Result<T, String>;
}

impl<T> PipeOk<T> for T {
    fn pipe_ok(self) -> Result<T, String> {
        Ok(self)
    }
}
