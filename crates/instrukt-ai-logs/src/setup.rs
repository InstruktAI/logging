//! The `instrukt-ai-log-setup` command (today's `instrukt_ai_logging/install.py`).
//!
//! Rotation runs entirely as the producing user: a per-user rotation conf plus
//! a per-user scheduler (launchd LaunchAgent with newsyslog on macOS, systemd
//! user timer with logrotate elsewhere; chosen at compile time). Files are
//! byte-identical to today's, at the same paths (under `~/.config`, not
//! `$XDG_CONFIG_HOME`), and the same scheduler commands run.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use clap::error::ErrorKind;
use clap::{Arg, ArgAction};

use crate::pyfmt::{os_error, os_error_path, py_strip, repr_str_list};
use crate::{Exit, paths};

const PROG: &str = "instrukt-ai-log-setup";
/// The launchd job label.
pub const LAUNCHD_LABEL: &str = "ai.instrukt.log-rotate";
/// The systemd user unit name (without suffix).
pub const SYSTEMD_UNIT_NAME: &str = "instrukt-ai-logrotate";
const NEWSYSLOG_BIN: &str = "/usr/sbin/newsyslog";
const ARCHIVE_DEPTH: u32 = 30;
const SCHEDULER_TIMEOUT: Duration = Duration::from_secs(10);

/// The one newsyslog line: gzip-compressed archive retention.
pub fn newsyslog_conf(log_root: &Path) -> String {
    format!(
        "{}/*/*.log  640  {ARCHIVE_DEPTH}  50000  *  ZG\n",
        log_root.display()
    )
}

/// The LaunchAgent plist running newsyslog every 30 minutes.
pub fn launchd_plist(conf_path: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LAUNCHD_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{NEWSYSLOG_BIN}</string>
        <string>-r</string>
        <string>-f</string>
        <string>{}</string>
    </array>
    <key>StartInterval</key>
    <integer>1800</integer>
    <key>RunAtLoad</key>
    <true/>
</dict>
</plist>
"#,
        conf_path.display()
    )
}

/// The logrotate conf: move+create rotation; writers reopen on inode change.
pub fn logrotate_conf(log_root: &Path) -> String {
    format!(
        "{}/*/*.log {{
    size 50M
    rotate {ARCHIVE_DEPTH}
    compress
    delaycompress
    missingok
    notifempty
}}
",
        log_root.display()
    )
}

/// The oneshot systemd service running logrotate with a user state file.
pub fn systemd_service(conf_path: &Path, state_file: &Path) -> String {
    format!(
        "[Unit]
Description=InstruktAI log rotation

[Service]
Type=oneshot
ExecStart=logrotate --state {} {}
",
        state_file.display(),
        conf_path.display()
    )
}

/// The systemd timer firing every 30 minutes.
pub fn systemd_timer() -> String {
    "[Unit]
Description=InstruktAI log rotation timer

[Timer]
OnCalendar=*:0/30
Persistent=true

[Install]
WantedBy=timers.target
"
    .to_string()
}

fn config_dir() -> PathBuf {
    paths::home_dir().join(".config").join("instrukt-ai")
}

/// `~/.config/instrukt-ai/newsyslog.conf`.
pub fn newsyslog_conf_path() -> PathBuf {
    config_dir().join("newsyslog.conf")
}

/// `~/Library/LaunchAgents/ai.instrukt.log-rotate.plist`.
pub fn launchd_plist_path() -> PathBuf {
    paths::home_dir()
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LAUNCHD_LABEL}.plist"))
}

/// `~/.config/instrukt-ai/logrotate.conf`.
pub fn logrotate_conf_path() -> PathBuf {
    config_dir().join("logrotate.conf")
}

fn systemd_user_dir() -> PathBuf {
    paths::home_dir()
        .join(".config")
        .join("systemd")
        .join("user")
}

/// `~/.config/systemd/user/instrukt-ai-logrotate.service`.
pub fn systemd_service_path() -> PathBuf {
    systemd_user_dir().join(format!("{SYSTEMD_UNIT_NAME}.service"))
}

/// `~/.config/systemd/user/instrukt-ai-logrotate.timer`.
pub fn systemd_timer_path() -> PathBuf {
    systemd_user_dir().join(format!("{SYSTEMD_UNIT_NAME}.timer"))
}

/// `Path.mkdir(parents=True, exist_ok=True)`, failing with the same path
/// Python would name.
fn mkdir_parents(path: &Path) -> Result<(), String> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            match path.parent() {
                Some(parent) if parent != path && !parent.as_os_str().is_empty() => {
                    mkdir_parents(parent)?;
                }
                _ => return Err(os_error_path(&e, path)),
            }
            match fs::create_dir(path) {
                Ok(()) => Ok(()),
                Err(e) if path.is_dir() => {
                    let _ = e;
                    Ok(())
                }
                Err(e) => Err(os_error_path(&e, path)),
            }
        }
        Err(_) if path.is_dir() => Ok(()),
        Err(e) => Err(os_error_path(&e, path)),
    }
}

/// Write `content` unless the file already holds exactly it.
fn write_if_changed(path: &Path, content: &str) -> Result<(), String> {
    if path.exists() {
        match fs::read(path) {
            Ok(existing) if existing == content.as_bytes() => return Ok(()),
            Ok(_) => {}
            Err(e) => return Err(os_error_path(&e, path)),
        }
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            mkdir_parents(parent)?;
        }
    }
    fs::write(path, content).map_err(|e| os_error_path(&e, path))
}

/// The outcome of one scheduler command.
struct Completed {
    code: Option<i32>,
    stderr: String,
}

/// Run a scheduler command found on `PATH`: stdin closed, output captured,
/// killed after 10 s. `Err` is today's message for a command that could not
/// run or timed out.
fn run_scheduler_command(argv: &[String]) -> Result<Completed, String> {
    run_with_timeout(argv, SCHEDULER_TIMEOUT)
}

/// [`run_scheduler_command`] with an explicit timeout. The timeout message
/// renders the seconds like Python's float `repr` (`10.0`).
fn run_with_timeout(argv: &[String], timeout: Duration) -> Result<Completed, String> {
    let Some((program, args)) = argv.split_first() else {
        return Err("empty command".to_string());
    };
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| os_error(&e, Some(program)))?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let out_reader = thread::spawn(move || {
        let mut sink = Vec::new();
        if let Some(mut s) = stdout {
            let _ = s.read_to_end(&mut sink);
        }
    });
    let err_reader = thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut s) = stderr {
            let _ = s.read_to_end(&mut buf);
        }
        buf
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "Command '{}' timed out after {:?} seconds",
                    repr_str_list(argv),
                    timeout.as_secs_f64()
                ));
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(e) => return Err(os_error(&e, None)),
        }
    };
    let _ = out_reader.join();
    let stderr = err_reader.join().unwrap_or_default();
    Ok(Completed {
        code: status.code(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_string()).collect()
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn uid() -> u32 {
    rustix::process::getuid().as_raw()
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn ensure_launchd_loaded(plist_path: &Path) -> Vec<String> {
    let uid = uid();
    let plist = plist_path.to_string_lossy();
    let bootstrap = match run_scheduler_command(&argv(&[
        "launchctl",
        "bootstrap",
        &format!("gui/{uid}"),
        &plist,
    ])) {
        Ok(r) => r,
        Err(e) => return vec![format!("launchctl bootstrap failed to run: {e}")],
    };
    if bootstrap.code == Some(0)
        || bootstrap
            .stderr
            .to_lowercase()
            .contains("already bootstrapped")
    {
        return Vec::new();
    }
    // macOS reports EIO(5), not "already bootstrapped", when re-bootstrapping
    // a loaded agent; `print` succeeding confirms it is loaded.
    let failed = vec![format!(
        "launchctl bootstrap failed: {}",
        py_strip(&bootstrap.stderr)
    )];
    match run_scheduler_command(&argv(&[
        "launchctl",
        "print",
        &format!("gui/{uid}/{LAUNCHD_LABEL}"),
    ])) {
        Ok(r) if r.code == Some(0) => Vec::new(),
        _ => failed,
    }
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
fn ensure_systemd_loaded() -> Vec<String> {
    match run_scheduler_command(&argv(&["systemctl", "--user", "daemon-reload"])) {
        Err(e) => return vec![format!("systemctl daemon-reload failed to run: {e}")],
        Ok(r) if r.code != Some(0) => {
            return vec![format!(
                "systemctl daemon-reload failed: {}",
                py_strip(&r.stderr)
            )];
        }
        Ok(_) => {}
    }
    let timer = format!("{SYSTEMD_UNIT_NAME}.timer");
    match run_scheduler_command(&argv(&["systemctl", "--user", "enable", "--now", &timer])) {
        Err(e) => vec![format!("systemctl enable failed to run: {e}")],
        Ok(r) if r.code != Some(0) => {
            vec![format!("systemctl enable failed: {}", py_strip(&r.stderr))]
        }
        Ok(_) => Vec::new(),
    }
}

/// The rotation conf and scheduler file this platform uses.
#[cfg(target_os = "macos")]
fn platform_paths() -> (PathBuf, PathBuf) {
    (newsyslog_conf_path(), launchd_plist_path())
}

#[cfg(not(target_os = "macos"))]
fn platform_paths() -> (PathBuf, PathBuf) {
    (logrotate_conf_path(), systemd_timer_path())
}

/// Idempotently write the rotation assets and load the scheduler. Returns the
/// problems found; empty means healthy.
#[cfg(target_os = "macos")]
pub fn ensure_rotation() -> Vec<String> {
    let log_root = paths::log_root();
    let conf_path = newsyslog_conf_path();
    let plist_path = launchd_plist_path();
    let written = write_if_changed(&conf_path, &newsyslog_conf(&log_root))
        .and_then(|()| write_if_changed(&plist_path, &launchd_plist(&conf_path)));
    if let Err(e) = written {
        return vec![format!("could not write rotation assets: {e}")];
    }
    ensure_launchd_loaded(&plist_path)
}

/// Idempotently write the rotation assets and load the scheduler. Returns the
/// problems found; empty means healthy.
#[cfg(not(target_os = "macos"))]
pub fn ensure_rotation() -> Vec<String> {
    let log_root = paths::log_root();
    let conf_path = logrotate_conf_path();
    let state_file = log_root.join("logrotate.state");
    let written = write_if_changed(&conf_path, &logrotate_conf(&log_root))
        .and_then(|()| {
            write_if_changed(
                &systemd_service_path(),
                &systemd_service(&conf_path, &state_file),
            )
        })
        .and_then(|()| write_if_changed(&systemd_timer_path(), &systemd_timer()));
    if let Err(e) = written {
        return vec![format!("could not write rotation assets: {e}")];
    }
    ensure_systemd_loaded()
}

/// Run setup with `argv` (including the program name).
pub fn run<I, T>(argv: I) -> Exit
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let command = clap::Command::new(PROG)
        .about("Ensure per-user log rotation for InstruktAI logs")
        .version(env!("CARGO_PKG_VERSION"))
        .disable_version_flag(true)
        .arg(
            Arg::new("version")
                .long("version")
                .action(ArgAction::Version)
                .help("show program's version number and exit"),
        );
    if let Err(e) = command.try_get_matches_from(argv) {
        return match e.kind() {
            ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
                Exit::Printed(e.render().to_string())
            }
            _ => Exit::Usage(e.render().to_string()),
        };
    }

    let (conf, scheduler) = platform_paths();
    let mut text = format!(
        "log root: {}\nrotation conf: {}\nscheduler: {}\n",
        paths::log_root().display(),
        conf.display(),
        scheduler.display()
    );
    let problems = ensure_rotation();
    for problem in &problems {
        text.push_str(&format!("problem: {problem}\n"));
    }
    if problems.is_empty() {
        text.push_str("status: healthy\n");
    }
    let mut out = io::stdout().lock();
    let written = out.write_all(text.as_bytes()).and_then(|()| out.flush());
    match written {
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Exit::Quiet,
        Err(e) => Exit::Fail(format!("{PROG}: write error: {e}")),
        Ok(()) if problems.is_empty() => Exit::Ok,
        Ok(()) => Exit::Silent(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_byte_identical_assets() {
        let root = Path::new("/s/instrukt-ai");
        assert_eq!(
            newsyslog_conf(root),
            "/s/instrukt-ai/*/*.log  640  30  50000  *  ZG\n"
        );
        let plist = launchd_plist(Path::new("/h/.config/instrukt-ai/newsyslog.conf"));
        assert!(plist.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist"));
        assert!(plist.contains("        <string>/h/.config/instrukt-ai/newsyslog.conf</string>\n"));
        assert!(plist.ends_with("    <true/>\n</dict>\n</plist>\n"));
        assert_eq!(
            logrotate_conf(root),
            "/s/instrukt-ai/*/*.log {\n    size 50M\n    rotate 30\n    compress\n    delaycompress\n    missingok\n    notifempty\n}\n"
        );
        assert_eq!(
            systemd_service(
                Path::new("/c/logrotate.conf"),
                Path::new("/s/instrukt-ai/logrotate.state")
            ),
            "[Unit]\nDescription=InstruktAI log rotation\n\n[Service]\nType=oneshot\nExecStart=logrotate --state /s/instrukt-ai/logrotate.state /c/logrotate.conf\n"
        );
        assert!(systemd_timer().contains("OnCalendar=*:0/30\nPersistent=true\n"));
    }

    #[test]
    fn write_if_changed_is_idempotent_and_names_failures() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a/b/c.conf");
        write_if_changed(&p, "x\n").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "x\n");
        write_if_changed(&p, "x\n").unwrap();
        write_if_changed(&p, "y\n").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "y\n");

        let blocker = dir.path().join("file");
        fs::write(&blocker, "").unwrap();
        let err = write_if_changed(&blocker.join("sub/c.conf"), "x").unwrap_err();
        assert_eq!(
            err,
            format!(
                "[Errno 20] Not a directory: '{}'",
                blocker.join("sub").display()
            )
        );
    }

    /// Spec `log_rotation/retention-window` UC-RW1: the newsyslog line keeps
    /// the declared archive depth in its count field, rotates at 50 MB and
    /// gzip-compresses (`Z`) with glob expansion (`G`).
    #[test]
    fn uc_rw1_newsyslog_declares_the_archive_depth() {
        let line = newsyslog_conf(Path::new("/s/instrukt-ai"));
        let fields: Vec<&str> = line.split_whitespace().collect();
        assert_eq!(fields.len(), 6, "{line:?}");
        assert_eq!(fields[2].parse::<u32>().ok(), Some(ARCHIVE_DEPTH));
        assert_eq!(fields[3], "50000");
        assert_eq!(fields[5], "ZG");
        assert_eq!(ARCHIVE_DEPTH, 30);
    }

    /// Spec `log_rotation/retention-window` UC-RW2: the logrotate stanza keeps
    /// the same archive depth in its `rotate` directive and rotates at 50 MB.
    #[test]
    fn uc_rw2_logrotate_declares_the_same_archive_depth() {
        let conf = logrotate_conf(Path::new("/s/instrukt-ai"));
        let directive = |name: &str| {
            conf.lines()
                .map(str::trim)
                .find_map(|l| l.strip_prefix(name).map(str::trim).map(str::to_owned))
        };
        assert_eq!(
            directive("rotate ").and_then(|v| v.parse::<u32>().ok()),
            Some(ARCHIVE_DEPTH)
        );
        assert_eq!(directive("size ").as_deref(), Some("50M"));
        assert!(conf.lines().any(|l| l.trim() == "compress"));
    }

    #[test]
    fn scheduler_timeout_is_reported_like_python() {
        let started = Instant::now();
        let err = run_with_timeout(&argv(&["sleep", "5"]), Duration::from_millis(200))
            .err()
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(4));
        assert_eq!(err, "Command '['sleep', '5']' timed out after 0.2 seconds");
        assert_eq!(
            format!("{:?}", SCHEDULER_TIMEOUT.as_secs_f64()),
            "10.0",
            "the production message keeps Python's '10.0 seconds'"
        );
    }

    #[test]
    fn missing_scheduler_is_reported_like_python() {
        let err = run_scheduler_command(&argv(&["/nonexistent/launchctl"]))
            .err()
            .unwrap();
        assert_eq!(
            err,
            "[Errno 2] No such file or directory: '/nonexistent/launchctl'"
        );
    }
}
