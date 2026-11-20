//! The explicitly supplied destination uses the ordinary writer contract.

use std::io::{self, Write};
use std::sync::{Arc, Mutex, PoisonError};

#[derive(Clone, Default)]
struct Output(Arc<Mutex<Vec<u8>>>);

impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn an_explicit_writer_uses_the_same_redaction_and_filter_without_opening_a_file()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let blocked = dir.path().join("blocked");
    std::fs::write(&blocked, b"not a directory")?;
    let log_root = blocked.clone().into_os_string();
    let output = Output::default();
    instrukt_ai_logging::Builder::new("demo")
        .env_lookup(move |var| match var {
            "XDG_STATE_HOME" => Some(log_root.clone()),
            _ => None,
        })
        .init_writer(output.clone())?;
    let credential = "fixture-credential-not-for-logs";
    tracing::info!(target: "demo", auth = %format_args!("Bearer {credential}"), "connected");
    tracing_log::log::warn!(target: "external", "Bearer {credential}");
    tracing::info!(target: "external", "third-party info suppressed");
    let buffer = output.0.lock().unwrap_or_else(PoisonError::into_inner);
    let text = std::str::from_utf8(&buffer)?;
    assert_eq!(text.lines().count(), 2, "{text}");
    assert!(
        text.contains("level=INFO logger=demo msg=\"connected\" auth=\"Bearer <REDACTED>\""),
        "{text}"
    );
    assert!(
        text.contains("level=WARNING logger=external msg=\"Bearer <REDACTED>\""),
        "{text}"
    );
    assert!(!text.contains(credential));
    assert!(blocked.is_file());
    Ok(())
}
