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
   keys and Telegram bot tokens reached the log file in clear. The rules in
   [Redaction](#redaction) now apply.
2. **Spotlight keeps third-party warnings.** In 0.6.x, setting
   `{APP}_THIRD_PARTY_LOGGERS` dropped *every* record from a third-party logger
   outside the spotlight, including errors. The documented rule always said
   those loggers are "forced to WARNING+", and they now are: they pass at
   WARNING and above.

## Changes in the writer-hardening release

Every output difference a consumer can see, in both writers unless marked:

1. **Terminal controls are escaped.** Inside quotes, every C0 control other
   than LF and CR (tab included), DEL, C1 and the bidi controls are written as
   `\u001b`-style escapes ([Values](#values)). A tab used to be written raw,
   and so were U+001C–U+001F.
2. **Lines are at most 16384 bytes.** Fields that do not fit are dropped and
   counted in a trailing `fields_dropped=<n>`; a message too long even alone
   is cut with `…(truncated)` ([Line limit](#line-limit)).
3. **`source` must be a bare file stem**; anything else is an init error
   ([Location](#location)).
4. **The log file is `0600` and its directories `0700`**, existing ones
   tightened; a symbolic link at the log path is refused
   ([File handling](#file-handling)).
5. **A line that cannot be written is dropped, counted and reported** in a
   bounded line on stderr, never raised or panicked
   ([Runtime failures](#runtime-failures)).
6. **Each line goes out in one `write(2)`** ([File handling](#file-handling)).
7. **Python enforces the selection policy itself**: a library's own logger
   level can no longer make a logger louder ([Selection](#selection)), and a
   repeated `configure_logging` starts clean.
8. **Python writes a lone surrogate as `\udcff`** instead of losing the
   event ([Values](#values)). Python's root handler is now the writer's own
   `LogFileHandler`, no longer a `logging.handlers.WatchedFileHandler`.
9. **Rotated archives are mode `600`**, and on Linux the newest archive stays
   uncompressed until the next rotation ([Rotation](#rotation)).
10. **Only the first 17408 characters of a value are kept.** The message and
    every string value are cut there before redaction, so text past the cut
    no longer reaches the line even where redaction would have shortened what
    came before it. An `sk-` key that the cut leaves fewer than ten
    characters of is still redacted ([Values](#values)).

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
  `cron.log`). An empty `source` means "not passed".
- A `source` must be a bare file stem: it fully matches
  `^[A-Za-z0-9][A-Za-z0-9._-]*$` and does not contain `..`. Anything else
  (`../../escape`, `a/b`, `.hidden`) is an error at initialization, and from
  `resolve_log_file`, in both writers (Python `ValueError`, Rust
  `Error::InvalidSource`), before any directory or file is created. So a
  source can never name a file outside the app's log directory.
- When `XDG_STATE_HOME` is unset or empty, `~/.local/state` is used. A
  leading `~` or `~/` is expanded against `HOME`; when `HOME` is unset the
  user's password database entry is used, and an empty `HOME` means `/`.
- Paths are rendered the way Python's `pathlib` renders them: repeated and
  trailing slashes collapse and `.` segments are dropped
  (`XDG_STATE_HOME=/tmp//x/` gives `/tmp/x/instrukt-ai/...`). Both writers
  return the path in this form, and the tool prints it in this form.
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

### Line limit

A formatted line, without its `\n` and with its timestamp, is at most
**16384 bytes** of UTF-8. A tail window of even a few hundred lines then stays
readable, and every line fits in one `write` (see
[File handling](#file-handling)). The limit never makes a line malformed: a
value is never cut in the middle, and an escape is never split.

- **It fits:** the line is written whole, as described above.
- **The fields do not all fit:** the fields are appended in their sorted
  order while they fit, leaving room for the marker ` fields_dropped=<n>`,
  where `<n>` is the number of fields left out. The first field that does not
  fit and every field after it are left out, and the marker ends the line.
  The room left for the marker is ` fields_dropped=` plus the digits of the
  number of fields the line has.
- **The head alone does not fit:** when `level`, `logger` and `msg` take so
  much that not even the marker fits after them, every field is left out and
  the message is cut on a character boundary to the longest prefix that,
  quoted and escaped and followed by the existing suffix `…(truncated)`,
  fits together with the marker (no marker on a line without fields). Should
  even an empty message not fit, the logger is cut the same way, then the
  level (each becomes quoted, `"…(truncated)"` at the least).

Per-value truncation (`max_message_chars`, default 4000 characters) still
applies first; the line limit is a byte bound on the whole line. The Python
`exc=` field counts as the last field. In Rust capture is bounded as well:
no value is held past the capture limit ([Values](#values)), string values
are redacted and truncated as they are recorded, and once an
event's fields certainly make the line too long, a field that sorts at or
after the first one that certainly cannot be kept is not formatted at all.

### Values

| Type | Written as |
| --- | --- |
| bool | `true` / `false` |
| int | decimal (`42`, `-7`) |
| float | Python `repr` spelling: shortest round-trip digits, and integral floats keep `.0` (`2.0`, `1.5`, `0.1`) |
| string | the string rule |

The string rule, applied in this order:

1. **Capture** at most `CAPTURE_CHARS` = 17408 (16384 + 1024) *characters*,
   not bytes: a longer string keeps its first 17408 characters, and every
   later step sees only the cut text. It is more than a line can hold, since
   every character takes at least a byte.

   **What the cut bounds.** The text that is kept, and all the processing
   after it (redaction, truncation, escaping, the line limit), whatever
   `max_message_chars` is (also when `0` disables truncation). It does **not**
   bound formatting the caller's value itself:

   - Rust copies at most the limit from a `&str`. It writes the message and
     `Debug`/`Display` values into a sink that keeps the first 17408
     characters and discards the rest. The sink accepts that rest and returns
     `Ok`, so the impl runs to its end: a value costs its whole formatting
     time, but at most 17408 characters of memory. The sink never fails,
     because a sink that returned `fmt::Error` would make a `Debug` impl that
     unwraps its writes panic inside a log call, and a log call never crashes
     the host.
   - Python cuts with `value[:17408]`, after `str()`, which builds the whole
     text first.

   **A secret that straddles the cut** is redacted against the cut text, in
   both writers. A `Bearer` or Telegram token is redacted up to the cut. An
   `sk-` key whose run of ASCII letters and digits reaches the end of the cut
   text is redacted whatever its length (one character or more), because it
   may go on past the cut ([Rules](#rules)). This applies only when the value
   was actually cut: a value of exactly 17408 characters ending in a short
   key keeps it. A key that far into a value reaches the line only when
   redaction shortened the 17000-odd characters before it (a giant `Bearer`
   token); otherwise the line limit keeps it off the line.
2. **Redact** (see below).
3. **Truncate** to `max_message_chars` *characters*, not bytes (default
   `4000`). A longer string keeps its first `max_message_chars` characters and
   gets the suffix `…(truncated)`. A value `<= 0` disables truncation.
4. **Bare or quoted.** A string that fully matches `^[A-Za-z0-9._/:+-]+$` is
   written bare. Any other string, including the empty string, is written in
   double quotes, with these escapes:

   | Character | Escape |
   | --- | --- |
   | `\` | `\\` |
   | `"` | `\"` |
   | newline (U+000A) | `\n` |
   | carriage return (U+000D) | `\r` |
   | every other C0 control, U+0000–U+001F (tab included) | `\u` and four lowercase hex digits: `\u0009`, `\u001b` |
   | DEL, U+007F | `\u007f` |
   | C1 controls, U+0080–U+009F | `\u0080` … `\u009f` |
   | bidi and format controls U+061C, U+200E, U+200F, U+202A–U+202E, U+2066–U+2069 | `\u061c`, `\u202e`, … |
   | Python only: a lone surrogate, U+D800–U+DFFF | `\udcff`, … |

   Nothing else is escaped: other non-ASCII characters (`é`, U+00A0) stay as
   they are. None of the escaped characters is bare-safe, so a value holding
   one is always quoted. Escaping applies to `msg` and to every string value,
   after redaction and truncation, so redaction still sees the original
   characters (U+001C–U+001F separate `Bearer` from its token, then are
   written as `\u001c` … `\u001f`). Because newlines and carriage returns
   are escaped, one event never spans two lines; because terminal controls
   are escaped, a value can neither drive the terminal a log is read in
   (`ESC [`, OSC title sequences) nor reorder what it shows (bidi overrides).

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
- A string holding a lone surrogate, such as a file name with undecodable
  bytes carried by `surrogateescape` (`os.fsdecode(b"\xff")` is `"\udcff"`),
  is written with the surrogate escaped as `\udcff`, so the event is never
  lost to a UTF-8 encoding error (in 0.9.x the whole event disappeared).
  Each surrogate is escaped on its own, also two that would form a pair. Rust
  strings cannot hold surrogates, so a Python unit test pins this, not a
  conformance case.

## Redaction

**Redaction is a last safety net, not secret handling.** Exactly three token
shapes are recognised: Telegram bot tokens in `api.telegram.org/bot…` URLs,
`Bearer` tokens, and `sk-` keys (see [Rules](#rules)). Any other secret (a
password, a database URL with credentials, a cookie, an API key of another
shape) is written as it is. Keeping secrets out of logs is the application's
job: hold them in secret types that never print their value (Rust
`secrecy::SecretString`, Pydantic `SecretStr`) and do not log them. The
writer only catches the three common shapes that slip through.

Redaction applies to the message and to every string value, after the
capture cut ([Values](#values)) and before truncation. Redacting first means truncation can never cut a token into a
shape the rules no longer recognize.

Redaction is defined on the **original** text as a set of secret spans. It is
not a sequence of regex replacements:

1. Every rule finds every one of its secret spans, at each candidate position
   on its own. Spans may overlap, and no match consumes text that another
   match could use.
2. The spans are merged: overlapping and touching spans become one.
3. Each merged span is replaced by `<REDACTED>`. Everything outside the spans
   stays byte-for-byte as it was.

The result does not depend on the order of the rules.

### Character sets

The rules use three named character sets. They are spelled out instead of
written as `\b`, `\w` or `\s`, because regex engines disagree on those for
non-ASCII input, and both writers must redact exactly the same text.

| Set | Characters |
| --- | --- |
| `WORD` | ASCII letters, digits and underscore, `[A-Za-z0-9_]`. Nothing else is a word character, whatever its Unicode category: `é`, `٣` and a combining mark such as U+0301 are not. |
| `SPACE` | Unicode `White_Space`: U+0009–U+000D, U+0020, U+0085, U+00A0, U+1680, U+2000–U+200A, U+2028, U+2029, U+202F, U+205F, U+3000. |
| `SEP` | `SPACE` plus U+001C–U+001F. |

A **boundary before** position *i* holds when *i* is the start of the text or
the character before *i* is not in `WORD`. A **boundary after** a position
holds when it is the end of the text or the character there is not in `WORD`.

### Rules

Each rule defines only the secret span. The literal that introduces it, and a
separator, stay in the text.

| Rule | Introducer | Secret span | Example |
| --- | --- | --- | --- |
| Telegram | the literal `api.telegram.org/bot`, no boundary | the longest non-empty run of characters that are neither `/` nor `SPACE`, directly after it | `api.telegram.org/bot123:ABC/send` → `api.telegram.org/bot<REDACTED>/send` |
| Bearer | the literal `Bearer` with a boundary before it, then the longest run of one or more `SEP` | the longest non-empty run of characters not in `SPACE`, directly after that run | `Bearer eyJ…` → `Bearer <REDACTED>` |
| `sk-` | the literal `sk-` with a boundary before it | the longest run of ASCII letters and digits directly after it, if it is 10 or more long and followed by a boundary; in a value cut at the capture limit also when it is 1 or more long and reaches the end of the cut text | `sk-abcdefghij123` → `sk-<REDACTED>` |

The literals are case-sensitive. In words:

- **Before a secret.** `Bearer` and `sk-` count only at the start of the text
  or right after a character that is not in `WORD`. `xBearer abc`, `9sk-…` and
  `_sk-…` stay as they are. `éBearer abc`, U+0301 followed by `Bearer abc`,
  `(Bearer abc)` and `=sk-…` are redacted.
- **After a key.** The key is the longest run of ASCII letters and digits, so
  the character after it is never one of those, and the boundary after it
  fails only on an underscore: `sk-…1234_` stays, `sk-…1234é` becomes
  `sk-<REDACTED>é`. For ASCII text this is exactly what 0.8.0's
  `\bsk-[A-Za-z0-9]{10,}\b` matched: that pattern could not end inside the run,
  where a letter or digit follows, so it failed on `_` and matched otherwise.
- **At the capture cut.** When a value was cut at the capture limit
  ([Values](#values)), its cut text may end inside a key. A run after `sk-`
  that reaches the end of the cut text is therefore a key of any length, one
  character or more: `… sk-abcde` cut there becomes `… sk-<REDACTED>`. `sk-`
  with no character left after it, and an `sk-` without a boundary before
  it, stay as they are. Text that was not cut follows only the rules above.
- **Around a Bearer token.** The separator run takes every `SEP` character
  after `Bearer`. The token is everything after it up to the next `SPACE`
  character or the end of the text. The information separators U+001C–U+001F
  therefore separate `Bearer` from a token but never end one. `Bearer` whose
  separator run reaches the end of the text has no token and stays. The
  separator stays as written: `Bearer\u00a0tok` becomes `Bearer\u00a0<REDACTED>`.
- **Spans, not consumed matches.** A token that is itself the word `Bearer`
  does not hide the next one: `éBearer Bearer abc` becomes
  `éBearer <REDACTED> <REDACTED>`, and `Bearer \u001c Bearer abc` becomes
  `Bearer \u001c <REDACTED> <REDACTED>`. A Telegram token that ends in `Bearer`
  does not hide that Bearer's token either:
  `api.telegram.org/botx\u001cBearer abc` becomes
  `api.telegram.org/bot<REDACTED> <REDACTED>`. In `sk-…1234.sk-…5678` both keys
  are redacted. Spans of different rules that overlap or touch become one
  `<REDACTED>`: `Bearer api.telegram.org/bot12:AB/send` becomes
  `Bearer <REDACTED>`.

### Guarantee

The writers' own tests prove this property against 0.8.0, which applied the
three regular expressions `(api\.telegram\.org/bot)([^/\s]+)`,
`\bBearer\s+[^\s]+` and `\bsk-[A-Za-z0-9]{10,}\b` one after the other: for
thousands of fixed-seed random and adversarial inputs that contain a marker
token, whenever that writer's 0.8.0 redaction hid the marker, redaction hides
it too. Python is compared with Python 0.8.0 and Rust with Rust 0.8.0, since
their regex engines differed.

Redaction runs in time linear in the length of the text, in both writers.

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
- Level names are case-insensitive in ASCII. Accepted names are `TRACE`,
  `DEBUG`, `INFO`, `WARN`, `WARNING`, `ERROR`, `CRITICAL`, `FATAL` and
  `NOTSET`.
- A level value is matched in ASCII only. It is trimmed of ASCII whitespace
  (space, tab, newline, carriage return, vertical tab, form feed) and nothing
  else, and only ASCII letters are upper-cased. Unicode whitespace and Unicode
  case mapping play no part: `ınfo` (dotless i) and a no-break space before
  `info` are not `INFO`.
- A level variable that is unset, empty or ASCII-whitespace-only means the
  default, silently. So does a value that is not valid UTF-8: it counts as
  unset and never fails initialization. (Python sees such a value in
  `os.environ` as a string with lone surrogates; Rust as an `OsString` that
  is not a `String`.)
- Any other value that is not an accepted name is **unknown**. Numeric
  strings such as `10` are unknown, and so is any value with a non-ASCII
  character left after trimming. An unknown value also means the default,
  and it never fails initialization. Initialization writes one line per
  unknown value, `{APP}_LOG_LEVEL` first, then `{APP}_THIRD_PARTY_LOG_LEVEL`,
  before any other line of that initialization:

  ```
  <ts> level=WARNING logger=instrukt_ai_logging msg="unknown log level; using the default" default=INFO value=deubg variable=TELECLAUDE_LOG_LEVEL
  ```

  - `default` is the default used: `INFO` for `{APP}_LOG_LEVEL`, `WARNING`
    for `{APP}_THIRD_PARTY_LOG_LEVEL`.
  - `value` is the raw value exactly as read, untrimmed, under the ordinary
    string rule (redaction, truncation, quoting). `de bug` is written as
    `value="de bug"`.
  - The fields are sorted by key, like on every line.
  - The line goes straight to the log file and **bypasses selection**. No
    level, spotlight or muted prefix can drop it, because it reports on the
    configuration of selection itself. It never goes through the `log` crate
    bridge, Python's logger hierarchy or its handler filters, and nothing is
    written to stderr.
  - Python warns on each `configure_logging` call. In Rust, `init()`,
    `Builder::init()` and `Builder::layer()` each warn once, when they open the
    file, so a caller that composes its own subscriber gets the line too.
  - If the line cannot be written, initialization fails like any other file
    error (see [File handling](#file-handling)).
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

The policy is enforced by the writer itself, whatever levels other code sets
on loggers: a logger's own level can make it quieter, never louder. In Python
a library may set its child logger to `DEBUG` (`httpx._client`), and
propagation hands such records to the root handler without checking ancestor
levels; the writer's handler filter therefore holds every record to the
threshold above, computed from the prefixes alone. (In 0.9.x that child's
`DEBUG` records passed a `WARNING` third-party threshold.) Rust has no
per-logger levels; its layer filter applies the same policy.

A prefix is compared as a literal string. In Rust, a sub-module prefix has to
be written the way its targets are spelled: `my_app::db` matches the target
`my_app::db::pool`, while `my_app.db` does not.

Rust: the `instrukt-ai-logging` builder can declare additional app prefixes
(`app_prefix(...)`). Each one is treated exactly like the app logger prefix.
This serves workspaces whose crates (`instrukt_cli`, `instrukt_core`) do not
sit under the app name.

### `CRITICAL`, `FATAL` and `NOTSET`

These names exist for compatibility with Python's level names. Both writers
follow Python's actual behaviour, and the `level_*notset*`, `level_critical_*`
and `level_fatal_*` writer cases pin it.

- **`NOTSET` gives a prefix no level of its own.** Selection skips it and the
  next shorter matching prefix decides, or the root threshold when none does
  (Python: a logger at `NOTSET` inherits its parent's level).
  - On `{APP}_LOG_LEVEL`, app loggers fall back to the root threshold: the
    third-party level without a spotlight, `WARNING` with one. A muted
    prefix under the app still holds `WARNING`.
  - On `{APP}_THIRD_PARTY_LOG_LEVEL` without a spotlight, the root itself is
    `NOTSET` and passes everything.
  - On `{APP}_THIRD_PARTY_LOG_LEVEL` with a spotlight, the spotlight prefixes
    fall back to the `WARNING` root, like every other third-party logger.
- **`CRITICAL` and `FATAL`** pass only `CRITICAL` records. Python can emit
  those; tracing has no level above `ERROR`, so in Rust nothing passes. Every
  call a conformance case can make (`TRACE` to `ERROR`) is dropped by both.
- **Python, other names.** Python also resolves any other integer attribute
  of the `logging` module by name. That is incidental, and nothing should rely
  on it.

## File handling

- **Initialization.** The log directory is created, including parents. The
  file is opened for append (UTF-8), and created if missing.
- **Permissions.** Only the producing user can read the logs, whatever the
  umask. Every directory the writer creates is mode `0700`, and an existing
  app log directory (`<log root>/<app>`) is set to `0700`; directories above
  it that already exist are left as they are. The log file is created `0600`,
  and an existing file is set to `0600` each time it is opened, at
  initialization and on every reopen after rotation. A directory or file whose
  mode cannot be set fails like one that cannot be opened.
- **No symbolic links.** The log file is opened with `O_NOFOLLOW`. A symbolic
  link at the log path is an error at initialization (`ELOOP`); found on a
  reopen after rotation, it is a [runtime failure](#runtime-failures), and
  the line is not written through it. Directories along the path may be symbolic links.
- **Fail fast.** If the directory or file cannot be created or opened,
  initialization fails immediately. Python raises from `configure_logging`;
  Rust returns `Err` from `init`. There is no degraded mode and no fallback
  destination, because a process that cannot write its logs must not start
  blind.
- **Reopen on rotation.** Before each write, the writer checks that the path
  still refers to the file it has open (same device and inode). If the file
  was moved or removed, the writer reopens the path first. Both writers
  implement this check themselves, the way Python's
  `logging.handlers.WatchedFileHandler` does; the path is checked without
  following a symbolic link.
- **One write per line.** Several processes may append to one file (the
  nginx and Apache model): each line, `\n` included, goes out in exactly one
  `write(2)` on an `O_APPEND` descriptor, with no buffering in between
  (Python writes the encoded line with one `os.write`). A short write is a
  write failure; the rest of the line is never written by a second call,
  where it would land after another process's line. A write interrupted
  before it wrote anything (`EINTR`) is retried. A short write is a
  [runtime failure](#runtime-failures).
- **Local filesystems only.** Whole lines from concurrent appenders are
  guaranteed on local filesystems (APFS, ext4), where an `O_APPEND` write is
  atomic with respect to other appenders. Network filesystems (NFS, SMB) give
  no such guarantee: lines from several processes can interleave or be lost
  there.
- **Re-initialization.** Python's `configure_logging` replaces the root
  handler on each call and starts clean: the logger levels an earlier call
  set are reset, so a prefix muted or spotlighted then is not still held, and
  the earlier call's file handler is closed. A line already being formatted
  through it on another thread finishes and is written before it closes; a
  line that reaches it after it is closed is a dropped line. A call that
  fails leaves the earlier configuration in place. Calls from several threads serialize only their publication (the
  logger levels and the swap of the root handler): each publishes its
  configuration whole, and the last one to publish is the configuration in
  place. Each call closes the handlers it replaced after that publication,
  outside the serialization, so closing can overlap with later calls: a call
  can return while an earlier one is still closing, and a replaced handler's
  file stays open until a line still being formatted through it has finished.
  Lines are therefore ordered only within one handler; across a replacement an
  older line can land after a newer one. A call may be made from inside
  logging (a value's `__str__` while a line is formatted); if it replaces the
  handler that line is going through, that line is a dropped line. Rust's
  `init` installs a global subscriber, so a
  second call (or an already-installed global subscriber) is an `Err`, never a
  panic.

- **Durability.** Lines are written to the file but never `fsync`ed. A
  process crash loses nothing that was written (the kernel holds it), but a
  power loss or kernel panic can lose the newest lines, the ones still in the
  page cache.

### Runtime failures

Startup and runtime fail differently, on purpose:

- **Startup fails fast.** Anything that stops initialization from opening the
  file and writing its first lines is an error from `configure_logging` or
  `init`, and the host must not start blind (see above).
- **Runtime failures drop the line.** Once logging runs, a line that cannot be
  written (a full disk, a file that rotation replaced with an unwritable one,
  a symbolic link planted at the path, a short write, and in Python a record
  that cannot be formatted) is dropped. The logging call never raises and
  never panics, and the next line is tried as usual, reopening first if
  needed.
- **Counted.** Each dropped line adds one to a process-wide counter, across
  every initialization: `instrukt_ai_logging.failure_count()` in Python and
  `instrukt_ai_logging::failure_count()` in Rust. Startup errors are not
  counted.
- **Reported, bounded.** A dropped line is reported on stderr, at most one
  line per 60 seconds; the next report says how many it held back:

  ```
  instrukt-ai-logging: dropped a log line: cannot write <path>: <kind> errno=<n> (<strerror>); <count> dropped since start, <m> reports suppressed
  ```

  `<kind>` is the Python exception type (`OSError`, `PermissionError`,
  `ShortWriteError`) or the Rust `io::ErrorKind` (`StorageFull`,
  `PermissionDenied`, `WriteZero`); without an errno it reads `errno=none`.
  The report never holds the record's message, its arguments or its fields,
  so a secret cannot reach stderr while the file is failing. (Python's own
  `logging.Handler.handleError` printed the message and its arguments,
  unredacted.) A broken stderr is ignored: Rust writes the report without
  panicking (`eprintln!` would), and Python ignores the error.
- **The report itself never fails or leaks (Python).** It renders only
  builtin values: the exception type's `__qualname__` (`unknown` unless it is
  exactly a `str`), an `errno` only when it is exactly an `int` (an `int`
  subclass, or an `errno` attribute that raises, reads `errno=none`), the log
  path and the counts. It never calls `str()` or `repr()` on anything from the
  record or the exception, and nothing in it raises into the logging call. A
  record logged while the report is being written (a stderr routed into
  `logging`) is not written through the handler that is reporting; it is
  dropped and not counted.

Reopening matters because both rotators rename the live file, and neither
truncates it:

- newsyslog moves `x.log` to `x.log.0`; logrotate moves it to `x.log.1`. A
  writer that kept its old descriptor would keep appending to an archive that
  is eventually compressed and deleted, so those lines would be lost.
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
  `size 50M`. There is no time-based trigger. The trigger is checked only
  when the scheduler runs, every 30 minutes, so 50 MB is when a file becomes
  due, not a cap: it keeps growing until the next run, and an archive holds
  50 MB plus whatever was written meanwhile.
- **Retention depth:** 30 archives per file.
- **Compression and the rotation window.** A writer checks the file's
  identity and then writes (see [File handling](#file-handling)); each writer
  (a Python handler, a Rust layer) does both under one lock. If the rename
  falls between the two, that one line is appended to the renamed file, and
  the writer's next check sees the new file. There is no timing guarantee:
  the gap lasts as long as the writer is paused between its check and its
  write (preempted, stopped, swapped out), and nothing bounds that.
  - logrotate: `compress` with `delaycompress` (and `missingok`,
    `notifempty`). `x.log.1` stays plain until the next rotation compresses
    it, so that line survives in `x.log.1` unless the writer stays paused
    until that next rotation too.
  - newsyslog: `600  30  50000  *  ZG`. newsyslog compresses the newest
    archive right after the rename and has no flag to delay it
    (`newsyslog.conf(5)` lists `B C D G J N U Z`; FreeBSD's `p`, "do not
    compress the first archive", is rejected as `illegal flag`).
    **Known limit: on macOS each writer can lose at most one in-flight line
    per rotation, and the loss is not counted.** When the line lands after
    gzip has read the renamed file, it is appended to a file that is already
    deleted. That write succeeds, so `failure_count()` does not see it and
    nothing is reported. Compression stays: that is the owner's decision.
- **Storage (estimates).** Plain, 30 archives of about 50 MB are about
  1.5 GB per log file. Compressed, at the roughly 14:1 that real logs get,
  they are on the order of 110 MB. Both are estimates, not bounds: an archive
  is larger than 50 MB by what was written until the scheduler ran, and the
  ratio depends on what is logged.
- **Mode:** newsyslog applies the conf's mode to the new live file and to
  every archive, so it is `600`, like the file the writers create; logrotate
  keeps the file's own mode.

Archives stay next to the live file (`x.log.0.gz`, `x.log.1.gz`, … with
newsyslog; `x.log.1`, `x.log.2.gz`, … with logrotate). Re-running
`instrukt-ai-log-setup` rewrites a stale conf. The reader includes them when it slices by
time.
