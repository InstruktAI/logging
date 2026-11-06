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

/// The longest line, in bytes of UTF-8, without its `\n`.
pub const MAX_LINE_BYTES: usize = 16 * 1024;

/// Appended, with the number of fields left out, to a line that would exceed
/// [`MAX_LINE_BYTES`] with all its fields.
pub(crate) const DROPPED_MARKER: &str = " fields_dropped=";

/// The shortest marker: one digit.
pub(crate) const MIN_MARKER_BYTES: usize = DROPPED_MARKER.len() + 1;

/// A lower bound of every line's head: the timestamp, `level=`, `logger=` and
/// `msg=""` with one-character level and logger.
pub(crate) const HEAD_LOWER_BOUND: usize =
    24 + " level=".len() + 1 + " logger=".len() + 1 + " msg=\"\"".len();

/// One extra field value, as captured from an event.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Value {
    /// Emitted as-is: booleans, integers and floats, already rendered.
    Raw(String),
    /// Already redacted and truncated: written bare or quoted.
    Text(String),
    /// Certainly left out by the line limit, so it was never formatted.
    Dropped,
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

    /// A string value: redacted, then cut at `max_chars` characters.
    pub(crate) fn text(raw: &str, max_chars: usize) -> Self {
        Self::Text(process(raw, max_chars).into_owned())
    }

    /// [`Value::text`] for an owned string, reused when nothing changes.
    pub(crate) fn text_owned(raw: String, max_chars: usize) -> Self {
        let changed = match process(&raw, max_chars) {
            Cow::Owned(changed) => Some(changed),
            Cow::Borrowed(_) => None,
        };
        Self::Text(changed.unwrap_or(raw))
    }

    /// A lower bound of the bytes ` <key>=<value>` takes on the line.
    pub(crate) fn field_lower_bound(&self, key: &str) -> usize {
        match self {
            Self::Raw(raw) => 2 + key.len() + raw.len(),
            // Bare text is written as it is; quoted text only grows. Empty
            // text is `""`.
            Self::Text(text) => 2 + key.len() + text.len().max(1),
            Self::Dropped => 0,
        }
    }
}

/// The string rule's first two steps: redact, then cut at `max_chars`.
pub(crate) fn process(text: &str, max_chars: usize) -> Cow<'_, str> {
    truncate(redact(text), max_chars)
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

/// A key that is written: not `msg`, and safe.
pub(crate) fn is_field_key(key: &str) -> bool {
    key != "msg" && is_safe_key(key)
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

/// Push `text` with every character [`escape_at`] names escaped, no quotes.
fn push_escaped(out: &mut String, text: &str) {
    let bytes = text.as_bytes();
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
}

fn push_quoted(out: &mut String, text: &str) {
    out.push('"');
    push_escaped(out, text);
    out.push('"');
}

/// Push an already processed (redacted, truncated) string, bare or quoted.
fn push_text(out: &mut String, text: &str, force_quote: bool) {
    if !force_quote && is_bare(text) {
        out.push_str(text);
    } else {
        push_quoted(out, text);
    }
}

/// The length of the escape of `escape`.
fn escape_len(escape: Escape) -> usize {
    match escape {
        Escape::Short(s) => s.len(),
        Escape::Unicode(_) => 6,
    }
}

/// The length in bytes of a UTF-8 character with lead byte `b`.
fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

/// The bytes the character at `bytes[i]` takes in `bytes`, and written.
fn char_cost(bytes: &[u8], i: usize) -> (usize, usize) {
    match escape_at(bytes, i) {
        Some((escape, consumed)) => (consumed, escape_len(escape)),
        None => {
            let n = utf8_len(bytes.get(i).copied().unwrap_or(0));
            (n, n)
        }
    }
}

/// The bytes `text` takes written as [`push_text`] writes it, or `None` once
/// that is more than `cap`.
fn text_len_capped(text: &str, cap: usize) -> Option<usize> {
    if is_bare(text) {
        return (text.len() <= cap).then_some(text.len());
    }
    let bytes = text.as_bytes();
    let mut len = 2;
    let mut i = 0;
    while i < bytes.len() {
        let (consumed, written) = char_cost(bytes, i);
        len += written;
        if len > cap {
            return None;
        }
        i += consumed;
    }
    (len <= cap).then_some(len)
}

/// Push `text` quoted, cut on a character boundary to the longest prefix
/// whose escaped form takes at most `avail` bytes, then marked with the
/// truncation suffix. An escape is never split.
fn push_cut(out: &mut String, text: &str, avail: usize) {
    let bytes = text.as_bytes();
    let mut taken = 0;
    let mut end = 0;
    while end < bytes.len() {
        let (consumed, written) = char_cost(bytes, end);
        if taken + written > avail {
            break;
        }
        taken += written;
        end += consumed;
    }
    out.push('"');
    push_escaped(out, text.get(..end).unwrap_or_default());
    out.push_str(TRUNCATION_SUFFIX);
    out.push('"');
}

/// A string as [`push_text`] writes it.
fn rendered(text: &str, force_quote: bool) -> String {
    let mut out = String::new();
    push_text(&mut out, text, force_quote);
    out
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

fn push_marker(out: &mut String, dropped: usize) {
    out.push_str(DROPPED_MARKER);
    let _ = write!(out, "{dropped}");
}

fn marker_len(dropped: usize) -> usize {
    let mut digits = 1;
    let mut rest = dropped / 10;
    while rest > 0 {
        digits += 1;
        rest /= 10;
    }
    DROPPED_MARKER.len() + digits
}

/// The full line, including the trailing `\n`, at most [`MAX_LINE_BYTES`]
/// bytes before it.
///
/// `level`, `logger` and `msg` are raw; field values are already processed.
/// A line that does not fit with every field goes through [`fit`].
pub(crate) fn line(
    ts: &str,
    level: &str,
    logger: &str,
    msg: &str,
    fields: &BTreeMap<&str, Value>,
    max_chars: usize,
) -> String {
    let level = process(level, max_chars);
    let logger = process(logger, max_chars);
    let msg = process(msg, max_chars);
    let mut out = String::with_capacity((64 + msg.len()).min(MAX_LINE_BYTES + 1));
    out.push_str(ts);
    out.push_str(" level=");
    push_text(&mut out, &level, false);
    out.push_str(" logger=");
    push_text(&mut out, &logger, false);
    out.push_str(" msg=");
    push_text(&mut out, &msg, true);
    let head = out.len();
    let mut over = head > MAX_LINE_BYTES;
    let mut count = 0;
    for (key, value) in fields {
        if !is_field_key(key) {
            continue;
        }
        count += 1;
        if over {
            continue;
        }
        match value {
            Value::Dropped => over = true,
            Value::Raw(raw) => {
                out.push(' ');
                out.push_str(key);
                out.push('=');
                out.push_str(raw);
            }
            // Text is never shorter written than held: text that cannot fit
            // is not written out first.
            Value::Text(text) if out.len() + 2 + key.len() + text.len() > MAX_LINE_BYTES => {
                over = true;
            }
            Value::Text(text) => {
                out.push(' ');
                out.push_str(key);
                out.push('=');
                push_text(&mut out, text, false);
            }
        }
        over |= out.len() > MAX_LINE_BYTES;
    }
    if over {
        out.truncate(head);
        fit(&mut out, ts, (&level, &logger, &msg), fields, count);
    }
    out.push('\n');
    out
}

/// The line limit, for a line that does not fit whole. `out` holds the head;
/// `count` fields would follow.
///
/// - When the head fits together with the marker, the fields are appended in
///   sorted order while they fit, leaving room for the marker, and
///   ` fields_dropped=<n>` says how many were left out.
/// - Otherwise every field is left out, and the head is cut so that it fits
///   together with the marker (no marker without fields): the message, on a
///   character boundary with the truncation suffix; should even an empty
///   message not do, then the logger as well, then the level.
#[cold]
fn fit(
    out: &mut String,
    ts: &str,
    (level, logger, msg): (&str, &str, &str),
    fields: &BTreeMap<&str, Value>,
    count: usize,
) {
    let marker = if count > 0 { marker_len(count) } else { 0 };
    if count > 0 && out.len() + marker <= MAX_LINE_BYTES {
        let mut kept = 0;
        for (key, value) in fields.iter().filter(|(k, _)| is_field_key(k)) {
            let room = MAX_LINE_BYTES.saturating_sub(out.len() + marker + 2 + key.len());
            let fits = match value {
                Value::Dropped => false,
                Value::Raw(raw) => raw.len() <= room,
                Value::Text(text) => text_len_capped(text, room).is_some(),
            };
            if !fits {
                break;
            }
            out.push(' ');
            out.push_str(key);
            out.push('=');
            match value {
                Value::Raw(raw) => out.push_str(raw),
                Value::Text(text) => push_text(out, text, false),
                Value::Dropped => {}
            }
            kept += 1;
        }
        push_marker(out, count - kept);
        return;
    }
    out.clear();
    let budget = MAX_LINE_BYTES - marker;
    // A cut value: its quotes and the suffix, around the kept prefix.
    let cut = 2 + TRUNCATION_SUFFIX.len();
    let level_r = rendered(level, false);
    let logger_r = rendered(logger, false);
    let (lv, lg, m) = (" level=".len(), " logger=".len(), " msg=".len());
    out.push_str(ts);
    out.push_str(" level=");
    if let Some(avail) =
        budget.checked_sub(ts.len() + lv + level_r.len() + lg + logger_r.len() + m + cut)
    {
        out.push_str(&level_r);
        out.push_str(" logger=");
        out.push_str(&logger_r);
        out.push_str(" msg=");
        push_cut(out, msg, avail);
    } else {
        if let Some(avail) = budget.checked_sub(ts.len() + lv + level_r.len() + lg + cut + m + cut)
        {
            out.push_str(&level_r);
            out.push_str(" logger=");
            push_cut(out, logger, avail);
        } else {
            let avail = budget.saturating_sub(ts.len() + lv + cut + lg + cut + m + cut);
            push_cut(out, level, avail);
            out.push_str(" logger=");
            push_cut(out, "", 0);
        }
        out.push_str(" msg=");
        push_cut(out, "", 0);
    }
    if count > 0 {
        push_marker(out, count);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields<'a>(pairs: &[(&'a str, Value)]) -> BTreeMap<&'a str, Value> {
        pairs.iter().map(|(k, v)| (*k, v.clone())).collect()
    }

    fn s(v: &str) -> Value {
        Value::Text(v.to_owned())
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

    const TS: &str = "2026-11-06T12:00:00.000Z";

    #[test]
    fn a_line_that_fits_is_whole_and_has_no_marker() {
        let f = fields(&[("a", s("x")), ("b", Value::int(2))]);
        let out = line(TS, "INFO", "app", "m", &f, 4000);
        assert_eq!(
            out,
            format!("{TS} level=INFO logger=app msg=\"m\" a=x b=2\n")
        );
    }

    #[test]
    fn every_line_is_at_most_the_limit() {
        for (level, logger, msg, value) in [
            ("INFO", "app", "m", "x".repeat(40_000)),
            ("INFO", "app", &*"é".repeat(20_000), "v".to_owned()),
            ("INFO", "app", &*"\u{1b}".repeat(5_000), String::new()),
            ("INFO", &*"a".repeat(20_000), "m", String::new()),
            (
                &*"L".repeat(20_000),
                &*"g".repeat(20_000),
                &*"m".repeat(20_000),
                "v".to_owned(),
            ),
        ] {
            let f = fields(&[("k", s(&value))]);
            let out = line(TS, level, logger, msg, &f, 0);
            assert!(out.len() <= MAX_LINE_BYTES + 1, "{} bytes", out.len());
            assert!(out.ends_with('\n'));
        }
    }

    #[test]
    fn a_level_too_long_for_even_empty_logger_and_message_is_cut_too() {
        let out = line(
            TS,
            &"L".repeat(20_000),
            &"g".repeat(20_000),
            "m",
            &BTreeMap::new(),
            0,
        );
        let cut = format!("\"{TRUNCATION_SUFFIX}\"");
        assert!(out.starts_with(&format!("{TS} level=\"LLL")));
        assert!(out.ends_with(&format!("{TRUNCATION_SUFFIX}\" logger={cut} msg={cut}\n")));
        assert_eq!(out.len(), MAX_LINE_BYTES + 1);
    }

    #[test]
    fn dropped_fields_are_left_out_and_counted() {
        // `b` is held as Dropped: certainly not on the line.
        let f = fields(&[
            ("a", s(&"x".repeat(16_400))),
            ("b", Value::Dropped),
            ("c", s("y")),
        ]);
        let out = line(TS, "INFO", "app", "m", &f, 0);
        assert_eq!(
            out,
            format!("{TS} level=INFO logger=app msg=\"m\" fields_dropped=3\n")
        );
        let f = fields(&[("a", s("x")), ("b", Value::Dropped), ("c", s("y"))]);
        let out = line(TS, "INFO", "app", "m", &f, 0);
        assert_eq!(
            out,
            format!("{TS} level=INFO logger=app msg=\"m\" a=x fields_dropped=2\n")
        );
    }

    #[test]
    fn timestamp_shape() {
        let t = OffsetDateTime::from_unix_timestamp_nanos(1_700_000_000_123_999_999)
            .unwrap_or(OffsetDateTime::UNIX_EPOCH);
        assert_eq!(timestamp(t), "2023-11-14T22:13:20.123Z");
    }
}
