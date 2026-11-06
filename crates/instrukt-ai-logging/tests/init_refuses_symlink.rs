//! `init` installs process-global state, so this check lives in its own test
//! binary with a single test: a symbolic link at the log path makes `init()`
//! fail before it installs anything, and nothing is written through the link.

use std::ffi::OsString;
use std::fs;

use instrukt_ai_logging::{Builder, Error};

#[test]
fn init_fails_on_a_symlinked_log_path_before_installing_anything()
-> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let state = tmp.path().join("state");
    let dir = state.join("instrukt-ai/teleclaude");
    fs::create_dir_all(&dir)?;
    let target = tmp.path().join("target.log");
    fs::write(&target, "")?;
    std::os::unix::fs::symlink(&target, dir.join("teleclaude.log"))?;
    let state_os = state.into_os_string();

    let res = Builder::new("teleclaude")
        .env_lookup(move |key| match key {
            "XDG_STATE_HOME" => Some(state_os.clone()),
            "TELECLAUDE_LOG_LEVEL" => Some(OsString::from("deubg")),
            _ => None,
        })
        .init();
    assert!(
        matches!(res, Err(Error::OpenFile { .. })),
        "init(): {res:?}"
    );
    assert!(!tracing::dispatcher::has_been_set());
    assert_eq!(fs::read_to_string(&target)?, "");
    Ok(())
}
