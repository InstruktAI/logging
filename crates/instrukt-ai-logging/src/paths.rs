//! Log location: `$XDG_STATE_HOME/instrukt-ai/<app>/<source or app>.log`.

use std::ffi::OsString;
use std::path::PathBuf;

use crate::Error;
use crate::env::EnvLookup;
use crate::names;

fn non_empty(value: Option<OsString>) -> Option<OsString> {
    value.filter(|v| !v.is_empty())
}

fn home(env: &EnvLookup) -> Result<PathBuf, Error> {
    non_empty(env.get("HOME"))
        .map(PathBuf::from)
        .ok_or(Error::HomeNotSet)
}

/// Expand a leading `~` or `~/` against `HOME`, like Python's `expanduser`.
fn expand_tilde(raw: OsString, env: &EnvLookup) -> Result<PathBuf, Error> {
    match raw.to_str() {
        Some("~") => home(env),
        Some(s) if s.starts_with("~/") => Ok(home(env)?.join(&s[2..])),
        _ => Ok(PathBuf::from(raw)),
    }
}

fn state_home(env: &EnvLookup) -> Result<PathBuf, Error> {
    match non_empty(env.get("XDG_STATE_HOME")) {
        Some(raw) => expand_tilde(raw, env),
        None => Ok(home(env)?.join(".local").join("state")),
    }
}

pub(crate) fn log_dir_with(app: &str, env: &EnvLookup) -> Result<PathBuf, Error> {
    let app_name = names::app_name(app)?;
    Ok(state_home(env)?.join("instrukt-ai").join(app_name))
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
    Ok(dir.join(format!("{stem}.log")))
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
        let e = env(&[("XDG_STATE_HOME", "~"), ("HOME", "/h")]);
        assert_eq!(
            log_dir_with("app", &e).ok(),
            Some(PathBuf::from("/h/instrukt-ai/app"))
        );
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
    fn missing_home_is_an_error() {
        assert!(matches!(
            log_dir_with("app", &env(&[])),
            Err(Error::HomeNotSet)
        ));
        assert!(matches!(
            log_dir_with("app", &env(&[("XDG_STATE_HOME", "~/x")])),
            Err(Error::HomeNotSet)
        ));
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
