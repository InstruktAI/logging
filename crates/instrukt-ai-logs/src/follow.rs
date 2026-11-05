//! `tail -f` style following of one file (0.6.x's `iter_follow_lines`).
//!
//! The first open starts at the end when the file exists at start; every
//! other open (after rotation, or once a missing file appears) starts at the
//! beginning. Rotation is an inode
//! change at the path; truncation is the read position passing the size.
//! Line endings follow Python's universal-newline reading: `\r\n` and a lone
//! `\r` both end a line, and every emitted line ends in `\n`.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::time::Duration;

/// Polling interval between checks when no new data arrived.
pub const POLL_INTERVAL: Duration = Duration::from_millis(250);

const READ_CHUNK: usize = 64 * 1024;

/// What one [`Follower::step`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Made progress (read data or reopened); step again right away.
    Busy,
    /// Nothing new; wait one poll interval before stepping again.
    Idle,
}

/// Follows one path across rotation and truncation.
pub struct Follower {
    path: PathBuf,
    file: Option<File>,
    inode: Option<u64>,
    pos: u64,
    start_at_end: bool,
    pending: Vec<u8>,
}

impl Follower {
    /// A follower for `path`; with `start_at_end` the first open skips the
    /// content already present.
    pub fn new(path: PathBuf, start_at_end: bool) -> Self {
        Self {
            path,
            file: None,
            inode: None,
            pos: 0,
            start_at_end,
            pending: Vec::new(),
        }
    }

    /// Emit complete lines in `pending`; with `flush`, a trailing partial line
    /// too (the file it came from is going away).
    fn drain(
        &mut self,
        flush: bool,
        emit: &mut dyn FnMut(&str) -> io::Result<()>,
    ) -> io::Result<()> {
        let mut start = 0;
        let mut i = 0;
        while i < self.pending.len() {
            let b = self.pending[i];
            if b == b'\n' || b == b'\r' {
                if b == b'\r' && i + 1 == self.pending.len() && !flush {
                    break; // may be the first half of `\r\n`
                }
                let line = String::from_utf8_lossy(&self.pending[start..i]).into_owned();
                emit(&line)?;
                i += if b == b'\r' && self.pending.get(i + 1) == Some(&b'\n') {
                    2
                } else {
                    1
                };
                start = i;
                continue;
            }
            i += 1;
        }
        self.pending.drain(..start);
        if flush && !self.pending.is_empty() {
            let line = String::from_utf8_lossy(&self.pending).into_owned();
            self.pending.clear();
            emit(&line)?;
        }
        Ok(())
    }

    fn close(&mut self, emit: &mut dyn FnMut(&str) -> io::Result<()>) -> io::Result<()> {
        self.file = None;
        self.inode = None;
        self.drain(true, emit)
    }

    /// One unit of work. `emit` receives each line without its terminator.
    /// An `Err` from opening or reading ends the follow for this path; an
    /// `Err` from `emit` is passed through.
    pub fn step(&mut self, emit: &mut dyn FnMut(&str) -> io::Result<()>) -> io::Result<Step> {
        if self.file.is_none() {
            let mut file = match File::open(&self.path) {
                Ok(f) => f,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    // A file that appears later is read from its beginning:
                    // everything in it was written after the follow started.
                    self.start_at_end = false;
                    return Ok(Step::Idle);
                }
                Err(e) => return Err(e),
            };
            self.inode = file.metadata().ok().map(|m| m.ino());
            self.pos = if self.start_at_end {
                file.seek(SeekFrom::End(0))?
            } else {
                0
            };
            self.start_at_end = false;
            self.file = Some(file);
        }
        let Some(file) = self.file.as_mut() else {
            return Ok(Step::Idle);
        };

        let mut chunk = vec![0u8; READ_CHUNK];
        let n = file.read(&mut chunk)?;
        if n > 0 {
            self.pos += n as u64;
            self.pending.extend_from_slice(&chunk[..n]);
            self.drain(false, emit)?;
            return Ok(Step::Busy);
        }

        let meta = match fs::metadata(&self.path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.close(emit)?;
                return Ok(Step::Idle);
            }
            Err(e) => return Err(e),
        };
        if self.inode.is_some_and(|ino| ino != meta.ino()) {
            self.close(emit)?;
            return Ok(Step::Busy);
        }
        if self.pos > meta.len() {
            file.seek(SeekFrom::Start(0))?;
            self.pos = 0;
            self.drain(true, emit)?;
        }
        Ok(Step::Idle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn append(path: &std::path::Path, text: &str) {
        let mut f = File::options()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        f.write_all(text.as_bytes()).unwrap();
    }

    fn run(f: &mut Follower) -> Vec<String> {
        let mut out = Vec::new();
        for _ in 0..8 {
            if f.step(&mut |l| {
                out.push(l.to_string());
                Ok(())
            })
            .unwrap()
                == Step::Idle
            {
                break;
            }
        }
        out
    }

    #[test]
    fn starts_at_end_then_reads_appends() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.log");
        append(&p, "old\n");
        let mut f = Follower::new(p.clone(), true);
        assert!(run(&mut f).is_empty());
        append(&p, "one\ntw");
        assert_eq!(run(&mut f), ["one"]);
        append(&p, "o\r\nthree\rfour\n");
        assert_eq!(run(&mut f), ["two", "three", "four"]);
    }

    #[test]
    fn waits_for_creation_and_reads_from_start() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.log");
        let mut f = Follower::new(p.clone(), true);
        assert!(run(&mut f).is_empty());
        append(&p, "first\n");
        assert_eq!(run(&mut f), ["first"]);
        append(&p, "second\n");
        assert_eq!(run(&mut f), ["second"]);
    }

    #[test]
    fn rotation_reopens_from_start() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.log");
        append(&p, "");
        let mut f = Follower::new(p.clone(), true);
        assert!(run(&mut f).is_empty());
        append(&p, "before\npartial");
        assert_eq!(run(&mut f), ["before"]);
        fs::rename(&p, dir.path().join("a.log.0")).unwrap();
        append(&p, "after\n");
        assert_eq!(run(&mut f), ["partial", "after"]);
    }

    #[test]
    fn truncation_rereads_from_start() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.log");
        append(&p, "aaaaaaaaaaaaaaaaaaaa\n");
        let mut f = Follower::new(p.clone(), true);
        assert!(run(&mut f).is_empty());
        File::create(&p).unwrap();
        assert!(run(&mut f).is_empty());
        append(&p, "fresh\n");
        assert_eq!(run(&mut f), ["fresh"]);
    }
}
