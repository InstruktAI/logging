//! `init` installs process-global state, so this check lives in its own test
//! binary with a single test: a log file whose writes fail (`/dev/full`,
//! Linux only) makes `init()` fail on the unknown-level warning, before it
//! installs anything.
#![cfg(target_os = "linux")]

use std::ffi::OsString;
use std::fs;

use instrukt_ai_logging::{Builder, Error};

#[test]
fn init_fails_when_the_level_warning_cannot_be_written() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let state = tmp.path().join("state");
    let dir = state.join("instrukt-ai/teleclaude");
    fs::create_dir_all(&dir)?;
    std::os::unix::fs::symlink("/dev/full", dir.join("teleclaude.log"))?;
    let state_os = state.into_os_string();

    let res = Builder::new("teleclaude")
        .env_lookup(move |key| match key {
            "XDG_STATE_HOME" => Some(state_os.clone()),
            "TELECLAUDE_LOG_LEVEL" => Some(OsString::from("deubg")),
            _ => None,
        })
        .init();
    assert!(
        matches!(res, Err(Error::WriteFile { .. })),
        "init(): {res:?}"
    );
    assert!(!tracing::dispatcher::has_been_set());
    Ok(())
}
