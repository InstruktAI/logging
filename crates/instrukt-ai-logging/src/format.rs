//! The logfmt line: `<ts> level=<L> logger=<name> msg="<msg>"[ <k>=<v>]...`.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::LazyLock;

use regex::Regex;
use time::OffsetDateTime;

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

/// The three secret patterns, applied in order.
static REDACTIONS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (r"(api\.telegram\.org/bot)([^/\s]+)", "${1}<REDACTED>"),
        (r"\bBearer\s+[^\s]+", "Bearer <REDACTED>"),
        (r"\bsk-[A-Za-z0-9]{10,}\b", "sk-<REDACTED>"),
    ]
    .into_iter()
    // The patterns are constants covered by tests; a failure to compile one
    // cannot happen at runtime, and if it did, dropping it beats panicking.
    .filter_map(|(p, r)| Regex::new(p).ok().map(|re| (re, r)))
    .collect()
});

pub(crate) fn redact(text: &str) -> String {
    let mut out = text.to_owned();
    for (re, replacement) in REDACTIONS.iter() {
        if re.is_match(&out) {
            out = re.replace_all(&out, *replacement).into_owned();
        }
    }
    out
}

/// Cut `text` to `max_chars` characters (Unicode scalar values) and mark it.
/// `0` disables truncation.
pub(crate) fn truncate(mut text: String, max_chars: usize) -> String {
    if max_chars == 0 {
        return text;
    }
    if let Some((idx, _)) = text.char_indices().nth(max_chars) {
        text.truncate(idx);
        text.push_str(TRUNCATION_SUFFIX);
    }
    text
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

fn push_quoted(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
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
    fn redaction_patterns() {
        assert_eq!(
            redact("GET https://api.telegram.org/bot123:ABC/sendMessage"),
            "GET https://api.telegram.org/bot<REDACTED>/sendMessage"
        );
        assert_eq!(
            redact("Authorization: Bearer abc.def x"),
            "Authorization: Bearer <REDACTED> x"
        );
        assert_eq!(redact("key sk-abcdefghij12 end"), "key sk-<REDACTED> end");
        assert_eq!(redact("short sk-abc"), "short sk-abc");
        assert_eq!(redact("task-abcdefghijkl"), "task-abcdefghijkl");
    }

    #[test]
    fn truncation_counts_characters() {
        assert_eq!(truncate("héllo".to_owned(), 2), "hé…(truncated)");
        assert_eq!(truncate("héllo".to_owned(), 5), "héllo");
        assert_eq!(truncate("héllo".to_owned(), 0), "héllo");
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
