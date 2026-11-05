//! Log-path resolution, mirroring today's Python `_resolve_log_root` and
//! `_normalize_app_name` (and install.py's `_state_home_root`).
//!
//! Deliberately tiny and isolated: at integration this is replaced by
//! `instrukt_ai_logging::resolve_log_dir`, so path rules live in one place.
//! Paths are rendered the way `pathlib.PurePosixPath` renders them (repeated
//! and trailing slashes collapsed, `.` segments dropped), because they appear
//! verbatim in printed messages and in generated rotation configs.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;

use crate::pyfmt::py_strip;

/// The filesystem app name: lower-case, runs of non-`[a-z0-9]` → `-`, trimmed.
pub(crate) fn normalize_app_name(name: &str) -> Result<String, String> {
    let raw = py_strip(name).to_lowercase();
    if raw.is_empty() {
        return Err("name must be non-empty".to_string());
    }
    let mut out = String::with_capacity(raw.len());
    let mut in_run = false;
    for c in raw.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
            in_run = false;
        } else if !in_run {
            out.push('-');
            in_run = true;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        return Err("name did not produce a valid app name".to_string());
    }
    Ok(trimmed.to_string())
}

/// `PurePosixPath(raw)` rendered back to bytes.
fn normalize(raw: &[u8]) -> Vec<u8> {
    let leading = raw.iter().take_while(|b| **b == b'/').count();
    let mut out: Vec<u8> = match leading {
        0 => Vec::new(),
        2 => b"//".to_vec(),
        _ => b"/".to_vec(),
    };
    let rooted = !out.is_empty();
    let mut first = true;
    for part in raw.split(|b| *b == b'/') {
        if part.is_empty() || part == b"." {
            continue;
        }
        if !first {
            out.push(b'/');
        }
        out.extend_from_slice(part);
        first = false;
    }
    if out.is_empty() && !rooted {
        out.push(b'.');
    }
    out
}

/// `base / child` with pathlib's rendering.
fn join(base: &[u8], child: &[u8]) -> Vec<u8> {
    if base == b"." {
        return normalize(child);
    }
    let mut out = base.to_vec();
    if !out.ends_with(b"/") {
        out.push(b'/');
    }
    out.extend_from_slice(child);
    normalize(&out)
}

/// `Path.home()`: `$HOME` when set (trailing slashes dropped, empty → `/`),
/// otherwise the password database.
fn home_bytes() -> Vec<u8> {
    let raw: OsString = match std::env::var_os("HOME") {
        Some(home) => home,
        #[allow(deprecated)]
        None => std::env::home_dir()
            .map(PathBuf::into_os_string)
            .unwrap_or_default(),
    };
    let bytes = raw.into_vec();
    let end = bytes.iter().rposition(|b| *b != b'/').map_or(0, |i| i + 1);
    let trimmed = &bytes[..end];
    if trimmed.is_empty() {
        b"/".to_vec()
    } else {
        normalize(trimmed)
    }
}

/// `Path(raw).expanduser()` for `~` and `~/...`. A `~user` prefix is left as
/// is (Python would consult the password database).
fn expanduser(raw: &[u8]) -> Vec<u8> {
    let norm = normalize(raw);
    if norm == b"~" {
        return home_bytes();
    }
    match norm.strip_prefix(b"~/") {
        Some(rest) => join(&home_bytes(), rest),
        None => norm,
    }
}

fn to_path(bytes: Vec<u8>) -> PathBuf {
    PathBuf::from(OsString::from_vec(bytes))
}

/// `Path.home()`.
pub(crate) fn home_dir() -> PathBuf {
    to_path(home_bytes())
}

/// `$XDG_STATE_HOME/instrukt-ai` (fallback `~/.local/state/instrukt-ai`).
pub(crate) fn log_root() -> PathBuf {
    let state_home = match std::env::var_os("XDG_STATE_HOME") {
        Some(value) if !value.is_empty() => expanduser(value.as_bytes()),
        _ => join(&home_bytes(), b".local/state"),
    };
    to_path(join(&state_home, b"instrukt-ai"))
}

/// The app's log directory: `<log root>/<normalized app name>`.
pub(crate) fn resolve_log_dir(app: &str) -> Result<PathBuf, String> {
    let fs_app = normalize_app_name(app)?;
    let root = log_root();
    Ok(to_path(join(
        root.as_os_str().as_bytes(),
        fs_app.as_bytes(),
    )))
}

/// Join a single file name onto a directory with pathlib's rendering.
pub(crate) fn child(dir: &std::path::Path, name: &OsStr) -> PathBuf {
    to_path(join(dir.as_os_str().as_bytes(), name.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_name_normalization() {
        assert_eq!(
            normalize_app_name("TeleClaude").as_deref(),
            Ok("teleclaude")
        );
        assert_eq!(normalize_app_name("crypto_ai").as_deref(), Ok("crypto-ai"));
        assert_eq!(normalize_app_name("  My  App!! ").as_deref(), Ok("my-app"));
        assert_eq!(normalize_app_name("--a__b--").as_deref(), Ok("a-b"));
        assert_eq!(
            normalize_app_name("   "),
            Err("name must be non-empty".to_string())
        );
        assert_eq!(
            normalize_app_name("---"),
            Err("name did not produce a valid app name".to_string())
        );
    }

    #[test]
    fn pathlib_rendering() {
        assert_eq!(normalize(b"/tmp//x/./y/"), b"/tmp/x/y");
        assert_eq!(normalize(b"//net/x"), b"//net/x");
        assert_eq!(normalize(b"///x"), b"/x");
        assert_eq!(normalize(b"rel/./"), b"rel");
        assert_eq!(normalize(b"."), b".");
        assert_eq!(normalize(b"a/../b"), b"a/../b");
        assert_eq!(join(b".", b"instrukt-ai"), b"instrukt-ai");
        assert_eq!(join(b"/", b"instrukt-ai"), b"/instrukt-ai");
        assert_eq!(join(b"/s", b"instrukt-ai"), b"/s/instrukt-ai");
    }
}
