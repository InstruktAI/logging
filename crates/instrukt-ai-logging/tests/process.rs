//! Checks that need several processes, or a process of their own. Each check
//! re-runs this test binary as a child (`--exact <child>`), selected and
//! configured through environment variables; without them a child test does
//! nothing, so a plain `cargo test` runs it as a no-op.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tracing_subscriber::layer::SubscriberExt;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const APP: &str = "proc-app";
const CHILD_STATE: &str = "INSTRUKT_AI_LOGGING_TEST_CHILD_STATE";
const CHILD_ID: &str = "INSTRUKT_AI_LOGGING_TEST_CHILD_ID";

/// Run this test binary as a child that runs only `test`, with `env` set.
fn child(test: &str, env: &[(&str, OsString)]) -> Result<Command, Box<dyn std::error::Error>> {
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.args(["--exact", test, "--nocapture", "--test-threads=1"]);
    for (k, v) in env {
        cmd.env(k, v);
    }
    Ok(cmd)
}

fn builder(state: &Path) -> instrukt_ai_logging::Builder {
    let state = state.as_os_str().to_owned();
    instrukt_ai_logging::Builder::new(APP).env_lookup(move |key| match key {
        "XDG_STATE_HOME" => Some(state.clone()),
        _ => None,
    })
}

fn log_path(state: &Path) -> PathBuf {
    state
        .join("instrukt-ai")
        .join(APP)
        .join(format!("{APP}.log"))
}

const WRITERS: usize = 4;
const LINES_PER_WRITER: usize = 300;
const FILLER: usize = 15 * 1024;

fn payload(writer: &str, i: usize) -> String {
    format!("w{writer} i{i} {}", "x".repeat(FILLER))
}

/// The child half of `concurrent_processes_append_whole_lines`.
#[test]
fn child_append_long_lines() -> TestResult {
    let (Some(state), Some(id)) = (std::env::var_os(CHILD_STATE), std::env::var(CHILD_ID).ok())
    else {
        return Ok(());
    };
    let (layer, _path) = builder(Path::new(&state))
        .max_message_chars(0)
        .layer::<tracing_subscriber::Registry>()?;
    tracing::subscriber::with_default(tracing_subscriber::registry().with(layer), || {
        for i in 0..LINES_PER_WRITER {
            tracing::info!(target: "proc_app", "{}", payload(&id, i));
        }
    });
    Ok(())
}

/// Several processes append ~15 KiB lines to one file at once: every line
/// arrives whole, because each goes out in one `write` on an `O_APPEND`
/// descriptor.
#[test]
fn concurrent_processes_append_whole_lines() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let state = tmp.path().join("state");
    let mut children = Vec::new();
    for id in 0..WRITERS {
        children.push(
            child(
                "child_append_long_lines",
                &[
                    (CHILD_STATE, state.clone().into_os_string()),
                    (CHILD_ID, OsString::from(id.to_string())),
                ],
            )?
            .stdout(Stdio::null())
            .spawn()?,
        );
    }
    for mut c in children {
        assert!(c.wait()?.success(), "a writer process failed");
    }
    let content = fs::read_to_string(log_path(&state))?;
    let mut seen = vec![0usize; WRITERS];
    for line in content.lines() {
        let msg = line
            .split_once(" msg=\"")
            .and_then(|(_, rest)| rest.strip_suffix('"'))
            .ok_or_else(|| format!("malformed line of {} bytes", line.len()))?;
        let (writer, rest) = msg[1..].split_once(' ').ok_or("no writer id")?;
        let (i, _) = rest[1..].split_once(' ').ok_or("no line number")?;
        let (writer, i): (usize, usize) = (writer.parse()?, i.parse()?);
        assert_eq!(msg, payload(&writer.to_string(), i), "line torn or merged");
        assert_eq!(i, seen[writer], "lines of one writer out of order");
        seen[writer] += 1;
    }
    assert_eq!(seen, vec![LINES_PER_WRITER; WRITERS]);
    Ok(())
}

const CHILD_FAIL: &str = "INSTRUKT_AI_LOGGING_TEST_CHILD_FAIL";
const SECRET: &str = "eyJhbGciOiJIUzI1NiJ9.secret-token-value";

/// The child half of the failed-write checks: build the layer, replace the
/// log file with a directory so every write fails, log three events that
/// carry a token, and check the failure count. A panic exits with 101.
#[test]
fn child_log_into_a_failing_file() -> TestResult {
    let Some(state) = std::env::var_os(CHILD_FAIL) else {
        return Ok(());
    };
    let state = PathBuf::from(state);
    let (layer, path) = builder(&state).layer::<tracing_subscriber::Registry>()?;
    fs::remove_file(&path)?;
    fs::create_dir(&path)?;
    tracing::subscriber::with_default(tracing_subscriber::registry().with(layer), || {
        for i in 0..3 {
            tracing::warn!(target: "proc_app", token = SECRET, "call {i} with Bearer {SECRET}");
        }
    });
    assert_eq!(instrukt_ai_logging::failure_count(), 3);
    // Still running after the failures: the host process is unaffected.
    fs::write(state.join("survived"), "")?;
    Ok(())
}

/// A failed write while stderr is broken (its pipe has no reader) neither
/// panics nor reaches the caller: `eprintln!` would panic here.
#[test]
fn a_failed_write_with_a_broken_stderr_does_not_panic() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let state = tmp.path().join("state");
    let mut cmd = child(
        "child_log_into_a_failing_file",
        &[(CHILD_FAIL, state.clone().into_os_string())],
    )?;
    let mut proc = cmd.stdout(Stdio::null()).stderr(Stdio::piped()).spawn()?;
    // Close the read end before the child writes: its stderr is a broken pipe.
    drop(proc.stderr.take());
    let status = proc.wait()?;
    assert!(status.success(), "child: {status}");
    assert!(state.join("survived").exists());
    Ok(())
}

/// The report names the error and the path, at most once a minute, and never
/// carries the message or a field value.
#[test]
fn a_failed_write_is_reported_once_without_the_line() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/redacted-path.json"))?;
    let state = tmp.path().join(
        fixture["state_directory"]
            .as_str()
            .ok_or("fixture state directory")?,
    );
    let displayed = tmp.path().join(
        fixture["reported_directory"]
            .as_str()
            .ok_or("fixture report directory")?,
    );
    let out = child(
        "child_log_into_a_failing_file",
        &[(CHILD_FAIL, state.clone().into_os_string())],
    )?
    .stdout(Stdio::null())
    .output()?;
    assert!(out.status.success(), "child: {}", out.status);
    let stderr = String::from_utf8(out.stderr)?;
    let reports: Vec<&str> = stderr
        .lines()
        .filter(|l| l.starts_with("instrukt-ai-logging:"))
        .collect();
    assert_eq!(reports.len(), 1, "{stderr}");
    let want = format!(
        "instrukt-ai-logging: dropped a log line: cannot write {}: IsADirectory errno={}",
        displayed.display(),
        libc_eisdir()
    );
    assert!(reports[0].starts_with(&want), "{}", reports[0]);
    assert!(reports[0].ends_with("; 1 dropped since start, 0 reports suppressed"));
    assert!(!stderr.contains(SECRET), "{stderr}");
    assert!(!stderr.contains(fixture["credential"].as_str().ok_or("fixture credential")?));
    assert!(!stderr.contains("call 0"), "{stderr}");
    Ok(())
}

fn libc_eisdir() -> i32 {
    // EISDIR is 21 on both Linux and macOS.
    21
}

const CHILD_WRITER_ERROR: &str = "INSTRUKT_AI_LOGGING_TEST_WRITER_ERROR";
const WRITER_ERROR_SECRET: &str = "fixture-initialisation-secret";

struct FailingWriter {
    native_errno: bool,
}

impl std::io::Write for FailingWriter {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        if self.native_errno {
            return Err(std::io::Error::from_raw_os_error(libc::ENOSPC));
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("Bearer {WRITER_ERROR_SECRET}"),
        ))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn child_propagates_a_supplied_writer_error() -> TestResult {
    let Ok(mode) = std::env::var(CHILD_WRITER_ERROR) else {
        return Ok(());
    };
    let result = instrukt_ai_logging::Builder::new(APP)
        .env_lookup(|name| (name == "PROC_APP_LOG_LEVEL").then(|| "invalid".into()))
        .init_writer(FailingWriter {
            native_errno: mode == "errno",
        });
    if let Err(error) = &result {
        eprintln!("{error}");
    }
    result?;
    Ok(())
}

#[test]
fn a_supplied_writer_initialisation_error_keeps_only_kind_and_errno() -> TestResult {
    let errno = libc::ENOSPC.to_string();
    for (mode, kind, display_errno, debug_errno) in [
        ("custom", "PermissionDenied", "none", "None".to_owned()),
        (
            "errno",
            "StorageFull",
            errno.as_str(),
            format!("Some({errno})"),
        ),
    ] {
        let output = child(
            "child_propagates_a_supplied_writer_error",
            &[(CHILD_WRITER_ERROR, mode.into())],
        )?
        .output()?;
        assert!(!output.status.success());
        let stderr = String::from_utf8(output.stderr)?;
        assert!(
            !stderr.contains(WRITER_ERROR_SECRET),
            "writer payload reached stderr"
        );
        assert!(
            stderr.contains(&format!(
                "cannot write to the supplied log writer: {kind} errno={display_errno}"
            )),
            "{stderr}"
        );
        assert!(
            stderr.contains(&format!(
                "WriteSink {{ kind: {kind}, errno: {debug_errno} }}"
            )),
            "{stderr}"
        );
    }
    Ok(())
}
