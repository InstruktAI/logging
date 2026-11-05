//! Python `re` patterns translated for `fancy-regex`.
//!
//! 0.6.x searched each line *with* its trailing `\n` using Python `re`. The
//! reader matches the same subject (the line with exactly one trailing `\n`)
//! and translates the parts of Python's syntax whose meaning differs:
//!
//! - Anchors. Python's `$` (outside multiline mode) matches at the end or
//!   before a final `\n`; it becomes `(?=\n?\z)`. Under `m` both engines match
//!   before every `\n` and at the end, so `$` stays. Python's `\Z` is the
//!   absolute end, fancy-regex's `\z` (fancy-regex's own `\Z` ignores
//!   trailing newlines). `^`, `\A` and `\b` mean the same in both.
//! - Inline flags and comments, tracked per scope (`(?flags)` for the rest of the pattern,
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
/// Python's `$` outside multiline mode, against a subject ending in `\n`.
const END_OR_BEFORE_FINAL_NEWLINE: &str = r"(?=\n?\z)";

/// The flags that change how the translator reads or rewrites a scope.
#[derive(Clone, Copy, Default)]
struct Mode {
    ascii: bool,
    verbose: bool,
    multiline: bool,
}

/// Python's whitespace for verbose mode.
fn is_verbose_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0B' | '\x0C')
}

/// Rewrite `pattern` so fancy-regex matches what Python `re.search` matched
/// on the same line with its trailing `\n`.
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
            i = skip_comment(&chars, i + 1, '\n');
            continue;
        }
        match c {
            '\\' if chars.get(i + 1).is_some_and(char::is_ascii_digit) => {
                i = numeric_escape(&chars, i, &mut out);
            }
            '\\' => {
                match chars.get(i + 1).copied() {
                    Some(n) => out.push_str(&escape_outside_class(n, mode)),
                    None => out.push('\\'),
                }
                i += 2;
            }
            '[' => i = copy_class(&chars, i, mode, &mut out),
            '$' if !mode.multiline => {
                out.push_str(END_OR_BEFORE_FINAL_NEWLINE);
                i += 1;
            }
            '(' => {
                if chars.get(i + 1) == Some(&'?') && chars.get(i + 2) == Some(&'#') {
                    i = skip_comment(&chars, i + 3, ')');
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

/// The index after a comment that starts at `start` and ends with `end`
/// (consumed). Like Python's tokenizer, an escape is one token, so `\)` does
/// not end a `(?#...)` comment and `\` before a newline does not end a
/// verbose `#` comment.
fn skip_comment(chars: &[char], start: usize, end: char) -> usize {
    let mut i = start;
    while i < chars.len() {
        match chars[i] {
            '\\' => i += 2,
            c if c == end => return i + 1,
            _ => i += 1,
        }
    }
    i
}

/// A Python numeric escape outside a class at `chars[start] == '\\'`; returns
/// the index after it. Python reads `\0` plus up to two octal digits, or
/// three octal digits, as an octal escape, and otherwise one or two digits as
/// a group reference. The digits are read from the raw pattern, never across
/// verbose whitespace or a comment, so the output must not let a following
/// digit join them: a group reference is written `\k<N>` and an octal escape
/// `\x{..}`. An octal value above 0o377 is left as is, so it fails to
/// compile, as it fails in Python.
fn numeric_escape(chars: &[char], start: usize, out: &mut String) -> usize {
    let is_oct = |i: usize| chars.get(i).is_some_and(|c| ('0'..='7').contains(c));
    let digit = |i: usize| chars.get(i).copied().filter(char::is_ascii_digit);
    let d1 = start + 1;
    let octal_end = if chars[d1] == '0' {
        let mut end = d1 + 1;
        while end < d1 + 3 && is_oct(end) {
            end += 1;
        }
        Some(end)
    } else if digit(d1 + 1).is_some() && is_oct(d1) && is_oct(d1 + 1) && is_oct(d1 + 2) {
        Some(d1 + 3)
    } else {
        None
    };
    if let Some(end) = octal_end {
        let text: String = chars[d1..end].iter().collect();
        match u32::from_str_radix(&text, 8) {
            Ok(value) if value <= 0o377 => out.push_str(&format!("\\x{{{value:X}}}")),
            _ => {
                out.push('\\');
                out.push_str(&text);
            }
        }
        return end;
    }
    let end = if digit(d1 + 1).is_some() {
        d1 + 2
    } else {
        d1 + 1
    };
    let group: String = chars[d1..end].iter().collect();
    out.push_str(&format!("\\k<{group}>"));
    end
}

/// `\<c>` outside a character class.
fn escape_outside_class(c: char, mode: Mode) -> String {
    if is_verbose_space(c) {
        return format!("\\x{{{:X}}}", u32::from(c));
    }
    if c == 'Z' {
        return r"\z".to_string();
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
        if self.positive.contains('m') {
            mode.multiline = true;
        }
        if self.negative.contains('m') {
            mode.multiline = false;
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

    /// Search `line` the way the reader does: with exactly one trailing `\n`.
    fn search(pattern: &str, line: &str) -> bool {
        let translated = translate(pattern);
        let subject = format!("{}\n", line.strip_suffix('\n').unwrap_or(line));
        Regex::new(&translated)
            .unwrap_or_else(|e| panic!("{pattern:?} -> {translated:?}: {e}"))
            .is_match(&subject)
            .unwrap()
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
    fn anchors_follow_python() {
        assert!(search(r"tail=x$", "a tail=x"));
        assert!(search(r"\n$", "a tail=x"));
        assert!(!search(r"tail=x\Z", "a tail=x"));
        assert!(search(r"\n\Z", "a tail=x"));
        assert!(!search(r"tail=x(?!\n)", "a tail=x"));
        assert!(!search(r"x$y", "x"));
        assert!(search(r"[$]", "a$b"));
        assert!(search(r"^a", "a"));
        assert!(search(r"\Aa", "a"));
        assert!(search(r"x\b", "x"));
        assert!(search(r"(?m)x$", "x"));
        assert!(search(r"(?m)^$", "x"));
        assert!(!search(r"x$\n", "y"));
        assert!(search(r"x$\n", "x"));
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
    fn numeric_escapes_keep_their_boundaries() {
        // Python reads the digits of a group reference from the raw pattern,
        // so removed whitespace or a comment never extends it.
        assert!(search("(?x)(foo) \\1 2$", "foofoo2"));
        assert!(search(r"(foo)\1(?#note)2$", "foofoo2"));
        assert!(search(
            r"(a)(b)(c)(d)(e)(f)(g)(h)(i)(j)(k)(l)\12",
            "abcdefghijkll"
        ));
        assert!(search(r"(foo)\1", "foofoo"));
        assert!(!search(r"(foo)\1", "foo"));
        assert!(search(r"\101", "A"));
        assert!(search(r"\0", "\0"));
        assert!(search(r"\07", "\x07"));
        assert!(search(r"\0101", "\x081"));
        assert_eq!(translate(r"(a)\1"), r"(a)\k<1>");
        assert!(Regex::new(&translate(r"(a)\12")).is_err());
        assert!(Regex::new(&translate(r"\400")).is_err());
    }

    #[test]
    fn comment_groups_are_dropped() {
        assert!(search(r"(?#c(c)x", "x"));
        assert!(search(r"foo(?#a\)b)$", "foo"));
        assert!(!search(r"foo(?#a\)b)$", "foob)"));
        assert!(search("(?x)a # c \\\n b", "ab"));
        assert_eq!(translate(r"a(?#note)b"), "ab");
    }

    #[test]
    fn locale_flag_still_fails() {
        assert!(Regex::new(&translate(r"(?aL)x")).is_err());
        assert!(Regex::new(&translate(r"(?L)x")).is_err());
    }
}
