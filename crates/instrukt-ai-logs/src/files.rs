//! Selecting an app's log files (0.6.x's `resolve_log_files`).

use std::fs;
use std::path::{Path, PathBuf};

use crate::paths;

/// The stem before the first `.log` (`cron.log.1` → `cron`); the whole name
/// when `.log` is absent or leads the name.
pub fn stem_of(path: &Path) -> String {
    let name = file_name(path);
    match name.find(".log") {
        Some(idx) if idx > 0 => name[..idx].to_string(),
        _ => name,
    }
}

/// True for `<stem>.log.<suffix>` rotation siblings, false for `<stem>.log`.
pub fn is_rotation_suffix(path: &Path) -> bool {
    let name = file_name(path);
    match name.find(".log") {
        Some(idx) if idx > 0 => name.len() > idx + ".log".len(),
        _ => false,
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// True when `name` belongs to the exact stem `stem`: `<stem>.log` or
/// `<stem>.log.<suffix>`. No glob expansion.
pub fn matches_stem(name: &str, stem: &str) -> bool {
    match name
        .strip_prefix(stem)
        .and_then(|rest| rest.strip_prefix(".log"))
    {
        Some("") => true,
        Some(rest) => rest.len() > 1 && rest.starts_with('.'),
        None => false,
    }
}

/// Every `*.log*` regular file in the app's log directory, or with `stems`
/// only those belonging to one of the exact stems ([`matches_stem`]).
/// Deduplicated and sorted by path. A missing or unreadable directory yields
/// no files. `Err` carries the app-name normalization message.
pub fn resolve_log_files(app: &str, stems: Option<&[String]>) -> Result<Vec<PathBuf>, String> {
    let dir = paths::resolve_log_dir(app)?;
    let Ok(entries) = fs::read_dir(&dir) else {
        return Ok(Vec::new());
    };
    let mut result: Vec<PathBuf> = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name();
        let text = name.to_string_lossy();
        let selected = match stems {
            None => text.contains(".log"),
            Some(stems) => stems.iter().any(|stem| matches_stem(&text, stem)),
        };
        if !selected {
            continue;
        }
        let path = dir.join(&name);
        if fs::metadata(&path).map(|m| m.is_file()).unwrap_or(false) {
            result.push(path);
        }
    }
    result.sort();
    result.dedup();
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_stem_selection() {
        assert!(matches_stem("cron.log", "cron"));
        assert!(matches_stem("cron.log.0", "cron"));
        assert!(matches_stem("cron.log.1.gz", "cron"));
        assert!(!matches_stem("cron.logfoo", "cron"));
        assert!(!matches_stem("cron.log.", "cron"));
        assert!(!matches_stem("cron-backup.log", "cron"));
        assert!(!matches_stem("cron.log", "*"));
        assert!(!matches_stem("cron.log", "cr"));
    }

    #[test]
    fn stems_and_rotation_suffixes() {
        assert_eq!(stem_of(Path::new("/d/cron.log.1")), "cron");
        assert_eq!(stem_of(Path::new("/d/app.log")), "app");
        assert_eq!(stem_of(Path::new("/d/.log")), ".log");
        assert!(is_rotation_suffix(Path::new("/d/cron.log.1")));
        assert!(is_rotation_suffix(Path::new("/d/cron.log.0.gz")));
        assert!(!is_rotation_suffix(Path::new("/d/cron.log")));
        assert!(!is_rotation_suffix(Path::new("/d/.log.1")));
    }
}
