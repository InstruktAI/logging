//! Env parsing and the selection rule: which events reach the file.

use tracing::Level;
use tracing::level_filters::LevelFilter;

use crate::env::EnvLookup;

/// Python's numeric levels. `CRITICAL` lets nothing tracing can emit
/// through. `NOTSET` on a prefix means the prefix has no level of its own (the
/// next shorter matching prefix, or the root, decides); as the root threshold
/// it lets everything through.
pub(crate) const NOTSET: u8 = 0;
pub(crate) const TRACE: u8 = 5;
pub(crate) const DEBUG: u8 = 10;
pub(crate) const INFO: u8 = 20;
pub(crate) const WARNING: u8 = 30;
pub(crate) const ERROR: u8 = 40;
pub(crate) const CRITICAL: u8 = 50;

pub(crate) fn level_number(level: Level) -> u8 {
    match level {
        Level::TRACE => TRACE,
        Level::DEBUG => DEBUG,
        Level::INFO => INFO,
        Level::WARN => WARNING,
        Level::ERROR => ERROR,
    }
}

/// The contract's level name for a tracing level (`WARN` → `WARNING`).
pub(crate) fn level_name(level: Level) -> &'static str {
    match level {
        Level::TRACE => "TRACE",
        Level::DEBUG => "DEBUG",
        Level::INFO => "INFO",
        Level::WARN => "WARNING",
        Level::ERROR => "ERROR",
    }
}

/// Case-insensitive level name → number; unknown, empty or unset → `default`.
pub(crate) fn parse_level(raw: Option<&str>, default: u8) -> u8 {
    let Some(raw) = raw else { return default };
    match raw.trim().to_ascii_uppercase().as_str() {
        "NOTSET" => NOTSET,
        "TRACE" => TRACE,
        "DEBUG" => DEBUG,
        "INFO" => INFO,
        "WARN" | "WARNING" => WARNING,
        "ERROR" => ERROR,
        "CRITICAL" | "FATAL" => CRITICAL,
        _ => default,
    }
}

/// Comma-separated list; items trimmed, empty items dropped.
pub(crate) fn parse_csv(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// `name` is `prefix` itself or lies under it, with `.` or `::` as separator.
pub(crate) fn under(name: &str, prefix: &str) -> bool {
    match name.strip_prefix(prefix) {
        Some("") => true,
        Some(rest) => rest.starts_with('.') || rest.starts_with("::"),
        None => false,
    }
}

#[derive(Debug, Clone)]
struct Rule {
    prefix: String,
    threshold: u8,
    muted: bool,
}

/// The resolved selection: per-prefix thresholds plus a root threshold.
#[derive(Debug, Clone)]
pub(crate) struct Selection {
    rules: Vec<Rule>,
    root: u8,
}

impl Selection {
    /// Build the selection for `env_prefix` from `env`.
    ///
    /// `app_prefixes` are the logger prefixes treated as the app's own (the
    /// normalized app name plus any extras from the builder).
    pub(crate) fn from_env(env_prefix: &str, app_prefixes: &[String], env: &EnvLookup) -> Self {
        let var = |suffix: &str| env.get_str(&format!("{env_prefix}_{suffix}"));
        let app_level = parse_level(var("LOG_LEVEL").as_deref(), INFO);
        let third_party_level = parse_level(var("THIRD_PARTY_LOG_LEVEL").as_deref(), WARNING);
        let spotlight = parse_csv(var("THIRD_PARTY_LOGGERS").as_deref());
        let muted = parse_csv(var("MUTED_LOGGERS").as_deref());
        Self::new(
            app_prefixes,
            app_level,
            third_party_level,
            &spotlight,
            &muted,
        )
    }

    pub(crate) fn new(
        app_prefixes: &[String],
        app_level: u8,
        third_party_level: u8,
        spotlight: &[String],
        muted: &[String],
    ) -> Self {
        let mut rules = Vec::new();
        for p in app_prefixes.iter().filter(|p| !p.is_empty()) {
            rules.push(Rule {
                prefix: p.clone(),
                threshold: app_level,
                muted: false,
            });
        }
        for p in spotlight {
            // A spotlight prefix inside the app's own namespace is not
            // third-party: the app level keeps governing it.
            if app_prefixes.iter().any(|a| !a.is_empty() && under(p, a)) {
                continue;
            }
            rules.push(Rule {
                prefix: p.clone(),
                threshold: third_party_level,
                muted: false,
            });
        }
        for p in muted {
            rules.push(Rule {
                prefix: p.clone(),
                threshold: WARNING,
                muted: true,
            });
        }
        let root = if spotlight.is_empty() {
            third_party_level
        } else {
            WARNING
        };
        Self { rules, root }
    }

    /// The threshold for logger `name`: the longest matching prefix with a
    /// level of its own wins, a muted prefix wins a tie, and without one the
    /// root threshold applies. A `NOTSET` prefix is skipped, like a Python
    /// logger whose level is `NOTSET` defers to its ancestors.
    pub(crate) fn threshold(&self, name: &str) -> u8 {
        let mut best: Option<&Rule> = None;
        for rule in &self.rules {
            if rule.threshold == NOTSET || !under(name, &rule.prefix) {
                continue;
            }
            let better = match best {
                None => true,
                Some(b) => {
                    rule.prefix.len() > b.prefix.len()
                        || (rule.prefix.len() == b.prefix.len() && rule.muted && !b.muted)
                }
            };
            if better {
                best = Some(rule);
            }
        }
        best.map_or(self.root, |r| r.threshold)
    }

    pub(crate) fn enabled(&self, name: &str, level: Level) -> bool {
        level_number(level) >= self.threshold(name)
    }

    /// The most verbose level any logger can pass at.
    pub(crate) fn max_level(&self) -> LevelFilter {
        let min = self
            .rules
            .iter()
            .map(|r| r.threshold)
            .filter(|t| *t != NOTSET)
            .chain(std::iter::once(self.root))
            .min()
            .unwrap_or(self.root);
        match min {
            m if m <= TRACE => LevelFilter::TRACE,
            m if m <= DEBUG => LevelFilter::DEBUG,
            m if m <= INFO => LevelFilter::INFO,
            m if m <= WARNING => LevelFilter::WARN,
            m if m <= ERROR => LevelFilter::ERROR,
            _ => LevelFilter::OFF,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_owned()).collect()
    }

    fn sel(app_level: u8, tp: u8, spotlight: &[&str], muted: &[&str]) -> Selection {
        Selection::new(&s(&["teleclaude"]), app_level, tp, &s(spotlight), &s(muted))
    }

    #[test]
    fn level_names_parse_case_insensitively_with_defaults() {
        assert_eq!(parse_level(Some(" debug "), INFO), DEBUG);
        assert_eq!(parse_level(Some("Warn"), INFO), WARNING);
        assert_eq!(parse_level(Some("fatal"), INFO), CRITICAL);
        assert_eq!(parse_level(Some("notset"), INFO), NOTSET);
        assert_eq!(parse_level(Some("trace"), INFO), TRACE);
        assert_eq!(parse_level(Some("verbose"), INFO), INFO);
        assert_eq!(parse_level(Some(""), WARNING), WARNING);
        assert_eq!(parse_level(None, WARNING), WARNING);
    }

    #[test]
    fn csv_trims_and_drops_empties() {
        assert_eq!(parse_csv(Some(" a, b.c ,,d ")), s(&["a", "b.c", "d"]));
        assert!(parse_csv(None).is_empty());
    }

    #[test]
    fn prefix_match_uses_dot_and_double_colon() {
        assert!(under("httpx", "httpx"));
        assert!(under("httpx.client", "httpx"));
        assert!(under("hyper::client", "hyper"));
        assert!(!under("httpxy", "httpx"));
        assert!(!under("hyper:x", "hyper"));
    }

    #[test]
    fn defaults_app_info_third_party_warning() {
        let s = sel(INFO, WARNING, &[], &[]);
        assert!(s.enabled("teleclaude", Level::INFO));
        assert!(s.enabled("teleclaude.core", Level::INFO));
        assert!(s.enabled("teleclaude::core", Level::INFO));
        assert!(!s.enabled("teleclaude.core", Level::DEBUG));
        assert!(!s.enabled("httpx", Level::INFO));
        assert!(s.enabled("httpx", Level::WARN));
        assert!(!s.enabled("teleclaudex", Level::INFO));
    }

    #[test]
    fn root_uses_third_party_level_without_spotlight() {
        let s = sel(INFO, DEBUG, &[], &[]);
        assert!(s.enabled("httpx", Level::DEBUG));
        assert!(!s.enabled("httpx", Level::TRACE));
    }

    #[test]
    fn spotlight_gets_third_party_level_and_root_drops_to_warning() {
        let s = sel(INFO, DEBUG, &["httpx"], &[]);
        assert!(s.enabled("httpx.client", Level::DEBUG));
        assert!(!s.enabled("httpcore", Level::INFO));
        // Corrected rule: non-spotlight third-party still passes at WARNING+.
        assert!(s.enabled("httpcore", Level::WARN));
        assert!(s.enabled("httpcore", Level::ERROR));
    }

    #[test]
    fn spotlight_under_the_app_prefix_is_ignored() {
        let s = sel(INFO, DEBUG, &["teleclaude.core"], &[]);
        assert!(!s.enabled("teleclaude.core", Level::DEBUG));
        assert!(s.enabled("teleclaude.core", Level::INFO));
    }

    #[test]
    fn muted_forces_warning_and_wins_ties() {
        let s = sel(
            DEBUG,
            DEBUG,
            &["httpx"],
            &["teleclaude.noisy", "httpx", "teleclaude"],
        );
        assert!(!s.enabled("teleclaude.noisy.x", Level::INFO));
        assert!(s.enabled("teleclaude.noisy.x", Level::WARN));
        assert!(!s.enabled("httpx", Level::INFO));
        assert!(!s.enabled("teleclaude.other", Level::INFO));
    }

    #[test]
    fn longest_prefix_wins() {
        let s = sel(
            INFO,
            DEBUG,
            &["teleclaude_ext.deep", "aiohttp.client.x"],
            &["aiohttp"],
        );
        assert!(!s.enabled("aiohttp.client", Level::INFO));
        assert!(s.enabled("aiohttp.client.x.y", Level::DEBUG));
    }

    #[test]
    fn extra_app_prefixes_count_as_app() {
        let s = Selection::new(&s(&["instrukt", "instrukt_cli"]), DEBUG, WARNING, &[], &[]);
        assert!(s.enabled("instrukt_cli::commands", Level::DEBUG));
        assert!(!s.enabled("tokio::runtime", Level::INFO));
    }

    #[test]
    fn critical_blocks_everything_and_a_notset_root_allows_everything() {
        let s = sel(CRITICAL, NOTSET, &[], &[]);
        assert!(!s.enabled("teleclaude", Level::ERROR));
        assert!(s.enabled("httpx", Level::TRACE));
        assert_eq!(s.max_level(), LevelFilter::TRACE);
        let s = sel(CRITICAL, CRITICAL, &[], &[]);
        assert_eq!(s.max_level(), LevelFilter::OFF);
    }

    #[test]
    fn notset_app_level_falls_back_to_the_root() {
        // No spotlight: the root is the third-party level.
        let s = sel(NOTSET, ERROR, &[], &[]);
        assert!(!s.enabled("teleclaude.core", Level::WARN));
        assert!(s.enabled("teleclaude.core", Level::ERROR));
        assert_eq!(s.max_level(), LevelFilter::ERROR);
        // Spotlight set: the root is WARNING.
        let s = sel(NOTSET, DEBUG, &["httpx"], &[]);
        assert!(!s.enabled("teleclaude", Level::INFO));
        assert!(s.enabled("teleclaude", Level::WARN));
        // A muted prefix under the app still holds WARNING.
        let s = sel(NOTSET, NOTSET, &[], &["teleclaude.noisy"]);
        assert!(s.enabled("teleclaude.core", Level::TRACE));
        assert!(!s.enabled("teleclaude.noisy", Level::INFO));
    }

    #[test]
    fn notset_third_party_level_leaves_the_spotlight_at_the_warning_root() {
        let s = sel(INFO, NOTSET, &["httpx"], &[]);
        assert!(!s.enabled("httpx", Level::INFO));
        assert!(s.enabled("httpx", Level::WARN));
        assert!(!s.enabled("other", Level::INFO));
        // Without spotlight the root passes everything.
        let s = sel(INFO, NOTSET, &[], &[]);
        assert!(s.enabled("other", Level::TRACE));
        assert!(!s.enabled("teleclaude", Level::DEBUG));
    }

    #[test]
    fn max_level_is_the_most_verbose_threshold() {
        assert_eq!(sel(INFO, WARNING, &[], &[]).max_level(), LevelFilter::INFO);
        assert_eq!(
            sel(ERROR, ERROR, &[], &["x"]).max_level(),
            LevelFilter::WARN
        );
        assert_eq!(
            sel(ERROR, TRACE, &["x"], &[]).max_level(),
            LevelFilter::TRACE
        );
    }

    #[test]
    fn from_env_reads_the_four_variables() {
        let env = EnvLookup::new(|k| {
            let v = match k {
                "TELECLAUDE_LOG_LEVEL" => "debug",
                "TELECLAUDE_THIRD_PARTY_LOG_LEVEL" => "info",
                "TELECLAUDE_THIRD_PARTY_LOGGERS" => "httpx",
                "TELECLAUDE_MUTED_LOGGERS" => "teleclaude.noisy",
                _ => return None,
            };
            Some(v.into())
        });
        let s = Selection::from_env("TELECLAUDE", &s(&["teleclaude"]), &env);
        assert!(s.enabled("teleclaude.core", Level::DEBUG));
        assert!(s.enabled("httpx", Level::INFO));
        assert!(!s.enabled("httpcore", Level::INFO));
        assert!(!s.enabled("teleclaude.noisy", Level::INFO));
    }
}
