//! Windowed reading and the timestamp merge across files.
//!
//! Mirrors 0.6.x's `iter_recent_log_lines_merged`, `_window_start_offset`,
//! `_tail_timestamp` and `newest_log_timestamp`: files are streamed, never
//! loaded; a short window over a large file seeks near its end via the
//! backward probe; the per-file streams are merged with a heap.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use flate2::read::MultiGzDecoder;

use crate::timestamp::{self, Micros};

/// Initial backward-probe size; doubles until the window start is inside it.
pub const WINDOW_PROBE_BYTES: u64 = 262_144;

/// A read failure, with the file it happened on.
#[derive(Debug)]
pub struct ReadError {
    /// The file being read.
    pub path: PathBuf,
    /// The underlying failure.
    pub error: io::Error,
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.error)
    }
}

impl std::error::Error for ReadError {}

fn is_gzip(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|n| n.as_encoded_bytes().ends_with(b".gz"))
}

/// Read one `\n`-terminated line (or the unterminated last one) into `buf`.
/// Returns false at end of input.
fn read_line(reader: &mut dyn BufRead, buf: &mut Vec<u8>) -> io::Result<bool> {
    buf.clear();
    Ok(reader.read_until(b'\n', buf)? > 0)
}

fn line_timestamp(raw: &[u8]) -> Option<Micros> {
    timestamp::parse_log_timestamp(&String::from_utf8_lossy(raw))
}

/// A byte offset at or before the first line newer than `cutoff`, found by
/// probing backward from EOF in doubling chunks. 0 when the window spans the
/// whole file or the file is no larger than one probe.
pub fn window_start_offset(path: &Path, cutoff: Micros) -> io::Result<u64> {
    let Ok(size) = fs::metadata(path).map(|m| m.len()) else {
        return Ok(0);
    };
    if size <= WINDOW_PROBE_BYTES {
        return Ok(0);
    }
    let mut reader = BufReader::new(File::open(path)?);
    let mut buf = Vec::new();
    let mut probe = WINDOW_PROBE_BYTES;
    while probe < size {
        let start = size - probe;
        reader.seek(SeekFrom::Start(start))?;
        read_line(&mut reader, &mut buf)?; // the partial line spanning the boundary
        let mut first_ts = None;
        while read_line(&mut reader, &mut buf)? {
            first_ts = line_timestamp(&buf);
            if first_ts.is_some() {
                break;
            }
        }
        if first_ts.is_some_and(|ts| ts < cutoff) {
            return Ok(start);
        }
        probe = probe.saturating_mul(2);
    }
    Ok(0)
}

/// The newest parsable timestamp near the tail of `path`; `None` when it holds
/// none or has vanished. Gzip archives are streamed from the start.
pub fn tail_timestamp(path: &Path) -> io::Result<Option<Micros>> {
    let mut buf = Vec::new();
    if is_gzip(path) {
        let file = match File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let mut reader = BufReader::new(MultiGzDecoder::new(file));
        let mut newest = None;
        while read_line(&mut reader, &mut buf)? {
            if let Some(ts) = line_timestamp(&buf) {
                newest = Some(ts);
            }
        }
        return Ok(newest);
    }

    let size = match fs::metadata(path) {
        Ok(m) => m.len(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut reader = BufReader::new(File::open(path)?);
    let mut probe = WINDOW_PROBE_BYTES;
    loop {
        let start = size.saturating_sub(probe);
        reader.seek(SeekFrom::Start(start))?;
        if start > 0 {
            read_line(&mut reader, &mut buf)?;
        }
        let mut newest = None;
        while read_line(&mut reader, &mut buf)? {
            if let Some(ts) = line_timestamp(&buf) {
                newest = Some(ts);
            }
        }
        if newest.is_some() || start == 0 {
            return Ok(newest);
        }
        probe = probe.saturating_mul(2);
    }
}

fn mtime(path: &Path) -> io::Result<Micros> {
    Ok(timestamp::from_system_time(fs::metadata(path)?.modified()?))
}

/// The newest parsable timestamp across `files`. Files are examined newest
/// mtime first; the scan stops once no remaining mtime can beat the best
/// timestamp found (a file's entries never postdate its own mtime).
pub fn newest_log_timestamp(files: &[PathBuf]) -> Result<Option<Micros>, ReadError> {
    let mut candidates: Vec<(Micros, &PathBuf)> = Vec::new();
    for path in files {
        match mtime(path) {
            Ok(m) => candidates.push((m, path)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(ReadError {
                    path: path.clone(),
                    error,
                });
            }
        }
    }
    candidates.sort_by_key(|c| std::cmp::Reverse(c.0));

    let mut best: Option<Micros> = None;
    for (m, path) in candidates {
        if best.is_some_and(|b| m <= b) {
            break;
        }
        let ts = tail_timestamp(path).map_err(|error| ReadError {
            path: path.clone(),
            error,
        })?;
        if let Some(ts) = ts {
            if best.is_none_or(|b| ts > b) {
                best = Some(ts);
            }
        }
    }
    Ok(best)
}

/// One file's lines inside the window, each with its sort key. Lines without
/// a timestamp inherit the previous timestamp of their own file.
struct FileStream {
    path: PathBuf,
    reader: Option<Box<dyn BufRead + Send>>,
    cutoff: Micros,
    last_ts: Option<Micros>,
    buf: Vec<u8>,
}

impl FileStream {
    fn new(path: PathBuf, cutoff: Micros) -> Self {
        Self {
            path,
            reader: None,
            cutoff,
            last_ts: None,
            buf: Vec::new(),
        }
    }

    fn open(&self) -> io::Result<Option<Box<dyn BufRead + Send>>> {
        let file = match File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        if is_gzip(&self.path) {
            return Ok(Some(Box::new(BufReader::new(MultiGzDecoder::new(file)))));
        }
        let mut reader = BufReader::new(file);
        let offset = window_start_offset(&self.path, self.cutoff)?;
        if offset > 0 {
            reader.seek(SeekFrom::Start(offset))?;
            let mut discard = Vec::new();
            read_line(&mut reader, &mut discard)?;
        }
        Ok(Some(Box::new(reader)))
    }

    fn next_line(&mut self, opened: &mut bool) -> io::Result<Option<(Micros, String)>> {
        if !*opened {
            *opened = true;
            self.reader = self.open()?;
        }
        let Some(reader) = self.reader.as_mut() else {
            return Ok(None);
        };
        loop {
            if !read_line(reader.as_mut(), &mut self.buf)? {
                self.reader = None;
                return Ok(None);
            }
            if self.buf.last() == Some(&b'\n') {
                self.buf.pop();
            }
            let line = String::from_utf8_lossy(&self.buf).into_owned();
            match timestamp::parse_log_timestamp(&line) {
                None => match self.last_ts {
                    Some(last) if last >= self.cutoff => return Ok(Some((last, line))),
                    _ => continue,
                },
                Some(ts) => {
                    self.last_ts = Some(ts);
                    if ts >= self.cutoff {
                        return Ok(Some((ts, line)));
                    }
                }
            }
        }
    }
}

/// Lines newer than `cutoff` across `files`, merged by timestamp.
///
/// Ties keep the order of `files` (callers pass them sorted by path), then
/// line order within a file. Yielded lines carry no trailing newline; a file
/// whose last line lacks one cannot glue it to the next file's line.
pub struct MergedLines {
    streams: Vec<FileStream>,
    opened: Vec<bool>,
    heads: Vec<Option<String>>,
    heap: BinaryHeap<Reverse<(Micros, usize)>>,
    primed: bool,
}

impl MergedLines {
    /// Prepare the merge. Files whose mtime is before `cutoff` (or that cannot
    /// be stat'ed) are skipped entirely.
    pub fn new(files: &[PathBuf], cutoff: Micros) -> Self {
        let streams: Vec<FileStream> = files
            .iter()
            .filter(|p| mtime(p).is_ok_and(|m| m >= cutoff))
            .map(|p| FileStream::new(p.clone(), cutoff))
            .collect();
        let n = streams.len();
        Self {
            streams,
            opened: vec![false; n],
            heads: vec![None; n],
            heap: BinaryHeap::with_capacity(n),
            primed: false,
        }
    }

    fn advance(&mut self, idx: usize) -> Result<(), ReadError> {
        let next = self.streams[idx]
            .next_line(&mut self.opened[idx])
            .map_err(|error| ReadError {
                path: self.streams[idx].path.clone(),
                error,
            })?;
        if let Some((ts, line)) = next {
            self.heads[idx] = Some(line);
            self.heap.push(Reverse((ts, idx)));
        }
        Ok(())
    }
}

impl Iterator for MergedLines {
    type Item = Result<String, ReadError>;

    fn next(&mut self) -> Option<Self::Item> {
        if !self.primed {
            self.primed = true;
            for idx in 0..self.streams.len() {
                if let Err(e) = self.advance(idx) {
                    return Some(Err(e));
                }
            }
        }
        let Reverse((_, idx)) = self.heap.pop()?;
        let line = self.heads[idx].take().unwrap_or_default();
        if let Err(e) = self.advance(idx) {
            return Some(Err(e));
        }
        Some(Ok(line))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn ts_line(base: Micros, offset_s: i64, msg: &str) -> String {
        format!(
            "{} level=INFO logger=app msg=\"{msg}\"",
            timestamp::format_timestamp(base + offset_s * 1_000_000)
        )
    }

    fn write(path: &Path, lines: &[String], newline_at_end: bool) {
        let mut f = File::create(path).unwrap();
        let body = lines.join("\n");
        f.write_all(body.as_bytes()).unwrap();
        if newline_at_end {
            f.write_all(b"\n").unwrap();
        }
    }

    fn collect(files: &[PathBuf], cutoff: Micros) -> Vec<String> {
        MergedLines::new(files, cutoff)
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    #[test]
    fn merges_by_timestamp_with_continuations_and_ties() {
        let dir = tempfile::tempdir().unwrap();
        let now = timestamp::now();
        let a = dir.path().join("a.log");
        let b = dir.path().join("b.log");
        write(
            &a,
            &[
                ts_line(now, -900, "old"),
                "old continuation".into(),
                ts_line(now, -100, "a1"),
                "a1 continuation".into(),
                ts_line(now, -50, "tie-a"),
            ],
            true,
        );
        write(
            &b,
            &[ts_line(now, -120, "b1"), ts_line(now, -50, "tie-b")],
            false,
        );
        let out = collect(&[a, b], now - 600 * 1_000_000);
        let msgs: Vec<&str> = out.iter().map(|l| l.rsplit(' ').next().unwrap()).collect();
        assert_eq!(
            msgs,
            [
                "msg=\"b1\"",
                "msg=\"a1\"",
                "continuation",
                "msg=\"tie-a\"",
                "msg=\"tie-b\""
            ]
        );
    }

    #[test]
    fn skips_files_older_than_the_cutoff() {
        let dir = tempfile::tempdir().unwrap();
        let now = timestamp::now();
        let a = dir.path().join("a.log");
        write(&a, &[ts_line(now, -10, "fresh-line-in-stale-file")], true);
        let old = ago(3600);
        File::options()
            .write(true)
            .open(&a)
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert!(collect(&[a], now - 600 * 1_000_000).is_empty());
    }

    fn ago(secs: u64) -> std::time::SystemTime {
        std::time::SystemTime::now() - std::time::Duration::from_secs(secs)
    }

    #[test]
    fn reads_gzip_members() {
        let dir = tempfile::tempdir().unwrap();
        let now = timestamp::now();
        let gz = dir.path().join("a.log.1.gz");
        let mut enc = flate2::write::GzEncoder::new(
            File::create(&gz).unwrap(),
            flate2::Compression::default(),
        );
        writeln!(enc, "{}", ts_line(now, -30, "zipped")).unwrap();
        enc.finish().unwrap();
        let out = collect(std::slice::from_ref(&gz), now - 600 * 1_000_000);
        assert_eq!(out.len(), 1);
        assert!(out[0].ends_with("msg=\"zipped\""));
        assert_eq!(
            tail_timestamp(&gz).unwrap(),
            Some(now - 30 * 1_000_000 - (now % 1000))
        );
    }

    #[test]
    fn backward_probe_finds_the_window_in_a_large_file() {
        let dir = tempfile::tempdir().unwrap();
        let now = timestamp::now();
        let path = dir.path().join("big.log");
        let mut lines = Vec::new();
        // ~2 MiB of old lines, then a few fresh ones.
        for i in 0..20_000 {
            lines.push(format!(
                "{} padding padding padding padding padding padding padding padding",
                ts_line(now, -7200 + i / 10, "old")
            ));
        }
        lines.push(ts_line(now, -30, "fresh"));
        write(&path, &lines, true);
        let size = fs::metadata(&path).unwrap().len();
        let cutoff = now - 600 * 1_000_000;
        let offset = window_start_offset(&path, cutoff).unwrap();
        assert!(
            offset > 0 && size - offset <= WINDOW_PROBE_BYTES,
            "{offset} of {size}"
        );
        let out = collect(std::slice::from_ref(&path), cutoff);
        assert_eq!(out.len(), 1);
        // The whole window older than the file: probe falls back to 0.
        assert_eq!(
            window_start_offset(&path, now - 86_400 * 1_000_000).unwrap(),
            0
        );
        assert_eq!(
            tail_timestamp(&path).unwrap(),
            Some(now - 30 * 1_000_000 - (now % 1000))
        );
    }

    #[test]
    fn large_file_keeps_a_continuation_after_the_seek_and_grows_to_the_start() {
        let dir = tempfile::tempdir().unwrap();
        let now = timestamp::now();
        let cutoff = now - 300 * 1_000_000;

        // Old body well past one probe, then a multi-line entry in the window.
        let tail = dir.path().join("tail.log");
        let mut lines: Vec<String> = (0..12_000)
            .map(|i| ts_line(now, -1800, &format!("old_line_{i}")))
            .collect();
        lines.push(ts_line(now, -60, "boom"));
        lines.push("Traceback (most recent call last):".into());
        lines.push(ts_line(now, -30, "after"));
        write(&tail, &lines, true);
        assert!(fs::metadata(&tail).unwrap().len() > WINDOW_PROBE_BYTES);
        let out = collect(std::slice::from_ref(&tail), cutoff);
        assert_eq!(out.len(), 3, "{out:?}");
        assert!(out[0].contains("msg=\"boom\""));
        assert!(out[1].starts_with("Traceback"));
        assert!(out[2].contains("msg=\"after\""));

        // Every line in the window: the probe grows to offset 0, nothing lost.
        let whole = dir.path().join("whole.log");
        let count = 12_000;
        let lines: Vec<String> = (0..count)
            .map(|i| ts_line(now, -60, &format!("line_{i}")))
            .collect();
        write(&whole, &lines, true);
        assert!(fs::metadata(&whole).unwrap().len() > WINDOW_PROBE_BYTES);
        assert_eq!(window_start_offset(&whole, cutoff).unwrap(), 0);
        let out = collect(std::slice::from_ref(&whole), cutoff);
        assert_eq!(out.len(), count);
        assert!(out[0].contains("msg=\"line_0\""));
        assert!(out[count - 1].contains(&format!("msg=\"line_{}\"", count - 1)));
    }

    #[test]
    fn newest_timestamp_across_files() {
        let dir = tempfile::tempdir().unwrap();
        let now = timestamp::now();
        let a = dir.path().join("a.log");
        let b = dir.path().join("b.log");
        write(&a, &[ts_line(now, -7200, "a")], true);
        write(&b, &["no timestamp".into()], true);
        let got = newest_log_timestamp(&[a, b]).unwrap();
        assert_eq!(got, Some(now - 7200 * 1_000_000 - (now % 1000)));
        assert_eq!(newest_log_timestamp(&[]).unwrap(), None);
    }
}
