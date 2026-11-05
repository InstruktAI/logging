//! Log location: `$XDG_STATE_HOME/instrukt-ai/<app>/<source or app>.log`.
//!
//! Paths are resolved and rendered the way the Python writer's `pathlib`
//! does, so both writers (and the `instrukt-ai-logs` tool, which prints these
//! paths and writes them into rotation configs) agree byte for byte:
//! repeated and trailing slashes collapse, `.` segments drop, `~` and `~/`
//! expand against `HOME`, an unset `HOME` falls back to the password
//! database and an empty `HOME` means `/`.

use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;

use crate::Error;
use crate::env::EnvLookup;
use crate::names;

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

/// `Path.home()`: `HOME` when set (trailing slashes dropped, empty → `/`),
/// otherwise the password database.
fn home(env: &EnvLookup) -> Result<Vec<u8>, Error> {
    let raw: OsString = match env.get("HOME") {
        Some(home) => home,
        #[allow(deprecated)]
        None => std::env::home_dir()
            .map(PathBuf::into_os_string)
            .ok_or(Error::HomeNotSet)?,
    };
    let bytes = raw.into_vec();
    let end = bytes.iter().rposition(|b| *b != b'/').map_or(0, |i| i + 1);
    let trimmed = &bytes[..end];
    Ok(if trimmed.is_empty() {
        b"/".to_vec()
    } else {
        normalize(trimmed)
    })
}

/// `Path(raw).expanduser()` for `~` and `~/...`. A `~user` prefix is left as
/// is.
fn expanduser(raw: &[u8], env: &EnvLookup) -> Result<Vec<u8>, Error> {
    let norm = normalize(raw);
    if norm == b"~" {
        return home(env);
    }
    match norm.strip_prefix(b"~/") {
        Some(rest) => Ok(join(&home(env)?, rest)),
        None => Ok(norm),
    }
}

fn to_path(bytes: Vec<u8>) -> PathBuf {
    PathBuf::from(OsString::from_vec(bytes))
}

fn log_root_bytes(env: &EnvLookup) -> Result<Vec<u8>, Error> {
    let state_home = match env.get("XDG_STATE_HOME") {
        Some(value) if !value.is_empty() => expanduser(value.as_bytes(), env)?,
        _ => join(&home(env)?, b".local/state"),
    };
    Ok(join(&state_home, b"instrukt-ai"))
}

pub(crate) fn log_root_with(env: &EnvLookup) -> Result<PathBuf, Error> {
    log_root_bytes(env).map(to_path)
}

pub(crate) fn log_dir_with(app: &str, env: &EnvLookup) -> Result<PathBuf, Error> {
    let app_name = names::app_name(app)?;
    Ok(to_path(join(&log_root_bytes(env)?, app_name.as_bytes())))
}

pub(crate) fn log_file_with(
    app: &str,
    source: Option<&str>,
    env: &EnvLookup,
) -> Result<PathBuf, Error> {
    let dir = log_dir_with(app, env)?;
    let stem = match source {
        Some(s) if !s.is_empty() => s.to_owned(),
        _ => names::app_name(app)?,
    };
    Ok(to_path(join(
        dir.as_os_str().as_bytes(),
        format!("{stem}.log").as_bytes(),
    )))
}

/// The root holding every app's log directory: `$XDG_STATE_HOME/instrukt-ai`
/// (with `~/.local/state` when `XDG_STATE_HOME` is unset or empty).
///
/// Reads the process environment; creates nothing.
pub fn resolve_log_root() -> Result<PathBuf, Error> {
    log_root_with(&EnvLookup::process())
}

/// The directory holding every log file of `app`:
/// `$XDG_STATE_HOME/instrukt-ai/<app>` (with `~/.local/state` when
/// `XDG_STATE_HOME` is unset or empty).
///
/// `app` is normalized for the filesystem (lowercase, runs of other characters
/// become `-`), so `crypto_ai` and `crypto-ai` resolve to the same directory.
/// Reads the process environment; creates nothing.
pub fn resolve_log_dir(app: &str) -> Result<PathBuf, Error> {
    log_dir_with(app, &EnvLookup::process())
}

/// The log file `app` writes to: `<log dir>/<source>.log`, or
/// `<log dir>/<app>.log` when `source` is `None` or empty.
///
/// `source` is the bare stem (for example `cron`); `.log` is always appended.
/// Reads the process environment; creates nothing.
pub fn resolve_log_file(app: &str, source: Option<&str>) -> Result<PathBuf, Error> {
    log_file_with(app, source, &EnvLookup::process())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> EnvLookup {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        EnvLookup::new(move |key| {
            owned
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| OsString::from(v))
        })
    }

    #[test]
    fn xdg_state_home_wins() {
        let e = env(&[("XDG_STATE_HOME", "/s"), ("HOME", "/h")]);
        assert_eq!(
            log_file_with("Tele_Claude", None, &e).ok(),
            Some(PathBuf::from("/s/instrukt-ai/tele-claude/tele-claude.log"))
        );
        assert_eq!(
            log_root_with(&e).ok(),
            Some(PathBuf::from("/s/instrukt-ai"))
        );
    }

    #[test]
    fn empty_or_unset_xdg_falls_back_to_home() {
        for e in [
            env(&[("XDG_STATE_HOME", ""), ("HOME", "/h")]),
            env(&[("HOME", "/h")]),
        ] {
            assert_eq!(
                log_dir_with("app", &e).ok(),
                Some(PathBuf::from("/h/.local/state/instrukt-ai/app"))
            );
        }
    }

    #[test]
    fn tilde_is_expanded() {
        let e = env(&[("XDG_STATE_HOME", "~/st"), ("HOME", "/h")]);
        assert_eq!(
            log_dir_with("app", &e).ok(),
            Some(PathBuf::from("/h/st/instrukt-ai/app"))
        );
        let e = env(&[("XDG_STATE_HOME", "~"), ("HOME", "/h/")]);
        assert_eq!(
            log_dir_with("app", &e).ok(),
            Some(PathBuf::from("/h/instrukt-ai/app"))
        );
    }

    #[test]
    fn paths_render_like_pathlib() {
        let e = env(&[("XDG_STATE_HOME", "/tmp//x/./y/"), ("HOME", "/h")]);
        assert_eq!(
            log_file_with("app", Some("cron"), &e).ok(),
            Some(PathBuf::from("/tmp/x/y/instrukt-ai/app/cron.log"))
        );
        let e = env(&[("HOME", "")]);
        assert_eq!(
            log_root_with(&e).ok(),
            Some(PathBuf::from("/.local/state/instrukt-ai"))
        );
        assert_eq!(normalize(b"//net/x"), b"//net/x");
        assert_eq!(normalize(b"///x"), b"/x");
        assert_eq!(normalize(b"rel/./"), b"rel");
        assert_eq!(normalize(b"."), b".");
        assert_eq!(normalize(b"a/../b"), b"a/../b");
        assert_eq!(join(b".", b"instrukt-ai"), b"instrukt-ai");
        assert_eq!(join(b"/", b"instrukt-ai"), b"/instrukt-ai");
    }

    #[test]
    fn source_is_the_stem_and_log_is_appended() {
        let e = env(&[("XDG_STATE_HOME", "/s")]);
        assert_eq!(
            log_file_with("app", Some("cron"), &e).ok(),
            Some(PathBuf::from("/s/instrukt-ai/app/cron.log"))
        );
        assert_eq!(
            log_file_with("app", Some(""), &e).ok(),
            Some(PathBuf::from("/s/instrukt-ai/app/app.log"))
        );
    }

    #[test]
    fn unset_home_falls_back_to_the_password_database() {
        #[allow(deprecated)]
        let Some(pw_home) = std::env::home_dir() else {
            return;
        };
        let expected = to_path(join(
            &normalize(pw_home.as_os_str().as_bytes()),
            b".local/state/instrukt-ai/app",
        ));
        assert_eq!(log_dir_with("app", &env(&[])).ok(), Some(expected));
    }

    #[test]
    fn invalid_app_is_an_error() {
        let e = env(&[("XDG_STATE_HOME", "/s")]);
        assert!(matches!(
            log_dir_with("--", &e),
            Err(Error::InvalidName { .. })
        ));
    }
}
