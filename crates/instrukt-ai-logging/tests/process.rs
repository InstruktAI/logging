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
