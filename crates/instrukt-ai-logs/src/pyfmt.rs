//! Renderings of Python's own text forms that 0.6.x's messages carry.
//!
//! The Python tool prints `repr()` of strings and lists, and `str()` of an
//! `OSError`, inside user-facing messages. These helpers reproduce those forms
//! so the messages stay byte-identical.

use std::io;
use std::path::Path;

/// Python's `str.isspace()`: Unicode White_Space plus the four ASCII
/// information separators (`\x1c`..`\x1f`) Python also counts as space.
pub(crate) fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// Python's `str.strip()` with no arguments.
pub(crate) fn py_strip(s: &str) -> &str {
    s.trim_matches(is_py_space)
}

/// Python's `repr()` of a `str`.
pub(crate) fn repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if c == ' ' || !(c.is_control() || c.is_whitespace()) => out.push(c),
            c => {
                let cp = u32::from(c);
                if cp < 0x100 {
                    out.push_str(&format!("\\x{cp:02x}"));
                } else if cp < 0x10000 {
                    out.push_str(&format!("\\u{cp:04x}"));
                } else {
                    out.push_str(&format!("\\U{cp:08x}"));
                }
            }
        }
    }
    out.push(quote);
    out
}

/// Python's `repr()` of a `list[str]`.
pub(crate) fn repr_str_list<S: AsRef<str>>(items: &[S]) -> String {
    let inner: Vec<String> = items.iter().map(|s| repr_str(s.as_ref())).collect();
    format!("[{}]", inner.join(", "))
}

/// The C library's message for an OS error, as Python's `strerror` holds it.
fn strerror(err: &io::Error, code: i32) -> String {
    let rendered = err.to_string();
    let suffix = format!(" (os error {code})");
    match rendered.strip_suffix(&suffix) {
        Some(msg) => msg.to_string(),
        None => rendered,
    }
}

/// Python's `str()` of the `OSError` an operation on `filename` raises:
/// `[Errno <n>] <strerror>: '<filename>'`.
pub(crate) fn os_error(err: &io::Error, filename: Option<&str>) -> String {
    match err.raw_os_error() {
        Some(code) => {
            let msg = strerror(err, code);
            match filename {
                Some(name) => format!("[Errno {code}] {msg}: {}", repr_str(name)),
                None => format!("[Errno {code}] {msg}"),
            }
        }
        None => err.to_string(),
    }
}

/// [`os_error`] for a filesystem path.
pub(crate) fn os_error_path(err: &io::Error, path: &Path) -> String {
    os_error(err, Some(&path.to_string_lossy()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repr_matches_python() {
        assert_eq!(repr_str("cron"), "'cron'");
        assert_eq!(repr_str("it's"), "\"it's\"");
        assert_eq!(repr_str("a'b\"c"), "'a\\'b\"c'");
        assert_eq!(repr_str("tab\there\n"), "'tab\\there\\n'");
        assert_eq!(repr_str("back\\slash"), "'back\\\\slash'");
        assert_eq!(repr_str("\x07"), "'\\x07'");
        assert_eq!(repr_str("caf\u{e9}"), "'caf\u{e9}'");
        assert_eq!(
            repr_str_list(&["cron", "docs-watch"]),
            "['cron', 'docs-watch']"
        );
        assert_eq!(repr_str_list::<&str>(&[]), "[]");
    }

    #[test]
    fn strip_matches_python() {
        assert_eq!(py_strip("\x1c a \n"), "a");
        assert_eq!(py_strip(" \u{a0}x"), "x");
    }

    #[test]
    fn os_error_matches_python() {
        let err = io::Error::from_raw_os_error(2);
        assert_eq!(
            os_error(&err, Some("launchctl")),
            "[Errno 2] No such file or directory: 'launchctl'"
        );
        assert_eq!(os_error(&err, None), "[Errno 2] No such file or directory");
    }
}
