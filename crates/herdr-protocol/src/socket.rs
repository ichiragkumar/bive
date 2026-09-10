//! Default socket path resolution, shared by daemon and every client.
//!
//! `${XDG_RUNTIME_DIR:-/tmp}/herdr-$UID.sock` — per-user, predictable, XDG-aware.

use std::path::PathBuf;

/// Resolve the default daemon socket path for the current user.
pub fn default_socket_path() -> PathBuf {
    let uid = current_uid();
    let dir = std::env::var("HERDR_SOCKDIR")
        .or_else(|_| std::env::var("XDG_RUNTIME_DIR"))
        .unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(dir).join(format!("herdr-{uid}.sock"))
}

fn current_uid() -> u32 {
    #[cfg(unix)]
    return unsafe { libc::getuid() };
    #[cfg(not(unix))]
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_ends_with_herdr_suffix() {
        let p = default_socket_path();
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with("herdr-"), "unexpected socket name: {name}");
        assert!(name.ends_with(".sock"), "unexpected socket name: {name}");
        // uid should be the real one, not env-derived
        #[cfg(unix)]
        assert_eq!(name, format!("herdr-{}.sock", unsafe { libc::getuid() }));
    }
}
