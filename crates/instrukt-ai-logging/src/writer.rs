//! An append-only log file that follows rotation, like Python's
//! `logging.handlers.WatchedFileHandler`.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use crate::Error;

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

/// Thread-safe handle to the log file. Each line goes out in one `write`.
#[derive(Debug)]
pub(crate) struct WatchedFile {
    path: PathBuf,
    state: Mutex<Open>,
}

fn open(path: &Path) -> io::Result<(File, Option<FileId>)> {
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    let id = file_id(&file.metadata()?);
    Ok((file, id))
}

impl WatchedFile {
    /// Create the parent directory and open `path` for append. Fails fast.
    pub(crate) fn open(path: PathBuf) -> Result<Self, Error> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|source| Error::CreateDir {
                path: dir.to_path_buf(),
                source,
            })?;
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
        let current = fs::metadata(&self.path).ok();
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
            Some(file) => file.write_all(line.as_bytes()),
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
