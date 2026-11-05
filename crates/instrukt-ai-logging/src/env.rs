//! Where environment variables are read from.

use std::ffi::OsString;
use std::fmt;
use std::sync::Arc;

type LookupFn = dyn Fn(&str) -> Option<OsString> + Send + Sync;

/// An environment source: the process environment by default, or any
/// function the caller injects (tests, embedding hosts).
#[derive(Clone)]
pub(crate) struct EnvLookup(Arc<LookupFn>);

impl EnvLookup {
    pub(crate) fn process() -> Self {
        Self(Arc::new(|key: &str| std::env::var_os(key)))
    }

    pub(crate) fn new<F>(f: F) -> Self
    where
        F: Fn(&str) -> Option<OsString> + Send + Sync + 'static,
    {
        Self(Arc::new(f))
    }

    pub(crate) fn get(&self, key: &str) -> Option<OsString> {
        (self.0)(key)
    }

    /// The value as UTF-8, or `None` when unset or not valid UTF-8.
    pub(crate) fn get_str(&self, key: &str) -> Option<String> {
        self.get(key).and_then(|v| v.into_string().ok())
    }
}

impl fmt::Debug for EnvLookup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EnvLookup(..)")
    }
}
