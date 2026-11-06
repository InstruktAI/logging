//! The `tracing` layer, its per-layer filter, and the [`Builder`].

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::{self, Write as _};
use std::path::PathBuf;

use time::OffsetDateTime;
use tracing::field::{Field, Visit};
use tracing::level_filters::LevelFilter;
use tracing::subscriber::Interest;
use tracing::{Event, Metadata, Subscriber};
use tracing_log::NormalizeEvent;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::{Context, Filter, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

use crate::env::EnvLookup;
use crate::format::{self, DEFAULT_MAX_MESSAGE_CHARS, Value};
use crate::select::{self, Selection, UnknownLevel};
use crate::writer::WatchedFile;
use crate::{Error, names, paths, report};

/// Fields the `log` bridge adds to every bridged record; they describe the
/// record (target, location) and are not user data.
const LOG_BRIDGE_FIELDS: [&str; 4] = ["log.target", "log.module_path", "log.file", "log.line"];

/// The `logger=` of the lines this crate writes about its own configuration;
/// the same string as the Python writer's module name.
const SELF_LOGGER: &str = "instrukt_ai_logging";

/// Write one `WARNING` line per unknown level value straight to `file`,
/// bypassing selection: it reports on the configuration of selection itself.
fn warn_unknown_levels(
    file: &WatchedFile,
    unknown: &[UnknownLevel],
    max_message_chars: usize,
) -> Result<(), Error> {
    for u in unknown {
        let fields = BTreeMap::from([
            ("default", Value::text(u.default, max_message_chars)),
            ("value", Value::text(&u.value, max_message_chars)),
            ("variable", Value::text(&u.variable, max_message_chars)),
        ]);
        let line = format::line(
            &format::timestamp(OffsetDateTime::now_utc()),
            select::level_name(tracing::Level::WARN),
            SELF_LOGGER,
            "unknown log level; using the default",
            &fields,
            max_message_chars,
        );
        file.write_line(&line).map_err(|source| Error::WriteFile {
            path: file.path().to_path_buf(),
            source,
        })?;
    }
    Ok(())
}

/// A `fmt::Write` sink that holds at most [`format::CAPTURE_CHARS`]
/// characters of a `Debug` or `Display` value.
///
/// Once full it keeps accepting text and discards it: it never returns
/// `fmt::Error`, because a caller's `Debug` impl that unwraps its write
/// results would then panic inside a log call, and a logging call must never
/// crash the host. A huge value's impl therefore still runs to the end, but
/// costs at most [`format::CAPTURE_CHARS`] characters of memory: the sink
/// counts characters, not bytes, and never reserves capacity from the size of
/// what it is given.
struct Capture {
    text: String,
    /// Characters it still accepts.
    room: usize,
}

impl Capture {
    fn new() -> Self {
        Self {
            text: String::new(),
            room: format::CAPTURE_CHARS,
        }
    }

    fn debug(value: &dyn fmt::Debug) -> String {
        let mut sink = Self::new();
        // The sink never fails; an error comes from the impl itself, and what
        // the sink holds is the captured text either way.
        let _ = write!(sink, "{value:?}");
        sink.text
    }

    fn display(value: &dyn fmt::Display) -> String {
        let mut sink = Self::new();
        let _ = write!(sink, "{value}");
        sink.text
    }
}

impl fmt::Write for Capture {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if self.room == 0 {
            return Ok(());
        }
        // A character takes at least one byte, so text this short fits.
        if s.len() <= self.room {
            self.text.push_str(s);
            self.room -= s.chars().count();
            return Ok(());
        }
        match s.char_indices().nth(self.room) {
            Some((idx, _)) => {
                self.text.push_str(s.get(..idx).unwrap_or_default());
                self.room = 0;
                Ok(())
            }
            None => {
                self.room -= s.chars().count();
                self.text.push_str(s);
                Ok(())
            }
        }
    }
}

/// The per-layer filter: decides by target and level only.
#[derive(Debug)]
pub(crate) struct SelectionFilter(Selection);

impl SelectionFilter {
    fn decide(&self, meta: &Metadata<'_>) -> bool {
        self.0.enabled(meta.target(), *meta.level())
    }
}

impl<S> Filter<S> for SelectionFilter {
    fn enabled(&self, meta: &Metadata<'_>, _cx: &Context<'_, S>) -> bool {
        // Bridged `log` records arrive here with their real target.
        self.decide(meta)
    }

    fn callsite_enabled(&self, meta: &'static Metadata<'static>) -> Interest {
        // The `log` bridge shares a few static callsites (target `log`) across
        // every record; decide those per event. Everything else has a fixed
        // target, so the decision can be cached on the callsite.
        if meta.fields().field("log.target").is_some() {
            Interest::sometimes()
        } else if self.decide(meta) {
            Interest::always()
        } else {
            Interest::never()
        }
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(self.0.max_level())
    }
}

/// Collects the message and the extra fields of one event.
///
/// The message and every string value are captured up to
/// [`format::CAPTURE_CHARS`] characters, whatever `max_message_chars` is.
/// String values are redacted and truncated as they are recorded, so a field
/// holds at most `max_message_chars` characters. Capture is bounded by the
/// line limit as well: once the fields held so far certainly make the line
/// longer than [`format::MAX_LINE_BYTES`], every field at or after the first
/// one that certainly cannot be kept (in sorted order) is held only as
/// [`Value::Dropped`], and its value is never formatted.
struct Collector {
    message: Option<String>,
    fields: BTreeMap<&'static str, Value>,
    max_message_chars: usize,
    skip_bridge_fields: bool,
    /// The sum of the held values' [`Value::field_lower_bound`]s.
    lower: usize,
    /// The line certainly exceeds the limit.
    overflowing: bool,
    /// The first key, in sorted order, that certainly cannot be kept.
    cutoff: Option<&'static str>,
}

impl Collector {
    fn new(max_message_chars: usize, skip_bridge_fields: bool) -> Self {
        Self {
            message: None,
            fields: BTreeMap::new(),
            max_message_chars,
            skip_bridge_fields,
            lower: 0,
            overflowing: false,
            cutoff: None,
        }
    }

    /// Whether field `name` is held at all: `message` is the message, bridge
    /// metadata and keys that are never written are skipped.
    fn wanted(&self, name: &str) -> bool {
        !(self.skip_bridge_fields && LOG_BRIDGE_FIELDS.contains(&name))
            && format::is_field_key(name)
    }

    fn put(&mut self, field: &Field, value: impl FnOnce(usize) -> Value) {
        let name = field.name();
        if !self.wanted(name) {
            return;
        }
        if self.cutoff.is_some_and(|cutoff| name >= cutoff) {
            self.insert(name, Value::Dropped);
            return;
        }
        let value = value(self.max_message_chars);
        self.lower += value.field_lower_bound(name);
        self.insert(name, value);
        let lower = format::HEAD_LOWER_BOUND + self.lower;
        if lower > format::MAX_LINE_BYTES
            || (self.overflowing && lower + format::MIN_MARKER_BYTES > format::MAX_LINE_BYTES)
        {
            self.overflowing = true;
            self.cut_off();
        }
    }

    fn insert(&mut self, name: &'static str, value: Value) {
        if let Some(old) = self.fields.insert(name, value) {
            self.lower -= old.field_lower_bound(name);
        }
    }

    /// The line certainly overflows, so a field is kept only when the head,
    /// the fields before it, itself and the marker fit. Drop every field from
    /// the first one whose lower bounds already rule that out.
    fn cut_off(&mut self) {
        let mut sum = format::HEAD_LOWER_BOUND;
        let mut cutoff = None;
        for (key, value) in &mut self.fields {
            let bound = value.field_lower_bound(key);
            if cutoff.is_none() {
                sum += bound;
                if sum + format::MIN_MARKER_BYTES <= format::MAX_LINE_BYTES {
                    continue;
                }
                cutoff = Some(*key);
            }
            self.lower -= bound;
            *value = Value::Dropped;
        }
        if let Some(cutoff) = cutoff {
            self.cutoff = Some(self.cutoff.map_or(cutoff, |old| old.min(cutoff)));
        }
    }

    fn message(&mut self, field: &Field, text: impl FnOnce() -> String) -> bool {
        if field.name() == "message" {
            self.message = Some(text());
            return true;
        }
        false
    }
}

impl Visit for Collector {
    fn record_f64(&mut self, field: &Field, value: f64) {
        if !self.message(field, || format::python_float_repr(value)) {
            self.put(field, |_| Value::float(value));
        }
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        if !self.message(field, || value.to_string()) {
            self.put(field, |_| Value::int(value));
        }
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        if !self.message(field, || value.to_string()) {
            self.put(field, |_| Value::int(value));
        }
    }
    fn record_i128(&mut self, field: &Field, value: i128) {
        if !self.message(field, || value.to_string()) {
            self.put(field, |_| Value::int(value));
        }
    }
    fn record_u128(&mut self, field: &Field, value: u128) {
        if !self.message(field, || value.to_string()) {
            self.put(field, |_| Value::int(value));
        }
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        if !self.message(field, || value.to_string()) {
            self.put(field, |_| Value::bool(value));
        }
    }
    // Text is captured up to `format::CAPTURE_CHARS` characters only: a
    // borrowed `&str` is cut before it is copied, a `Debug` or `Display` value
    // is written into a bounded `Capture`.
    fn record_str(&mut self, field: &Field, value: &str) {
        if !self.message(field, || format::capture_cut(value).to_owned()) {
            self.put(field, |max| Value::text(value, max));
        }
    }
    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        if !self.message(field, || Capture::display(value)) {
            self.put(field, |max| Value::text_owned(Capture::display(value), max));
        }
    }
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if !self.message(field, || Capture::debug(value)) {
            self.put(field, |max| Value::text_owned(Capture::debug(value), max));
        }
    }
}

/// Formats and writes events. Only reached for events the filter let through.
#[derive(Debug)]
pub(crate) struct WriterLayer {
    file: WatchedFile,
    max_message_chars: usize,
}

impl<S: Subscriber> Layer<S> for WriterLayer {
    fn on_event(&self, event: &Event<'_>, _cx: Context<'_, S>) {
        let normalized = event.normalized_metadata();
        let meta = normalized.as_ref().unwrap_or_else(|| event.metadata());
        let mut collector = Collector::new(self.max_message_chars, normalized.is_some());
        event.record(&mut collector);
        let line = format::line(
            &format::timestamp(OffsetDateTime::now_utc()),
            select::level_name(*meta.level()),
            meta.target(),
            collector.message.as_deref().unwrap_or(""),
            &collector.fields,
            self.max_message_chars,
        );
        if let Err(err) = self.file.write_line(&line) {
            // The line is dropped; the log file is the failing part, so the
            // report goes to stderr, rate limited and never panicking.
            report::write_failed(self.file.path(), &err);
        }
    }
}

/// Configures and builds the logging layer, for the knobs [`init`](crate::init)
/// does not take.
///
/// ```no_run
/// let path = instrukt_ai_logging::Builder::new("instrukt")
///     .source("cron")
///     .app_prefix("instrukt_cli")
///     .app_prefix("instrukt_core")
///     .max_message_chars(2000)
///     .init()?;
/// tracing::info!(path = %path.display(), "logging_ready");
/// # Ok::<(), instrukt_ai_logging::Error>(())
/// ```
#[derive(Debug)]
pub struct Builder {
    app: String,
    source: Option<String>,
    max_message_chars: usize,
    app_prefixes: Vec<String>,
    env: EnvLookup,
}

impl Builder {
    /// Start a builder for `app` (normalized into the env prefix, the app
    /// logger prefix and the directory name).
    pub fn new(app: impl Into<String>) -> Self {
        Self {
            app: app.into(),
            source: None,
            max_message_chars: DEFAULT_MAX_MESSAGE_CHARS,
            app_prefixes: Vec::new(),
            env: EnvLookup::process(),
        }
    }

    /// Write to `<source>.log` instead of `<app>.log` (bare stem; `.log` is
    /// appended). An empty source means the default. A source that is not a
    /// bare stem (`^[A-Za-z0-9][A-Za-z0-9._-]*$`, no `..`) makes building fail
    /// with [`Error::InvalidSource`].
    #[must_use]
    pub fn source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// Cut the message and every string value at this many characters
    /// (default 4000); `0` disables truncation.
    #[must_use]
    pub fn max_message_chars(mut self, max: usize) -> Self {
        self.max_message_chars = max;
        self
    }

    /// Treat targets under `prefix` (separator `.` or `::`) as the app's own
    /// loggers, governed by `{APP}_LOG_LEVEL`. The normalized app name is
    /// always an app prefix; use this for sibling crates such as
    /// `instrukt_cli` under app `instrukt`.
    #[must_use]
    pub fn app_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.app_prefixes.push(prefix.into());
        self
    }

    /// Read environment variables (the four contract variables,
    /// `XDG_STATE_HOME` and `HOME`) through `lookup` instead of the process
    /// environment. Meant for tests and embedding hosts.
    #[must_use]
    pub fn env_lookup<F>(mut self, lookup: F) -> Self
    where
        F: Fn(&str) -> Option<OsString> + Send + Sync + 'static,
    {
        self.env = EnvLookup::new(lookup);
        self
    }

    fn build(self) -> Result<(WriterLayer, SelectionFilter, PathBuf), Error> {
        let env_prefix = names::env_prefix(&self.app)?;
        let mut app_prefixes = vec![names::logger_prefix(&self.app)?];
        app_prefixes.extend(self.app_prefixes.into_iter().filter(|p| !p.is_empty()));
        let (selection, unknown) = Selection::from_env(&env_prefix, &app_prefixes, &self.env);
        let path = paths::log_file_with(&self.app, self.source.as_deref(), &self.env)?;
        let file = WatchedFile::open(path.clone())?;
        warn_unknown_levels(&file, &unknown, self.max_message_chars)?;
        let layer = WriterLayer {
            file,
            max_message_chars: self.max_message_chars,
        };
        Ok((layer, SelectionFilter(selection), path))
    }

    /// Build the layer (already carrying its per-layer filter) for a
    /// subscriber you compose yourself, plus the log file path.
    ///
    /// Opens (and creates) the log file now: directories `0700`, the file
    /// `0600`, never through a symbolic link. An unwritable location or a
    /// symbolic link at the log path is an `Err`. An unknown level value is reported there right away, as one
    /// `WARNING` line per variable that no filter can drop. The filter only affects this layer, so other layers on the same
    /// registry still see every event. Does not bridge the `log` crate.
    pub fn layer<S>(self) -> Result<(impl Layer<S> + Send + Sync + 'static, PathBuf), Error>
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        let (layer, filter, path) = self.build()?;
        Ok((layer.with_filter(filter), path))
    }

    /// Install the layer as the global default subscriber, bridge the `log`
    /// crate into it, and return the log file path.
    ///
    /// Fails, without panicking, when a global subscriber or a `log` logger is
    /// already set.
    pub fn init(self) -> Result<PathBuf, Error> {
        if tracing::dispatcher::has_been_set() {
            return Err(Error::AlreadyInitialized);
        }
        let (layer, filter, path) = self.build()?;
        let max = filter.0.max_level();
        let subscriber = tracing_subscriber::registry().with(layer.with_filter(filter));
        tracing_log::LogTracer::builder()
            .with_max_level(level_filter_to_log(max))
            .init()
            .map_err(|e| Error::LogBridge(e.to_string()))?;
        tracing::subscriber::set_global_default(subscriber)
            .map_err(|_| Error::AlreadyInitialized)?;
        Ok(path)
    }
}

fn level_filter_to_log(max: LevelFilter) -> tracing_log::log::LevelFilter {
    use tracing_log::log::LevelFilter as L;
    match max.into_level() {
        None => L::Off,
        Some(tracing::Level::ERROR) => L::Error,
        Some(tracing::Level::WARN) => L::Warn,
        Some(tracing::Level::INFO) => L::Info,
        Some(tracing::Level::DEBUG) => L::Debug,
        Some(tracing::Level::TRACE) => L::Trace,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn builder(state: &Path, app: &str, env: &[(&str, &str)]) -> Builder {
        let mut owned: Vec<(String, OsString)> = env
            .iter()
            .map(|(k, v)| ((*k).to_owned(), OsString::from(v)))
            .collect();
        owned.push(("XDG_STATE_HOME".to_owned(), state.as_os_str().to_owned()));
        Builder::new(app)
            .env_lookup(move |key| owned.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()))
    }

    /// Run `f` with only this layer installed (thread-local), return the lines
    /// without their timestamps.
    fn capture(b: Builder, f: impl FnOnce()) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        let (layer, path) = b.layer()?;
        tracing::subscriber::with_default(tracing_subscriber::registry().with(layer), f);
        let content = fs::read_to_string(path)?;
        Ok(content
            .lines()
            .map(|l| l.split_once(' ').map_or(l, |(_, rest)| rest).to_owned())
            .collect())
    }

    #[test]
    fn macro_event_with_literal_target() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let lines = capture(builder(tmp.path(), "teleclaude", &[]), || {
            tracing::info!(target: "teleclaude.core", job_id = "abc123", attempt = 3, "job_started");
            tracing::debug!(target: "teleclaude.core", "hidden");
            tracing::info!(target: "httpx", "hidden");
            tracing::warn!(target: "httpx", ratio = 0.5, ok = true, "slow {}", "call");
        })?;
        assert_eq!(
            lines,
            [
                "level=INFO logger=teleclaude.core msg=\"job_started\" attempt=3 job_id=abc123",
                "level=WARNING logger=httpx msg=\"slow call\" ok=true ratio=0.5",
            ]
        );
        Ok(())
    }

    #[test]
    fn module_path_targets_use_double_colon() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let lines = capture(
            builder(
                tmp.path(),
                "teleclaude",
                &[("TELECLAUDE_LOG_LEVEL", "trace")],
            ),
            || {
                tracing::trace!(target: "teleclaude::jobs::runner", n = 1u64, "tick");
                tracing::info!(target: "hyper::client", "hidden");
                tracing::error!(target: "hyper::client", err = %"boom", "failed");
            },
        )?;
        assert_eq!(
            lines,
            [
                "level=TRACE logger=teleclaude::jobs::runner msg=\"tick\" n=1",
                "level=ERROR logger=hyper::client msg=\"failed\" err=boom",
            ]
        );
        Ok(())
    }

    #[test]
    fn debug_display_and_error_values_are_strings() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let err = std::io::Error::other("disk full");
        let lines = capture(builder(tmp.path(), "app", &[]), || {
            tracing::info!(
                target: "app",
                list = ?vec![1, 2],
                shown = %"two words",
                big = u128::MAX,
                neg = -5i128,
                e = &err as &(dyn std::error::Error + 'static),
                "m"
            );
        })?;
        assert_eq!(
            lines,
            [concat!(
                "level=INFO logger=app msg=\"m\" big=340282366920938463463374607431768211455 ",
                "e=\"disk full\" list=\"[1, 2]\" neg=-5 shown=\"two words\""
            )]
        );
        Ok(())
    }

    /// A value whose `Debug` must never run: formatting it fails the test.
    struct NeverFormatted;
    impl fmt::Debug for NeverFormatted {
        fn fmt(&self, _f: &mut fmt::Formatter<'_>) -> fmt::Result {
            panic!("a field past the line limit's cutoff was formatted")
        }
    }

    #[test]
    fn capture_stops_formatting_fields_that_cannot_fit() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let big = "x".repeat(4000);
        let lines = capture(builder(tmp.path(), "app", &[]), || {
            tracing::info!(
                target: "app",
                a = big.as_str(),
                b = big.as_str(),
                c = big.as_str(),
                d = big.as_str(),
                e = big.as_str(),
                z = ?NeverFormatted,
                "m"
            );
        })?;
        let line = lines.first().ok_or("no line")?;
        let kept = format!(" a={big} b={big} c={big} d={big}");
        assert_eq!(
            *line,
            format!("level=INFO logger=app msg=\"m\"{kept} fields_dropped=2")
        );
        Ok(())
    }

    const MIB: usize = 1 << 20;

    /// `Debug` text of `kib` KiB written 1 KiB per `write_str`, counting the
    /// writes the formatter accepted.
    struct Huge {
        kib: usize,
        accepted: std::cell::Cell<usize>,
    }

    impl Huge {
        fn new(bytes: usize) -> Self {
            Self {
                kib: bytes / 1024,
                accepted: std::cell::Cell::new(0),
            }
        }
    }

    impl fmt::Debug for Huge {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            let chunk = "x".repeat(1024);
            for _ in 0..self.kib {
                f.write_str(&chunk)?;
                self.accepted.set(self.accepted.get() + 1);
            }
            Ok(())
        }
    }

    #[test]
    fn capture_stops_a_huge_debug_value_at_the_capture_limit() {
        let huge = Huge::new(8 * MIB);
        let text = Capture::debug(&huge);
        assert_eq!(text.chars().count(), format::CAPTURE_CHARS);
        // The impl ran to the end (the sink never fails it), but only the
        // capture limit was kept.
        assert_eq!(huge.accepted.get(), 8 * 1024, "KiB written");
        assert!(
            text.capacity() <= 2 * format::CAPTURE_CHARS,
            "{}",
            text.capacity()
        );
    }

    #[test]
    fn capture_counts_characters_not_bytes() {
        // One write far over the limit, cut inside it on a character boundary.
        let text = Capture::display(&"é".repeat(2 * format::CAPTURE_CHARS));
        assert_eq!(text.chars().count(), format::CAPTURE_CHARS);
        assert_eq!(text.len(), 2 * format::CAPTURE_CHARS);
        // Many small writes reach exactly the limit too.
        let mut sink = Capture::new();
        for _ in 0..format::CAPTURE_CHARS {
            assert!(sink.write_str("ab").is_ok());
        }
        assert_eq!(sink.text.chars().count(), format::CAPTURE_CHARS);
        // Full: text is discarded, never refused.
        assert!(sink.write_str("c").is_ok());
        assert_eq!(sink.text.chars().count(), format::CAPTURE_CHARS);
    }

    /// A `Debug` impl that unwraps its write results must not panic once the
    /// sink is full: a log call never crashes the host.
    #[test]
    fn a_debug_impl_that_unwraps_writes_does_not_panic_when_the_sink_is_full() {
        struct Unwraps;
        impl fmt::Debug for Unwraps {
            #[allow(clippy::unwrap_used)]
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                for _ in 0..(2 * format::CAPTURE_CHARS) {
                    f.write_str("x").unwrap();
                }
                Ok(())
            }
        }
        let text = Capture::debug(&Unwraps);
        assert_eq!(text.chars().count(), format::CAPTURE_CHARS);
    }

    /// The message an event's `Collector` holds: its length in characters and
    /// the bytes it reserved.
    fn captured_messages(max_message_chars: usize, f: impl FnOnce()) -> Vec<(usize, usize)> {
        use std::sync::{Arc, Mutex};

        struct Inspect(Arc<Mutex<Vec<(usize, usize)>>>, usize);
        impl<S: Subscriber> Layer<S> for Inspect {
            fn on_event(&self, e: &Event<'_>, _cx: Context<'_, S>) {
                let mut c = Collector::new(self.1, false);
                e.record(&mut c);
                if let Some(m) = c.message {
                    self.0
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push((m.chars().count(), m.capacity()));
                }
            }
        }

        let seen = Arc::new(Mutex::new(Vec::new()));
        let layer = Inspect(seen.clone(), max_message_chars);
        tracing::subscriber::with_default(tracing_subscriber::registry().with(layer), f);
        seen.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    #[test]
    fn an_8_mib_message_is_captured_bounded() {
        let big = "m".repeat(8 * MIB);
        let huge = Huge::new(8 * MIB);
        // `max_message_chars = 0`: no per-value truncation, only the cut.
        let seen = captured_messages(0, || {
            tracing::info!(target: "app", message = big.as_str());
            tracing::info!(target: "app", message = ?huge);
            tracing::info!(target: "app", "{}", big);
        });
        assert_eq!(seen.len(), 3);
        for (chars, capacity) in seen {
            assert_eq!(chars, format::CAPTURE_CHARS);
            assert!(capacity <= 2 * format::CAPTURE_CHARS, "{capacity} bytes");
        }
        // The sink never fails the impl, so `huge` may run to the end; memory
        // is what is bounded, asserted above.
    }

    #[test]
    fn huge_values_with_no_truncation_are_bounded_by_the_cut() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let big = "v".repeat(8 * MIB);
        let huge = Huge::new(8 * MIB);
        let b = builder(tmp.path(), "app", &[]).max_message_chars(0);
        let lines = capture(b, || {
            tracing::info!(target: "app", message = big.as_str());
            tracing::info!(target: "app", d = ?huge, s = big.as_str(), "m");
            tracing::info!(target: "app", e = &std::io::Error::other(big.clone()) as &(dyn std::error::Error + 'static), "m");
        })?;
        assert_eq!(lines.len(), 3);
        let first = lines.first().ok_or("no line")?;
        assert!(first.starts_with("level=INFO logger=app msg=\"vvvv"));
        assert!(first.ends_with(&format!("{}\"", format::TRUNCATION_SUFFIX)));
        assert_eq!(
            lines.get(1).map(String::as_str),
            Some("level=INFO logger=app msg=\"m\" fields_dropped=2")
        );
        assert_eq!(
            lines.get(2).map(String::as_str),
            Some("level=INFO logger=app msg=\"m\" fields_dropped=1")
        );
        Ok(())
    }

    #[test]
    fn collector_holds_dropped_fields_only_past_the_cutoff() {
        let mut c = Collector::new(0, false);
        c.fields.insert("a", Value::Text("x".repeat(9000)));
        c.fields.insert("m", Value::Text("y".repeat(9000)));
        c.fields.insert("z", Value::Text("z".to_owned()));
        c.lower = c.fields.iter().map(|(k, v)| v.field_lower_bound(k)).sum();
        c.overflowing = true;
        c.cut_off();
        assert!(matches!(c.fields.get("a"), Some(Value::Text(_))));
        assert_eq!(c.fields.get("m"), Some(&Value::Dropped));
        assert_eq!(c.fields.get("z"), Some(&Value::Dropped));
        assert_eq!(c.cutoff, Some("m"));
        assert_eq!(
            c.lower,
            Value::Text("x".repeat(9000)).field_lower_bound("a")
        );
    }

    #[test]
    fn event_without_message_has_empty_msg() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let lines = capture(builder(tmp.path(), "app", &[]), || {
            tracing::info!(target: "app", k = 1);
        })?;
        assert_eq!(lines, ["level=INFO logger=app msg=\"\" k=1"]);
        Ok(())
    }

    #[test]
    fn app_prefix_extends_the_app_namespace() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let b = builder(tmp.path(), "instrukt", &[("INSTRUKT_LOG_LEVEL", "debug")])
            .app_prefix("instrukt_cli");
        let lines = capture(b, || {
            tracing::debug!(target: "instrukt_cli::commands", "shown");
            tracing::info!(target: "tokio::runtime", "hidden");
        })?;
        assert_eq!(
            lines,
            ["level=DEBUG logger=instrukt_cli::commands msg=\"shown\""]
        );
        Ok(())
    }

    #[test]
    fn source_and_max_message_chars() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let b = builder(tmp.path(), "app", &[])
            .source("cron")
            .max_message_chars(3);
        let (layer, path) = b.layer()?;
        assert_eq!(path, tmp.path().join("instrukt-ai/app/cron.log"));
        tracing::subscriber::with_default(tracing_subscriber::registry().with(layer), || {
            tracing::info!(target: "app", v = "abcdef", "hello");
        });
        let content = fs::read_to_string(path)?;
        assert!(content.ends_with(
            " level=\"INF…(truncated)\" logger=app msg=\"hel…(truncated)\" v=\"abc…(truncated)\"\n"
        ));
        Ok(())
    }

    #[test]
    fn other_layers_still_see_filtered_events() -> TestResult {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct Count(Arc<AtomicUsize>);
        impl<S: Subscriber> Layer<S> for Count {
            fn on_event(&self, _e: &Event<'_>, _cx: Context<'_, S>) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        let tmp = tempfile::tempdir()?;
        let seen = Arc::new(AtomicUsize::new(0));
        let (layer, path) = builder(tmp.path(), "app", &[]).layer()?;
        let sub = tracing_subscriber::registry()
            .with(layer)
            .with(Count(seen.clone()));
        tracing::subscriber::with_default(sub, || {
            tracing::debug!(target: "app", "not in file");
            tracing::info!(target: "app", "in file");
        });
        assert_eq!(seen.load(Ordering::SeqCst), 2);
        assert_eq!(fs::read_to_string(path)?.lines().count(), 1);
        Ok(())
    }

    #[test]
    fn layer_fails_fast_when_the_file_cannot_be_opened() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let blocker = tmp.path().join("blocker");
        fs::write(&blocker, "")?;
        let res = builder(&blocker, "app", &[]).layer::<tracing_subscriber::Registry>();
        assert!(matches!(res, Err(Error::CreateDir { .. })));
        Ok(())
    }

    #[test]
    fn rotation_mid_run_lands_in_a_new_file() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let (layer, path) = builder(tmp.path(), "app", &[]).layer()?;
        let rotated = path.with_extension("log.0");
        tracing::subscriber::with_default(tracing_subscriber::registry().with(layer), || {
            tracing::info!(target: "app", "before");
            let _ = fs::rename(&path, &rotated);
            tracing::info!(target: "app", "after");
        });
        assert!(fs::read_to_string(&rotated)?.contains("msg=\"before\""));
        let fresh = fs::read_to_string(&path)?;
        assert_eq!(fresh.lines().count(), 1);
        assert!(fresh.contains("msg=\"after\""));
        Ok(())
    }

    #[test]
    fn layer_writes_the_unknown_level_warning_first() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let b = builder(
            tmp.path(),
            "teleclaude",
            &[
                ("TELECLAUDE_LOG_LEVEL", "deubg"),
                ("TELECLAUDE_THIRD_PARTY_LOG_LEVEL", "de bug"),
            ],
        );
        let lines = capture(b, || {
            tracing::debug!(target: "teleclaude", "hidden");
            tracing::info!(target: "teleclaude", "shown");
        })?;
        assert_eq!(
            lines,
            [
                concat!(
                    "level=WARNING logger=instrukt_ai_logging msg=\"unknown log level; using the default\" ",
                    "default=INFO value=deubg variable=TELECLAUDE_LOG_LEVEL"
                ),
                concat!(
                    "level=WARNING logger=instrukt_ai_logging msg=\"unknown log level; using the default\" ",
                    "default=WARNING value=\"de bug\" variable=TELECLAUDE_THIRD_PARTY_LOG_LEVEL"
                ),
                "level=INFO logger=teleclaude msg=\"shown\"",
            ]
        );
        Ok(())
    }

    #[test]
    fn layer_warning_needs_no_subscriber_and_bypasses_selection() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let b = builder(
            tmp.path(),
            "teleclaude",
            &[
                ("TELECLAUDE_LOG_LEVEL", "10"),
                ("TELECLAUDE_THIRD_PARTY_LOG_LEVEL", "ERROR"),
                ("TELECLAUDE_MUTED_LOGGERS", "instrukt_ai_logging"),
            ],
        );
        // Building the layer is the initialization: the line is in the file
        // before the layer is installed anywhere.
        let (_layer, path) = b.layer::<tracing_subscriber::Registry>()?;
        let content = fs::read_to_string(path)?;
        assert_eq!(content.lines().count(), 1);
        assert!(content.ends_with(
            " level=WARNING logger=instrukt_ai_logging msg=\"unknown log level; using the default\" default=INFO value=10 variable=TELECLAUDE_LOG_LEVEL\n"
        ));
        Ok(())
    }

    #[test]
    fn blank_and_known_level_values_are_silent() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let b = builder(
            tmp.path(),
            "teleclaude",
            &[
                ("TELECLAUDE_LOG_LEVEL", "  "),
                ("TELECLAUDE_THIRD_PARTY_LOG_LEVEL", "Error"),
            ],
        );
        let lines = capture(b, || {})?;
        assert!(lines.is_empty(), "{lines:?}");
        Ok(())
    }

    fn unknown(variable: &str, value: &str, default: &'static str) -> UnknownLevel {
        UnknownLevel {
            variable: variable.to_owned(),
            value: value.to_owned(),
            default,
        }
    }

    fn lines_without_ts(path: &Path) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        Ok(fs::read_to_string(path)?
            .lines()
            .map(|l| l.split_once(' ').map_or(l, |(_, rest)| rest).to_owned())
            .collect())
    }

    const WARN_APP: &str = concat!(
        "level=WARNING logger=instrukt_ai_logging msg=\"unknown log level; using the default\" ",
        "default=INFO value=deubg variable=TELECLAUDE_LOG_LEVEL"
    );
    const WARN_THIRD_PARTY: &str = concat!(
        "level=WARNING logger=instrukt_ai_logging msg=\"unknown log level; using the default\" ",
        "default=WARNING value=chatty variable=TELECLAUDE_THIRD_PARTY_LOG_LEVEL"
    );

    #[test]
    fn unknown_level_warning_follows_rotation() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("teleclaude.log");
        let archive = tmp.path().join("teleclaude.log.0");
        let app = unknown("TELECLAUDE_LOG_LEVEL", "deubg", "INFO");
        let third_party = unknown("TELECLAUDE_THIRD_PARTY_LOG_LEVEL", "chatty", "WARNING");

        // Rotated after the file was opened, before the first warning: both
        // land in the live file.
        let file = WatchedFile::open(path.clone())?;
        fs::rename(&path, &archive)?;
        warn_unknown_levels(&file, &[app.clone(), third_party.clone()], 4000)?;
        assert_eq!(lines_without_ts(&path)?, [WARN_APP, WARN_THIRD_PARTY]);
        assert!(lines_without_ts(&archive)?.is_empty());

        // Rotated between two warnings: the second one lands in the new live
        // file.
        fs::remove_file(&archive)?;
        fs::remove_file(&path)?;
        let file = WatchedFile::open(path.clone())?;
        warn_unknown_levels(&file, &[app], 4000)?;
        fs::rename(&path, &archive)?;
        warn_unknown_levels(&file, &[third_party], 4000)?;
        assert_eq!(lines_without_ts(&archive)?, [WARN_APP]);
        assert_eq!(lines_without_ts(&path)?, [WARN_THIRD_PARTY]);
        Ok(())
    }

    #[test]
    fn unknown_level_warning_write_failure_is_write_file_error() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("teleclaude.log");
        let file = WatchedFile::open(path.clone())?;
        // The path now names a directory: the reopen before the write fails.
        fs::remove_file(&path)?;
        fs::create_dir(&path)?;
        let res = warn_unknown_levels(
            &file,
            &[unknown("TELECLAUDE_LOG_LEVEL", "deubg", "INFO")],
            4000,
        );
        match res {
            Err(Error::WriteFile { path: p, .. }) => assert_eq!(p, path),
            other => return Err(format!("expected Error::WriteFile, got {other:?}").into()),
        }
        Ok(())
    }

    /// End to end through `layer()`: a symbolic link at the log path fails
    /// initialization, and nothing is written through it. `init()` builds
    /// through the same path; its own check is `tests/init_refuses_symlink.rs`,
    /// because `init()` refuses to run once any test in this binary has set a
    /// dispatcher.
    #[test]
    fn a_symlink_at_the_log_path_fails_initialization() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let dir = tmp.path().join("instrukt-ai/teleclaude");
        fs::create_dir_all(&dir)?;
        let target = tmp.path().join("target.log");
        fs::write(&target, "")?;
        std::os::unix::fs::symlink(&target, dir.join("teleclaude.log"))?;
        let unknown_env = [("TELECLAUDE_LOG_LEVEL", "deubg")];

        let res =
            builder(tmp.path(), "teleclaude", &unknown_env).layer::<tracing_subscriber::Registry>();
        match res {
            Err(err @ Error::OpenFile { .. }) => {
                assert!(err.to_string().contains("symbolic link"), "{err}");
            }
            Err(other) => return Err(format!("expected OpenFile, got {other:?}").into()),
            Ok(_) => return Err("a symlinked log path must fail".into()),
        }
        assert_eq!(fs::read_to_string(&target)?, "");
        Ok(())
    }

    #[test]
    fn a_source_that_is_not_a_bare_stem_fails_initialization() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let res = builder(tmp.path(), "app", &[])
            .source("../../escape")
            .layer::<tracing_subscriber::Registry>();
        assert!(matches!(res, Err(Error::InvalidSource { .. })));
        // Nothing was created outside (or inside) the log root.
        assert!(!tmp.path().join("escape.log").exists());
        assert!(!tmp.path().join("instrukt-ai").exists());
        Ok(())
    }

    #[test]
    fn log_records_are_normalized() -> TestResult {
        // Drive the bridge's own dispatch path without installing a global
        // `log` logger: `format_trace` is what `LogTracer` calls per record.
        let tmp = tempfile::tempdir()?;
        let lines = capture(builder(tmp.path(), "app", &[]), || {
            let record = tracing_log::log::Record::builder()
                .args(format_args!("from log"))
                .level(tracing_log::log::Level::Warn)
                .target("reqwest::connect")
                .module_path(Some("reqwest::connect"))
                .file(Some("src/connect.rs"))
                .line(Some(10))
                .build();
            let _ = tracing_log::format_trace(&record);
            let quiet = tracing_log::log::Record::builder()
                .args(format_args!("hidden"))
                .level(tracing_log::log::Level::Info)
                .target("reqwest::connect")
                .build();
            let _ = tracing_log::format_trace(&quiet);
        })?;
        assert_eq!(
            lines,
            ["level=WARNING logger=reqwest::connect msg=\"from log\""]
        );
        Ok(())
    }
}
