//! An append-only log file that follows rotation, like Python's
//! `logging.handlers.WatchedFileHandler`, and that only this user can read:
//! directories are `0700`, the file is `0600`, and a symbolic link at the log
//! path is never followed.

use std::fs::{self, DirBuilder, File, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use crate::Error;

/// Mode of every directory the writer creates, and of the app's log directory.
pub(crate) const DIR_MODE: u32 = 0o700;
/// Mode of the log file.
pub(crate) const FILE_MODE: u32 = 0o600;

/// Identity of the file a path refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileId {
    dev: u64,
    ino: u64,
}

#[cfg(unix)]
fn file_id(meta: &fs::Metadata) -> Option<FileId> {
    use std::os::unix::fs::MetadataExt;
    Some(FileId {
        dev: meta.dev(),
        ino: meta.ino(),
    })
}

#[cfg(not(unix))]
fn file_id(_meta: &fs::Metadata) -> Option<FileId> {
    // No stable dev/inode outside unix: only a missing path triggers a reopen.
    None
}

#[derive(Debug)]
struct Open {
    file: Option<File>,
    id: Option<FileId>,
}

/// Write `bytes` in exactly one `write(2)` on the `O_APPEND` descriptor, so
/// that lines from several processes appending to one file never interleave.
/// A short write is an error, never followed by a second write: the rest would
/// land after another process's line. A write interrupted before it wrote
/// anything (`EINTR`) is retried; it wrote nothing.
fn write_once(file: &mut File, bytes: &[u8]) -> io::Result<()> {
    loop {
        match file.write(bytes) {
            Ok(n) if n == bytes.len() => return Ok(()),
            Ok(n) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    format!("short write: {n} of {} bytes", bytes.len()),
                ));
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}

/// Thread-safe handle to the log file. Each line goes out in one `write`.
#[derive(Debug)]
pub(crate) struct WatchedFile {
    path: PathBuf,
    state: Mutex<Open>,
}

/// Open `path` for append, creating it `0600`. A symbolic link at `path` is
/// refused (`O_NOFOLLOW`, `ELOOP`), and an existing file is set to `0600`.
fn open(path: &Path) -> io::Result<(File, Option<FileId>)> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(FILE_MODE)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let meta = file.metadata()?;
    if meta.permissions().mode() & 0o7777 != FILE_MODE {
        file.set_permissions(Permissions::from_mode(FILE_MODE))?;
    }
    Ok((file, file_id(&meta)))
}

/// Set `path` to `mode` unless it already has exactly that mode.
fn tighten(path: &Path, mode: u32) -> io::Result<()> {
    if fs::metadata(path)?.permissions().mode() & 0o7777 != mode {
        fs::set_permissions(path, Permissions::from_mode(mode))?;
    }
    Ok(())
}

/// Create `dir` and its missing ancestors, each `0700` whatever the umask,
/// then set `dir` itself (the app's log directory) to `0700` if it already
/// existed with another mode. Fails with the directory that could not be
/// created or set.
fn ensure_dir(dir: &Path) -> Result<(), (PathBuf, io::Error)> {
    let missing: Vec<&Path> = dir
        .ancestors()
        .take_while(|d| !d.as_os_str().is_empty() && fs::symlink_metadata(d).is_err())
        .collect();
    for d in missing.into_iter().rev() {
        match DirBuilder::new().mode(DIR_MODE).create(d) {
            Ok(()) => tighten(d, DIR_MODE).map_err(|e| (d.to_path_buf(), e))?,
            // Created concurrently by another process: its mode is its own.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && d.is_dir() => {}
            Err(e) => return Err((d.to_path_buf(), e)),
        }
    }
    tighten(dir, DIR_MODE).map_err(|e| (dir.to_path_buf(), e))
}

impl WatchedFile {
    /// Create the parent directory (`0700`) and open `path` for append
    /// (`0600`, never through a symbolic link). Fails fast.
    pub(crate) fn open(path: PathBuf) -> Result<Self, Error> {
        if let Some(dir) = path.parent() {
            ensure_dir(dir).map_err(|(path, source)| Error::CreateDir { path, source })?;
        }
        let (file, id) = open(&path).map_err(|source| Error::OpenFile {
            path: path.clone(),
            source,
        })?;
        Ok(Self {
            path,
            state: Mutex::new(Open {
                file: Some(file),
                id,
            }),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Append `line`, reopening first if the path no longer refers to the
    /// open file (moved or removed by rotation).
    pub(crate) fn write_line(&self, line: &str) -> io::Result<()> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        // `symlink_metadata`: a symbolic link planted at the path is a new
        // identity, so the reopen below sees it, and refuses it.
        let current = fs::symlink_metadata(&self.path).ok();
        let stale = match (&current, state.id) {
            (None, _) => true,
            (Some(meta), Some(id)) => file_id(meta) != Some(id),
            (Some(_), None) => false,
        };
        if stale || state.file.is_none() {
            state.file = None;
            let (file, id) = open(&self.path)?;
            state.file = Some(file);
            state.id = id;
        }
        match state.file.as_mut() {
            Some(file) => write_once(file, line.as_bytes()),
            None => Err(io::Error::other("log file is not open")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap_or_default()
    }

    #[test]
    fn creates_dir_and_empty_file_on_open() -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("a/b/app.log");
        let w = WatchedFile::open(path.clone())?;
        assert_eq!(w.path(), path);
        assert_eq!(fs::metadata(&path)?.len(), 0);
        Ok(())
    }

    #[test]
    fn appends_to_existing_content() -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("app.log");
        fs::write(&path, "old\n")?;
        let w = WatchedFile::open(path.clone())?;
        w.write_line("new\n")?;
        assert_eq!(read(&path), "old\nnew\n");
        Ok(())
    }

    #[test]
    fn open_failure_is_an_error() -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let blocker = tmp.path().join("file");
        fs::write(&blocker, "")?;
        let res = WatchedFile::open(blocker.join("sub/app.log"));
        assert!(matches!(res, Err(Error::CreateDir { .. })));
        let dir_as_file = tmp.path().join("dir.log");
        fs::create_dir(&dir_as_file)?;
        assert!(matches!(
            WatchedFile::open(dir_as_file),
            Err(Error::OpenFile { .. })
        ));
        Ok(())
    }

    #[test]
    fn rename_mid_run_reopens_at_original_path() -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("app.log");
        let rotated = tmp.path().join("app.log.0");
        let w = WatchedFile::open(path.clone())?;
        w.write_line("one\n")?;
        fs::rename(&path, &rotated)?;
        w.write_line("two\n")?;
        assert_eq!(read(&rotated), "one\n");
        assert_eq!(read(&path), "two\n");
        Ok(())
    }

    #[test]
    fn removal_mid_run_recreates_the_file() -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("app.log");
        let w = WatchedFile::open(path.clone())?;
        w.write_line("one\n")?;
        fs::remove_file(&path)?;
        w.write_line("two\n")?;
        assert_eq!(read(&path), "two\n");
        Ok(())
    }

    fn mode(path: &Path) -> u32 {
        fs::symlink_metadata(path).map_or(0, |m| m.permissions().mode() & 0o7777)
    }

    #[test]
    fn dirs_are_0700_and_the_file_0600_whatever_the_umask() -> Result<(), Box<dyn std::error::Error>>
    {
        // The test process runs under umask 022 (the default); the modes must
        // not depend on it. `umask(2)` is process-wide, so it is not changed.
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("state/instrukt-ai/app/app.log");
        WatchedFile::open(path.clone())?;
        assert_eq!(mode(&path), 0o600);
        for dir in ["state", "state/instrukt-ai", "state/instrukt-ai/app"] {
            assert_eq!(mode(&tmp.path().join(dir)), 0o700, "{dir}");
        }
        Ok(())
    }

    #[test]
    fn an_existing_file_and_app_dir_are_tightened() -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let dir = tmp.path().join("app");
        fs::create_dir(&dir)?;
        fs::set_permissions(&dir, Permissions::from_mode(0o755))?;
        let path = dir.join("app.log");
        fs::write(&path, "old\n")?;
        fs::set_permissions(&path, Permissions::from_mode(0o644))?;
        WatchedFile::open(path.clone())?;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&path), 0o600);
        // The parent of the app dir existed already: its mode is left alone.
        assert_ne!(mode(tmp.path()), 0);
        Ok(())
    }

    #[test]
    fn a_rotated_in_file_is_tightened_on_reopen() -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("app.log");
        let w = WatchedFile::open(path.clone())?;
        fs::rename(&path, tmp.path().join("app.log.0"))?;
        fs::write(&path, "")?;
        fs::set_permissions(&path, Permissions::from_mode(0o640))?;
        w.write_line("x\n")?;
        assert_eq!(mode(&path), 0o600);
        Ok(())
    }

    #[test]
    fn a_symlink_at_the_log_path_is_refused_at_open() -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let target = tmp.path().join("elsewhere");
        fs::write(&target, "")?;
        let path = tmp.path().join("app.log");
        std::os::unix::fs::symlink(&target, &path)?;
        match WatchedFile::open(path) {
            Err(Error::OpenFile { source, .. }) => {
                assert_eq!(source.raw_os_error(), Some(libc::ELOOP));
            }
            other => return Err(format!("expected OpenFile(ELOOP), got {other:?}").into()),
        }
        assert_eq!(fs::read_to_string(&target)?, "");
        Ok(())
    }

    #[test]
    fn a_symlink_planted_after_rotation_is_refused() -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let target = tmp.path().join("elsewhere");
        fs::write(&target, "")?;
        let path = tmp.path().join("app.log");
        let w = WatchedFile::open(path.clone())?;
        fs::rename(&path, tmp.path().join("app.log.0"))?;
        std::os::unix::fs::symlink(&target, &path)?;
        let err = w
            .write_line("secret\n")
            .err()
            .ok_or("the write must fail")?;
        assert_eq!(err.raw_os_error(), Some(libc::ELOOP));
        assert_eq!(fs::read_to_string(&target)?, "");
        Ok(())
    }

    #[test]
    fn concurrent_writers_never_interleave_lines() -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("app.log");
        let w = std::sync::Arc::new(WatchedFile::open(path.clone())?);
        let handles: Vec<_> = (0..8)
            .map(|t| {
                let w = w.clone();
                std::thread::spawn(move || {
                    for i in 0..200 {
                        let _ = w.write_line(&format!("t{t} i{i} {}\n", "x".repeat(100)));
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().map_err(|_| "writer thread panicked")?;
        }
        let content = read(&path);
        assert_eq!(content.lines().count(), 1600);
        assert!(content.lines().all(|l| l.ends_with(&"x".repeat(100))));
        Ok(())
    }
}
