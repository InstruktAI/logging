//! Paths the tool needs. Log locations come from the writer crate
//! ([`instrukt_ai_logging::resolve_log_dir`], [`resolve_log_root`]), so the
//! path rules live in one place; this module only maps their errors to the
//! messages 0.6.x printed and resolves the home directory the rotation files
//! live under (`~/.config`, `~/Library/LaunchAgents`).

use std::path::{Component, PathBuf};

use instrukt_ai_logging::Error;

use crate::pyfmt::py_strip;

/// The message 0.6.x's `_normalize_app_name` raised for `app`, or the
/// writer's own message for anything else.
fn message(app: &str, err: &Error) -> String {
    match err {
        Error::InvalidName { .. } if py_strip(app).is_empty() => {
            "name must be non-empty".to_string()
        }
        Error::InvalidName { .. } => "name did not produce a valid app name".to_string(),
        other => other.to_string(),
    }
}

/// The app's log directory: `<log root>/<normalized app name>`.
pub(crate) fn resolve_log_dir(app: &str) -> Result<PathBuf, String> {
    instrukt_ai_logging::resolve_log_dir(app).map_err(|e| message(app, &e))
}

/// `$XDG_STATE_HOME/instrukt-ai` (fallback `~/.local/state/instrukt-ai`).
pub(crate) fn log_root() -> Result<PathBuf, String> {
    instrukt_ai_logging::resolve_log_root().map_err(|e| e.to_string())
}

/// `Path.home()`: `HOME` when set (empty means `/`), otherwise the password
/// database, with repeated and trailing slashes and `.` segments dropped.
pub(crate) fn home_dir() -> PathBuf {
    let raw = match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home),
        #[allow(deprecated)]
        None => std::env::home_dir().unwrap_or_default(),
    };
    let normalized: PathBuf = raw
        .components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect();
    if normalized.as_os_str().is_empty() {
        PathBuf::from("/")
    } else {
        normalized
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_app_names_keep_the_python_messages() {
        assert_eq!(
            resolve_log_dir("   ").err().as_deref(),
            Some("name must be non-empty")
        );
        assert_eq!(
            resolve_log_dir("---").err().as_deref(),
            Some("name did not produce a valid app name")
        );
    }
}
