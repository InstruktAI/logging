//! The crate's error type.

use std::fmt;
use std::io;
use std::path::PathBuf;

/// Everything that can go wrong while resolving paths or initializing logging.
///
/// Logging is an essential subsystem: every one of these is meant to stop the
/// host process from starting blind.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The app name normalizes to nothing (empty, or only separators).
    InvalidName {
        /// The name as passed by the caller.
        name: String,
    },
    /// `XDG_STATE_HOME` is unset or empty and `HOME` is unset or empty, so the
    /// default `~/.local/state` cannot be resolved (or `XDG_STATE_HOME` starts
    /// with `~` and there is no `HOME` to expand it with).
    HomeNotSet,
    /// The log directory could not be created.
    CreateDir {
        /// The directory that could not be created.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },
    /// The log file could not be opened for appending.
    OpenFile {
        /// The file that could not be opened.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },
    /// A global `tracing` subscriber is already installed (by an earlier
    /// [`init`](crate::init) or by someone else).
    AlreadyInitialized,
    /// The `log` crate bridge could not be installed, usually because another
    /// `log` logger is already set.
    LogBridge(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName { name } => {
                write!(f, "app name {name:?} does not produce a valid name")
            }
            Self::HomeNotSet => f.write_str(
                "cannot resolve the log location: XDG_STATE_HOME is unset and HOME is unset",
            ),
            Self::CreateDir { path, source } => {
                write!(
                    f,
                    "cannot create log directory {}: {source}",
                    path.display()
                )
            }
            Self::OpenFile { path, source } => {
                write!(f, "cannot open log file {}: {source}", path.display())
            }
            Self::AlreadyInitialized => f.write_str("a global tracing subscriber is already set"),
            Self::LogBridge(msg) => write!(f, "cannot install the log crate bridge: {msg}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CreateDir { source, .. } | Self::OpenFile { source, .. } => Some(source),
            _ => None,
        }
    }
}
