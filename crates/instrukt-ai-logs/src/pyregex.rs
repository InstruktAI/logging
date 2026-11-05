//! Python `re` inline flags translated for `fancy-regex`.
//!
//! Python accepts the inline flags `a` (ASCII), `u` (Unicode, the default for
//! `str` patterns), `L`, `i`, `m`, `s` and `x`, as `(?flags)` or scoped
//! `(?flags-flags:...)`. fancy-regex knows `i`, `m`, `s` and `x`, and rejects
//! any inline change of its Unicode mode, so:
//!
//! - `u` is dropped: Unicode is already the mode. It clears `a` in its scope,
//!   so `(?a:x(?u:\w))` is Unicode again inside the `(?u:...)`.
//! - `a` is dropped and, inside its scope, the escapes it affects are written
//!   as explicit ASCII classes: `\w \W \d \D \s \S \b \B` (inside a character
//!   class `\w \d \s` and their negations).
//! - `L` is left in place, so compiling fails, as it does in Python for a
//!   `str` pattern.
//!
//! Known difference: with `a` and `i` together Python folds case ASCII-only;
//! here case folding stays Unicode (`(?ai)k` also matches the Kelvin sign).

const WORD: &str = "0-9A-Za-z_";
const DIGIT: &str = "0-9";
const SPACE: &str = r"\t\n\x0B\x0C\r ";

/// Rewrite `pattern` so fancy-regex reads Python's inline flags as Python does.
pub(crate) fn translate(pattern: &str) -> String {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::with_capacity(pattern.len());
    // ASCII mode of each open group, restored when it closes.
    let mut scopes: Vec<bool> = Vec::new();
    let mut ascii = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' => {
                let next = chars.get(i + 1).copied();
                match next.filter(|_| ascii).and_then(escape_outside_class) {
                    Some(rewritten) => out.push_str(&rewritten),
                    None => {
                        out.push(c);
                        if let Some(n) = next {
                            out.push(n);
                        }
                    }
                }
                i += 2;
            }
            '[' => i = copy_class(&chars, i, ascii, &mut out),
            '(' => {
                if let Some((flags, end)) = flag_group(&chars, i) {
                    let scoped = chars[end] == ':';
                    if scoped {
                        scopes.push(ascii);
                    }
                    // `a` and `u` are exclusive in Python: an explicit `u`
                    // clears ASCII until the scope ends.
                    if flags.positive.contains('a') {
                        ascii = true;
                    }
                    if flags.positive.contains('u') {
                        ascii = false;
                    }
                    out.push_str(&flags.render(scoped));
                    i = end + 1;
                } else {
                    scopes.push(ascii);
                    out.push(c);
                    i += 1;
                }
            }
            ')' => {
                ascii = scopes.pop().unwrap_or(ascii);
                out.push(c);
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

/// An ASCII-mode replacement for `\<c>` outside a character class.
fn escape_outside_class(c: char) -> Option<String> {
    let boundary_ahead = format!("(?=[{WORD}])");
    let boundary_behind = format!("(?<=[{WORD}])");
    let not_ahead = format!("(?![{WORD}])");
    let not_behind = format!("(?<![{WORD}])");
    Some(match c {
        'w' => format!("[{WORD}]"),
        'W' => format!("[^{WORD}]"),
        'd' => format!("[{DIGIT}]"),
        'D' => format!("[^{DIGIT}]"),
        's' => format!("[{SPACE}]"),
        'S' => format!("[^{SPACE}]"),
        'b' => format!("(?:{boundary_behind}{not_ahead}|{not_behind}{boundary_ahead})"),
        'B' => format!("(?:{boundary_behind}{boundary_ahead}|{not_behind}{not_ahead})"),
        _ => return None,
    })
}

/// An ASCII-mode replacement for `\<c>` inside a character class.
fn escape_inside_class(c: char) -> Option<String> {
    Some(match c {
        'w' => WORD.to_string(),
        'd' => DIGIT.to_string(),
        's' => SPACE.to_string(),
        'W' => format!("[^{WORD}]"),
        'D' => format!("[^{DIGIT}]"),
        'S' => format!("[^{SPACE}]"),
        _ => return None,
    })
}

/// Copy the character class starting at `chars[start] == '['`; return the
/// index after it. A `]` right after `[` or `[^` is a literal, as in Python.
fn copy_class(chars: &[char], start: usize, ascii: bool, out: &mut String) -> usize {
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
            let next = chars.get(i + 1).copied();
            match next.filter(|_| ascii).and_then(escape_inside_class) {
                Some(rewritten) => out.push_str(&rewritten),
                None => {
                    out.push(c);
                    if let Some(n) = next {
                        out.push(n);
                    }
                }
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

struct Flags {
    positive: String,
    negative: String,
}

impl Flags {
    /// The group without `a`/`u`; an empty global group disappears.
    fn render(&self, scoped: bool) -> String {
        let pos: String = self
            .positive
            .chars()
            .filter(|c| !"au".contains(*c))
            .collect();
        let mut body = pos;
        if !self.negative.is_empty() {
            body.push('-');
            body.push_str(&self.negative);
        }
        match (scoped, body.is_empty()) {
            (true, true) => "(?:".to_string(),
            (true, false) => format!("(?{body}:"),
            (false, true) => String::new(),
            (false, false) => format!("(?{body})"),
        }
    }
}

/// A Python inline-flag group at `chars[start] == '('` that mentions `a` or
/// `u`: its flags and the index of its closing `)` or `:`. Other groups are
/// left to fancy-regex untouched.
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
    if chars.get(i) == Some(&'-') {
        i += 1;
        while let Some(&c) = chars.get(i).filter(|c| "imsx".contains(**c)) {
            negative.push(c);
            i += 1;
        }
    }
    let end = chars.get(i)?;
    let scoped = *end == ':';
    if !(scoped || *end == ')') || positive.is_empty() && negative.is_empty() {
        return None;
    }
    if !positive.contains(['a', 'u']) {
        return None;
    }
    if !scoped && !negative.is_empty() {
        return None;
    }
    Some((Flags { positive, negative }, i))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fancy_regex::Regex;

    fn matches(pattern: &str, text: &str) -> bool {
        let translated = translate(pattern);
        Regex::new(&translated)
            .unwrap_or_else(|e| panic!("{pattern:?} -> {translated:?}: {e}"))
            .is_match(text)
            .unwrap()
    }

    #[test]
    fn patterns_without_a_or_u_are_untouched() {
        for p in [
            r"foo$",
            r"(?i)abc",
            r"(?=x)\w+",
            r"[\w\]]",
            r"(?P<n>a)(?:b)",
            r"\(?a\)",
        ] {
            assert_eq!(translate(p), p);
        }
    }

    #[test]
    fn ascii_flag_matches_python() {
        // Expected values from Python 3.11+ `re.search`.
        assert!(!matches(r"(?a)\w+", "é"));
        assert!(matches(r"(?a)\w+", "abc"));
        assert!(!matches(r"(?a:\w)+x", "éx"));
        assert!(matches(r"(?a:\w)+x(\w)", "axé")); // \w outside the scope stays Unicode
        assert!(matches(r"(?ai)k", "K"));
        assert!(matches(r"(?ia)\bx", "éx"));
        assert!(!matches(r"(?a)\Bx", "éx"));
        assert!(matches(r"(?a)[\w-]+$", "a-b"));
        assert!(!matches(r"(?a)^[\w]+$", "é"));
        assert!(matches(r"(?a)^[\W]+$", "é"));
        assert!(matches(r"(?a)\d", "7"));
        assert!(!matches(r"(?a)\d", "٣"));
        assert!(matches(r"(?a)\S\s\S", "a b"));
    }

    #[test]
    fn unicode_flag_clears_ascii_in_its_scope() {
        // Expected values from Python 3.12 `re.search`.
        assert!(matches(r"(?a:msg=(?u:\w+))$", "msg=café"));
        assert!(!matches(r"(?a:msg=(?u:\w+)\w)$", "msg=cafeé"));
        assert!(matches(r"(?a:x(?u:\w))", "xé"));
        assert!(matches(r"(?a)x(?u:\w)", "xé"));
        assert!(!matches(r"(?a:(?u:x)\w)", "xé"));
        assert!(matches(r"(?a:(?u:(?a:x)\w))", "xé"));
    }

    #[test]
    fn unicode_flag_is_dropped() {
        assert_eq!(translate(r"(?u)\w"), r"\w");
        assert_eq!(translate(r"(?iu)\w"), r"(?i)\w");
        assert_eq!(translate(r"(?u:x)"), r"(?:x)");
        assert!(matches(r"(?u)\w", "é"));
    }

    #[test]
    fn locale_flag_still_fails() {
        assert!(Regex::new(&translate(r"(?aL)x")).is_err());
        assert!(Regex::new(&translate(r"(?L)x")).is_err());
    }
}
