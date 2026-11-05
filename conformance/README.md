# Conformance fixtures

Language-neutral cases that pin the logging contract (`docs/contract.md`).
Every case is one JSON file. The same files are run by the Python test suite
and the Rust test suite in the same CI; a case that one side cannot run is a
defect in the case, not a reason to skip it on that side.

Runners always use a fresh temporary `HOME`, `XDG_STATE_HOME` and
`XDG_CONFIG_HOME`. They never touch the real `~/.local/state`, and they never
reach a real `launchctl` or `systemctl` (see `setup/`).

Normalization happens in the runner, never by weakening a fixture: timestamps
are checked for shape and range, then stripped; temporary paths are substituted
back into placeholders before comparison.

## Placeholders

Rendered by the runner before writing inputs and before comparing outputs.

| Placeholder | Value |
| --- | --- |
| `{{T-<n>}}` | the runner's "now" minus `<n>` seconds, formatted as a log timestamp `YYYY-MM-DDTHH:MM:SS.mmmZ` (UTC) |
| `{{STATE}}` | the temporary `XDG_STATE_HOME` |
| `{{HOME}}` | the temporary `HOME` |
| `{{UID}}` | the current numeric user id |
| `{{VERSION}}` | the version under test (the repository version) |

"Now" is captured once per case, before inputs are written.

## `writer/*.json` — calls → exact lines and file path

```json
{
  "description": "what this pins",
  "app": "teleclaude",
  "source": null,
  "max_message_chars": null,
  "env": { "TELECLAUDE_LOG_LEVEL": "DEBUG" },
  "calls": [
    {
      "logger": "teleclaude.core",
      "level": "INFO",
      "msg": "job_started",
      "kv": [
        { "key": "job_id", "type": "str", "value": "abc123" },
        { "key": "attempt", "type": "int", "value": 3 }
      ]
    }
  ],
  "expect": {
    "path": "{{STATE}}/instrukt-ai/teleclaude/teleclaude.log",
    "lines": ["level=INFO logger=teleclaude.core msg=\"job_started\" attempt=3 job_id=abc123"]
  }
}
```

- `source` / `max_message_chars`: `null` means "not passed" (library default).
- `env` is the complete environment contract for the case; runners clear every
  `<PREFIX>_*` contract variable that the case does not set. `env` may also
  set `XDG_STATE_HOME`, overriding the runner's temporary one; an empty value
  means "unset" and the expected path then lives under `{{HOME}}/.local/state`.
- `level` is one of `TRACE`, `DEBUG`, `INFO`, `WARNING`, `ERROR`. (`CRITICAL`
  has no tracing equivalent and is pinned by Python-only unit tests.)
- `kv[].type` is one of `str`, `int`, `float`, `bool`. Order in `kv` is call
  order; the formatter sorts. (`None`/`null` cannot be expressed by a tracing
  field and is pinned by Python-only unit tests.)
- `logger` always uses `.` as separator in fixtures.
- `expect.lines` is every line in the file, in order, each without its leading
  `<timestamp> `. The runner asserts each line starts with a valid timestamp
  between case start and case end, strips it, then compares exactly. An empty
  list means the file exists and is empty.

## `reader/*.json` — log tree + arguments → stdout, stderr, exit code

```json
{
  "description": "what this pins",
  "files": [
    {
      "path": "teleclaude/teleclaude.log",
      "mtime_offset_s": -10,
      "gzip": false,
      "lines": ["{{T-120}} level=INFO logger=teleclaude msg=\"a\"", "continuation line"]
    }
  ],
  "args": ["teleclaude", "--since", "5m"],
  "follow": null,
  "expect": {
    "exit": 0,
    "stdout": ["{{T-120}} level=INFO logger=teleclaude msg=\"a\"", "continuation line"],
    "stderr": { "match": "exact", "value": "" }
  }
}
```

- `files[].path` is relative to `{{STATE}}/instrukt-ai/`. Each line is written
  followed by `\n`. `gzip: true` writes the file gzip-compressed.
  `mtime_offset_s` (seconds relative to now, default `0`) sets the file mtime.
- `stdout` is the exact list of output lines (each compared without its `\n`).
  It may instead be an object of the same shape as `stderr`
  (`{ "match": "exact" | "contains" | "regex", "value": "..." }`), matched
  against the whole captured stdout text; this is for banners such as
  `--version` that print to stdout and are not byte-pinned.
- `stderr.match` is `exact`, `contains`, or `regex`; `value` is rendered with
  placeholders first. Help, usage and `--version` banners are not byte-pinned
  across implementations: such cases pin the exit code and use `contains`.
- Keep every timestamp at least 30 s away from a `--since` boundary so the
  runner's and the binary's "now" cannot disagree about a line.
- `follow` (only with `-f`/`--follow` in `args`):

  ```json
  {
    "run_ms": 2500,
    "appends": [
      { "after_ms": 600, "path": "teleclaude/teleclaude.log", "lines": ["{{T-0}} level=INFO logger=teleclaude msg=\"live\""] },
      { "after_ms": 900, "rotate": "teleclaude/teleclaude.log" }
    ]
  }
  ```

  The runner starts the process, performs each step at `after_ms` after start
  (`lines` appends; `rotate` renames the file to `<name>.0` and creates a new
  empty file at the same path), then at `run_ms` sends SIGINT and collects
  output. `expect.exit` is the exit status after SIGINT.

## `setup/*.json` — temporary home → files written, scheduler calls, output

```json
{
  "description": "what this pins",
  "platform": "darwin",
  "args": [],
  "preexisting": { "{{HOME}}/.config/instrukt-ai/newsyslog.conf": "stale\n" },
  "scheduler": [
    { "argv_prefix": ["launchctl", "bootstrap"], "exit": 5, "stderr": "Bootstrap failed: 5: Input/output error" },
    { "argv_prefix": ["launchctl", "print"], "exit": 0, "stderr": "" }
  ],
  "expect": {
    "exit": 0,
    "stdout": ["log root: {{STATE}}/instrukt-ai", "status: healthy"],
    "files": { "{{HOME}}/Library/LaunchAgents/ai.instrukt.log-rotate.plist": "<?xml ...full content...>" },
    "calls": [["launchctl", "bootstrap", "gui/{{UID}}", "{{HOME}}/Library/LaunchAgents/ai.instrukt.log-rotate.plist"]]
  }
}
```

- `platform` is `darwin` or `linux`; a runner executes only the cases for the
  host platform.
- The runner puts fake `launchctl` and `systemctl` executables in a temporary
  directory and sets `PATH` to that directory **only**, so a real scheduler can
  never be reached. Each fake records its argv (one JSON array per line) and
  answers with the first `scheduler` entry whose `argv_prefix` matches
  (default: exit 0, empty stderr).
- `expect.files` maps each path to its exact full content. Paths not listed
  must not have been created under `{{HOME}}` or `{{STATE}}`, except the fake
  scheduler's own files.
- `expect.calls` is the exact, ordered list of scheduler invocations.
