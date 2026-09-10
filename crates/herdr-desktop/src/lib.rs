//! herdr-desktop — Phase 3 Tauri 2 desktop client.
//!
//! The Tauri Rust side owns the single daemon socket and exposes typed events and
//! commands to the webview (see `specs/12-phase-3-desktop.md`). The core here is
//! Tauri-free so it compiles and tests everywhere; the app shell is behind the
//! `tauri` feature (`cargo build -p herdr-desktop --features tauri`).

#[cfg(feature = "tauri")]
pub mod app;

pub mod bridge;
pub mod notify;
pub mod tray;
pub mod ui_state;

/// Marker for the (feature-gated) Tauri shell presence.
pub const PHASE: &str = "3 — desktop client (Tauri shell behind the `tauri` feature)";

#[cfg(test)]
mod tests {
    #[test]
    fn phase_marker() {
        assert!(crate::PHASE.starts_with("3"));
    }
}
