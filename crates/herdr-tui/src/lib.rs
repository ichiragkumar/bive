//! herdr-tui library surface.
//!
//! The binary is a thin terminal loop over this library. Exposing the modules
//! as a lib lets integration tests (and future embedders) drive the *real*
//! `HerdrClient` and `App` against a real daemon over a real socket.

pub mod ansi;
pub mod app;
pub mod client;
pub mod logs;
pub mod ui;

pub use app::App;
pub use client::HerdrClient;
