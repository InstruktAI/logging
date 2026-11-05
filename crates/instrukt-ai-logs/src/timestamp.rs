//! Log timestamps and `--since` durations.
//!
//! Instants are UTC microseconds since the Unix epoch, the precision Python's
//! `datetime` carries, so comparisons decide exactly as today's tool does.

use std::time::SystemTime;

use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time};

use crate::pyfmt::{is_py_space, py_strip};

/// UTC microseconds since the Unix epoch.
pub type Micros = i64;

const MICROS_PER_SECOND: i64 = 1_000_000;

/// The current instant.
pub fn now() -> Micros {
    from_system_time(SystemTime::now())
}

/// A [`SystemTime`] (e.g. a file mtime) as [`Micros`], saturating at the ends.
pub fn from_system_time(t: SystemTime) -> Micros {
    match t.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_micros()).unwrap_or(i64::MAX),
        Err(e) => i64::try_from(e.duration().as_micros()).map_or(i64::MIN, |m| -m),
    }
}

/// Parse a `--since` duration like `30s`, `10m`, `2h`, `1d` into microseconds.
///
/// Messages are today's `parse_since` messages. Durations too large to
/// represent saturate (the window then covers everything).
pub fn parse_since(value: &str) -> Result<Micros, String> {
    let raw = py_strip(value).to_lowercase();
    let Some(unit) = raw.chars().last() else {
        return Err("Empty duration".to_string());
    };
    let number = &raw[..raw.len() - unit.len_utf8()];
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("Invalid duration: {value}"));
    }
    let n: i64 = number.parse().unwrap_or(i64::MAX);
    let seconds_per_unit: i64 = match unit {
        's' => 1,
        'm' => 60,
        'h' => 3600,
        'd' => 86_400,
        other => return Err(format!("Invalid duration unit: {other}")),
    };
    Ok(n.saturating_mul(seconds_per_unit)
        .saturating_mul(MICROS_PER_SECOND))
}

fn digits(s: &[u8]) -> Option<u32> {
    if s.is_empty() || !s.iter().all(u8::is_ascii_digit) {
        return None;
    }
    s.iter().try_fold(0u32, |acc, b| {
        acc.checked_mul(10)?.checked_add(u32::from(b - b'0'))
    })
}

/// Parse the timestamp at the start of a log line.
///
/// The token is the text before the first space, stripped of whitespace, and
/// must end with `Z`. Accepted shapes are the ones Python's
/// `datetime.fromisoformat` takes for such a token in practice:
/// `YYYY-MM-DD<sep>HH[:MM[:SS[(.|,)fraction]]]Z`, where `<sep>` is any single
/// character and the fraction is truncated to microseconds.
pub fn parse_log_timestamp(line: &str) -> Option<Micros> {
    let token = line.split(' ').next().unwrap_or("");
    let token = token.trim_matches(is_py_space);
    let body = token.strip_suffix('Z')?;
    let b = body.as_bytes();
    if b.len() < 13 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let year = i32::try_from(digits(&b[0..4])?).ok()?;
    let month = u8::try_from(digits(&b[5..7])?).ok()?;
    let day = u8::try_from(digits(&b[8..10])?).ok()?;
    // The separator is one character, possibly multi-byte.
    let sep_len = body[10..].chars().next()?.len_utf8();
    let t = &b[10 + sep_len..];
    let (hour, rest) = (digits(t.get(0..2)?)?, &t[2..]);
    let (minute, rest) = match rest {
        [] => (0, rest),
        [b':', m1, m2, tail @ ..] => (digits(&[*m1, *m2])?, tail),
        _ => return None,
    };
    let (second, rest) = match rest {
        [] => (0, rest),
        [b':', s1, s2, tail @ ..] => (digits(&[*s1, *s2])?, tail),
        _ => return None,
    };
    let micro = match rest {
        [] => 0,
        [b'.' | b',', frac @ ..] => {
            if !frac.iter().all(u8::is_ascii_digit) {
                return None;
            }
            let mut v: u32 = 0;
            for i in 0..6 {
                v = v * 10 + frac.get(i).map_or(0, |d| u32::from(d - b'0'));
            }
            v
        }
        _ => return None,
    };
    if year < 1 {
        return None;
    }
    let date = Date::from_calendar_date(year, Month::try_from(month).ok()?, day).ok()?;
    let time = Time::from_hms_micro(
        u8::try_from(hour).ok()?,
        u8::try_from(minute).ok()?,
        u8::try_from(second).ok()?,
        micro,
    )
    .ok()?;
    let nanos = PrimitiveDateTime::new(date, time)
        .assume_utc()
        .unix_timestamp_nanos();
    i64::try_from(nanos / 1000).ok()
}

/// Format an instant the way log lines carry it: `YYYY-MM-DDTHH:MM:SS.mmmZ`
/// with milliseconds truncated.
pub fn format_timestamp(ts: Micros) -> String {
    let nanos = i128::from(ts) * 1000;
    match OffsetDateTime::from_unix_timestamp_nanos(nanos) {
        Ok(dt) => format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
            dt.year(),
            u8::from(dt.month()),
            dt.day(),
            dt.hour(),
            dt.minute(),
            dt.second(),
            dt.microsecond() / 1000
        ),
        Err(_) => ts.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> Option<String> {
        parse_log_timestamp(s).map(|m| {
            let dt = OffsetDateTime::from_unix_timestamp_nanos(i128::from(m) * 1000).unwrap();
            format!("{dt}")
        })
    }

    #[test]
    fn parses_the_standard_shape() {
        let m = parse_log_timestamp("2025-01-02T03:04:05.678Z level=INFO msg=\"x\"\n").unwrap();
        assert_eq!(format_timestamp(m), "2025-01-02T03:04:05.678Z");
        assert_eq!(m % 1_000_000, 678_000);
    }

    #[test]
    fn accepts_what_python_accepts() {
        // Each of these was checked against Python 3.12's parse_log_timestamp.
        assert_eq!(
            parse_log_timestamp("2025-01-01T00:00:00.1234567Z").map(|m| m % 1_000_000),
            Some(123_456)
        );
        assert_eq!(
            parse_log_timestamp("2025-01-01T00:00:00.1Z").map(|m| m % 1_000_000),
            Some(100_000)
        );
        assert!(ts("2025-01-01T00:00Z").is_some());
        assert!(ts("2025-01-01T00Z").is_some());
        assert!(ts("2025-01-01T00:00:00,123Z").is_some());
        assert!(ts("2025-01-01X00:00:00Z").is_some());
        assert!(ts("2025-01-01T00:00:00.Z").is_some());
        assert!(ts("0001-01-01T00:00:00Z").is_some());
        assert!(ts("2025-01-01T00:00:00Z\n").is_some());
    }

    #[test]
    fn rejects_what_python_rejects() {
        for bad in [
            "2025-01-01T24:00:00Z",
            "2025-02-30T00:00:00Z",
            "\t2025-01-01T00:00:00.000Z\tx y",
            "2025-01-01T00:00:00.000z",
            "2025-01-01T00:00:00.000ZZ",
            " 2025-01-01T00:00:00Z",
            "continuation line",
            "",
            "Z",
            "2025-01-01T00:0Z",
            "0000-01-01T00:00:00Z",
        ] {
            assert_eq!(parse_log_timestamp(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn since_parsing() {
        assert_eq!(parse_since("30s"), Ok(30_000_000));
        assert_eq!(parse_since("10m"), Ok(600_000_000));
        assert_eq!(parse_since(" 2H "), Ok(7_200_000_000));
        assert_eq!(parse_since("1d"), Ok(86_400_000_000));
        assert_eq!(parse_since("0m"), Ok(0));
        assert_eq!(parse_since(""), Err("Empty duration".to_string()));
        assert_eq!(parse_since("  "), Err("Empty duration".to_string()));
        assert_eq!(parse_since("m"), Err("Invalid duration: m".to_string()));
        assert_eq!(parse_since("-5m"), Err("Invalid duration: -5m".to_string()));
        assert_eq!(
            parse_since("1.5h"),
            Err("Invalid duration: 1.5h".to_string())
        );
        assert_eq!(
            parse_since("10x"),
            Err("Invalid duration unit: x".to_string())
        );
        assert_eq!(
            parse_since("10W"),
            Err("Invalid duration unit: w".to_string())
        );
        assert_eq!(parse_since("99999999999999999999999d"), Ok(i64::MAX));
    }
}
