//! InstruktAI logging contract for Rust.
//!
//! A [`tracing`] layer that writes single-line logfmt to the shared InstruktAI
//! log location, following the same contract (`docs/contract.md` in the
//! repository) as the Python writer `instrukt_ai_logging`, line for line:
//!
//! ```text
//! 2026-11-05T09:30:12.345Z level=INFO logger=teleclaude::jobs msg="job_started" attempt=3 job_id=abc123
//! ```
//!
//! # Usage
//!
//! ```no_run
//! fn main() -> Result<(), instrukt_ai_logging::Error> {
//!     // Logs to $XDG_STATE_HOME/instrukt-ai/teleclaude/teleclaude.log
//!     // (~/.local/state when XDG_STATE_HOME is unset).
//!     let path = instrukt_ai_logging::init("teleclaude", None)?;
//!     tracing::info!(job_id = "abc123", attempt = 3, "job_started");
//!     # let _ = path;
//!     Ok(())
//! }
//! ```
//!
//! [`init`] installs the global subscriber and bridges the `log` crate, so
//! third-party crates that use `log` go through the same selection. Use the
//! [`Builder`] for a `source` (a separate `<source>.log` in the same
//! directory), `max_message_chars`, extra app prefixes, or to get a
//! [`Layer`](tracing_subscriber::Layer) for a subscriber you compose yourself.
//!
//! # Selection
//!
//! Read once, at build time, from the environment (`APP` is the app name
//! upper-cased, runs of other characters turned into `_`):
//!
//! - `{APP}_LOG_LEVEL` — the app's own targets (default `INFO`).
//! - `{APP}_THIRD_PARTY_LOG_LEVEL` — third-party targets (default `WARNING`).
//! - `{APP}_THIRD_PARTY_LOGGERS` — comma-separated spotlight prefixes. When
//!   set, only these third-party prefixes get the third-party level; every
//!   other third-party target passes at `WARNING` and above.
//! - `{APP}_MUTED_LOGGERS` — comma-separated prefixes forced to `WARNING`+.
//!
//! The most specific (longest) matching prefix decides; a muted prefix wins a
//! tie. Prefixes match on `.` and `::` boundaries, so `hyper` covers
//! `hyper::client`. The app's own prefix is the app name lower-cased with runs
//! of other characters turned into `_` (`my-app` → `my_app`); add sibling crates
//! with [`Builder::app_prefix`]. Level names are case-insensitive in ASCII
//! (values are trimmed of ASCII whitespace only, see `docs/contract.md`): `TRACE`,
//! `DEBUG`, `INFO`, `WARN`/`WARNING`, `ERROR`, `CRITICAL`/`FATAL` (nothing
//! tracing can emit passes), `NOTSET` (the prefix has no level of its own:
//! app targets fall back to the root threshold, and a `NOTSET` third-party
//! level without spotlight lets everything through). An unset, empty or
//! ASCII-whitespace-only value means the default, silently, and so does a
//! value that is not valid UTF-8. Any other value means
//! the default too, and building the layer writes one `WARNING` line per such
//! variable to the file, before anything else and past every filter:
//!
//! ```text
//! <ts> level=WARNING logger=instrukt_ai_logging msg="unknown log level; using the default" default=INFO value=deubg variable=TELECLAUDE_LOG_LEVEL
//! ```
//!
//! # Line format
//!
//! `<UTC ms timestamp> level=<L> logger=<target> msg="<message>"` followed by
//! the event's fields sorted by key. `WARN` is written as `WARNING`. Strings
//! have secrets redacted (Bearer tokens, `sk-` keys, Telegram bot tokens), are
//! cut at `max_message_chars` characters, and are quoted unless they consist
//! only of `A-Za-z0-9._/:+-`. Floats are written like Python's `repr`.
//!
//! Only **event** fields are written; span fields and span context are not.
//!
//! # Files and rotation
//!
//! The directory and file are created at build time; failure is an `Err`
//! (logging never starts degraded). Before each write the layer checks that the
//! path still refers to the open file and reopens it otherwise, so external
//! rotation (newsyslog, logrotate, configured by `instrukt-ai-log-setup`)
//! works. This crate never configures rotation itself.

mod env;
mod error;
mod format;
mod layer;
mod names;
mod paths;
mod redact;
mod select;
mod writer;

use std::path::PathBuf;

pub use error::Error;
pub use format::DEFAULT_MAX_MESSAGE_CHARS;
pub use layer::Builder;
pub use paths::{resolve_log_dir, resolve_log_file, resolve_log_root};

/// Initialize logging for `app`: open the log file, install the global
/// subscriber, bridge the `log` crate, and return the log file path.
///
/// `source` selects `<source>.log` instead of `<app>.log`. A second call, or
/// a global subscriber or `log` logger already set elsewhere, is an `Err`.
pub fn init(app: &str, source: Option<&str>) -> Result<PathBuf, Error> {
    let mut builder = Builder::new(app);
    if let Some(source) = source {
        builder = builder.source(source);
    }
    builder.init()
}
