//! `init` installs process-global state (the tracing subscriber and the `log`
//! bridge), so it lives in its own test binary with a single test.

use std::ffi::OsString;
use std::fs;

use instrukt_ai_logging::{Builder, Error};

#[test]
fn init_installs_globally_bridges_log_and_refuses_a_second_call()
-> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let state = tmp.path().join("state");
    let state_os = state.clone().into_os_string();
    let path = Builder::new("teleclaude")
        .source("worker")
        .env_lookup(move |key| match key {
            "XDG_STATE_HOME" => Some(state_os.clone()),
            "TELECLAUDE_LOG_LEVEL" => Some(OsString::from("debug")),
            // Unknown: the default (WARNING) applies, and init warns once.
            "TELECLAUDE_THIRD_PARTY_LOG_LEVEL" => Some(OsString::from("chatty")),
            _ => None,
        })
        .init()?;
    assert_eq!(path, state.join("instrukt-ai/teleclaude/worker.log"));

    tracing::debug!(target: "teleclaude::worker", n = 1, "from_tracing");
    tracing_log::log::warn!(target: "reqwest::connect", "from log");
    tracing_log::log::info!(target: "reqwest::connect", "hidden");
    tracing_log::log::debug!(target: "teleclaude.bridge", "app via log");

    let lines: Vec<String> = fs::read_to_string(&path)?
        .lines()
        .map(|l| l.split_once(' ').map_or(l, |(_, r)| r).to_owned())
        .collect();
    assert_eq!(
        lines,
        [
            concat!(
                "level=WARNING logger=instrukt_ai_logging msg=\"unknown log level; using the default\" ",
                "default=WARNING value=chatty variable=TELECLAUDE_THIRD_PARTY_LOG_LEVEL"
            ),
            "level=DEBUG logger=teleclaude::worker msg=\"from_tracing\" n=1",
            "level=WARNING logger=reqwest::connect msg=\"from log\"",
            "level=DEBUG logger=teleclaude.bridge msg=\"app via log\"",
        ]
    );

    // A second init is an error, never a panic, and touches no files.
    assert!(matches!(
        instrukt_ai_logging::init("teleclaude", None),
        Err(Error::AlreadyInitialized)
    ));
    Ok(())
}
