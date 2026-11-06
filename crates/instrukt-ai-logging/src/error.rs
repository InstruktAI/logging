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
    /// The `source` is not a bare file stem: it must match
    /// `^[A-Za-z0-9][A-Za-z0-9._-]*$` and must not contain `..`, so it can
    /// never name a file outside the app's log directory.
    InvalidSource {
        /// The source as passed by the caller.
        name: String,
    },
    /// A home directory is needed (`XDG_STATE_HOME` is unset or empty, or
    /// starts with `~`) but `HOME` is unset and the password database has no
    /// entry for the current user.
    HomeNotSet,
    /// The log directory could not be created, or not set to mode `0700`.
    CreateDir {
        /// The directory that could not be created.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },
    /// The log file could not be opened for appending, or not set to mode
    /// `0600`. A symbolic link at the log path is refused.
    OpenFile {
        /// The file that could not be opened.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },
    /// The log file was opened but a line could not be written to it during
    /// initialization.
    WriteFile {
        /// The file that could not be written.
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
            Self::InvalidSource { name } => write!(
                f,
                "log source {name:?} is not a bare file stem (A-Z, a-z, 0-9, then also '.', '_', '-'; no \"..\")"
            ),
            Self::HomeNotSet => f.write_str(
                "cannot resolve the log location: no home directory (HOME is unset and the user has no password database entry)",
            ),
            Self::CreateDir { path, source } => {
                write!(
                    f,
                    "cannot create log directory {}: {source}",
                    path.display()
                )
            }
            Self::OpenFile { path, source } if source.raw_os_error() == Some(libc::ELOOP) => {
                write!(
                    f,
                    "cannot open log file {}: it is a symbolic link, which is refused",
                    path.display()
                )
            }
            Self::OpenFile { path, source } => {
                write!(f, "cannot open log file {}: {source}", path.display())
            }
            Self::WriteFile { path, source } => {
                write!(f, "cannot write to log file {}: {source}", path.display())
            }
            Self::AlreadyInitialized => f.write_str("a global tracing subscriber is already set"),
            Self::LogBridge(msg) => write!(f, "cannot install the log crate bridge: {msg}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CreateDir { source, .. }
            | Self::OpenFile { source, .. }
            | Self::WriteFile { source, .. } => Some(source),
            _ => None,
        }
    }
}
