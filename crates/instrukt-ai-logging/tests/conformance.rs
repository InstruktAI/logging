//! Runs every shared writer case in `conformance/writer/*.json` (schema:
//! `conformance/README.md`) through the real layer.
//!
//! Fixture targets and field names are runtime data, but tracing callsites are
//! static. Each call therefore gets a dynamic callsite (leaked `Metadata` and
//! `FieldSet`, acceptable in a test), is registered with tracing, and is
//! dispatched the way the `event!` macro does it: callsite interest, global
//! max level, `Dispatch::enabled`, then `Event::dispatch`. So every case goes
//! through the same per-layer filter, formatter and file writer as a macro
//! event. Each case gets its own temp HOME/XDG tree, an injected environment
//! (the process environment is never read or changed) and a fresh subscriber
//! installed only on the running thread.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

use serde_json::Value as Json;
use time::OffsetDateTime;
use tracing::callsite::{Callsite, Identifier};
use tracing::field::{FieldSet, Value};
use tracing::level_filters::LevelFilter;
use tracing::metadata::Kind;
use tracing::subscriber::Interest;
use tracing::{Event, Level, Metadata};
use tracing_subscriber::layer::SubscriberExt;

const INTEREST_UNKNOWN: u8 = 0;
const INTEREST_NEVER: u8 = 1;
const INTEREST_SOMETIMES: u8 = 2;
const INTEREST_ALWAYS: u8 = 3;

struct DynCallsite {
    meta: OnceLock<Metadata<'static>>,
    interest: AtomicU8,
}

impl Callsite for DynCallsite {
    fn set_interest(&self, interest: Interest) {
        let v = if interest.is_never() {
            INTEREST_NEVER
        } else if interest.is_always() {
            INTEREST_ALWAYS
        } else {
            INTEREST_SOMETIMES
        };
        self.interest.store(v, Ordering::SeqCst);
    }

    fn metadata(&self) -> &Metadata<'_> {
        self.meta
            .get()
            .expect("metadata is set before registration")
    }
}

fn leak(s: &str) -> &'static str {
    Box::leak(s.to_owned().into_boxed_str())
}

/// A registered callsite for `target`/`level` with fields `message` + `keys`.
fn callsite(target: &str, level: Level, keys: &[String]) -> &'static DynCallsite {
    let cs: &'static DynCallsite = Box::leak(Box::new(DynCallsite {
        meta: OnceLock::new(),
        interest: AtomicU8::new(INTEREST_UNKNOWN),
    }));
    let mut names: Vec<&'static str> = vec!["message"];
    names.extend(keys.iter().map(|k| leak(k)));
    let names: &'static [&'static str] = Box::leak(names.into_boxed_slice());
    let meta = Metadata::new(
        "conformance event",
        leak(target),
        level,
        Some(file!()),
        Some(line!()),
        None,
        FieldSet::new(names, Identifier(cs)),
        Kind::EVENT,
    );
    assert!(cs.meta.set(meta).is_ok());
    tracing::callsite::register(cs);
    cs
}

enum KvValue {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}

impl KvValue {
    fn as_value(&self) -> &dyn Value {
        match self {
            Self::Str(s) => s,
            Self::Int(i) => i,
            Self::Float(f) => f,
            Self::Bool(b) => b,
        }
    }
}

/// Emit one event the way `tracing::event!` does.
fn emit(cs: &'static DynCallsite, msg: &str, values: &[KvValue]) {
    let meta: &'static Metadata<'static> = cs.meta.get().expect("metadata is set");
    if *meta.level() > LevelFilter::current() {
        return;
    }
    let enabled = match cs.interest.load(Ordering::SeqCst) {
        INTEREST_NEVER => false,
        INTEREST_ALWAYS => true,
        _ => tracing::dispatcher::get_default(|d| d.enabled(meta)),
    };
    if !enabled {
        return;
    }
    // Bound to a local so the borrow below lives long enough on the MSRV
    // (format_args! temporaries are only extended by newer compilers).
    let message = tracing::field::display(msg);
    let mut refs: Vec<Option<&dyn Value>> = vec![Some(&message as &dyn Value)];
    refs.extend(values.iter().map(|v| Some(v.as_value())));
    Event::dispatch(meta, &meta.fields().value_set_all(&refs));
}

fn level(name: &str) -> Level {
    match name {
        "TRACE" => Level::TRACE,
        "DEBUG" => Level::DEBUG,
        "INFO" => Level::INFO,
        "WARNING" => Level::WARN,
        "ERROR" => Level::ERROR,
        other => panic!("unknown fixture level {other:?}"),
    }
}

fn kv_value(kv: &Json) -> KvValue {
    let v = &kv["value"];
    match kv["type"].as_str().expect("kv.type") {
        "str" => KvValue::Str(v.as_str().expect("str value").to_owned()),
        "int" => KvValue::Int(v.as_i64().expect("int value")),
        "float" => KvValue::Float(v.as_f64().expect("float value")),
        "bool" => KvValue::Bool(v.as_bool().expect("bool value")),
        other => panic!("unknown kv type {other:?}"),
    }
}

struct Dirs {
    home: PathBuf,
    state: PathBuf,
    config: PathBuf,
}

fn render(s: &str, dirs: &Dirs) -> String {
    s.replace("{{STATE}}", &dirs.state.to_string_lossy())
        .replace("{{HOME}}", &dirs.home.to_string_lossy())
}

fn now_ts() -> String {
    let t = OffsetDateTime::now_utc();
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second(),
        t.millisecond()
    )
}

fn ts_shape_ok(ts: &str) -> bool {
    let b = ts.as_bytes();
    b.len() == 24
        && b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            10 => *c == b'T',
            13 | 16 => *c == b':',
            19 => *c == b'.',
            23 => *c == b'Z',
            _ => c.is_ascii_digit(),
        })
}

type Call = (&'static DynCallsite, String, Vec<KvValue>);

/// Run one case; `Err` describes the first mismatch.
fn run_case(path: &Path) -> Result<(), String> {
    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let case: Json = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let tmp = tempfile::tempdir().map_err(|e| e.to_string())?;
    let dirs = Dirs {
        home: tmp.path().join("home"),
        state: tmp.path().join("state"),
        config: tmp.path().join("config"),
    };
    fs::create_dir_all(&dirs.home).map_err(|e| e.to_string())?;

    // The case's env is the complete contract environment: nothing else
    // (in particular no `<PREFIX>_*` variable of the real process) is visible.
    let mut env: BTreeMap<String, String> = BTreeMap::new();
    env.insert("HOME".into(), dirs.home.to_string_lossy().into_owned());
    env.insert(
        "XDG_STATE_HOME".into(),
        dirs.state.to_string_lossy().into_owned(),
    );
    env.insert(
        "XDG_CONFIG_HOME".into(),
        dirs.config.to_string_lossy().into_owned(),
    );
    for (k, v) in case["env"].as_object().ok_or("env is not an object")? {
        let v = v.as_str().ok_or("env value is not a string")?;
        env.insert(k.clone(), render(v, &dirs));
    }

    let app = case["app"].as_str().ok_or("app")?;
    let mut builder = instrukt_ai_logging::Builder::new(app)
        .env_lookup(move |key| env.get(key).map(OsString::from));
    if let Some(source) = case["source"].as_str() {
        builder = builder.source(source);
    }
    if let Some(max) = case["max_message_chars"].as_i64() {
        // The contract: a value <= 0 disables truncation, which is 0 here.
        builder = builder.max_message_chars(usize::try_from(max).unwrap_or(0));
    }

    let calls: Vec<Call> = case["calls"]
        .as_array()
        .ok_or("calls")?
        .iter()
        .map(|call| {
            let kv = call["kv"].as_array().expect("kv");
            let keys: Vec<String> = kv
                .iter()
                .map(|k| k["key"].as_str().expect("key").to_owned())
                .collect();
            let cs = callsite(
                call["logger"].as_str().expect("logger"),
                level(call["level"].as_str().expect("level")),
                &keys,
            );
            let msg = call["msg"].as_str().expect("msg").to_owned();
            (cs, msg, kv.iter().map(kv_value).collect())
        })
        .collect();

    let start = now_ts();
    let (layer, log_path) = builder.layer().map_err(|e| format!("layer: {e}"))?;
    // Creating the dispatcher rebuilds the interest of every registered
    // callsite, including the ones above.
    tracing::subscriber::with_default(tracing_subscriber::registry().with(layer), || {
        for (cs, msg, values) in &calls {
            emit(cs, msg, values);
        }
    });
    let end = now_ts();

    let want_path = case["expect"]["path"].as_str().ok_or("expect.path")?;
    let want_path = PathBuf::from(render(want_path, &dirs));
    if log_path != want_path {
        return Err(format!(
            "path: got {}, want {}",
            log_path.display(),
            want_path.display()
        ));
    }
    let content =
        fs::read_to_string(&want_path).map_err(|e| format!("read {}: {e}", want_path.display()))?;
    let mut got = Vec::new();
    for line in content.lines() {
        let (ts, rest) = line
            .split_once(' ')
            .ok_or_else(|| format!("no timestamp: {line:?}"))?;
        if !ts_shape_ok(ts) || ts < start.as_str() || ts > end.as_str() {
            return Err(format!("timestamp {ts:?} not in [{start}, {end}]"));
        }
        got.push(rest.to_owned());
    }
    let want: Vec<String> = case["expect"]["lines"]
        .as_array()
        .ok_or("expect.lines")?
        .iter()
        .map(|l| {
            l.as_str()
                .map(|s| render(s, &dirs))
                .ok_or("line is not a string")
        })
        .collect::<Result<_, _>>()?;
    if got != want {
        return Err(format!("lines differ\n  got:  {got:#?}\n  want: {want:#?}"));
    }
    Ok(())
}

#[test]
fn writer_cases() {
    // Run-time CARGO_MANIFEST_DIR first: with a target dir shared between
    // checkouts, a binary built in another checkout must read this one's cases.
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR")
        .map_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")), PathBuf::from);
    let dir = manifest.join("../../conformance/writer");
    let mut cases: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    cases.sort();
    assert!(!cases.is_empty(), "no writer cases in {}", dir.display());

    let failures: Vec<String> = cases
        .iter()
        .filter_map(|p| run_case(p).err().map(|e| format!("{}: {e}", p.display())))
        .collect();
    assert!(
        failures.is_empty(),
        "{} of {} writer cases failed:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
    eprintln!("{} writer cases passed", cases.len());
}
