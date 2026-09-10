//! `herdr install-service` / `herdr uninstall-service` — register the daemon as a
//! per-user launchd (macOS) or systemd (Linux) service.
//!
//! Design contract (see README / Phase 1 spec):
//! * The service runs `herdr daemon` from a **resolved absolute binary path** — a
//!   launch agent has no shell and no user PATH.
//! * macOS KeepAlive = `{SuccessfulExit: false}`: launchd respawns the daemon after
//!   a crash but leaves it down after `herdr shutdown` (clean exit 0). The daemon
//!   cooperates by exiting 0 when a live daemon already owns the socket, so login
//!   and manual starts settle instead of looping.
//! * On Linux, `Restart=on-failure` gives exactly "restart on crash", plus
//!   `RestartSec` backoff; the unit is a **user** unit (no root anywhere).
//! * Uninstall removes the file we would install (existence is not required) and
//!   stops/disables live units best-effort.
//!
//! Everything here is pure string/template logic except the final copy step, so the
//! generated definitions are fully unit-testable.

use std::path::{Path, PathBuf};

/// Which init system to target (resolved from the host OS, overridable for tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceKind {
    /// macOS launchd user agent (`~/Library/LaunchAgents`).
    Launchd,
    /// Linux systemd user unit (`~/.config/systemd/user`).
    Systemd,
    /// Anything else: registration is not supported (bare `herdr daemon` still works).
    Unsupported,
}

impl ServiceKind {
    /// Detect from the compile target OS (matches the platforms CI covers).
    pub fn detect() -> Self {
        if cfg!(target_os = "macos") {
            ServiceKind::Launchd
        } else if cfg!(target_os = "linux") {
            ServiceKind::Systemd
        } else {
            ServiceKind::Unsupported
        }
    }
}

/// Which way `install-service` / `uninstall-service` should act.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceAction {
    Install,
    Uninstall,
}

/// Everything the templates need.
pub struct ServiceSpec {
    /// Absolute path to the `herdr` binary (resolved via `std::env::current_exe`).
    pub binary: PathBuf,
    /// Absolute path of the daemon socket (so the unit matches `HERDR_SOCKDIR`).
    pub socket: PathBuf,
    /// Extra env for the daemon process (e.g. `RUST_LOG`).
    pub env: Vec<(String, String)>,
}

impl ServiceSpec {
    /// Build a spec for the running binary and default socket location.
    pub fn for_current_process() -> Result<Self, String> {
        let binary = std::env::current_exe()
            .map_err(|e| format!("cannot resolve the herdr binary path: {e}"))?
            .canonicalize()
            .map_err(|e| format!("cannot canonicalize the herdr binary path: {e}"))?;
        let socket = herdr_protocol::default_socket_path();
        Ok(Self {
            binary,
            socket,
            env: Vec::new(),
        })
    }
}

// ---------------------------------------------------------------------------
// launchd (macOS)
// ---------------------------------------------------------------------------

/// Label for the launch agent (`<label>.plist`).
pub const LAUNCHD_LABEL: &str = "io.github.herdr.daemon";

/// `~/Library/LaunchAgents/<label>.plist`
pub fn launchd_plist_path(home: &Path) -> PathBuf {
    home.join("Library")
        .join("LaunchAgents")
        .join(format!("{LAUNCHD_LABEL}.plist"))
}

/// Generate the launch agent plist. XML is templated directly (no plist crate —
/// the content is fixed-shape and fully deterministic).
pub fn launchd_plist(spec: &ServiceSpec) -> String {
    // One EnvironmentVariables dict total: HERDR_SOCKDIR plus any extras.
    let sockdir = spec.socket.parent().unwrap_or(Path::new("/tmp")).display();
    let mut env_pairs = vec![("HERDR_SOCKDIR".to_string(), sockdir.to_string())];
    env_pairs.extend(spec.env.iter().cloned());
    let mut env_block = String::from("  <key>EnvironmentVariables</key>\n  <dict>\n");
    for (k, v) in &env_pairs {
        env_block.push_str(&format!("    <key>{k}</key>\n    <string>{v}</string>\n"));
    }
    env_block.push_str("  </dict>\n");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{bin}</string>
    <string>daemon</string>
  </array>
{env_block}  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
  </dict>
  <key>ProcessType</key>
  <string>Background</string>
  <key>StandardOutPath</key>
  <string>{tmp}/herdr-daemon.log</string>
  <key>StandardErrorPath</key>
  <string>{tmp}/herdr-daemon.log</string>
</dict>
</plist>
"#,
        label = LAUNCHD_LABEL,
        bin = spec.binary.display(),
        env_block = env_block,
        tmp = std::env::temp_dir().display(),
    )
}

/// `launchctl` commands to activate the agent after writing the plist.
pub fn launchd_install_commands(plist: &Path) -> Vec<String> {
    vec![format!(
        "launchctl bootstrap gui/$(id -u) {}",
        plist.display()
    )]
}

/// `launchctl` commands to deactivate before removing the plist.
pub fn launchd_uninstall_commands() -> Vec<String> {
    vec![format!("launchctl bootout gui/$(id -u)/{LAUNCHD_LABEL}")]
}

// ---------------------------------------------------------------------------
// systemd (Linux)
// ---------------------------------------------------------------------------

/// `~/.config/systemd/user/herdr-daemon.service`
pub fn systemd_unit_path(home: &Path) -> PathBuf {
    home.join(".config")
        .join("systemd")
        .join("user")
        .join("herdr-daemon.service")
}

/// Generate the systemd user unit.
pub fn systemd_unit(spec: &ServiceSpec) -> String {
    let mut env_lines = String::new();
    for (k, v) in &spec.env {
        env_lines.push_str(&format!("Environment={k}={v}\n"));
    }
    let sockdir = spec.socket.parent().unwrap_or(Path::new("/tmp")).display();
    format!(
        r#"[Unit]
Description=herdr unified agent runtime daemon
Documentation=https://github.com/ichiragkumar/bive
After=network-online.target

[Service]
ExecStart={bin} daemon
Environment=HERDR_SOCKDIR={sockdir}
{env_lines}Restart=on-failure
RestartSec=2s
# Crash-only: `herdr shutdown` exits 0 and stays down; SIGTERM is the normal stop path.
RestartSteps=3
RestartMaxDelaySec=10s

[Install]
WantedBy=default.target
"#,
        bin = spec.binary.display(),
        sockdir = sockdir,
        env_lines = env_lines,
    )
}

/// `systemctl --user` commands to enable the unit (daemon-reload, enable, start).
pub fn systemd_install_commands() -> Vec<String> {
    vec![
        "systemctl --user daemon-reload".into(),
        "systemctl --user enable --now herdr-daemon.service".into(),
    ]
}

/// `systemctl --user` commands to stop/disable before removing the unit.
pub fn systemd_uninstall_commands() -> Vec<String> {
    vec![
        "systemctl --user disable --now herdr-daemon.service".into(),
        "systemctl --user daemon-reload".into(),
    ]
}

/// Paths + commands for a kind — the single dispatch used by `main.rs`.
pub struct ServicePlan {
    /// File to write (already inside the user's home tree).
    pub path: PathBuf,
    /// File content.
    pub content: String,
    /// Human-readable file path for echo output.
    pub display_path: String,
    /// Post-write activation commands (printed and executed).
    pub activate: Vec<String>,
    /// Commands to run before removing the file on uninstall.
    pub deactivate: Vec<String>,
}

/// Build the plan for `install-service` on this platform.
pub fn plan_install(
    kind: ServiceKind,
    spec: &ServiceSpec,
    home: &Path,
) -> Result<ServicePlan, String> {
    match kind {
        ServiceKind::Launchd => {
            let path = launchd_plist_path(home);
            Ok(ServicePlan {
                display_path: path.display().to_string(),
                path,
                content: launchd_plist(spec),
                activate: launchd_install_commands(&launchd_plist_path(home)),
                deactivate: launchd_uninstall_commands(),
            })
        }
        ServiceKind::Systemd => {
            let path = systemd_unit_path(home);
            Ok(ServicePlan {
                display_path: path.display().to_string(),
                path,
                content: systemd_unit(spec),
                activate: systemd_install_commands(),
                deactivate: systemd_uninstall_commands(),
            })
        }
        ServiceKind::Unsupported => Err(
            "automatic service registration is only supported on macOS (launchd) and Linux (systemd) — run `herdr daemon` under your own supervisor instead".into(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> ServiceSpec {
        ServiceSpec {
            binary: PathBuf::from("/Users/tester/.cargo/bin/herdr"),
            socket: PathBuf::from("/tmp/herdr-501.sock"),
            env: vec![("RUST_LOG".into(), "herdr_daemon=debug".into())],
        }
    }

    #[test]
    fn plist_contains_binary_daemon_and_keepalive() {
        let plist = launchd_plist(&spec());
        assert!(plist.contains("<string>/Users/tester/.cargo/bin/herdr</string>"));
        assert!(plist.contains("<string>daemon</string>"));
        // Restart on crash only: SuccessfulExit=false (not blanket KeepAlive,
        // which would respawn after `herdr shutdown`).
        assert!(plist.contains("<key>SuccessfulExit</key>\n    <false/>"));
        assert!(!plist.contains("<key>KeepAlive</key>\n  <true/>"));
        assert!(plist.contains("<key>RunAtLoad</key>\n  <true/>"));
        assert!(plist.contains("<key>HERDR_SOCKDIR</key>"));
        assert!(plist.contains("<string>/tmp</string>")); // HERDR_SOCKDIR parent
        assert!(plist.contains("RUST_LOG"));
        // Exactly one EnvironmentVariables dict despite extra env.
        assert_eq!(plist.matches("EnvironmentVariables").count(), 1);
    }

    #[test]
    fn plist_is_well_formed_xml() {
        // No plist/XML crate in deps; a structural check is enough for a template.
        let plist = launchd_plist(&spec());
        assert_eq!(
            plist.matches("<dict>").count(),
            plist.matches("</dict>").count()
        );
        assert!(plist.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
        assert!(plist.trim_end().ends_with("</plist>"));
    }

    #[test]
    fn systemd_unit_restarts_on_failure_only() {
        let unit = systemd_unit(&spec());
        assert!(unit.contains("ExecStart=/Users/tester/.cargo/bin/herdr daemon"));
        assert!(unit.contains("Restart=on-failure"));
        assert!(!unit.contains("Restart=always"));
        assert!(unit.contains("WantedBy=default.target"));
        assert!(unit.contains("Environment=HERDR_SOCKDIR=/tmp"));
        assert!(unit.contains("Environment=RUST_LOG=herdr_daemon=debug"));
        assert!(unit.starts_with("[Unit]"));
        assert!(unit.contains("[Service]"));
        assert!(unit.contains("[Install]"));
    }

    #[test]
    fn paths_live_under_home() {
        let home = Path::new("/Users/tester");
        assert_eq!(
            launchd_plist_path(home),
            PathBuf::from("/Users/tester/Library/LaunchAgents/io.github.herdr.daemon.plist")
        );
        assert_eq!(
            systemd_unit_path(home),
            PathBuf::from("/Users/tester/.config/systemd/user/herdr-daemon.service")
        );
    }

    #[test]
    fn launchd_commands_use_per_user_gui_domain() {
        let cmds = launchd_install_commands(&PathBuf::from("/x.plist"));
        assert_eq!(cmds.len(), 1);
        assert!(cmds[0].starts_with("launchctl bootstrap gui/$(id -u) /x.plist"));
        let out = launchd_uninstall_commands();
        assert!(out[0].starts_with("launchctl bootout gui/$(id -u)/"));
        assert!(out[0].contains(LAUNCHD_LABEL));
    }

    #[test]
    fn systemd_commands_enable_now_and_disable_on_uninstall() {
        assert!(systemd_install_commands()
            .iter()
            .any(|c| c.contains("enable --now")));
        assert!(systemd_uninstall_commands()
            .iter()
            .any(|c| c.contains("disable --now")));
    }

    #[test]
    fn plan_install_dispatches_per_kind() {
        let s = spec();
        let mac = plan_install(ServiceKind::Launchd, &s, Path::new("/Users/tester")).unwrap();
        assert!(mac
            .path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with(".plist"));
        assert!(mac.content.contains("SuccessfulExit"));
        assert_eq!(mac.activate.len(), 1);

        let linux = plan_install(ServiceKind::Systemd, &s, Path::new("/home/tester")).unwrap();
        assert!(linux
            .path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with(".service"));
        assert!(linux.content.contains("Restart=on-failure"));
        assert_eq!(linux.activate.len(), 2);

        assert!(plan_install(ServiceKind::Unsupported, &s, Path::new("/")).is_err());
    }

    #[test]
    fn spec_resolves_current_binary() {
        let s = ServiceSpec::for_current_process().unwrap();
        assert!(s.binary.is_absolute());
        assert!(s.socket.is_absolute());
        // Binary should be the test binary inside target/ — still absolute & herdr-ish.
        assert!(s.binary.to_string_lossy().contains("herdr"));
    }
}
