# instrukt-ai-logging

A [`tracing`](https://docs.rs/tracing) layer that writes the InstruktAI logging
contract: one human-readable logfmt line per event, in one predictable
per-user location, safe under external log rotation. It follows the same
contract as the Python writer
([`instruktai-python-logger`](https://pypi.org/project/instruktai-python-logger/)),
so a Rust service and a Python service under the same app write identical
lines, and `instrukt-ai-logs` reads both.

```text
2026-11-05T09:30:12.345Z level=INFO logger=teleclaude::jobs msg="job_started" attempt=3 job_id=abc123
```

## Usage

```toml
[dependencies]
instrukt-ai-logging = "0.7"
tracing = "0.1"
```

```rust,no_run
fn main() -> Result<(), instrukt_ai_logging::Error> {
    // $XDG_STATE_HOME/instrukt-ai/teleclaude/teleclaude.log
    // (~/.local/state when XDG_STATE_HOME is unset).
    instrukt_ai_logging::init("teleclaude", None)?;
    tracing::info!(job_id = "abc123", attempt = 3, "job_started");
    Ok(())
}
```

`init` installs the global subscriber and bridges the `log` crate, so
dependencies that log through `log` go through the same selection. It fails
(never panics) when the log file cannot be opened or a global subscriber or
`log` logger is already set.

For the less common knobs, use the `Builder`:

```rust,no_run
use tracing_subscriber::layer::SubscriberExt;

// A separate cron.log next to the app's main log, extra crates counted as the
// app's own, and a shorter cut-off for long values.
let path = instrukt_ai_logging::Builder::new("instrukt")
    .source("cron")
    .app_prefix("instrukt_cli")
    .app_prefix("instrukt_core")
    .max_message_chars(2000)
    .init()?;

// Or compose your own subscriber: the layer carries its own per-layer
// filter, so other layers still see every event.
let (layer, path) = instrukt_ai_logging::Builder::new("teleclaude").layer()?;
let subscriber = tracing_subscriber::registry().with(layer);
# Ok::<(), instrukt_ai_logging::Error>(())
```

`resolve_log_root()`, `resolve_log_dir(app)` and `resolve_log_file(app, source)`
return the locations without creating anything.

## Configuration

Read once, at initialization. `APP` is the app name upper-cased, with runs of
other characters turned into `_`.

| Variable | Meaning | Default |
| --- | --- | --- |
| `{APP}_LOG_LEVEL` | threshold for the app's own targets | `INFO` |
| `{APP}_THIRD_PARTY_LOG_LEVEL` | threshold for third-party targets | `WARNING` |
| `{APP}_THIRD_PARTY_LOGGERS` | CSV of third-party prefixes to spotlight; every other third-party target then passes at `WARNING`+ | none |
| `{APP}_MUTED_LOGGERS` | CSV of prefixes held at `WARNING`+ | none |

The longest matching prefix decides (muted wins a tie). Prefixes match on `.`
and `::` boundaries, so `hyper` covers `hyper::client`.

## What gets written

- `level`, `logger` (the tracing target, unchanged), the always-quoted `msg`,
  then the event's fields sorted by key. `WARN` is written as `WARNING`.
- Bearer tokens, `sk-` keys and Telegram bot tokens are redacted; strings are
  cut at `max_message_chars` characters (default 4000) and quoted unless they
  are bare-safe; floats are written like Python's `repr`.
- Only event fields are written. Span fields and span context are not.

## Rotation

The crate never configures rotation. Before each write it checks that the path
still refers to the open file and reopens it when it was moved or removed, so
rotation set up by `instrukt-ai-log-setup` (newsyslog or logrotate, in user
space) just works.

The full contract: [`docs/contract.md`](https://github.com/InstruktAI/logging/blob/main/docs/contract.md).

## License

MIT
