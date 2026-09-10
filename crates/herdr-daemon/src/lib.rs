//! # herdr-daemon
//!
//! The core herdr engine. The binary entry point is in `herdr-cli` (`herdr daemon`);
//! this library exposes the modules so they are unit-testable and reusable by the
//! Phase 4 remote runner.

pub mod ansi;
pub mod bus;
pub mod ipc;
pub mod media;
pub mod pty;
pub mod registry;
pub mod remote;
pub mod state;
pub mod supervisor;

pub use bus::EventBus;
pub use registry::Registry;
pub use supervisor::Supervisor;

/// Run the daemon until a fatal error or shutdown signal.
pub async fn run(socket_path: std::path::PathBuf) -> anyhow::Result<()> {
    let supervisor = std::sync::Arc::new(Supervisor::new());

    // Periodic idle-state tick (500ms) — drives Working → Idle transitions.
    let tick_supervisor = supervisor.clone();
    let tick_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));
        loop {
            interval.tick().await;
            tick_supervisor.tick_idle();
        }
    });

    // Graceful shutdown on SIGINT/SIGTERM: kill agents, then fire the watch channel
    // so the IPC accept loop breaks.
    let signal_supervisor = supervisor.clone();
    let signal_task = tokio::spawn(async move {
        wait_for_shutdown_signal().await;
        tracing::info!("shutdown signal received; killing agents");
        signal_supervisor.shutdown_all().await;
        signal_supervisor.signal_shutdown();
    });

    let shutdown_rx = supervisor.subscribe_shutdown();
    let result = ipc::serve(socket_path.clone(), supervisor.clone(), shutdown_rx).await;

    tick_task.abort();
    signal_task.abort();

    // Clean up the socket file so the next daemon start is clean.
    let _ = tokio::fs::remove_file(&socket_path).await;
    result
}

#[cfg(unix)]
async fn wait_for_shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut sigint = signal(SignalKind::interrupt()).expect("install SIGINT handler");
    let mut sigterm = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    tokio::select! {
        _ = sigint.recv() => {},
        _ = sigterm.recv() => {},
    }
}

#[cfg(not(unix))]
async fn wait_for_shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
