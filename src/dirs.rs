//! XDG base directories.

use std::path::PathBuf;

fn xdg(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(fallback))
}

pub fn config() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config")
}

/// History and the like (`~/.local/state/hero`).
pub fn state() -> PathBuf {
    xdg("XDG_STATE_HOME", ".local/state").join("hero")
}

/// Pictures apps sent as pixels (`~/.cache/hero/notifications`).
pub fn images() -> PathBuf {
    xdg("XDG_CACHE_HOME", ".cache").join("hero").join("notifications")
}
