//! Secret redaction (`docs/contract.md`, Redaction).
//!
//! Redaction is a set of secret spans found on the original text: every rule
//! finds its spans at each candidate position on its own, overlaps allowed,
//! and no match consumes text another match could use. The spans are merged,
//! joining overlapping and touching ones, and each merged span is replaced by
//! `<REDACTED>`. Everything outside the spans stays byte-for-byte as it was, so
//! the result does not depend on rule order.
//!
//! The scanner is explicit rather than a regex: the literals are found by
//! forward search, the boundary before a literal is checked first, and the end
//! of a separator run or of a token, which depends only on the position, is
//! answered by a forward-only cursor ([`NextIndex`]). No suffix is scanned
//! twice, so redaction is linear in the input length. Offsets are byte offsets
//! that always fall on a char boundary: the literals are ASCII and the cursor
//! only stops at the start of a char.

const REDACTED: &str = "<REDACTED>";
const TELEGRAM: &str = "api.telegram.org/bot";
const BEARER: &str = "Bearer";
const SK: &str = "sk-";
const SK_MIN_KEY: usize = 10;

/// `WORD`: ASCII letters, digits and underscore, nothing else. Every set is
/// spelled out instead of `\b`, `\w` or `\s`, whose meaning differs between
/// regex engines for non-ASCII input.
fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// `SPACE`: Unicode White_Space. A token ends only here.
fn is_space(c: char) -> bool {
    matches!(
        c,
        '\t'..='\r'
            | ' '
            | '\u{85}'
            | '\u{a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
    )
}

/// `SEP`: `SPACE` plus U+001C-U+001F. Separates `Bearer` from its token.
fn is_sep(c: char) -> bool {
    is_space(c) || matches!(c, '\u{1c}'..='\u{1f}')
}

/// A boundary before byte `i`: the start of the text, or a char before it that
/// is not in `WORD`. `WORD` is ASCII and a UTF-8 continuation byte never is,
/// so the byte before `i` decides.
fn boundary_before(text: &str, i: usize) -> bool {
    match i.checked_sub(1).and_then(|j| text.as_bytes().get(j)) {
        Some(&b) => !is_word_byte(b),
        None => true,
    }
}

/// A boundary at byte `i`, looking forward: the end of the text, or a char
/// there that is not in `WORD`.
fn boundary_after(text: &str, i: usize) -> bool {
    text.as_bytes().get(i).is_none_or(|&b| !is_word_byte(b))
}

/// The first byte offset at or after `i` whose char satisfies `stop`, or the
/// text length.
///
/// The answer depends only on `i`, and the scanner asks with non-decreasing
/// `i`. While a question falls inside the last answered range the last answer
/// still holds, so no suffix of the text is scanned twice.
struct NextIndex<'t> {
    text: &'t str,
    stop: fn(char) -> bool,
    asked: usize,
    answer: usize,
}

impl<'t> NextIndex<'t> {
    fn new(text: &'t str, stop: fn(char) -> bool) -> Self {
        Self {
            text,
            stop,
            asked: text.len() + 1,
            answer: text.len(),
        }
    }

    fn at(&mut self, i: usize) -> usize {
        if !(self.asked <= i && i <= self.answer) {
            let stop = self.stop;
            // `i` is always a char boundary. If it were not, treating the rest
            // as one token redacts more, never less.
            self.answer = self.text.get(i..).map_or(self.text.len(), |rest| {
                rest.char_indices()
                    .find(|&(_, c)| stop(c))
                    .map_or(self.text.len(), |(j, _)| i + j)
            });
            self.asked = i;
        }
        self.answer
    }
}

/// Collects one rule's spans, given in start order, joining overlapping and
/// touching ones as they arrive.
#[derive(Default)]
struct Spans(Vec<(usize, usize)>);

impl Spans {
    fn push(&mut self, start: usize, end: usize) {
        match self.0.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => self.0.push((start, end)),
        }
    }
}

/// Every occurrence of `literal`. None of the literals can overlap itself, so
/// the non-overlapping matches are all of them.
fn positions<'t>(text: &'t str, literal: &'static str) -> impl Iterator<Item = usize> + 't {
    text.match_indices(literal).map(|(i, _)| i)
}

/// `api.telegram.org/bot`, no boundary: the secret is the maximal non-empty run
/// of chars that are neither `/` nor `SPACE`, directly after it.
fn telegram(text: &str, out: &mut Spans) {
    let mut token_end = NextIndex::new(text, |c| c == '/' || is_space(c));
    for i in positions(text, TELEGRAM) {
        let start = i + TELEGRAM.len();
        let end = token_end.at(start);
        if end > start {
            out.push(start, end);
        }
    }
}

/// `Bearer` after a boundary, then a maximal run of one or more `SEP`: the
/// secret is the maximal non-empty run of non-`SPACE` chars after that run.
fn bearer(text: &str, out: &mut Spans) {
    let mut sep_end = NextIndex::new(text, |c| !is_sep(c));
    let mut token_end = NextIndex::new(text, is_space);
    for i in positions(text, BEARER) {
        if !boundary_before(text, i) {
            continue;
        }
        let after = i + BEARER.len();
        let start = sep_end.at(after);
        if start == after || start == text.len() {
            continue;
        }
        // The separator run is maximal, so the char at `start` is not in `SEP`
        // and hence not in `SPACE`: the token is never empty here.
        out.push(start, token_end.at(start));
    }
}

/// `sk-` after a boundary: the secret is a maximal run of 10 or more ASCII
/// alphanumerics directly after it, followed by a boundary. The run is maximal,
/// so what follows is not alphanumeric and the boundary fails only on `_`.
fn sk(text: &str, out: &mut Spans) {
    let mut key_end = NextIndex::new(text, |c| !c.is_ascii_alphanumeric());
    for i in positions(text, SK) {
        if !boundary_before(text, i) {
            continue;
        }
        let start = i + SK.len();
        let end = key_end.at(start);
        if end - start >= SK_MIN_KEY && boundary_after(text, end) {
            out.push(start, end);
        }
    }
}

pub(crate) fn redact(text: &str) -> String {
    let mut by_rule = [Spans::default(), Spans::default(), Spans::default()];
    let [t, b, k] = &mut by_rule;
    telegram(text, t);
    bearer(text, b);
    sk(text, k);
    let mut spans: Vec<(usize, usize)> = by_rule.into_iter().flat_map(|s| s.0).collect();
    if spans.is_empty() {
        return text.to_owned();
    }
    // Three runs, each already in start order: the stable sort merges them in
    // linear time.
    spans.sort();
    let mut merged = Spans::default();
    for (start, end) in spans {
        merged.push(start, end);
    }
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    for (start, end) in merged.0 {
        out.push_str(text.get(copied..start).unwrap_or_default());
        out.push_str(REDACTED);
        copied = end;
    }
    out.push_str(text.get(copied..).unwrap_or_default());
    out
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use regex::Regex;

    use super::*;

    #[test]
    fn patterns() {
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
        assert_eq!(redact(""), "");
    }

    #[test]
    fn word_boundary_is_ascii() {
        let cases = [
            // A combining mark is not a redaction word character.
            ("\u{301}Bearer abcdefghij", "\u{301}Bearer <REDACTED>"),
            ("\u{301}sk-abcdefghij1234", "\u{301}sk-<REDACTED>"),
            ("sk-abcdefghij1234\u{301}", "sk-<REDACTED>\u{301}"),
            // Nor is a non-ASCII letter or digit.
            ("\u{e9}Bearer abcdefghij", "\u{e9}Bearer <REDACTED>"),
            ("\u{663}sk-abcdefghij1234", "\u{663}sk-<REDACTED>"),
            ("sk-abcdefghij1234\u{e9}", "sk-<REDACTED>\u{e9}"),
            // ASCII letters, digits and underscore are.
            ("xBearer abcdefghij", "xBearer abcdefghij"),
            ("9sk-abcdefghij1234", "9sk-abcdefghij1234"),
            ("_sk-abcdefghij1234", "_sk-abcdefghij1234"),
            ("sk-abcdefghij1234_", "sk-abcdefghij1234_"),
            // ASCII punctuation is a boundary.
            ("(Bearer abcdefghij)", "(Bearer <REDACTED>"),
            ("=sk-abcdefghij1234.", "=sk-<REDACTED>."),
        ];
        for (text, want) in cases {
            assert_eq!(redact(text), want, "{text:?}");
        }
    }

    #[test]
    fn sk_trailing_boundary_fails_only_on_underscore() {
        // The key is a maximal alphanumeric run, so the char after it is never
        // an ASCII letter or digit; of WORD only `_` is left.
        assert_eq!(redact("sk-abcdefghij1234_"), "sk-abcdefghij1234_");
        assert_eq!(redact("sk-abcdefghij1234-x"), "sk-<REDACTED>-x");
        assert_eq!(redact("sk-abcdefghij1234"), "sk-<REDACTED>");
        // Nine characters are too short, ten are enough.
        assert_eq!(redact("sk-abcdefghi"), "sk-abcdefghi");
        assert_eq!(redact("sk-abcdefghij"), "sk-<REDACTED>");
    }

    #[test]
    fn rejected_start_does_not_hide_a_later_secret() {
        assert_eq!(
            redact("xBearer abc Bearer def"),
            "xBearer abc Bearer <REDACTED>"
        );
        assert_eq!(
            redact("xsk-abcdefghij1234 sk-abcdefghij1234"),
            "xsk-abcdefghij1234 sk-<REDACTED>"
        );
        assert_eq!(
            redact("sk-abcdefghij1234_ sk-abcdefghij1234"),
            "sk-abcdefghij1234_ sk-<REDACTED>"
        );
        // A rejected start right before a multi-byte character.
        assert_eq!(
            redact("xBearer \u{e9}\u{e9} \u{e9}Bearer t"),
            "xBearer \u{e9}\u{e9} \u{e9}Bearer <REDACTED>"
        );
    }

    #[test]
    fn adjacent_keys_are_both_redacted() {
        assert_eq!(
            redact("sk-abcdefghij1234.sk-abcdefghij5678"),
            "sk-<REDACTED>.sk-<REDACTED>"
        );
    }

    #[test]
    fn whitespace_classes() {
        let cases = [
            // Any Unicode White_Space separates Bearer from its token, as do
            // U+001C-U+001F. The separator stays as it was.
            ("Bearer\u{a0}abcdefghij", "Bearer\u{a0}<REDACTED>"),
            ("Bearer\u{3000}abcdefghij", "Bearer\u{3000}<REDACTED>"),
            ("Bearer\u{1c}abcdefghij", "Bearer\u{1c}<REDACTED>"),
            ("Bearer\u{1f}abcdefghij", "Bearer\u{1f}<REDACTED>"),
            ("Bearer  \t abcdefghij", "Bearer  \t <REDACTED>"),
            // A token ends only at Unicode White_Space; U+001C-U+001F stay
            // inside it.
            ("Bearer abc\u{1f}def ghi", "Bearer <REDACTED> ghi"),
            ("Bearer abc\u{2028}def", "Bearer <REDACTED>\u{2028}def"),
            (
                "api.telegram.org/bot12:AB\u{1c}CD/send",
                "api.telegram.org/bot<REDACTED>/send",
            ),
            (
                "api.telegram.org/bot12:AB\u{a0}CD",
                "api.telegram.org/bot<REDACTED>\u{a0}CD",
            ),
            // Bearer with no token is left alone, also when the separator run
            // reaches the end.
            ("Bearer\u{1f}", "Bearer\u{1f}"),
            ("Bearer \u{1c}", "Bearer \u{1c}"),
            ("api.telegram.org/bot", "api.telegram.org/bot"),
            ("api.telegram.org/bot/x", "api.telegram.org/bot/x"),
        ];
        for (text, want) in cases {
            assert_eq!(redact(text), want, "{text:?}");
        }
    }

    #[test]
    fn spans_are_found_on_the_original_text() {
        let cases = [
            // The review's three inputs: a token that is itself `Bearer` does
            // not hide the next `Bearer`'s token.
            (
                "\u{e9}Bearer Bearer abcdefghij",
                "\u{e9}Bearer <REDACTED> <REDACTED>",
            ),
            (
                "Bearer \u{1c} Bearer abcdefghij",
                "Bearer \u{1c} <REDACTED> <REDACTED>",
            ),
            (
                "api.telegram.org/botx\u{1c}Bearer abcdefghij",
                "api.telegram.org/bot<REDACTED> <REDACTED>",
            ),
            // An sk- key inside, or right after, a Bearer token.
            ("Bearer sk-abcdefghij1234", "Bearer <REDACTED>"),
            ("Bearer abc.sk-abcdefghij1234", "Bearer <REDACTED>"),
            (
                "Bearer abc sk-abcdefghij1234",
                "Bearer <REDACTED> sk-<REDACTED>",
            ),
            // A Bearer token that contains a Telegram URL.
            (
                "Bearer api.telegram.org/bot12:AB/send x",
                "Bearer <REDACTED> x",
            ),
            // Overlapping spans of two rules become one.
            (
                "api.telegram.org/botA\u{1c}Bearer\u{1c}TOKEN/send x",
                "api.telegram.org/bot<REDACTED> x",
            ),
            // Touching spans of two rules become one.
            (
                "api.telegram.org/bot.Bearer\u{1c}/rest x",
                "api.telegram.org/bot<REDACTED> x",
            ),
        ];
        for (text, want) in cases {
            assert_eq!(redact(text), want, "{text:?}");
        }
    }

    /// 0.8.0's redaction, verbatim: three regex replacements applied in turn.
    fn redact_0_8_0(rules: &[(Regex, &str)], text: &str) -> String {
        let mut out = text.to_owned();
        for (re, replacement) in rules {
            out = re.replace_all(&out, *replacement).into_owned();
        }
        out
    }

    fn rules_0_8_0() -> Vec<(Regex, &'static str)> {
        [
            (r"(api\.telegram\.org/bot)([^/\s]+)", "${1}<REDACTED>"),
            (r"\bBearer\s+[^\s]+", "Bearer <REDACTED>"),
            (r"\bsk-[A-Za-z0-9]{10,}\b", "sk-<REDACTED>"),
        ]
        .into_iter()
        .map(|(p, r)| (Regex::new(p).expect("0.8.0 pattern compiles"), r))
        .collect()
    }

    /// splitmix64: a fixed seed gives the same cases on every run.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        }

        fn below(&mut self, n: usize) -> usize {
            usize::try_from(self.next() % n as u64).expect("fits")
        }
    }

    /// Lowercase letters and a digit that no redaction ever writes, so the
    /// marker is in the output only if an input occurrence survived intact.
    const MARKER: &str = "qzx9";

    const FRAGMENTS: &[&str] = &[
        "Bearer",
        "Bearer ",
        "sk-",
        "api.telegram.org/bot",
        MARKER,
        MARKER,
        "abcdefghij",
        "0123",
        "x",
        "_",
        "-",
        ".",
        "/",
        ":",
        "=",
        "(",
        " ",
        " ",
        "\t",
        "\n",
        "\u{1c}",
        "\u{1f}",
        "\u{85}",
        "\u{a0}",
        "\u{2028}",
        "\u{3000}",
        "\u{e9}",
        "\u{301}",
        "\u{663}",
        "B",
        "s",
        "k",
    ];

    fn review_inputs() -> Vec<String> {
        [
            "\u{e9}Bearer Bearer abcdefghij",
            "Bearer \u{1c} Bearer abcdefghij",
            "api.telegram.org/botx\u{1c}Bearer abcdefghij",
        ]
        .iter()
        .map(|s| s.replace("abcdefghij", &format!("{MARKER}abcdefghij")))
        .collect()
    }

    #[test]
    fn differential_never_shows_what_0_8_0_hid() {
        let mut rng = Rng(18);
        let mut cases = review_inputs();
        // Adversarial: every pair and triple of fragments around a marker.
        for a in FRAGMENTS {
            for b in FRAGMENTS {
                cases.push(format!("{a}{b}{MARKER}abcdefghij"));
                cases.push(format!("{a}{MARKER}{b}abcdefghij"));
            }
        }
        // Random: up to 16 fragments each.
        for _ in 0..20_000 {
            let n = 1 + rng.below(16);
            let case: String = (0..n)
                .map(|_| FRAGMENTS[rng.below(FRAGMENTS.len())])
                .collect();
            cases.push(case);
        }
        let rules = rules_0_8_0();
        let mut hidden_by_0_8_0 = 0;
        for case in &cases {
            if !redact_0_8_0(&rules, case).contains(MARKER) {
                hidden_by_0_8_0 += 1;
                let now = redact(case);
                assert!(!now.contains(MARKER), "{case:?} -> {now:?}");
            }
        }
        eprintln!(
            "differential: {} cases, 0.8.0 hid the marker in {hidden_by_0_8_0}",
            cases.len()
        );
        assert!(
            hidden_by_0_8_0 > 1000,
            "the generator must exercise redaction"
        );
    }

    fn timed(name: &str, text: &str) {
        let started = Instant::now();
        let out = redact(text);
        let took = started.elapsed();
        eprintln!("perf {name}: {} bytes in {took:?}", text.len());
        assert!(!out.is_empty());
        assert!(took < Duration::from_secs(2), "{name} took {took:?}");
    }

    #[test]
    fn linear_on_adversarial_input() {
        const MB: usize = 1 << 20;
        for (name, literal) in [("bearer", BEARER), ("sk", SK), ("telegram", TELEGRAM)] {
            let unit = format!("x{literal}\u{1c}");
            timed(
                &format!("{name}: rejected starts"),
                &(unit.repeat(MB / unit.len()) + "a"),
            );
            let unit = format!("{literal}\u{1c}");
            timed(
                &format!("{name}: chain without whitespace"),
                &(" ".to_owned() + &unit.repeat(MB / unit.len()) + "a"),
            );
        }
        timed(
            "sk: chain of short keys",
            &(" ".to_owned() + &"sk-".repeat(MB / 3) + &"a".repeat(9)),
        );
        timed(
            "sk: one long key with a trailing underscore",
            &(" sk-".to_owned() + &"a".repeat(MB) + "_"),
        );
        timed(
            "telegram: one long token",
            &(TELEGRAM.to_owned() + &"a".repeat(MB)),
        );
    }
}
