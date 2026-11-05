//! Runs every `conformance/reader/*.json` and `conformance/setup/*.json` case
//! (setup: host platform only) against the built binaries, per
//! `conformance/README.md`.
//!
//! Each case gets fresh temporary HOME / XDG_STATE_HOME / XDG_CONFIG_HOME and
//! a PATH holding only a directory of fake `launchctl`/`systemctl`, so no real
//! scheduler is reachable. The fakes are symlinks to this very test binary:
//! started under one of those names it records its argv and answers from the
//! case's scheduler table instead of running cases.
//!
//! Cases named `tool-*` pin the tool's deliberate departures from 0.6.x's
//! Python CLI; they are ordinary cases in the same tree.
//!
//! The fixture tree is found through `CARGO_MANIFEST_DIR` read at run time
//! (cargo sets it for `cargo test`), falling back to the compile-time value:
//! with a target dir shared between checkouts, a test binary built in another
//! checkout must still read this checkout's fixtures.
//!
//! `CONFORMANCE_FILTER=<substring>` limits the run to matching case names.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;

const READER_BIN: &str = env!("CARGO_BIN_EXE_instrukt-ai-logs");
const SETUP_BIN: &str = env!("CARGO_BIN_EXE_instrukt-ai-log-setup");
const FAKE_SCHEDULERS: [&str; 2] = ["launchctl", "systemctl"];
const PROCESS_TIMEOUT: Duration = Duration::from_secs(10);

#[cfg(target_os = "macos")]
const HOST_PLATFORM: &str = "darwin";
#[cfg(not(target_os = "macos"))]
const HOST_PLATFORM: &str = "linux";

type CaseResult = Result<(), String>;

fn main() {
    let argv0 = std::env::args().next().unwrap_or_default();
    let name = Path::new(&argv0)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if FAKE_SCHEDULERS.contains(&name.as_str()) {
        std::process::exit(fake_scheduler(&name));
    }

    let filter = std::env::var("CONFORMANCE_FILTER").unwrap_or_default();
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR")
        .map_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")), PathBuf::from);
    let root = manifest.join("../../conformance");
    let mut failures = Vec::new();
    let mut ran = 0;
    for kind in ["reader", "setup"] {
        for (name, case) in load_cases(&root.join(kind)) {
            if !name.contains(&filter) {
                continue;
            }
            if kind == "setup" && case["platform"].as_str() != Some(HOST_PLATFORM) {
                println!("skip  {kind}/{name} (platform {})", case["platform"]);
                continue;
            }
            ran += 1;
            let outcome = std::panic::catch_unwind(|| run_case(kind, &case))
                .unwrap_or_else(|_| Err("runner panicked".to_string()));
            match outcome {
                Ok(()) => println!("ok    {kind}/{name}"),
                Err(why) => {
                    println!("FAIL  {kind}/{name}\n{why}\n");
                    failures.push(format!("{kind}/{name}"));
                }
            }
        }
    }
    println!(
        "\nconformance: {ran} run, {} passed, {} failed",
        ran - failures.len(),
        failures.len()
    );
    if ran == 0 {
        println!("no cases ran");
        std::process::exit(1);
    }
    if !failures.is_empty() {
        for f in &failures {
            println!("  failed: {f}");
        }
        std::process::exit(1);
    }
}

fn run_case(kind: &str, case: &Value) -> CaseResult {
    let sandbox = Sandbox::create()?;
    match kind {
        "reader" => check_reader_case(case, &sandbox),
        _ => check_setup_case(case, &sandbox),
    }
}

fn load_cases(dir: &Path) -> Vec<(String, Value)> {
    let mut paths: Vec<PathBuf> = fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "json"))
                .collect()
        })
        .unwrap_or_default();
    paths.sort();
    paths
        .into_iter()
        .map(|p| {
            let name = p.file_stem().unwrap().to_string_lossy().into_owned();
            let text = fs::read_to_string(&p).unwrap();
            let case: Value = serde_json::from_str(&text)
                .unwrap_or_else(|e| panic!("{}: invalid JSON: {e}", p.display()));
            (name, case)
        })
        .collect()
}

// --- fake scheduler ----------------------------------------------------------

fn fake_scheduler(name: &str) -> i32 {
    let mut argv: Vec<String> = vec![name.to_string()];
    argv.extend(std::env::args().skip(1));
    let calls = std::env::var("CONFORMANCE_FAKE_CALLS").unwrap();
    let table = std::env::var("CONFORMANCE_FAKE_TABLE").unwrap();
    let mut f = File::options()
        .create(true)
        .append(true)
        .open(calls)
        .unwrap();
    writeln!(f, "{}", serde_json::to_string(&argv).unwrap()).unwrap();
    let entries: Value = serde_json::from_str(&fs::read_to_string(table).unwrap()).unwrap();
    for entry in entries.as_array().into_iter().flatten() {
        let prefix: Vec<&str> = entry["argv_prefix"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        if argv.len() >= prefix.len() && argv.iter().zip(&prefix).all(|(a, p)| a == p) {
            let _ = std::io::stderr().write_all(entry["stderr"].as_str().unwrap_or("").as_bytes());
            return entry["exit"].as_i64().map_or(0, |c| c as i32);
        }
    }
    0
}

// --- sandbox -----------------------------------------------------------------

struct Sandbox {
    _dir: tempfile::TempDir,
    home: PathBuf,
    state: PathBuf,
    config: PathBuf,
    fakebin: PathBuf,
    now: SystemTime,
}

fn format_log_timestamp(t: SystemTime) -> String {
    let d = t.duration_since(SystemTime::UNIX_EPOCH).unwrap();
    let dt = time::OffsetDateTime::from_unix_timestamp_nanos(d.as_nanos() as i128).unwrap();
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        dt.year(),
        u8::from(dt.month()),
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second(),
        dt.millisecond()
    )
}

impl Sandbox {
    fn create() -> Result<Self, String> {
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let mk = |name: &str| -> Result<PathBuf, String> {
            let p = dir.path().join(name);
            fs::create_dir_all(&p).map_err(|e| e.to_string())?;
            Ok(p)
        };
        Ok(Self {
            home: mk("home")?,
            state: mk("state")?,
            config: mk("config")?,
            fakebin: mk("fakebin")?,
            now: SystemTime::now(),
            _dir: dir,
        })
    }

    fn render(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(start) = rest.find("{{T-") {
            out.push_str(&rest[..start]);
            let after = &rest[start + 4..];
            let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
            if !digits.is_empty() && after[digits.len()..].starts_with("}}") {
                let secs: u64 = digits.parse().unwrap();
                out.push_str(&format_log_timestamp(self.now - Duration::from_secs(secs)));
                rest = &after[digits.len() + 2..];
            } else {
                out.push_str("{{T-");
                rest = after;
            }
        }
        out.push_str(rest);
        out.replace("{{STATE}}", &self.state.to_string_lossy())
            .replace("{{HOME}}", &self.home.to_string_lossy())
            .replace("{{UID}}", &rustix::process::getuid().as_raw().to_string())
            .replace("{{VERSION}}", env!("CARGO_PKG_VERSION"))
    }

    fn command(&self, program: &str, args: &[String]) -> Command {
        let mut cmd = Command::new(program);
        cmd.args(args)
            .env_clear()
            .env("HOME", &self.home)
            .env("XDG_STATE_HOME", &self.state)
            .env("XDG_CONFIG_HOME", &self.config)
            .env("PATH", &self.fakebin)
            .env("CONFORMANCE_FAKE_CALLS", self.fakebin.join("calls.jsonl"))
            .env("CONFORMANCE_FAKE_TABLE", self.fakebin.join("table.json"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }

    fn install_fake_schedulers(&self, table: &Value) -> CaseResult {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        fs::write(self.fakebin.join("table.json"), table.to_string()).map_err(|e| e.to_string())?;
        for name in FAKE_SCHEDULERS {
            std::os::unix::fs::symlink(&exe, self.fakebin.join(name)).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    fn recorded_calls(&self) -> Vec<Vec<String>> {
        fs::read_to_string(self.fakebin.join("calls.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}

// --- process handling --------------------------------------------------------

struct Outcome {
    exit: i32,
    stdout: String,
    stderr: String,
}

fn collect(mut child: Child, deadline: Instant) -> Result<Outcome, String> {
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    let out_t = thread::spawn(move || {
        let mut s = Vec::new();
        let _ = out.read_to_end(&mut s);
        s
    });
    let err_t = thread::spawn(move || {
        let mut s = Vec::new();
        let _ = err.read_to_end(&mut s);
        s
    });
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("process timed out".to_string());
        }
        thread::sleep(Duration::from_millis(10));
    };
    let stdout = String::from_utf8_lossy(&out_t.join().unwrap()).into_owned();
    let stderr = String::from_utf8_lossy(&err_t.join().unwrap()).into_owned();
    let exit = status.code().unwrap_or(-1);
    Ok(Outcome {
        exit,
        stdout,
        stderr,
    })
}

// --- comparisons -------------------------------------------------------------

fn lines(text: &str) -> Vec<String> {
    let mut v: Vec<String> = text.split('\n').map(str::to_string).collect();
    if v.last().is_some_and(String::is_empty) {
        v.pop();
    }
    v
}

fn check_text(label: &str, actual: &str, spec: &Value, sb: &Sandbox) -> CaseResult {
    let expected = sb.render(spec["value"].as_str().unwrap_or(""));
    let ok = match spec["match"].as_str() {
        Some("exact") => actual == expected,
        Some("contains") => actual.contains(&expected),
        Some("regex") => fancy_regex::Regex::new(&expected)
            .map_err(|e| e.to_string())?
            .is_match(actual)
            .unwrap_or(false),
        other => return Err(format!("unknown match mode {other:?}")),
    };
    if ok {
        Ok(())
    } else {
        Err(format!(
            "{label} ({}) mismatch\n  expected: {expected:?}\n  actual:   {actual:?}",
            spec["match"]
        ))
    }
}

fn check_stdout(actual: &str, expected: &Value, sb: &Sandbox) -> CaseResult {
    if expected.is_object() {
        return check_text("stdout", actual, expected, sb);
    }
    let want: Vec<String> = expected
        .as_array()
        .into_iter()
        .flatten()
        .map(|l| sb.render(l.as_str().unwrap_or("")))
        .collect();
    let got = lines(actual);
    if got == want {
        Ok(())
    } else {
        Err(format!(
            "stdout mismatch\n  expected: {want:#?}\n  actual:   {got:#?}"
        ))
    }
}

fn check_exit(outcome: &Outcome, expect: &Value) -> CaseResult {
    let want = expect["exit"].as_i64().unwrap_or(0);
    if i64::from(outcome.exit) == want {
        Ok(())
    } else {
        Err(format!(
            "exit {} (expected {want})\n  stdout: {:?}\n  stderr: {:?}",
            outcome.exit, outcome.stdout, outcome.stderr
        ))
    }
}

// --- reader ------------------------------------------------------------------

fn write_lines(sb: &Sandbox, lines: &Value) -> Vec<u8> {
    lines
        .as_array()
        .into_iter()
        .flatten()
        .map(|l| sb.render(l.as_str().unwrap_or("")) + "\n")
        .collect::<String>()
        .into_bytes()
}

fn write_log_file(sb: &Sandbox, spec: &Value) -> CaseResult {
    let path = sb
        .state
        .join("instrukt-ai")
        .join(spec["path"].as_str().unwrap_or(""));
    fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    let data = write_lines(sb, &spec["lines"]);
    let bytes = if spec["gzip"].as_bool().unwrap_or(false) {
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&data).map_err(|e| e.to_string())?;
        enc.finish().map_err(|e| e.to_string())?
    } else {
        data
    };
    fs::write(&path, bytes).map_err(|e| e.to_string())?;
    let offset = spec["mtime_offset_s"].as_i64().unwrap_or(0);
    let mtime = if offset >= 0 {
        sb.now + Duration::from_secs(offset as u64)
    } else {
        sb.now - Duration::from_secs(offset.unsigned_abs())
    };
    File::options()
        .write(true)
        .open(&path)
        .and_then(|f| f.set_modified(mtime))
        .map_err(|e| e.to_string())
}

fn apply_follow_step(sb: &Sandbox, step: &Value) -> CaseResult {
    let root = sb.state.join("instrukt-ai");
    if let Some(rel) = step["rotate"].as_str() {
        let path = root.join(rel);
        let mut rotated = path.clone().into_os_string();
        rotated.push(".0");
        fs::rename(&path, rotated).map_err(|e| e.to_string())?;
        File::create(&path).map_err(|e| e.to_string())?;
        return Ok(());
    }
    let path = root.join(step["path"].as_str().unwrap_or(""));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    File::options()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| f.write_all(&write_lines(sb, &step["lines"])))
        .map_err(|e| e.to_string())
}

fn run_reader_case(case: &Value, sb: &Sandbox) -> Result<Outcome, String> {
    for spec in case["files"].as_array().into_iter().flatten() {
        write_log_file(sb, spec)?;
    }
    let args: Vec<String> = case["args"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|a| sb.render(a.as_str().unwrap_or("")))
        .collect();
    let child = sb
        .command(READER_BIN, &args)
        .spawn()
        .map_err(|e| e.to_string())?;
    let started = Instant::now();
    let follow = &case["follow"];
    if follow.is_null() {
        return collect(child, started + PROCESS_TIMEOUT);
    }

    let mut steps: Vec<&Value> = follow["appends"].as_array().into_iter().flatten().collect();
    steps.sort_by_key(|s| s["after_ms"].as_u64().unwrap_or(0));
    let sleep_until = |ms: u64| {
        let at = started + Duration::from_millis(ms);
        let now = Instant::now();
        if at > now {
            thread::sleep(at - now);
        }
    };
    for step in steps {
        sleep_until(step["after_ms"].as_u64().unwrap_or(0));
        apply_follow_step(sb, step)?;
    }
    sleep_until(follow["run_ms"].as_u64().unwrap_or(0));
    let pid = rustix::process::Pid::from_child(&child);
    let _ = rustix::process::kill_process(pid, rustix::process::Signal::INT);
    collect(child, Instant::now() + PROCESS_TIMEOUT)
}

fn check_reader_case(case: &Value, sb: &Sandbox) -> CaseResult {
    let outcome = run_reader_case(case, sb)?;
    let expect = &case["expect"];
    check_exit(&outcome, expect)?;
    check_stdout(&outcome.stdout, &expect["stdout"], sb)?;
    check_text("stderr", &outcome.stderr, &expect["stderr"], sb)
}

// --- setup -------------------------------------------------------------------

fn files_under(roots: &[&Path]) -> BTreeSet<PathBuf> {
    fn walk(dir: &Path, out: &mut BTreeSet<PathBuf>) {
        for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(t) if t.is_dir() => walk(&path, out),
                Ok(_) => {
                    out.insert(path);
                }
                Err(_) => {}
            }
        }
    }
    let mut out = BTreeSet::new();
    for root in roots {
        walk(root, &mut out);
    }
    out
}

fn check_setup_case(case: &Value, sb: &Sandbox) -> CaseResult {
    sb.install_fake_schedulers(&case["scheduler"])?;
    for (raw, content) in case["preexisting"].as_object().into_iter().flatten() {
        let path = PathBuf::from(sb.render(raw));
        fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
        fs::write(&path, sb.render(content.as_str().unwrap_or(""))).map_err(|e| e.to_string())?;
    }
    let args: Vec<String> = case["args"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|a| sb.render(a.as_str().unwrap_or("")))
        .collect();
    let child = sb
        .command(SETUP_BIN, &args)
        .spawn()
        .map_err(|e| e.to_string())?;
    let outcome = collect(child, Instant::now() + PROCESS_TIMEOUT)?;
    let expect = &case["expect"];
    check_exit(&outcome, expect)?;
    check_stdout(&outcome.stdout, &expect["stdout"], sb)?;
    if let Some(spec) = expect.get("stderr") {
        check_text("stderr", &outcome.stderr, spec, sb)?;
    }

    let mut expected_files = BTreeSet::new();
    for (raw, content) in expect["files"].as_object().into_iter().flatten() {
        let path = PathBuf::from(sb.render(raw));
        let want = sb.render(content.as_str().unwrap_or(""));
        let got = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if got != want {
            return Err(format!(
                "content of {}\n  expected: {want:?}\n  actual:   {got:?}",
                path.display()
            ));
        }
        expected_files.insert(path);
    }
    let present = files_under(&[&sb.home, &sb.state]);
    if present != expected_files {
        return Err(format!(
            "files under HOME/STATE\n  expected: {expected_files:#?}\n  actual:   {present:#?}"
        ));
    }

    let want_calls: Vec<Vec<String>> = expect["calls"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|call| {
            call.as_array()
                .into_iter()
                .flatten()
                .map(|a| sb.render(a.as_str().unwrap_or("")))
                .collect()
        })
        .collect();
    let got_calls = sb.recorded_calls();
    if got_calls != want_calls {
        return Err(format!(
            "scheduler calls\n  expected: {want_calls:#?}\n  actual:   {got_calls:#?}"
        ));
    }
    Ok(())
}
