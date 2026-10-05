//! Where mint keeps its configuration: `~/.config/mint` on macOS and Linux
//! (honouring `XDG_CONFIG_HOME`), `%APPDATA%\mint` on Windows.

use std::path::PathBuf;

pub fn config_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        return std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("mint"));
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(xdg).join("mint"));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config").join("mint"))
}

pub fn presets_file() -> Option<PathBuf> {
    config_dir().map(|d| d.join("presets.toml"))
}

pub fn settings_file() -> Option<PathBuf> {
    config_dir().map(|d| d.join("config.toml"))
}
