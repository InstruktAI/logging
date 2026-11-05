//! Implementation of the `instrukt-ai-logs` reader and `instrukt-ai-log-setup`.
//!
//! Both binaries are drop-in replacements for the console scripts the 0.6.x
//! Python package shipped: same arguments, output, messages, exit codes, files
//! and scheduler commands. The binaries are thin wrappers around [`reader::run`]
//! and [`setup::run`].

pub mod files;
pub mod follow;
pub mod merge;
mod paths;
mod pyfmt;
mod pyregex;
pub mod reader;
pub mod setup;
pub mod timestamp;

use std::io::Write;

/// How a command ends.
#[derive(Debug)]
pub enum Exit {
    /// Success, exit 0.
    Ok,
    /// Stdout went away (`| head`): exit 0 without a message.
    Quiet,
    /// Help or version text for stdout, exit 0.
    Printed(String),
    /// A usage error: the full text (with newline) for stderr, exit 2.
    Usage(String),
    /// A failure: the message for stderr (a newline is added), exit 1.
    Fail(String),
    /// Exit with this code, nothing more to print.
    Silent(i32),
}

impl Exit {
    /// Print what remains to be printed and return the process exit code.
    pub fn report(self) -> i32 {
        match self {
            Exit::Ok | Exit::Quiet => 0,
            Exit::Printed(text) => {
                let mut out = std::io::stdout().lock();
                let _ = out.write_all(text.as_bytes());
                let _ = out.flush();
                0
            }
            Exit::Usage(text) => {
                let _ = std::io::stderr().write_all(text.as_bytes());
                2
            }
            Exit::Fail(msg) => {
                let _ = writeln!(std::io::stderr(), "{msg}");
                1
            }
            Exit::Silent(code) => code,
        }
    }
}
