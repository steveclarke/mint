use std::fmt;

/// Every failure mint reports. Each kind maps to one process exit code, and
/// each message is one sentence that names the next step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Bad flags or arguments. Exit code 2.
    Usage(String),
    /// The rule cannot be satisfied (too short, too few classes). Exit code 3.
    Unsatisfiable(String),
    /// `op` is missing, not signed in, or failed. Exit code 4.
    OnePassword(String),
    /// The clipboard could not be written or cleared. Exit code 5.
    Clipboard(String),
}

impl Error {
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::Usage(_) => 2,
            Error::Unsatisfiable(_) => 3,
            Error::OnePassword(_) => 4,
            Error::Clipboard(_) => 5,
        }
    }

    /// Stable machine name for JSON output.
    pub fn kind(&self) -> &'static str {
        match self {
            Error::Usage(_) => "usage",
            Error::Unsatisfiable(_) => "unsatisfiable",
            Error::OnePassword(_) => "onepassword",
            Error::Clipboard(_) => "clipboard",
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Error::Usage(m) | Error::Unsatisfiable(m) | Error::OnePassword(m) | Error::Clipboard(m) => m,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
