//! Name normalization: one `app` string yields three names.

use crate::Error;

/// Replace every run of characters not accepted by `keep` with `sep`, then
/// trim `sep` from both ends.
fn collapse(raw: &str, keep: impl Fn(char) -> bool, sep: char) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut in_run = false;
    for c in raw.chars() {
        if keep(c) {
            out.push(c);
            in_run = false;
        } else if !in_run {
            out.push(sep);
            in_run = true;
        }
    }
    out.trim_matches(sep).to_owned()
}

fn non_empty(out: String, name: &str) -> Result<String, Error> {
    if out.is_empty() {
        Err(Error::InvalidName {
            name: name.to_owned(),
        })
    } else {
        Ok(out)
    }
}

/// `teleclaude` → `TELECLAUDE`, `my-app` → `MY_APP`.
pub(crate) fn env_prefix(name: &str) -> Result<String, Error> {
    let upper = name.trim().to_uppercase();
    non_empty(
        collapse(
            &upper,
            |c| c.is_ascii_uppercase() || c.is_ascii_digit(),
            '_',
        ),
        name,
    )
}

/// `teleclaude` → `teleclaude`, `my-app` → `my_app`.
pub(crate) fn logger_prefix(name: &str) -> Result<String, Error> {
    let lower = name.trim().to_lowercase();
    non_empty(
        collapse(
            &lower,
            |c| c.is_ascii_lowercase() || c.is_ascii_digit(),
            '_',
        ),
        name,
    )
}

/// `teleclaude` → `teleclaude`, `My_App` → `my-app`.
pub(crate) fn app_name(name: &str) -> Result<String, Error> {
    let lower = name.trim().to_lowercase();
    non_empty(
        collapse(
            &lower,
            |c| c.is_ascii_lowercase() || c.is_ascii_digit(),
            '-',
        ),
        name,
    )
}

/// `source` is a bare file stem: `^[A-Za-z0-9][A-Za-z0-9._-]*$` without
/// `..`. It can neither leave the log directory nor be hidden.
pub(crate) fn check_source(source: &str) -> Result<(), Error> {
    let mut chars = source.chars();
    let ok = chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && !source.contains("..");
    if ok {
        Ok(())
    } else {
        Err(Error::InvalidSource {
            name: source.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_prefix_uppercases_and_collapses_runs() {
        assert_eq!(env_prefix("teleclaude").ok().as_deref(), Some("TELECLAUDE"));
        assert_eq!(
            env_prefix(" my--app.v2 ").ok().as_deref(),
            Some("MY_APP_V2")
        );
        assert_eq!(
            env_prefix("-_crypto ai_-").ok().as_deref(),
            Some("CRYPTO_AI")
        );
    }

    #[test]
    fn logger_prefix_lowercases_with_underscores() {
        assert_eq!(
            logger_prefix("Crypto-AI").ok().as_deref(),
            Some("crypto_ai")
        );
        assert_eq!(logger_prefix("a..b").ok().as_deref(), Some("a_b"));
    }

    #[test]
    fn app_name_lowercases_with_dashes() {
        assert_eq!(app_name("Crypto_AI").ok().as_deref(), Some("crypto-ai"));
        assert_eq!(app_name("__x__").ok().as_deref(), Some("x"));
    }

    #[test]
    fn sources_are_bare_stems() {
        for good in ["cron", "docs-watch", "a.b_c-1", "9", "worker.2"] {
            assert!(check_source(good).is_ok(), "{good:?}");
        }
        for bad in [
            "",
            "../../escape",
            "..",
            "a..b",
            ".hidden",
            "-x",
            "_x",
            "a/b",
            "/abs",
            "a b",
            "a\\b",
            "caf\u{e9}",
            "a\n",
        ] {
            assert!(
                matches!(check_source(bad), Err(Error::InvalidSource { .. })),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn empty_results_are_errors() {
        for bad in ["", "   ", "---", "__", "é"] {
            assert!(env_prefix(bad).is_err(), "{bad:?}");
            assert!(logger_prefix(bad).is_err(), "{bad:?}");
            assert!(app_name(bad).is_err(), "{bad:?}");
        }
    }
}
