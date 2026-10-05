//! `config.toml`: preferences shared by the CLI and the window.
//!
//! ```toml
//! hotkey = "Ctrl+Alt+Cmd+P"   # global shortcut that toggles the window
//! clear_after = 45            # seconds before a copied password is cleared; 0 = never
//! hide_on_blur = true         # hide the window when it loses focus
//! default_preset = "moneris"  # rule the window and bare `mint` start from
//! ```

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::paths;

pub const DEFAULT_CLEAR_AFTER: u64 = 45;

/// ⌃⌥⌘P on macOS: Spotlight (⌘Space), 1Password Quick Access (⇧⌘Space),
/// Raycast (⌥Space) and the system shortcuts leave it free. On Windows
/// Ctrl+Shift+Alt+P avoids AltGr (Ctrl+Alt) layouts.
pub fn default_hotkey() -> &'static str {
    if cfg!(target_os = "macos") { "Ctrl+Alt+Cmd+P" } else { "Ctrl+Shift+Alt+P" }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub hotkey: String,
    pub clear_after: u64,
    pub hide_on_blur: bool,
    pub default_preset: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            hotkey: default_hotkey().to_string(),
            clear_after: DEFAULT_CLEAR_AFTER,
            hide_on_blur: true,
            default_preset: None,
        }
    }
}

impl Settings {
    /// Reads `config.toml`; a missing file means defaults.
    pub fn load() -> Result<Settings> {
        let Some(path) = paths::settings_file() else { return Ok(Settings::default()) };
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).map_err(|e| {
                Error::Usage(format!(
                    "{}: {}; fix the file and try again.",
                    path.display(),
                    e.message().trim().trim_end_matches('.')
                ))
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
            Err(e) => Err(Error::Usage(format!("Cannot read {}: {e}; check its permissions.", path.display()))),
        }
    }
}
