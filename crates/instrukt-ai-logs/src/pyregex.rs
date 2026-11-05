//! Python `re` patterns translated for `fancy-regex`.
//!
//! Python's inline flags and comments, which fancy-regex reads differently,
//! are translated:
//!
//! - Inline flags, tracked per scope (`(?flags)` for the rest of the pattern,
//!   `(?flags-flags:...)` for a group, the enclosing flags restored at its
//!   `)`):
//!   - `a` (ASCII): dropped, and inside its scope `\w \W \d \D \s \S \b \B`
//!     (and the class forms) are written as explicit ASCII classes, because
//!     fancy-regex refuses inline changes of its Unicode mode.
//!   - `u` (Unicode): dropped; it clears `a` in its scope.
//!   - `x` (verbose): dropped, and the translator itself removes unescaped
//!     whitespace and `#` comments outside character classes in its scope, as
//!     Python does. Comments are never parsed for flags or parentheses.
//!   - `i`, `m`, `s` pass through; `L` passes through and fails to compile,
//!     as it does in Python for a `str` pattern.
//! - `(?#...)` comments are dropped; an escaped whitespace character becomes
//!   a `\x{..}` escape, which fancy-regex accepts outside verbose mode too.
//!
//! Known differences: with `a` and `i` together Python folds case ASCII-only,
//! here case folding stays Unicode (`(?ai)k` also matches the Kelvin sign).
//! CPython also applies a pattern-start `(?a)` to a `(?u:...)` group that
//! opens the pattern (`(?a)(?u:\w)` misses `é`); here `u` always wins in its
//! scope.

const WORD: &str = "0-9A-Za-z_";
const DIGIT: &str = "0-9";
const SPACE: &str = r"\t\n\x0B\x0C\r ";

/// The flags that change how the translator reads or rewrites a scope.
#[derive(Clone, Copy, Default)]
struct Mode {
    ascii: bool,
    verbose: bool,
}

/// Python's whitespace for verbose mode.
fn is_verbose_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0B' | '\x0C')
}

/// Rewrite `pattern` so fancy-regex reads Python's inline flags as Python does.
pub(crate) fn translate(pattern: &str) -> String {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::with_capacity(pattern.len() + 16);
    let mut scopes: Vec<Mode> = Vec::new();
    let mut mode = Mode::default();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if mode.verbose && is_verbose_space(c) {
            i += 1;
            continue;
        }
        if mode.verbose && c == '#' {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        match c {
            '\\' => {
                match chars.get(i + 1).copied() {
                    Some(n) => out.push_str(&escape_outside_class(n, mode)),
                    None => out.push('\\'),
                }
                i += 2;
            }
            '[' => i = copy_class(&chars, i, mode, &mut out),
            '(' => {
                if chars.get(i + 1) == Some(&'?') && chars.get(i + 2) == Some(&'#') {
                    // A `(?#...)` comment ends at the first `)`.
                    while i < chars.len() && chars[i] != ')' {
                        i += 1;
                    }
                    i += 1;
                } else if let Some((flags, end)) = flag_group(&chars, i) {
                    if flags.scoped {
                        scopes.push(mode);
                    }
                    mode = flags.apply(mode);
                    out.push_str(&flags.render());
                    i = end + 1;
                } else {
                    scopes.push(mode);
                    out.push('(');
                    i += 1;
                }
            }
            ')' => {
                mode = scopes.pop().unwrap_or(mode);
                out.push(')');
                i += 1;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// `\<c>` outside a character class.
fn escape_outside_class(c: char, mode: Mode) -> String {
    if is_verbose_space(c) {
        return format!("\\x{{{:X}}}", u32::from(c));
    }
    if mode.ascii {
        let ahead = format!("(?=[{WORD}])");
        let behind = format!("(?<=[{WORD}])");
        let not_ahead = format!("(?![{WORD}])");
        let not_behind = format!("(?<![{WORD}])");
        let ascii = match c {
            'w' => Some(format!("[{WORD}]")),
            'W' => Some(format!("[^{WORD}]")),
            'd' => Some(format!("[{DIGIT}]")),
            'D' => Some(format!("[^{DIGIT}]")),
            's' => Some(format!("[{SPACE}]")),
            'S' => Some(format!("[^{SPACE}]")),
            'b' => Some(format!("(?:{behind}{not_ahead}|{not_behind}{ahead})")),
            'B' => Some(format!("(?:{behind}{ahead}|{not_behind}{not_ahead})")),
            _ => None,
        };
        if let Some(rewritten) = ascii {
            return rewritten;
        }
    }
    format!("\\{c}")
}

/// `\<c>` inside a character class.
fn escape_inside_class(c: char, mode: Mode) -> String {
    if is_verbose_space(c) {
        return format!("\\x{{{:X}}}", u32::from(c));
    }
    if mode.ascii {
        let ascii = match c {
            'w' => Some(WORD.to_string()),
            'd' => Some(DIGIT.to_string()),
            's' => Some(SPACE.to_string()),
            'W' => Some(format!("[^{WORD}]")),
            'D' => Some(format!("[^{DIGIT}]")),
            'S' => Some(format!("[^{SPACE}]")),
            _ => None,
        };
        if let Some(rewritten) = ascii {
            return rewritten;
        }
    }
    format!("\\{c}")
}

/// Copy the character class starting at `chars[start] == '['`; return the
/// index after it. A `]` right after `[` or `[^` is a literal, as in Python.
/// Whitespace and `#` inside a class are literal even in verbose mode.
fn copy_class(chars: &[char], start: usize, mode: Mode, out: &mut String) -> usize {
    out.push('[');
    let mut i = start + 1;
    if chars.get(i) == Some(&'^') {
        out.push('^');
        i += 1;
    }
    if chars.get(i) == Some(&']') {
        out.push(']');
        i += 1;
    }
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' {
            match chars.get(i + 1).copied() {
                Some(n) => out.push_str(&escape_inside_class(n, mode)),
                None => out.push('\\'),
            }
            i += 2;
            continue;
        }
        out.push(c);
        i += 1;
        if c == ']' {
            return i;
        }
    }
    i
}

/// A Python inline-flag group.
struct Flags {
    positive: String,
    negative: String,
    scoped: bool,
}

impl Flags {
    fn apply(&self, mut mode: Mode) -> Mode {
        if self.positive.contains('a') {
            mode.ascii = true;
        }
        if self.positive.contains('u') {
            mode.ascii = false;
        }
        if self.positive.contains('x') {
            mode.verbose = true;
        }
        if self.negative.contains('x') {
            mode.verbose = false;
        }
        mode
    }

    /// The group as fancy-regex reads it: `a`, `u` and `x` are handled by the
    /// translator and dropped; an empty global group disappears.
    fn render(&self) -> String {
        let keep = |set: &str| -> String { set.chars().filter(|c| !"aux".contains(*c)).collect() };
        let mut body = keep(&self.positive);
        let negative = keep(&self.negative);
        if !negative.is_empty() {
            body.push('-');
            body.push_str(&negative);
        }
        match (self.scoped, body.is_empty()) {
            (true, true) => "(?:".to_string(),
            (true, false) => format!("(?{body}:"),
            (false, true) => String::new(),
            (false, false) => format!("(?{body})"),
        }
    }
}

/// A Python inline-flag group at `chars[start] == '('`: its flags and the
/// index of its closing `)` or `:`. Anything else (`(?:`, `(?P<..>`,
/// lookaround, plain groups) is `None`.
fn flag_group(chars: &[char], start: usize) -> Option<(Flags, usize)> {
    if chars.get(start + 1) != Some(&'?') {
        return None;
    }
    let mut i = start + 2;
    let mut positive = String::new();
    while let Some(&c) = chars.get(i).filter(|c| "aiLmsux".contains(**c)) {
        positive.push(c);
        i += 1;
    }
    let mut negative = String::new();
    let has_minus = chars.get(i) == Some(&'-');
    if has_minus {
        i += 1;
        while let Some(&c) = chars.get(i).filter(|c| "imsx".contains(**c)) {
            negative.push(c);
            i += 1;
        }
    }
    let scoped = match chars.get(i)? {
        ':' => true,
        ')' => false,
        _ => return None,
    };
    if positive.is_empty() && negative.is_empty() {
        return None;
    }
    // Python rejects a global group that turns flags off; leave it to
    // fancy-regex untranslated rather than guess.
    if !scoped && has_minus {
        return None;
    }
    Some((
        Flags {
            positive,
            negative,
            scoped,
        },
        i,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fancy_regex::Regex;

    /// Search `line` the way the reader does: without and with its `\n`.
    fn search(pattern: &str, line: &str) -> bool {
        let translated = translate(pattern);
        let re = Regex::new(&translated)
            .unwrap_or_else(|e| panic!("{pattern:?} -> {translated:?}: {e}"));
        re.is_match(line).unwrap() || re.is_match(&format!("{line}\n")).unwrap()
    }

    // Expected values: Python 3.12 `bool(re.search(pattern, line + "\n"))`.

    #[test]
    fn plain_patterns_pass_through() {
        for p in [
            r"(?i)abc",
            r"(?=x)\w+",
            r"[\w\]]",
            r"(?P<n>a)(?:b)",
            r"\(?a\)",
            r"a\$",
        ] {
            assert_eq!(translate(p), p);
        }
    }

    #[test]
    fn ascii_flag_matches_python() {
        assert!(!search(r"(?a)\w+", "é"));
        assert!(search(r"(?a)\w+", "abc"));
        assert!(!search(r"(?a:\w)+x", "éx"));
        assert!(search(r"(?a:\w)+x(\w)", "axé"));
        assert!(search(r"(?ai)k", "K"));
        assert!(search(r"(?ia)\bx", "éx"));
        assert!(!search(r"(?a)\Bx", "éx"));
        assert!(search(r"(?a)[\w-]+$", "a-b"));
        assert!(!search(r"(?a)^[\w]+$", "é"));
        assert!(search(r"(?a)^[\W]+$", "é"));
        assert!(search(r"(?a)\d", "7"));
        assert!(!search(r"(?a)\d", "٣"));
        assert!(search(r"(?a)\S\s\S", "a b"));
    }

    #[test]
    fn unicode_flag_clears_ascii_in_its_scope() {
        assert!(search(r"(?a:msg=(?u:\w+))$", "msg=café"));
        assert!(!search(r"(?a:msg=(?u:\w+)\w)$", "msg=cafeé"));
        assert!(search(r"(?a:x(?u:\w))", "xé"));
        assert!(search(r"(?a)x(?u:\w)", "xé"));
        assert!(!search(r"(?a:(?u:x)\w)", "xé"));
        assert!(search(r"(?a:(?u:(?a:x)\w))", "xé"));
        assert_eq!(translate(r"(?u)\w"), r"\w");
        assert_eq!(translate(r"(?iu)\w"), r"(?i)\w");
        assert_eq!(translate(r"(?u:x)"), r"(?:x)");
    }

    #[test]
    fn verbose_mode_matches_python() {
        assert!(search("(?x)# (?a)\nmsg=\\w+$", "msg=café"));
        assert!(!search("(?x)(?a: # a ) in a comment\n\\w+$)", "café"));
        assert!(search("(?x)(?a: # a ) in a comment\n\\w+$)", "cafe"));
        assert!(search("(?x)a b # c\n c", "abc"));
        assert!(search("(?x)[ ]a", " a"));
        assert!(search(r"(?x)a\ b", "a b"));
        assert!(search(r"(?x)a\#b", "a#b"));
        assert!(search("(?x: a b )c d", "abc d"));
        assert!(!search("(?x: a b )c d", "abcd"));
        assert!(search("(?x)a(?-x: b)", "a b"));
        assert!(search(r"a\ b", "a b"));
    }

    #[test]
    fn comment_groups_are_dropped() {
        assert!(search(r"(?#c(c)x", "x"));
        assert_eq!(translate(r"a(?#note)b"), "ab");
    }

    #[test]
    fn locale_flag_still_fails() {
        assert!(Regex::new(&translate(r"(?aL)x")).is_err());
        assert!(Regex::new(&translate(r"(?L)x")).is_err());
    }
}
