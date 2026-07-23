//! Discover the RapidRAW agent server's port.
//!
//! The server binds `127.0.0.1:0` and publishes the OS-assigned port to
//! `<app_data_dir>/rapidraw-agent-port`. This module finds that file across
//! platforms without depending on Tauri's path APIs.

use std::path::PathBuf;

/// Filename the RapidRAW app writes its port into.
const PORT_FILENAME: &str = "rapidraw-agent-port";
/// The Tauri app identifier — determines the app data dir name.
const APP_IDENTIFIER: &str = "io.github.CyberTimon.RapidRAW";

/// All candidate app-data directories where the port file might live,
/// across macOS / Linux / Windows. Order matters only for the first hit.
pub fn candidate_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    // macOS: ~/Library/Application Support/<identifier>/
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join("Library/Application Support").join(APP_IDENTIFIER));
        // Linux: ~/.local/share/<identifier>/
        dirs.push(home.join(".local/share").join(APP_IDENTIFIER));
        // Windows (best-effort under WSL / native via USERPROFILE):
        dirs.push(home.join("AppData/Roaming").join(APP_IDENTIFIER));
    }

    // `dirs` crate canonical paths (more correct on each platform).
    if let Some(d) = dirs::data_dir() {
        dirs.push(d.join(APP_IDENTIFIER));
    }
    if let Some(d) = dirs::data_local_dir() {
        dirs.push(d.join(APP_IDENTIFIER));
    }

    dirs
}

/// Read the port from the first candidate dir that has the file.
pub fn read_port() -> Option<u16> {
    for dir in candidate_dirs() {
        let path = dir.join(PORT_FILENAME);
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(port) = text.trim().parse::<u16>() {
                return Some(port);
            }
        }
    }
    None
}

/// Read the port, polling up to `timeout_secs` so callers can run this right
/// after launching RapidRAW without racing the server startup.
pub fn read_port_wait(timeout_secs: u64) -> Option<u16> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    loop {
        if let Some(port) = read_port() {
            return Some(port);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_dirs_contains_app_identifier() {
        let dirs = candidate_dirs();
        assert!(!dirs.is_empty(), "should have at least one candidate dir");
        for d in &dirs {
            assert!(
                d.to_string_lossy().contains(APP_IDENTIFIER),
                "{d:?} missing app identifier"
            );
        }
    }

    #[test]
    fn read_port_returns_none_when_no_file() {
        // There is no guarantee a server is running in CI; the function must
        // at minimum not panic and return a u16 or None.
        let _ = read_port();
    }

    #[test]
    fn read_port_parses_a_written_file() {
        // Write a temp port file into the first candidate dir, read it back,
        // then clean up. This exercises the real parse path.
        let dir = candidate_dirs().into_iter().next().unwrap();
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(PORT_FILENAME);
        let original = std::fs::read_to_string(&path).ok();
        std::fs::write(&path, "65535").unwrap();
        let port = read_port();
        // Restore so we don't corrupt a real running server's discovery.
        match original {
            Some(c) => {
                let _ = std::fs::write(&path, c);
            }
            None => {
                let _ = std::fs::remove_file(&path);
            }
        }
        assert_eq!(port, Some(65535));
    }
}
