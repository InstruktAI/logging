//! The writer benchmark cases, shared by the `writer` (timing) and
//! `writer_allocs` (allocation counting) bench targets.
//!
//! Every case goes through the public API only: [`Builder::layer`] on a
//! `Registry`, installed as the thread's default subscriber for the duration
//! of one benchmark, and a `tracing` macro call per iteration. The same file
//! therefore runs unchanged against any release with that API (0.8.0 on), which
//! is how the baseline compares releases.
//!
//! Sinks. The writer always writes to its log file and checks, before each
//! line, that the path still names the open file (device + inode). To keep
//! disk I/O out of the formatting cases, their log path is a symlink to
//! `/dev/null`: the line is formatted, redacted and selected exactly as in
//! production, then handed to a device that discards it. The per-line `stat`
//! and `write` system calls remain; CodSpeed's CPU simulation does not count
//! system calls, so in CI these cases measure formatting, selection and
//! redaction alone. The `file` case writes to a real file.
//!
//! The environment is passed through [`Builder::env_lookup`], with
//! `XDG_STATE_HOME` and `HOME` inside a temporary directory: nothing touches
//! the real home or XDG trees.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use divan::Bencher;
use instrukt_ai_logging::Builder;
use tempfile::TempDir;
use tracing::dispatcher::{self, DefaultGuard, Dispatch};
use tracing_subscriber::Registry;
use tracing_subscriber::layer::SubscriberExt;

/// App name; env prefix `BENCH_APP`, app logger prefix `bench_app`.
const APP: &str = "bench-app";

/// Where the log line goes.
#[derive(Clone, Copy)]
enum Sink {
    /// `/dev/null` behind the log path: no disk I/O.
    Discard,
    /// A real file in the temporary directory.
    File,
}

/// One installed writer: the temporary state tree and the thread-default
/// subscriber guard. Dropping it uninstalls the subscriber.
struct Writer {
    _guard: DefaultGuard,
    _dir: TempDir,
}

fn env_lookup(
    state: &Path,
    home: &Path,
    vars: &[(&str, &str)],
) -> impl Fn(&str) -> Option<OsString> + Send + Sync + 'static {
    let mut owned: Vec<(String, OsString)> = vars
        .iter()
        .map(|(k, v)| ((*k).to_owned(), OsString::from(v)))
        .collect();
    owned.push(("XDG_STATE_HOME".to_owned(), state.as_os_str().to_owned()));
    owned.push(("HOME".to_owned(), home.as_os_str().to_owned()));
    move |key| owned.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
}

fn log_path(state: &Path) -> PathBuf {
    state
        .join("instrukt-ai")
        .join(APP)
        .join(format!("{APP}.log"))
}

/// Build the layer for `vars`, install it for this thread, and keep it there
/// until the returned guard drops.
fn install(sink: Sink, vars: &[(&str, &str)]) -> Writer {
    let dir = tempfile::tempdir().expect("bench: create a temporary directory");
    let state = dir.path().join("state");
    let home = dir.path().join("home");
    if let Sink::Discard = sink {
        let path = log_path(&state);
        let parent = path.parent().expect("bench: the log path has a parent");
        std::fs::create_dir_all(parent).expect("bench: create the log directory");
        std::os::unix::fs::symlink("/dev/null", &path)
            .expect("bench: link the log path to /dev/null");
    }
    let (layer, _path) = Builder::new(APP)
        .env_lookup(env_lookup(&state, &home, vars))
        .layer::<Registry>()
        .expect("bench: build the writer layer");
    let guard = dispatcher::set_default(&Dispatch::new(Registry::default().with(layer)));
    Writer {
        _guard: guard,
        _dir: dir,
    }
}

/// A string of exactly `len` characters, built from `unit` repeated.
fn repeat_to(unit: &str, len: usize) -> String {
    unit.chars().cycle().take(len).collect()
}

/// About 200 characters of ordinary text with no secret in it.
fn clean_200() -> String {
    repeat_to(
        "GET /api/v1/users/42/sessions?page=2&limit=50 status=200 took=12ms skip=task-runner ",
        200,
    )
}

/// About 200 characters with one Bearer token in the middle.
fn bearer_200() -> String {
    let token = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJhbGljZSJ9.c2lnbmF0dXJlLXZhbHVl";
    let head = repeat_to("POST /api/v1/sessions retry=1 ", 60);
    let tail = repeat_to(
        " upstream=auth-gateway region=eu-west-1 ",
        200 - 60 - 22 - token.len(),
    );
    format!("{head}Authorization: Bearer {token}{tail}")
}

/// 4100 characters: over the default 4000-character limit, so it is cut.
fn long_4100() -> String {
    repeat_to(
        "lorem ipsum dolor sit amet, consectetur adipiscing elit ",
        4100,
    )
}

const MB: usize = 1 << 20;

/// About 1 MiB of Bearer starts that the boundary rule rejects (`xBearer`).
fn rejected_starts_1mb() -> String {
    let unit = "xBearer\u{1c}";
    unit.repeat(MB / unit.len()) + "a"
}

/// About 1 MiB of Bearers, each one the token of the one before.
fn chain_1mb() -> String {
    let unit = "Bearer\u{1c}";
    " ".to_owned() + &unit.repeat(MB / unit.len()) + "a"
}

/// The app line every formatting case shares: a message plus a str, an int
/// and a bool field.
macro_rules! app_line {
    ($target:literal) => {
        tracing::info!(target: $target, user = "alice", attempt = 3_i64, cached = true, "request handled")
    };
}

/// Emit one app event with a string field `payload`.
fn payload_event(payload: &str) {
    tracing::info!(target: "bench_app::http", payload, "request handled");
}

fn bench_payload(bencher: Bencher, payload: &str) {
    let _writer = install(Sink::Discard, &[]);
    bencher.bench_local(|| payload_event(divan::black_box(payload)));
}

/// A `debug!` with three fields while the app level is INFO: rejected by the
/// level filter before any field is visited.
#[divan::bench]
fn disabled(bencher: Bencher) {
    let _writer = install(Sink::Discard, &[]);
    bencher.bench_local(|| {
        tracing::debug!(target: "bench_app::http", user = "alice", attempt = 3_i64, cached = true, "request handled");
    });
}

/// An enabled app event, formatted and written to `/dev/null`.
#[divan::bench]
fn app_line(bencher: Bencher) {
    let _writer = install(Sink::Discard, &[]);
    bencher.bench_local(|| app_line!("bench_app::http"));
}

/// The app line from a third-party target under a spotlight at INFO.
#[divan::bench]
fn third_party(bencher: Bencher) {
    let _writer = install(
        Sink::Discard,
        &[
            ("BENCH_APP_THIRD_PARTY_LOGGERS", "hyper"),
            ("BENCH_APP_THIRD_PARTY_LOG_LEVEL", "INFO"),
        ],
    );
    bencher.bench_local(|| app_line!("hyper::client::pool"));
}

/// The app line appended to a real file, with the rotation check per line.
#[divan::bench]
fn file(bencher: Bencher) {
    let _writer = install(Sink::File, &[]);
    bencher.bench_local(|| app_line!("bench_app::http"));
}

/// Building the layer: env parsing, path resolution, directory creation and
/// opening the file. Once per process in production.
#[divan::bench]
fn init(bencher: Bencher) {
    let dir = tempfile::tempdir().expect("bench: create a temporary directory");
    let state = dir.path().join("state");
    let home = dir.path().join("home");
    bencher.bench_local(|| {
        Builder::new(APP)
            .env_lookup(env_lookup(&state, &home, &[]))
            .layer::<Registry>()
            .map(|(layer, _path)| layer)
    });
}

/// String values through redaction and truncation.
mod redaction {
    use super::*;

    /// About 200 characters, no secret.
    #[divan::bench]
    fn clean_200(bencher: Bencher) {
        bench_payload(bencher, &super::clean_200());
    }

    /// About 200 characters with a Bearer token.
    #[divan::bench]
    fn bearer_200(bencher: Bencher) {
        bench_payload(bencher, &super::bearer_200());
    }

    /// 4100 characters, truncated to 4000.
    #[divan::bench]
    fn truncated_4100(bencher: Bencher) {
        bench_payload(bencher, &long_4100());
    }

    /// 1 MiB of rejected Bearer starts; truncated after redaction.
    #[divan::bench(sample_count = 20)]
    fn adversarial_rejected_starts_1mb(bencher: Bencher) {
        bench_payload(bencher, &rejected_starts_1mb());
    }

    /// 1 MiB Bearer chain; truncated after redaction.
    #[divan::bench(sample_count = 20)]
    fn adversarial_chain_1mb(bencher: Bencher) {
        bench_payload(bencher, &chain_1mb());
    }
}
