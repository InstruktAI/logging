//! Runtime write failures: counted, and reported on stderr at most once a
//! minute, without ever panicking or printing what the line held.

use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// At most one report per this interval.
pub(crate) const REPORT_INTERVAL: Duration = Duration::from_secs(60);

static FAILURES: AtomicU64 = AtomicU64::new(0);
static LIMITER: Mutex<Limiter> = Mutex::new(Limiter::new());

/// How many log lines this process has dropped because they could not be
/// written, since it started (all layers together).
///
/// Initialization failures are not counted: they are an `Err` from
/// [`init`](crate::init) or [`Builder::layer`](crate::Builder::layer).
pub fn failure_count() -> u64 {
    FAILURES.load(Ordering::Relaxed)
}

/// The rate limit: one report per [`REPORT_INTERVAL`], counting the ones it
/// holds back.
#[derive(Debug)]
pub(crate) struct Limiter {
    last: Option<Instant>,
    suppressed: u64,
}

impl Limiter {
    pub(crate) const fn new() -> Self {
        Self {
            last: None,
            suppressed: 0,
        }
    }

    /// `Some(suppressed since the last report)` when a report is due at
    /// `now`, `None` when this one is held back.
    pub(crate) fn admit(&mut self, now: Instant) -> Option<u64> {
        let due = self
            .last
            .is_none_or(|last| now.saturating_duration_since(last) >= REPORT_INTERVAL);
        if due {
            self.last = Some(now);
            Some(std::mem::take(&mut self.suppressed))
        } else {
            self.suppressed += 1;
            None
        }
    }
}

/// The text of `err`'s OS error, without Rust's ` (os error N)` suffix.
fn os_message(code: i32) -> String {
    let text = io::Error::from_raw_os_error(code).to_string();
    match text.rfind(" (os error ") {
        Some(cut) => text[..cut].to_owned(),
        None => text,
    }
}

/// The report line: the error kind and errno, the log path and the counts.
/// It never holds any part of the line that was dropped.
pub(crate) fn message(path: &Path, err: &io::Error, failures: u64, suppressed: u64) -> String {
    let errno = match err.raw_os_error() {
        Some(code) => format!("errno={code} ({})", os_message(code)),
        None => "errno=none".to_owned(),
    };
    format!(
        "instrukt-ai-logging: dropped a log line: cannot write {}: {:?} {errno}; \
         {failures} dropped since start, {suppressed} reports suppressed\n",
        path.display(),
        err.kind(),
    )
}

/// Count a line that could not be written and report it on stderr, rate
/// limited. A broken stderr is ignored: logging never panics.
pub(crate) fn write_failed(path: &Path, err: &io::Error) {
    let failures = FAILURES.fetch_add(1, Ordering::Relaxed).saturating_add(1);
    let admitted = LIMITER
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .admit(Instant::now());
    if let Some(suppressed) = admitted {
        let _ = io::stderr().write_all(message(path, err, failures, suppressed).as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_report_per_interval_counting_the_ones_held_back() {
        let t0 = Instant::now();
        let mut l = Limiter::new();
        assert_eq!(l.admit(t0), Some(0));
        assert_eq!(l.admit(t0 + Duration::from_secs(1)), None);
        assert_eq!(l.admit(t0 + Duration::from_secs(59)), None);
        assert_eq!(l.admit(t0 + Duration::from_secs(60)), Some(2));
        assert_eq!(l.admit(t0 + Duration::from_secs(61)), None);
        assert_eq!(l.admit(t0 + Duration::from_secs(200)), Some(1));
        assert_eq!(l.admit(t0 + Duration::from_secs(260)), Some(0));
    }

    #[test]
    fn the_message_names_kind_errno_path_and_counts() {
        let err = io::Error::from_raw_os_error(libc::ENOSPC);
        let text = message(Path::new("/s/app.log"), &err, 7, 3);
        assert_eq!(
            text,
            format!(
                "instrukt-ai-logging: dropped a log line: cannot write /s/app.log: StorageFull errno={} (No space left on device); 7 dropped since start, 3 reports suppressed\n",
                libc::ENOSPC
            )
        );
        let short = io::Error::new(io::ErrorKind::WriteZero, "short write: 1 of 2 bytes");
        assert!(
            message(Path::new("/x"), &short, 1, 0).contains(": WriteZero errno=none; 1 dropped")
        );
    }
}
