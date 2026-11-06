//! The `tracing` layer, its per-layer filter, and the [`Builder`].

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
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
use crate::{Error, names, paths};

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
            ("default".to_owned(), Value::Str(u.default.to_owned())),
            ("value".to_owned(), Value::Str(u.value.clone())),
            ("variable".to_owned(), Value::Str(u.variable.clone())),
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
struct Collector {
    message: Option<String>,
    fields: BTreeMap<String, Value>,
    skip_bridge_fields: bool,
}

impl Collector {
    fn put(&mut self, field: &Field, value: Value) {
        let name = field.name();
        if name == "message" {
            self.message = Some(match value {
                Value::Raw(s) | Value::Str(s) => s,
            });
            return;
        }
        if self.skip_bridge_fields && LOG_BRIDGE_FIELDS.contains(&name) {
            return;
        }
        self.fields.insert(name.to_owned(), value);
    }
}

impl Visit for Collector {
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.put(field, Value::float(value));
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.put(field, Value::int(value));
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.put(field, Value::int(value));
    }
    fn record_i128(&mut self, field: &Field, value: i128) {
        self.put(field, Value::int(value));
    }
    fn record_u128(&mut self, field: &Field, value: u128) {
        self.put(field, Value::int(value));
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.put(field, Value::bool(value));
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.put(field, Value::Str(value.to_owned()));
    }
    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.put(field, Value::Str(value.to_string()));
    }
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.put(field, Value::Str(format!("{value:?}")));
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
        let mut collector = Collector {
            message: None,
            fields: BTreeMap::new(),
            skip_bridge_fields: normalized.is_some(),
        };
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
            // Nowhere else to report it: the log file is the failing part.
            eprintln!(
                "instrukt-ai-logging: cannot write to {}: {err}",
                self.file.path().display()
            );
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
    /// appended). An empty source means the default.
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
    /// Opens (and creates) the log file now; an unwritable location is an
    /// `Err`. An unknown level value is reported there right away, as one
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
