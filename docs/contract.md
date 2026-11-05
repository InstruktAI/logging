# Logging contract

This is the single contract both writers follow: the Python SDK
(`instrukt_ai_logging.configure_logging`) and the Rust crate
(`instrukt-ai-logging`). Given the same app name, environment and calls,
both write the same lines to the same file.

The executable form of this contract is [`conformance/`](../conformance/README.md).
The `writer/*.json` cases run against both writers in the same CI. Where this
document and a case disagree, treat it as a defect and fix one of them; never
skip a case on one side.

Design intent (why the contract looks like this) lives in [`design.md`](design.md).

## Changes from 0.6.x

0.7.0 makes two deliberate corrections. Both writers implement them.

1. **Redaction works.** In 0.6.x the redaction patterns were raw strings with
   doubled backslashes, so none of them ever matched. Bearer tokens, `sk-`
   keys and Telegram bot tokens reached the log file in clear. The patterns
   below are now applied as written.
2. **Spotlight keeps third-party warnings.** In 0.6.x, setting
   `{APP}_THIRD_PARTY_LOGGERS` dropped *every* record from a third-party logger
   outside the spotlight, including errors. The documented rule always said
   those loggers are "forced to WARNING+", and they now are: they pass at
   WARNING and above.

## Names

Every name comes from the one `app` string passed at initialization. It is
normalized three ways:

| Use | Rule | `My-App` | `" Crypto  AI! "` |
| --- | --- | --- | --- |
| env prefix | upper-case; each run of non-`[A-Z0-9]` becomes `_`; leading and trailing `_` trimmed | `MY_APP` | `CRYPTO_AI` |
| app logger prefix | lower-case; each run of non-`[a-z0-9]` becomes `_`; trimmed | `my_app` | `crypto_ai` |
| filesystem app name | lower-case; each run of non-`[a-z0-9]` becomes `-`; trimmed | `my-app` | `crypto-ai` |

An app string that normalizes to empty is an error at initialization.

## Location

```
$XDG_STATE_HOME/instrukt-ai/<app>/<stem>.log
```

- `<app>` is the filesystem app name.
- `<stem>` is `source` when one is passed. Otherwise it is the filesystem app
  name. `.log` is always appended, so callers pass the bare stem (`cron`, not
  `cron.log`).
- When `XDG_STATE_HOME` is unset or empty, `~/.local/state` is used. `~` is
  expanded.
- There is no override variable. The path is user-writable by construction.

Several processes of one app can each write their own `<source>.log` into the
same directory. The reader (`instrukt-ai-logs`) merges every `*.log*` file in
that directory by timestamp.

## Line format

Each event is exactly one line, terminated by `\n`:

```
<ts> level=<level> logger=<logger> msg="<msg>"[ <key>=<value>]...
```

- `<ts>` is UTC `YYYY-MM-DDTHH:MM:SS.mmmZ`. Milliseconds are truncated, not
  rounded.
- `<level>` is one of `TRACE`, `DEBUG`, `INFO`, `WARNING`, `ERROR`. Python can
  also emit `CRITICAL`. Rust maps tracing's `WARN` to `WARNING`.
- `<logger>` is the logger name. In Rust it is the tracing target, carried
  unchanged (for example `my_app::db`).
- `level` and `logger` are formatted with the string value rule below. In
  practice they are bare, because level names and logger names are bare-safe.
- `msg` is formatted with the string rule, but it is **always** quoted, even
  when it is bare-safe.
- The extra fields follow, sorted by key in code-point order. Upper-case keys
  sort before lower-case keys, and `_` sorts between them.
  - A field whose key is `msg` is dropped.
  - A field whose key does not fully match `^[A-Za-z_][A-Za-z0-9_.-]*$` is
    dropped.

### Values

| Type | Written as |
| --- | --- |
| bool | `true` / `false` |
| int | decimal (`42`, `-7`) |
| float | Python `repr` spelling: shortest round-trip digits, and integral floats keep `.0` (`2.0`, `1.5`, `0.1`) |
| string | the string rule |

The string rule, applied in this order:

1. **Redact** (see below).
2. **Truncate** to `max_message_chars` *characters*, not bytes (default
   `4000`). A longer string keeps its first `max_message_chars` characters and
   gets the suffix `…(truncated)`. A value `<= 0` disables truncation.
3. **Bare or quoted.** A string that fully matches `^[A-Za-z0-9._/:+-]+$` is
   written bare. Any other string, including the empty string, is written in
   double quotes, with these escapes:

   | Character | Escape |
   | --- | --- |
   | `\` | `\\` |
   | `"` | `\"` |
   | newline | `\n` |
   | carriage return | `\r` |

   Nothing else is escaped. A tab, for example, stays a literal tab inside the
   quotes. Because newlines and carriage returns are escaped, one event never
   spans two lines.

A string value that looks like a number or a bool (`"42"`, `"true"`) is still
a string. It follows the string rule, so it is written bare.

Python-only details. Tracing fields cannot express these, so no conformance
case pins them:

- `None` is written as `null`.
- A value of any other type is converted with `str()` and then follows the
  string rule.
- A record with exception info gets a trailing `exc=` field. It holds the
  formatted traceback, with its newlines pre-escaped to `\n`. The string rule
  then escapes that backslash, so the file shows `\\n`.
- A message whose `%`-formatting fails is written as `<unprintable>`.
- Non-finite floats are written as `nan`, `inf` and `-inf`.

## Redaction

These patterns are applied, in this order, to the message and to every string
value, before truncation. Redacting first means truncation can never cut a
token into a shape the patterns no longer recognize.

| Pattern | Replacement | Example |
| --- | --- | --- |
| `(api\.telegram\.org/bot)([^/\s]+)` | `\1<REDACTED>` | `api.telegram.org/bot123:ABC/send` → `api.telegram.org/bot<REDACTED>/send` |
| `\bBearer\s+[^\s]+` | `Bearer <REDACTED>` | `Bearer eyJ…` → `Bearer <REDACTED>` |
| `\bsk-[A-Za-z0-9]{10,}\b` | `sk-<REDACTED>` | `sk-abcdefghij123` → `sk-<REDACTED>` |

`<REDACTED>` is not bare-safe, so a redacted value is always quoted. The list
is kept small and high-signal on purpose. Add to it only with strong
justification, and add a conformance case with it.

## Environment

The prefix is the env prefix from [Names](#names).

| Variable | Meaning | Default |
| --- | --- | --- |
| `{APP}_LOG_LEVEL` | threshold for app loggers | `INFO` |
| `{APP}_THIRD_PARTY_LOG_LEVEL` | threshold for third-party loggers (all of them, or only the spotlighted ones) | `WARNING` |
| `{APP}_THIRD_PARTY_LOGGERS` | CSV of third-party logger prefixes to spotlight | none |
| `{APP}_MUTED_LOGGERS` | CSV of logger prefixes held at WARNING+, app or third-party | none |

- The variables are read once, at initialization.
- Level names are case-insensitive. Accepted names are `TRACE`, `DEBUG`,
  `INFO`, `WARN`, `WARNING`, `ERROR`, `CRITICAL`, `FATAL` and `NOTSET`. An
  unknown or empty value means the default.
- CSV items are trimmed, and empty items are ignored (`" httpx , ,telegram,"`
  is `httpx` and `telegram`).

### Selection

A logger is an **app logger** when its name matches the app logger prefix.
Every other logger is **third-party**.

Prefix `p` matches name `n` when `n == p`, or when `n` starts with `p`
followed by a separator. Python's separator is `.`. In Rust both `.` and `::`
are separators, because tracing targets are module paths. Matching never
crosses a separator boundary, so `httpx` matches `httpx` and `httpx._client`
but not `httpxyz`.

A record's threshold comes from the most specific (longest) matching prefix
among:

- the app logger prefix → `{APP}_LOG_LEVEL`
- each spotlight prefix that is not the app prefix or under it →
  `{APP}_THIRD_PARTY_LOG_LEVEL`. A spotlight prefix under the app prefix is
  ignored, and app loggers keep the app level.
- each muted prefix → `WARNING`. When a muted prefix and another prefix have
  the same length, muted wins.

When nothing matches, the root threshold applies:

- **No spotlight set:** `{APP}_THIRD_PARTY_LOG_LEVEL`.
- **Spotlight set:** `WARNING`. Third-party loggers outside the spotlight pass
  at WARNING and above. (In 0.6.x they were dropped entirely; see
  [Changes from 0.6.x](#changes-from-06x).)

So a longer spotlight under a shorter muted prefix
(`MUTED=httpx`, `THIRD_PARTY_LOGGERS=httpx._client`) emits `httpx._client` at
the third-party level, while the rest of `httpx` stays at WARNING+.

A prefix is compared as a literal string. In Rust, a sub-module prefix has to
be written the way its targets are spelled: `my_app::db` matches the target
`my_app::db::pool`, while `my_app.db` does not.

Rust: the `instrukt-ai-logging` builder can declare additional app prefixes
(`app_prefix(...)`). Each one is treated exactly like the app logger prefix.
This serves workspaces whose crates (`instrukt_cli`, `instrukt_core`) do not
sit under the app name.

### `CRITICAL`, `FATAL` and `NOTSET`

These names exist for compatibility with Python's level names. Their effect
differs between the writers, and no conformance case pins them.

- **Rust.** `CRITICAL` and `FATAL` mean "nothing below CRITICAL". Tracing has
  no level above `ERROR`, so nothing that tracing can emit passes. `NOTSET`
  means everything passes.
- **Python.** `CRITICAL` and `FATAL` pass only `CRITICAL` records. `NOTSET`
  follows stdlib semantics: the logger has no level of its own and inherits
  its parent's.
  - On `{APP}_LOG_LEVEL`, the app loggers fall back to the root threshold.
  - On `{APP}_THIRD_PARTY_LOG_LEVEL` without a spotlight, the root passes
    everything.
- **Python, other names.** Python also resolves any other integer attribute
  of the `logging` module by name. That is incidental, and nothing should rely
  on it.

## File handling

- **Initialization.** The log directory is created, including parents. The
  file is opened for append (UTF-8), and created if missing.
- **Fail fast.** If the directory or file cannot be created or opened,
  initialization fails immediately. Python raises from `configure_logging`;
  Rust returns `Err` from `init`. There is no degraded mode and no fallback
  destination, because a process that cannot write its logs must not start
  blind.
- **Reopen on rotation.** Before each write, the writer checks that the path
  still refers to the file it has open (same device and inode). If the file
  was moved or removed, the writer reopens the path first. Python gets this
  from `logging.handlers.WatchedFileHandler`; Rust implements the same check.
- **Re-initialization.** Python's `configure_logging` replaces the root
  handler on each call. Rust's `init` installs a global subscriber, so a
  second call (or an already-installed global subscriber) is an `Err`, never a
  panic.

Reopening matters because both rotators rename the live file, and neither
truncates it:

- newsyslog moves `x.log` to `x.log.0` and compresses it. A writer that kept
  its old descriptor would keep appending to a renamed file that is about to be
  gzipped and eventually deleted, so those lines would be lost.
- With the reopen check, the next write lands in the fresh `x.log`. That is the
  file the reader follows, and the one the next rotation sees.

## Rotation

Writers never set up rotation. They only ensure the directory and file.
Rotation is set up once per user by `instrukt-ai-log-setup`. It is idempotent,
and it writes a file only when the file's content changed. Everything runs as
the producing user: no `/etc` configuration, no sudo, no root-owned files.

| | macOS | Linux |
| --- | --- | --- |
| rotation conf | `~/.config/instrukt-ai/newsyslog.conf` | `~/.config/instrukt-ai/logrotate.conf` |
| scheduler | `~/Library/LaunchAgents/ai.instrukt.log-rotate.plist` (label `ai.instrukt.log-rotate`) | `~/.config/systemd/user/instrukt-ai-logrotate.service` + `.timer` |
| runs | `/usr/sbin/newsyslog -r -f <conf>`, every 1800 s and at load | `logrotate --state <log root>/logrotate.state <conf>`, `OnCalendar=*:0/30`, persistent |
| activation | `launchctl bootstrap gui/<uid> <plist>` | `systemctl --user daemon-reload`, then `enable --now instrukt-ai-logrotate.timer` |

Both rotation confs cover `<log root>/*/*.log`, where the log root is
`$XDG_STATE_HOME/instrukt-ai`, and both keep the same retention:

- **Size trigger:** 50 MB. newsyslog states it as `50000` (KB); logrotate as
  `size 50M`. There is no time-based trigger.
- **Retention depth:** 30 archives per file.
- **Compression:** gzip. newsyslog: `640  30  50000  *  ZG`. logrotate:
  `compress`, `delaycompress`, `missingok`, `notifempty`.

Archives stay next to the live file (`x.log.0.gz` with newsyslog; `x.log.1`,
`x.log.2.gz`, … with logrotate). The reader includes them when it slices by
time.
