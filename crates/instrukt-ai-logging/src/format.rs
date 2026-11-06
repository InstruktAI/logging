//! The logfmt line: `<ts> level=<L> logger=<name> msg="<msg>"[ <k>=<v>]...`.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use time::OffsetDateTime;

use crate::redact::redact;

/// Appended to a string value cut at `max_message_chars` characters.
pub(crate) const TRUNCATION_SUFFIX: &str = "…(truncated)";

/// The library default for `max_message_chars`.
pub const DEFAULT_MAX_MESSAGE_CHARS: usize = 4000;

/// One extra field value, as captured from an event.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Value {
    /// Emitted as-is: booleans, integers and floats, already rendered.
    Raw(String),
    /// Redacted, truncated, then bare or quoted.
    Str(String),
}

impl Value {
    pub(crate) fn bool(v: bool) -> Self {
        Self::Raw(if v { "true" } else { "false" }.to_owned())
    }

    pub(crate) fn int(v: impl std::fmt::Display) -> Self {
        Self::Raw(v.to_string())
    }

    pub(crate) fn float(v: f64) -> Self {
        Self::Raw(python_float_repr(v))
    }
}

/// The shortest round-trip digits of `v` in `{:e}` form (`-1.25e-7`, `0e0`),
/// choosing, like Python's dtoa, the candidate nearest the exact value (ties
/// to even). Rust's shortest formatter can pick the other of two equally near
/// candidates, so the digit count it finds is re-rounded exactly.
fn shortest_sci(v: f64) -> String {
    let shortest = format!("{v:e}");
    let mantissa = shortest
        .split_once('e')
        .map_or(shortest.as_str(), |(m, _)| m);
    let digits = mantissa.chars().filter(char::is_ascii_digit).count();
    let rounded = format!("{v:.prec$e}", prec = digits.saturating_sub(1));
    if rounded.parse::<f64>().ok() == Some(v) {
        rounded
    } else {
        shortest
    }
}

/// Render `v` the way Python's `repr(float)` does: shortest round-trip digits,
/// fixed notation for decimal exponents in `[-4, 16)` with a trailing `.0` for
/// integral values, scientific (`1e+16`, `1.5e-05`) otherwise.
pub(crate) fn python_float_repr(v: f64) -> String {
    if v.is_nan() {
        return "nan".to_owned();
    }
    if v.is_infinite() {
        return if v > 0.0 { "inf" } else { "-inf" }.to_owned();
    }
    let sci = shortest_sci(v);
    let (sign, unsigned) = match sci.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", sci.as_str()),
    };
    let (mantissa, exp) = unsigned.split_once('e').unwrap_or((unsigned, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    // Position of the decimal point relative to the start of `digits`.
    let decpt = exp + 1;
    let n = i32::try_from(digits.len()).unwrap_or(i32::MAX);

    let mut out = String::from(sign);
    if decpt <= -4 || decpt > 16 {
        let (first, rest) = digits.split_at(1);
        out.push_str(first);
        if !rest.is_empty() {
            out.push('.');
            out.push_str(rest);
        }
        let e = decpt - 1;
        let _ = write!(
            out,
            "e{}{:02}",
            if e < 0 { '-' } else { '+' },
            e.unsigned_abs()
        );
    } else if decpt <= 0 {
        out.push_str("0.");
        out.extend(std::iter::repeat_n('0', decpt.unsigned_abs() as usize));
        out.push_str(&digits);
    } else if decpt < n {
        let (int_part, frac) = digits.split_at(decpt.unsigned_abs() as usize);
        out.push_str(int_part);
        out.push('.');
        out.push_str(frac);
    } else {
        out.push_str(&digits);
        out.extend(std::iter::repeat_n(
            '0',
            (decpt - n).unsigned_abs() as usize,
        ));
        out.push_str(".0");
    }
    out
}

/// Cut `text` to `max_chars` characters (Unicode scalar values) and mark it.
/// `0` disables truncation. Text that fits is returned as it came.
pub(crate) fn truncate(text: Cow<'_, str>, max_chars: usize) -> Cow<'_, str> {
    if max_chars == 0 {
        return text;
    }
    let Some((idx, _)) = text.char_indices().nth(max_chars) else {
        return text;
    };
    let mut cut = match text {
        Cow::Owned(mut owned) => {
            owned.truncate(idx);
            owned
        }
        Cow::Borrowed(borrowed) => {
            let mut cut = String::with_capacity(idx + TRUNCATION_SUFFIX.len());
            cut.push_str(borrowed.get(..idx).unwrap_or_default());
            cut
        }
    };
    cut.push_str(TRUNCATION_SUFFIX);
    Cow::Owned(cut)
}

fn is_bare(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | ':' | '+' | '-'))
}

/// `^[A-Za-z_][A-Za-z0-9_.-]*$`
pub(crate) fn is_safe_key(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

/// Bytes that can start a character [`push_quoted`] escapes: `\\`, `"`, every
/// C0 control and DEL, and the lead bytes of C1 (`0xC2`), U+061C (`0xD8`) and
/// the U+2xxx bidi controls (`0xE2`), which need their next bytes checked.
const ESCAPE_START: [bool; 256] = {
    let mut table = [false; 256];
    let mut b = 0;
    while b < 0x20 {
        table[b] = true;
        b += 1;
    }
    table[b'\\' as usize] = true;
    table[b'"' as usize] = true;
    table[0x7f] = true;
    table[0xc2] = true;
    table[0xd8] = true;
    table[0xe2] = true;
    table
};

/// The escape for the character starting at `bytes[i]`, and its length in
/// bytes; `None` when it is written as it is. `\\`, `"`, LF and CR get their
/// short escapes; every other C0 control, DEL, C1 (U+0080–U+009F) and the
/// bidi controls U+061C, U+200E, U+200F, U+202A–U+202E and U+2066–U+2069 are
/// written as `\u` and four lowercase hex digits.
fn escape_at(bytes: &[u8], i: usize) -> Option<(Escape, usize)> {
    let next = |k: usize| bytes.get(i + k).copied().unwrap_or(0);
    let b = bytes.get(i).copied()?;
    let (escape, len) = match b {
        b'\\' => (Escape::Short("\\\\"), 1),
        b'"' => (Escape::Short("\\\""), 1),
        b'\n' => (Escape::Short("\\n"), 1),
        b'\r' => (Escape::Short("\\r"), 1),
        0x00..=0x1f | 0x7f => (Escape::Unicode(u32::from(b)), 1),
        // U+0080..U+009F: C2 80..C2 9F.
        0xc2 if (0x80..=0x9f).contains(&next(1)) => (Escape::Unicode(u32::from(next(1))), 2),
        // U+061C: D8 9C.
        0xd8 if next(1) == 0x9c => (Escape::Unicode(0x061c), 2),
        // U+200E, U+200F, U+202A..U+202E: E2 80 8E/8F/AA..AE.
        0xe2 if next(1) == 0x80 && matches!(next(2), 0x8e | 0x8f | 0xaa..=0xae) => {
            (Escape::Unicode(0x2000 + u32::from(next(2) - 0x80)), 3)
        }
        // U+2066..U+2069: E2 81 A6..A9.
        0xe2 if next(1) == 0x81 && (0xa6..=0xa9).contains(&next(2)) => {
            (Escape::Unicode(0x2040 + u32::from(next(2) - 0x80)), 3)
        }
        _ => return None,
    };
    Some((escape, len))
}

#[derive(Clone, Copy)]
enum Escape {
    Short(&'static str),
    Unicode(u32),
}

fn push_escape(out: &mut String, escape: Escape) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    match escape {
        Escape::Short(s) => out.push_str(s),
        Escape::Unicode(cp) => {
            out.push_str("\\u");
            for shift in [12, 8, 4, 0] {
                out.push(char::from(HEX[((cp >> shift) & 0xf) as usize]));
            }
        }
    }
}

fn push_quoted(out: &mut String, text: &str) {
    let bytes = text.as_bytes();
    out.push('"');
    let mut copied = 0;
    let mut i = 0;
    while i < bytes.len() {
        if !ESCAPE_START[usize::from(bytes[i])] {
            i += 1;
            continue;
        }
        let Some((escape, len)) = escape_at(bytes, i) else {
            i += 1;
            continue;
        };
        // `i` starts a character: every byte that can start an escape is
        // ASCII or a UTF-8 lead byte.
        out.push_str(text.get(copied..i).unwrap_or_default());
        push_escape(out, escape);
        i += len;
        copied = i;
    }
    out.push_str(text.get(copied..).unwrap_or_default());
    out.push('"');
}

fn push_string(out: &mut String, text: &str, max_chars: usize, force_quote: bool) {
    let text = truncate(redact(text), max_chars);
    if !force_quote && is_bare(&text) {
        out.push_str(&text);
    } else {
        push_quoted(out, &text);
    }
}

/// UTC `YYYY-MM-DDTHH:MM:SS.mmmZ`, milliseconds truncated.
pub(crate) fn timestamp(now: OffsetDateTime) -> String {
    let now = now.to_offset(time::UtcOffset::UTC);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
        now.millisecond()
    )
}

/// The full line, including the trailing `\n`.
pub(crate) fn line(
    ts: &str,
    level: &str,
    logger: &str,
    msg: &str,
    fields: &BTreeMap<String, Value>,
    max_chars: usize,
) -> String {
    let mut out = String::with_capacity(64 + msg.len());
    out.push_str(ts);
    out.push_str(" level=");
    push_string(&mut out, level, max_chars, false);
    out.push_str(" logger=");
    push_string(&mut out, logger, max_chars, false);
    out.push_str(" msg=");
    push_string(&mut out, msg, max_chars, true);
    for (key, value) in fields {
        if key == "msg" || !is_safe_key(key) {
            continue;
        }
        out.push(' ');
        out.push_str(key);
        out.push('=');
        match value {
            Value::Raw(raw) => out.push_str(raw),
            Value::Str(s) => push_string(&mut out, s, max_chars, false),
        }
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect()
    }

    fn s(v: &str) -> Value {
        Value::Str(v.to_owned())
    }

    #[test]
    fn float_repr_matches_python() {
        let cases: &[(f64, &str)] = &[
            (2.0, "2.0"),
            (1.5, "1.5"),
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (0.1, "0.1"),
            (0.1 + 0.2, "0.30000000000000004"),
            (1e15, "1000000000000000.0"),
            (1e16, "1e+16"),
            (1.5e16, "1.5e+16"),
            (123456789.125, "123456789.125"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1.25e-7, "1.25e-07"),
            (-3.5e300, "-3.5e+300"),
            (5e-324, "5e-324"),
            (f64::MAX, "1.7976931348623157e+308"),
            // Exactly halfway between two shortest candidates: ties to even.
            (-1_113_178_120_592_002.0 - 0.25, "-1113178120592002.2"),
            (111_659_285_584_252.0 + 0.125, "111659285584252.12"),
            (f64::NAN, "nan"),
            (f64::INFINITY, "inf"),
            (f64::NEG_INFINITY, "-inf"),
        ];
        for (v, want) in cases {
            assert_eq!(python_float_repr(*v), *want, "{v:e}");
        }
    }

    #[test]
    fn redaction_happens_before_truncation() {
        let out = line("T", "INFO", "a", "Bearer secrettoken", &BTreeMap::new(), 10);
        assert_eq!(
            out,
            "T level=INFO logger=a msg=\"Bearer <RE…(truncated)\"\n"
        );
    }

    #[test]
    fn values_quote_and_escape() {
        let f = fields(&[
            ("bare", s("a.b/c:d+e-f_1")),
            ("empty", s("")),
            ("space", s("a b")),
            ("esc", s("q\"b\\n\nr\r")),
            ("t", Value::bool(true)),
            ("n", Value::int(-3)),
            ("x", Value::float(2.0)),
        ]);
        assert_eq!(
            line("T", "INFO", "a", "m", &f, 4000),
            "T level=INFO logger=a msg=\"m\" bare=a.b/c:d+e-f_1 empty=\"\" esc=\"q\\\"b\\\\n\\nr\\r\" n=-3 space=\"a b\" t=true x=2.0\n"
        );
    }

    #[test]
    fn terminal_controls_are_escaped() {
        let f = fields(&[(
            "v",
            s(
                "\u{1b}[31m\u{0}\u{7}\t\u{7f}\u{85}\u{9f}\u{61c}\u{200e}\u{200f}\u{202a}\u{202e}\u{2066}\u{2069}",
            ),
        )]);
        assert_eq!(
            line("T", "INFO", "a", "x\u{1b}y", &f, 4000),
            concat!(
                "T level=INFO logger=a msg=\"x\\u001by\" ",
                "v=\"\\u001b[31m\\u0000\\u0007\\u0009\\u007f\\u0085\\u009f\\u061c\\u200e\\u200f\\u202a\\u202e\\u2066\\u2069\"\n"
            )
        );
    }

    #[test]
    fn neighbours_of_the_escaped_controls_stay_as_they_are() {
        // Same lead bytes, other characters: U+00A0, U+00E9, U+0600, U+2010,
        // U+2029, U+202F, U+2065, U+206A, and a 4-byte character.
        let text = "\u{a0}\u{e9}\u{600}\u{2010}\u{2029}\u{202f}\u{2065}\u{206a}\u{1f600}";
        let f = fields(&[("v", s(text))]);
        assert_eq!(
            line("T", "INFO", "a", "m", &f, 4000),
            format!("T level=INFO logger=a msg=\"m\" v=\"{text}\"\n")
        );
    }

    #[test]
    fn keys_are_sorted_and_unsafe_or_msg_keys_dropped() {
        let f = fields(&[
            ("zeta", s("1")),
            ("alpha", s("2")),
            ("msg", s("x")),
            ("9bad", s("x")),
            ("a.b-c", s("3")),
        ]);
        assert_eq!(
            line("T", "DEBUG", "a::b", "m", &f, 4000),
            "T level=DEBUG logger=a::b msg=\"m\" a.b-c=3 alpha=2 zeta=1\n"
        );
    }

    #[test]
    fn msg_is_always_quoted_and_logger_quoted_when_needed() {
        let out = line("T", "INFO", "weird logger", "plain", &BTreeMap::new(), 4000);
        assert_eq!(out, "T level=INFO logger=\"weird logger\" msg=\"plain\"\n");
    }

    #[test]
    fn timestamp_shape() {
        let t = OffsetDateTime::from_unix_timestamp_nanos(1_700_000_000_123_999_999)
            .unwrap_or(OffsetDateTime::UNIX_EPOCH);
        assert_eq!(timestamp(t), "2023-11-14T22:13:20.123Z");
    }
}
