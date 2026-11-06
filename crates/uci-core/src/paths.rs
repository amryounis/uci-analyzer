//! Where the shim and the viewer keep their files.
//!
//! `UCI_VISUALIZER_HOME` overrides the base folder (used by tests).

use std::path::PathBuf;

/// Base folder: `%LOCALAPPDATA%\uci-visualizer` on Windows,
/// `$XDG_DATA_HOME/uci-visualizer` or `~/.local/share/uci-visualizer` elsewhere.
pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("UCI_VISUALIZER_HOME") {
        return PathBuf::from(dir);
    }
    if cfg!(windows) {
        if let Some(dir) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(dir).join("uci-visualizer");
        }
    }
    if let Some(dir) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(dir).join("uci-visualizer");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("uci-visualizer");
    }
    std::env::temp_dir().join("uci-visualizer")
}

/// One `.jsonl` transcript per shim session.
pub fn transcripts_dir() -> PathBuf {
    data_dir().join("transcripts")
}

/// One `<pid>.json` file per running shim, so the viewer can find it.
pub fn sessions_dir() -> PathBuf {
    data_dir().join("sessions")
}
