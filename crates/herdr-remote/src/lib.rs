//! herdr-remote — Phase 4 SSH bridge.
//!
//! Remote agents are first-class fleet members: the daemon spawns them on a remote
//! host over SSH, streams their output/state back through the same event bus, and
//! controls them (send/kill/logs) as if local. The remote side runs `herdr-agent`,
//! a headless herdr daemon speaking the same NDJSON protocol; this crate
//! multiplexes the client/daemon conversation over one SSH connection.
//!
//! Transport note (documented deviation, see spec §Design): the tunnel uses the
//! user's `ssh` binary (`ProcessSshTransport`) rather than embedding russh. All
//! channels — control + events — are multiplexed over a single SSH exec pipe with
//! an internal frame layer, so behavior matches the russh design; auth (agent,
//! keys, known_hosts, ProxyJump) comes from the user's existing SSH config for
//! free. A russh backend can replace `SshTransport` without touching anything else.

use std::time::Duration;

use herdr_protocol::RemoteHost;

pub mod bridge;
pub mod frame;
pub mod runner_proto;

pub use bridge::{Bridge, EventSink, ProcessSshTransport, SshTransport};
pub use runner_proto::{retag_event, split_tagged_id, tag_id};

/// Reconnect backoff policy: doubling from 500 ms to 8 s with jitter-free
/// determinism (unit-testable), reset on success.
#[derive(Debug, Clone)]
pub struct Backoff {
    base_ms: u64,
    max_ms: u64,
    current: u64,
    pub attempts: u32,
}

impl Backoff {
    pub fn new() -> Self {
        Self {
            base_ms: 500,
            max_ms: 8000,
            current: 500,
            attempts: 0,
        }
    }

    /// Next wait duration, advancing the schedule. (Not `next` — clippy
    /// rightly flags the confusion with `Iterator::next`.)
    pub fn next_delay(&mut self) -> Duration {
        let d = Duration::from_millis(self.current);
        self.attempts += 1;
        self.current = (self.current * 2).min(self.max_ms);
        d
    }

    /// Reset after a successful exchange.
    pub fn reset(&mut self) {
        self.current = self.base_ms;
        self.attempts = 0;
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new()
    }
}

/// Registry of configured remote hosts (daemon-side state).
#[derive(Debug, Default)]
pub struct HostRegistry {
    hosts: std::collections::HashMap<String, RemoteHost>,
}

impl HostRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace. Returns the previous entry, if any.
    pub fn upsert(&mut self, host: RemoteHost) -> Option<RemoteHost> {
        self.hosts.insert(host.name.clone(), host)
    }

    pub fn remove(&mut self, name: &str) -> Option<RemoteHost> {
        self.hosts.remove(name)
    }

    pub fn get(&self, name: &str) -> Option<&RemoteHost> {
        self.hosts.get(name)
    }

    pub fn list(&self) -> Vec<RemoteHost> {
        let mut hosts: Vec<_> = self.hosts.values().cloned().collect();
        hosts.sort_by(|a, b| a.name.cmp(&b.name));
        hosts
    }
}

/// Ensure `herdr-agent` (this workspace's runner binary) is available on the
/// remote host, installing the current binary if needed. Returns the remote
/// command to execute for the tunnel.
///
/// Strategy: try `herdr-agent --version` over ssh first. If that fails (not
/// installed, stale, not executable), scp the local `herdr-agent` binary to
/// `~/.herdr/bin/herdr-agent` on the remote host. The transport then runs that
/// absolute path so the runner doesn't need to be on the remote PATH.
pub fn ensure_runner(transport_args: &[String], host: &RemoteHost) -> anyhow::Result<String> {
    let remote_path = "~/.herdr/bin/herdr-agent";
    let probe = |target: &str| -> bool {
        std::process::Command::new("ssh")
            .args(transport_args)
            .arg(target)
            .arg("--")
            .arg(remote_path)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    if probe(&host.ssh_target) {
        return Ok(remote_path.to_string());
    }

    // Install: scp the just-built runner. The caller resolves the local binary
    // path (current_exe) so what we install is exactly what we run.
    let local = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("herdr-agent")))
        .ok_or_else(|| anyhow::anyhow!("cannot locate local herdr-agent binary"))?;
    if !local.exists() {
        anyhow::bail!(
            "herdr-agent binary not found at {} — build it with `cargo build -p herdr-agent`",
            local.display()
        );
    }
    let status = std::process::Command::new("ssh")
        .args(transport_args)
        .arg(&host.ssh_target)
        .arg("--")
        .arg("mkdir")
        .arg("-p")
        .arg("~/.herdr/bin")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| anyhow::anyhow!("ssh mkdir failed: {e}"))?;
    if !status.success() {
        anyhow::bail!("ssh mkdir failed for {}", host.ssh_target);
    }
    let status = std::process::Command::new("scp")
        .args(transport_args)
        .arg(&local)
        .arg(format!("{}:{remote_path}", host.ssh_target))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| anyhow::anyhow!("scp failed: {e}"))?;
    if !status.success() {
        anyhow::bail!("scp {} failed for {}", local.display(), host.ssh_target);
    }
    Ok(remote_path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(name: &str) -> RemoteHost {
        RemoteHost {
            name: name.into(),
            ssh_target: format!("{name}.example.com"),
            port: 22,
            user: None,
        }
    }

    #[test]
    fn backoff_doubles_to_max_then_stays() {
        let mut b = Backoff::new();
        assert_eq!(b.next_delay(), Duration::from_millis(500));
        assert_eq!(b.next_delay(), Duration::from_millis(1000));
        assert_eq!(b.next_delay(), Duration::from_millis(2000));
        assert_eq!(b.next_delay(), Duration::from_millis(4000));
        assert_eq!(b.next_delay(), Duration::from_millis(8000));
        assert_eq!(b.next_delay(), Duration::from_millis(8000), "capped");
        assert_eq!(b.attempts, 6);
        b.reset();
        assert_eq!(b.attempts, 0);
        assert_eq!(b.next_delay(), Duration::from_millis(500), "reset to base");
    }

    #[test]
    fn registry_upsert_get_remove_sorts() {
        let mut r = HostRegistry::new();
        r.upsert(host("zeta"));
        r.upsert(host("alpha"));
        let prev = r.upsert(host("alpha")); // replace
        assert!(prev.is_some());
        assert_eq!(r.list().len(), 2);
        let names: Vec<_> = r.list().iter().map(|h| h.name.clone()).collect();
        assert_eq!(names, ["alpha", "zeta"], "sorted");
        assert!(r.get("alpha").is_some());
        assert!(r.remove("alpha").is_some());
        assert!(r.remove("alpha").is_none());
        assert!(r.get("alpha").is_none());
    }
}
