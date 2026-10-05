//! mint's engine: every way into mint (CLI, window, menu bar, Omarchy plugin)
//! generates through this crate.

pub mod clipboard;
pub mod error;
pub mod onepassword;
pub mod paths;
pub mod presets;
pub mod random;
pub mod rule;
pub mod settings;
pub mod wordlist;

pub use error::{Error, Result};
pub use rule::{CharClass, Kind, LengthSpec, Password, Rule};

#[cfg(test)]
mod tests;

#[cfg(target_os = "linux")]
pub mod clipboard_process;
