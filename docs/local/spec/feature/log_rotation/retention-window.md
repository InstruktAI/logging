---
description:
  "Feature: how much log history the rotation assets keep — a stated archive
  depth, identical on both rotator backends, rather than a side effect of write volume."
---

# Log Retention Window — Spec

## What it is

The retention window is how far back `instrukt-ai-logs` can still read. It is
set by the rotation assets `instrukt-ai-log-setup` writes: each backend keeps a
fixed number of rotated archives per log file and discards the oldest on every
rotation.

The depth is a stated contract of this library, identical on macOS and Linux,
and it is the only retention knob that exists — there is no per-app override
and no age-based expiry.

## Canonical fields

### Archive depth

Both backends retain **30** archives per log file, alongside the live segment
(gzipped; on Linux the newest stays plain until the next rotation):

- macOS: the `count` field of the `newsyslog.conf` line.
- Linux: the `rotate` directive of the `logrotate.conf` stanza.

The library declares this depth once and emits it into both templates, so the
two backends cannot drift apart.

### Rotation trigger

Rotation is triggered by segment size — 50 MB — on both backends, unchanged by
this contract. Size is the sole trigger; there is no time-based trigger.

### What the depth does and does not promise

The contract is **30 archives**, not 30 days. The window in days is
`30 × 50 MB ÷ daily write volume`, so it stretches when a service is quiet and
contracts when it is loud. At a typical daemon volume of tens of MB per day the
window is roughly a month; during an error storm it is shorter, and that is the
regime in which history matters most.

Retention is bounded by count rather than by age because neither backend can
expire archives by age on its own terms:

- `newsyslog` has no age-of-archive field at all. Its fields are logfile, owner,
  mode, count, size, when, and flags; `when` triggers a rotation, it never
  expires an archive.
- `logrotate`'s `maxage` is evaluated only when a rotation is actually triggered
  for that logfile. With size as the trigger, a low-volume log never reaches the
  threshold, so `maxage` never runs — and when it eventually does, it deletes
  archives the depth would otherwise have kept. An age bound therefore reduces
  retention for quiet services without adding any for loud ones.

Storage is not the constraint at this depth: real archives compress at roughly
14:1, so 30 archives cost on the order of 100 MB per log file.

### Use cases

#### UC-RW1: the macOS rotation contract declares the library's archive depth

```gherkin
Given a host whose platform uses the newsyslog backend
When the runtime install writes the rotation assets
Then the emitted newsyslog line carries the library's declared archive depth in
  the rotator's count field
And the line still rotates on the 50 MB size trigger
And the archives are gzip-compressed, mode 600
```

#### UC-RW2: the Linux rotation contract declares the same archive depth

```gherkin
Given a host whose platform uses the logrotate backend
When the runtime install writes the rotation assets
Then the emitted logrotate stanza carries the library's declared archive depth
  in the rotator's rotate directive
And the depth is the same one the newsyslog backend declares
And the stanza still rotates on the 50 MB size trigger
```

## Known caveats

- The live segment is outside the window's protection: nothing bounds it but the
  size trigger, so a service writing faster than it rotates keeps the newest
  history in one uncompressed file.
- Rotation, and therefore discarding of the oldest archive, only happens when the
  per-user scheduler runs. A host whose scheduler is not wired retains
  everything and grows without bound; `instrukt-ai-log-setup` reports that as a
  problem rather than silently tolerating it.

## See Also

- docs/local/design/architecture.md — the rotation-ensure flow that emits these assets.
