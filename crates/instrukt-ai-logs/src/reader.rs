//! The `instrukt-ai-logs` command (today's `instrukt_ai_logging/cli.py`).

use std::ffi::OsString;
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use clap::error::{ContextKind, ContextValue, ErrorKind};
use clap::{Arg, ArgAction, CommandFactory, FromArgMatches, Parser};
use fancy_regex::Regex;

use crate::files::{self, is_rotation_suffix, stem_of};
use crate::follow::{Follower, POLL_INTERVAL, Step};
use crate::merge::{self, MergedLines};
use crate::pyfmt::{py_strip, repr_str_list};
use crate::timestamp::{self, format_timestamp, parse_since};
use crate::{Exit, paths};

const PROG: &str = "instrukt-ai-logs";
const USAGE: &str = "usage: instrukt-ai-logs [-h] [--version] [--since SINCE] [-f] [--grep GREP]
                        [--exclude EXCLUDE] [--logs LOGS]
                        app
";

#[derive(Parser, Debug)]
#[command(
    name = PROG,
    version,
    disable_version_flag = true,
    args_override_self = true,
    infer_long_args = true
)]
struct Args {
    /// App/service name (folder under $XDG_STATE_HOME/instrukt-ai/, fallback ~/.local/state)
    #[arg(allow_negative_numbers = true)]
    app: String,
    /// Time window (e.g. 10m, 2h, 1d). Default: 10m
    #[arg(
        long,
        default_value = "10m",
        allow_negative_numbers = true,
        value_name = "SINCE"
    )]
    since: String,
    /// Follow the log (tail -f style) after printing the --since window
    #[arg(short = 'f', long)]
    follow: bool,
    /// Regex to keep only matching lines (optional)
    #[arg(
        long,
        default_value = "",
        allow_negative_numbers = true,
        value_name = "GREP"
    )]
    grep: String,
    /// Regex to drop matching lines (applied after --grep; optional)
    #[arg(
        long,
        default_value = "",
        allow_negative_numbers = true,
        value_name = "EXCLUDE"
    )]
    exclude: String,
    /// Comma-separated list of stems to include (e.g. 'cron,docs-watch').
    /// Default: all log files in the app's directory. Alias: --include.
    #[arg(
        long,
        visible_alias = "include",
        default_value = "",
        allow_negative_numbers = true,
        value_name = "LOGS"
    )]
    logs: String,
}

/// argparse's display name for an option, from clap's `--since <SINCE>`.
fn argparse_name(clap_arg: &str) -> String {
    let flag = clap_arg.split_whitespace().next().unwrap_or(clap_arg);
    match flag {
        "--logs" | "--include" => "--logs/--include".to_string(),
        "-f" | "--follow" => "-f/--follow".to_string(),
        other => other.to_string(),
    }
}

fn context_str(err: &clap::Error, kind: ContextKind) -> Option<String> {
    match err.get(kind)? {
        ContextValue::String(s) => Some(s.clone()),
        ContextValue::Strings(v) => Some(v.join(" ")),
        _ => None,
    }
}

/// Render a clap parse failure the way argparse reports it, exit 2.
fn usage_error(err: &clap::Error) -> Exit {
    let message = match err.kind() {
        ErrorKind::MissingRequiredArgument => {
            "the following arguments are required: app".to_string()
        }
        ErrorKind::InvalidValue | ErrorKind::NoEquals => {
            match context_str(err, ContextKind::InvalidArg) {
                Some(arg) => format!("argument {}: expected one argument", argparse_name(&arg)),
                None => clap_message(err),
            }
        }
        ErrorKind::TooManyValues => match context_str(err, ContextKind::InvalidArg) {
            Some(arg) if arg.starts_with("-f") || arg.starts_with("--follow") => {
                let value = context_str(err, ContextKind::InvalidValue).unwrap_or_default();
                format!("argument -f/--follow: ignored explicit argument '{value}'")
            }
            _ => clap_message(err),
        },
        ErrorKind::UnknownArgument => match context_str(err, ContextKind::InvalidArg) {
            Some(arg) => format!("unrecognized arguments: {arg}"),
            None => clap_message(err),
        },
        _ => clap_message(err),
    };
    Exit::Usage(format!("{USAGE}{PROG}: error: {message}\n"))
}

fn clap_message(err: &clap::Error) -> String {
    let rendered = err.render().to_string();
    let first = rendered.lines().next().unwrap_or("");
    first.strip_prefix("error: ").unwrap_or(first).to_string()
}

fn compile(flag: &str, pattern: &str) -> Result<Option<Regex>, Exit> {
    if pattern.is_empty() {
        return Ok(None);
    }
    Regex::new(pattern)
        .map(Some)
        .map_err(|e| Exit::Fail(format!("Invalid {flag} regex {pattern:?}: {e}")))
}

/// Apply the inclusive `--grep` keep filter, then the `--exclude` drop filter.
/// A regex that fails at match time (backtrack limit) counts as no match.
fn line_passes(line: &str, keep: Option<&Regex>, drop: Option<&Regex>) -> bool {
    if let Some(keep) = keep {
        if !keep.is_match(line).unwrap_or(false) {
            return false;
        }
    }
    if let Some(drop) = drop {
        if drop.is_match(line).unwrap_or(false) {
            return false;
        }
    }
    true
}

fn parse_stems(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(py_strip)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Write one line, mapping a closed pipe to a quiet exit.
fn write_line(out: &mut dyn Write, line: &str) -> Result<(), Exit> {
    out.write_all(line.as_bytes())
        .and_then(|()| out.write_all(b"\n"))
        .map_err(write_failure)
}

fn write_failure(e: io::Error) -> Exit {
    if e.kind() == io::ErrorKind::BrokenPipe {
        Exit::Quiet
    } else {
        Exit::Fail(format!("{PROG}: write error: {e}"))
    }
}

/// Run the reader with `argv` (including the program name).
pub fn run<I, T>(argv: I) -> Exit
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let command = Args::command().arg(
        Arg::new("version")
            .long("version")
            .action(ArgAction::Version)
            .help("show program's version number and exit"),
    );
    let matches = match command.try_get_matches_from(argv) {
        Ok(m) => m,
        Err(e) if matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) => {
            return Exit::Printed(e.render().to_string());
        }
        Err(e) => return usage_error(&e),
    };
    let args = match Args::from_arg_matches(&matches) {
        Ok(a) => a,
        Err(e) => return usage_error(&e),
    };
    match execute(&args) {
        Ok(()) => Exit::Ok,
        Err(exit) => exit,
    }
}

fn execute(args: &Args) -> Result<(), Exit> {
    let since = parse_since(&args.since).map_err(Exit::Fail)?;
    let keep = compile("--grep", &args.grep)?;
    let drop = compile("--exclude", &args.exclude)?;
    let stems = parse_stems(&args.logs);
    let stems = (!stems.is_empty()).then_some(stems);

    let files = files::resolve_log_files(&args.app, stems.as_deref()).map_err(Exit::Fail)?;
    if files.is_empty() {
        if let Some(stems) = &stems {
            let all = files::resolve_log_files(&args.app, None).map_err(Exit::Fail)?;
            let mut available: Vec<String> = all.iter().map(|p| stem_of(p)).collect();
            available.sort();
            available.dedup();
            let available = if available.is_empty() {
                "(none)".to_string()
            } else {
                available.join(", ")
            };
            return Err(Exit::Fail(format!(
                "No log files matched stems {} in app '{}'. Available: {available}",
                repr_str_list(stems),
                args.app
            )));
        }
        return Err(Exit::Fail(format!(
            "No log files found for app '{}'",
            args.app
        )));
    }

    let cutoff = timestamp::now().saturating_sub(since);
    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());
    let mut wrote_a_line = false;
    for line in MergedLines::new(&files, cutoff) {
        let line = line.map_err(|e| Exit::Fail(format!("{PROG}: {e}")))?;
        if line_passes(&line, keep.as_ref(), drop.as_ref()) {
            write_line(&mut out, &line)?;
            wrote_a_line = true;
        }
    }
    out.flush().map_err(write_failure)?;
    std::mem::drop(out);

    if !args.follow {
        if !wrote_a_line {
            let newest = merge::newest_log_timestamp(&files)
                .map_err(|e| Exit::Fail(format!("{PROG}: {e}")))?;
            let newest =
                newest.map_or_else(|| "no timestamped entry".to_string(), format_timestamp);
            let root = files[0].parent().map(PathBuf::from).unwrap_or_default();
            let mut err = io::stderr().lock();
            let _ = err.write_all(b"No lines matched. Resolved log root: ");
            let _ = err.write_all(root.as_os_str().as_bytes());
            let _ = writeln!(err, ". Newest entry in that root: {newest}.");
        }
        return Ok(());
    }

    let targets = follow_targets(&args.app, &files, stems.as_deref())?;
    follow(targets, keep, drop)
}

/// The canonical files to follow: every non-rotation file found, or for
/// `--logs` each requested stem's `<stem>.log`, whether or not it exists yet.
fn follow_targets(
    app: &str,
    files: &[PathBuf],
    stems: Option<&[String]>,
) -> Result<Vec<PathBuf>, Exit> {
    let Some(stems) = stems else {
        return Ok(files
            .iter()
            .filter(|p| !is_rotation_suffix(p))
            .cloned()
            .collect());
    };
    let dir = paths::resolve_log_dir(app).map_err(Exit::Fail)?;
    let mut targets: Vec<PathBuf> = Vec::new();
    for stem in stems {
        let path = paths::child(&dir, std::ffi::OsStr::new(&format!("{stem}.log")));
        if !targets.contains(&path) {
            targets.push(path);
        }
    }
    Ok(targets)
}

enum Event {
    Interrupted,
    Done(Exit),
}

fn follow(targets: Vec<PathBuf>, keep: Option<Regex>, drop: Option<Regex>) -> Result<(), Exit> {
    if targets.is_empty() {
        return Ok(());
    }
    let (tx, rx) = mpsc::channel::<Event>();
    let interrupt = tx.clone();
    ctrlc::set_handler(move || {
        let _ = interrupt.send(Event::Interrupted);
    })
    .map_err(|e| Exit::Fail(format!("{PROG}: cannot install the Ctrl-C handler: {e}")))?;

    let filters = Arc::new((keep, drop));
    let out = Arc::new(Mutex::new(io::stdout()));
    let mut handles = Vec::new();
    for path in targets {
        let filters = Arc::clone(&filters);
        let out = Arc::clone(&out);
        let tx = tx.clone();
        handles.push(thread::spawn(move || {
            let mut follower = Follower::new(path, true);
            let mut emit = |line: &str| -> io::Result<()> {
                if !line_passes(line, filters.0.as_ref(), filters.1.as_ref()) {
                    return Ok(());
                }
                let Ok(mut guard) = out.lock() else {
                    return Ok(());
                };
                guard.write_all(line.as_bytes())?;
                guard.write_all(b"\n")?;
                guard.flush()
            };
            loop {
                match follower.step(&mut emit) {
                    Ok(Step::Busy) => {}
                    Ok(Step::Idle) => thread::sleep(POLL_INTERVAL),
                    Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {
                        let _ = tx.send(Event::Done(Exit::Quiet));
                        return;
                    }
                    // Like today's tool, a follow that cannot read its file
                    // ends quietly; the others keep going.
                    Err(_) => return,
                }
            }
        }));
    }
    std::mem::drop(tx);

    loop {
        match rx.recv_timeout(POLL_INTERVAL) {
            Ok(Event::Interrupted) => return Ok(()),
            Ok(Event::Done(exit)) => return Err(exit),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if handles.iter().all(thread::JoinHandle::is_finished) {
                    return Ok(());
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stderr_of(argv: &[&str]) -> (i32, String) {
        match run(std::iter::once(PROG).chain(argv.iter().copied())) {
            Exit::Usage(msg) => (2, msg),
            Exit::Fail(msg) => (1, msg),
            Exit::Printed(msg) => (0, msg),
            Exit::Ok | Exit::Quiet => (0, String::new()),
            Exit::Silent(code) => (code, String::new()),
        }
    }

    #[test]
    fn argparse_shaped_usage_errors() {
        let (code, msg) = stderr_of(&[]);
        assert_eq!(code, 2);
        assert!(msg.starts_with(USAGE));
        assert!(
            msg.ends_with("instrukt-ai-logs: error: the following arguments are required: app\n")
        );
        assert!(
            stderr_of(&["a", "--since"])
                .1
                .ends_with("error: argument --since: expected one argument\n")
        );
        assert!(
            stderr_of(&["a", "--logs"])
                .1
                .ends_with("error: argument --logs/--include: expected one argument\n")
        );
        assert!(
            stderr_of(&["a", "b"])
                .1
                .ends_with("error: unrecognized arguments: b\n")
        );
        assert!(
            stderr_of(&["a", "--foo"])
                .1
                .ends_with("error: unrecognized arguments: --foo\n")
        );
    }

    #[test]
    fn version_and_help() {
        let (code, msg) = stderr_of(&["--version"]);
        assert_eq!(
            (code, msg.as_str()),
            (
                0,
                concat!("instrukt-ai-logs ", env!("CARGO_PKG_VERSION"), "\n")
            )
        );
        assert!(stderr_of(&["-h"]).1.contains("--include"));
    }

    #[test]
    fn invalid_regex_is_a_clear_error() {
        let (code, msg) = stderr_of(&["app", "--grep", "("]);
        assert_eq!(code, 1);
        assert!(msg.starts_with("Invalid --grep regex \"(\""), "{msg}");
    }

    #[test]
    fn filters_and_stems() {
        let keep = Regex::new("a(?=b)").unwrap();
        let drop = Regex::new("x").unwrap();
        assert!(line_passes("ab", Some(&keep), None));
        assert!(!line_passes("ac", Some(&keep), None));
        assert!(!line_passes("abx", Some(&keep), Some(&drop)));
        assert!(line_passes("anything", None, None));
        assert_eq!(parse_stems(" cron , ,docs-watch,"), ["cron", "docs-watch"]);
        assert!(parse_stems(" , ").is_empty());
    }
}
