# cal-tui — design

A calcurse-style terminal calendar and todo manager in Rust, built on plain
iCalendar files (vdir) and synced to CalDAV through vdirsyncer.

`cal-tui` is a working name.

## Goals

- **A visual TUI for planning time.** It follows calcurse's look: an
  appointments panel, a month/week calendar panel and a todo panel, with a
  status bar and vim-style keys.
- **Quick shell commands** for use outside the TUI (`today`, `agenda`, `next`,
  `add`, `todo`), with `--json` output for scripts and status bars.
- **Desktop notifications** from a background reminder service.
- **Multiple calendars.** Each calendar has its own colour, can be hidden or
  shown, and can be marked read-only (for example a subscribed university feed).
- **Reliability.** It never corrupts or quietly drops data, whether written by
  this app, another client (a phone) or a feed.

## Non-goals

- **No networking in the app.** Sync is vdirsyncer's job, both two-way sync
  with Radicale and read-only `http` subscriptions.
- **No import from calcurse.** Everything starts fresh.
- **No custom file format.** Data is standard `.ics` files only.

## Architecture

```
            ┌──────────── Radicale (CalDAV) ───────────┐   uni .ics feed (http)
            │                                          │          │
            └──────────────── vdirsyncer ──────────────┴──────────┘
                                   │  (systemd timer / `S` key in TUI)
                                   ▼
        ~/.local/share/calendars/<calendar>/<uid>.ics      (vdir)
                                   ▲
              ┌────────────────────┼─────────────────────┐
              │               cal-core (lib)              │
              │  store · model · recurrence · query · edit│
              └───────┬───────────────┬──────────────┬────┘
                      │               │              │
                  TUI (ratatui)   CLI (clap)   notify daemon
                      └─────── single binary `cal-tui` ──┘
```

### Crates

| Crate | Responsibility |
|---|---|
| `cal-core` | Read and write vdirs; parse and serialise iCal; expand recurrences; run queries over date ranges; edit (whole series or one occurrence); read config. No UI code. |
| `cal-tui` (bin) | Subcommands: none (starts the TUI), `today`, `agenda`, `next`, `add`, `todo`, `notify`. |

Expected libraries: `icalendar` (or a lenient parser of our own if feeds need
it), `rrule`, `chrono` + `chrono-tz`, `ratatui` + `crossterm`, `clap`,
`notify` (file watching), `notify-rust` (desktop notifications), `serde` +
`toml`, `directories`.

## Storage (vdir)

- Each calendar is a folder under `calendars_dir`. vdirsyncer writes `color`
  and `displayname` metadata files into it, and the config can override them.
- Each item is one `.ics` file holding one `VCALENDAR`, which holds one
  `VEVENT` or `VTODO` main component plus any override components (same `UID`
  with a `RECURRENCE-ID`) and `VTIMEZONE`s.
- **Item identity is `(calendar, UID)`.** The filename is never used as an
  identifier.

### Reliability rules

1. **Lossless round-trip.** The parsed model keeps every property and
   parameter, including unknown `X-` properties, `VALARM`s and `ATTENDEE`s.
   An edit changes only the fields the user touched and then re-serialises.
   This is the most important rule.
2. **Atomic writes.** Write `<file>.tmp`, fsync, then rename. Never write in
   place.
3. **Lenient reading, strict writing.** If a file fails to parse, the item is
   skipped and a warning is shown (status bar or stderr). It never crashes the
   app or hides a whole calendar.
4. **Read-only calendars** are enforced in the core: every edit API returns an
   error for them, whatever the UI does.
5. **Detect external changes.** Record each file's mtime/hash when it is read.
   Before writing, check it again; if the file changed underneath us (a sync
   ran), reload and ask instead of overwriting.
6. **Time zones.** Map Windows/Outlook TZIDs (for example
   `W. Europe Standard Time`) to IANA names. Floating times use the local zone.
   New items are written with an IANA `TZID`.

## Recurrence

- Expand `RRULE`/`RDATE`/`EXDATE` within the queried range only (lazily).
- Apply overrides (`RECURRENCE-ID`) on top of the expanded instances.
  Cancelled instances (`STATUS:CANCELLED`) and `EXDATE`s are removed.
- **Editing scope.** When editing or deleting a repeating item, the UI asks:
  - **This occurrence:** write or update an override component (edit), or add
    an `EXDATE` (delete).
  - **All occurrences:** edit the main component. Moving the start shifts the
    existing overrides consistently.
  - *(Later)* **This and following:** split the series (end the old `RRULE`
    with `UNTIL`, then a new UID).
- **Test heavily:** daylight-saving transitions, all-day vs. timed items,
  overrides that move an instance to another day, and `UNTIL` vs. `COUNT`.

### Editing rules as implemented

- Files with syntax problems are shown but never rewritten (the lenient parser
  may have skipped lines). The edit is refused with the reason.
- Changes touch only the edited properties. `DTSTAMP`, `LAST-MODIFIED` and
  `SEQUENCE` are updated. A `VTIMEZONE` is added for IANA `TZID`s that lack
  one, so other clients understand the times. Existing `TZID`s (including
  Windows names) and floating/UTC forms are kept.
- Whole-series time changes move the series by the wall-clock difference of
  the occurrence being edited (so DST does not shift it). `EXDATE`s and
  override `RECURRENCE-ID`s move along; overrides the user moved keep their
  own time.
- Moving a series to another *day* when its rule pins days (`BYDAY`,
  `BYMONTHDAY`, …) is refused unless the rule is changed in the same edit.
- A series title/location/notes change also updates overrides that still had
  the old value.
- "Does not repeat" keeps the occurrence being viewed and drops exceptions.
- Notes are edited in the form on one line (`\\n` = new line). Editing them in
  `$EDITOR` is still to do.

## Todos

- Stored as `VTODO`. Fields: `SUMMARY`, `PRIORITY` (1–9, same as calcurse; 0 =
  none), `DUE`, `STATUS`/`COMPLETED`, `DESCRIPTION`.
- The todo panel sorts by priority, then due date. Completed todos can be
  shown or hidden with a toggle.
- Todos with a due date also appear on that day in the appointments panel.

## TUI

Default layout (classic, like calcurse):

```
┌─ Appointments ──────────────┐┌─ Calendar ─┐
│ all-day items               ││ month grid │
│ ─────────                   │└────────────┘
│ timed items, in calendar    │┌─ Todo ─────┐
│ colours, [calendar] tag     ││            │
└─────────────────────────────┘└────────────┘
 date | time | visible calendars | next: … in 0:28
 key hints
```

- **Panels:** `appointments`, `calendar` (monthly or weekly), `todo`.
  `agenda` and `week` panels may come later.
- **Calendar colours** are used for items and for the day markers in the grid.
  Read-only items are dimmed or marked and cannot be edited.
- **Notes:** the description is opened in `$EDITOR`.

### Layout system

The layout is a tree of `rows`/`columns` splits whose leaves are panels. A size
is a percentage, a fixed number of cells, or omitted (takes the remaining
space). Named presets live in the config.

```toml
[layout]
active = "classic"
narrow = { below_width = 80, preset = "stacked" }

[layout.presets.classic]
split = "columns"
children = [
  { panel = "appointments", size = "60%" },
  { split = "rows", children = [
      { panel = "calendar", size = 9 },
      { panel = "todo" },
  ]},
]
```

In the TUI you can cycle through presets, grow or shrink the focused panel,
swap or move the focused panel, hide or show a panel, and save the current
layout as a preset.

As implemented: the model and TOML parsing live in `cal_core::layout` (so
`check-config` and the Nix build validate layouts); built-in presets are
`classic`, `classic-left`, `three-columns`, `stacked`. Layout mode is `w`
(`L` stays "next week"). Resizing turns the panel's size into a percentage;
fixed-size neighbours give way and percentage neighbours shrink proportionally.
Invalid layout settings never stop the app: it falls back to `classic` and says
why.

**The app never writes `config.toml`.** It may be generated by Nix and be
read-only. Layouts saved from inside the TUI go to
`$XDG_STATE_HOME/cal-tui/layout.toml` (default `~/.local/state/cal-tui/`).
Precedence: a `layout` set in `config.toml` (for example from Nix) wins over the
state file; the TUI says so when you try to save over a config-defined layout.

### Keys (initial; follow calcurse where they make sense)

| Key | Action |
|---|---|
| `h j k l` / arrows | move by day/week (calendar), select item (lists) |
| `Tab` / `Shift-Tab` | next/previous panel |
| `a` / `e` / `d` | add / edit / delete (asks for the scope on repeating items) |
| `g` / `t` | go to date / today |
| `n` | edit note (description) in `$EDITOR` |
| `c` | toggle calendar visibility |
| `S` | run sync command, then reload |
| `w` | layout mode (cycle presets, resize, swap, rotate, hide, save) |
| `?` / `q` | help / quit |

## CLI

```
cal-tui                          # TUI
cal-tui today [--json]
cal-tui agenda [--days N] [--calendar NAME]... [--json]
cal-tui next [--format FMT]     # one line, for status bars
cal-tui add "Dentist fri 14:00 1h" [-c CAL] [--remind 15m]
cal-tui todo [list|add|done] ...
cal-tui notify                   # reminder daemon
```

## Notifications

As implemented (milestone 4):

- No file watcher: every 20 s the service compares a cheap fingerprint of the
  calendar folders (`Store::fingerprint`) and reloads on change. Simpler and
  robust across sync tools; the TUI uses the same check.
- Reminders are computed by `Store::alarms(from, to)` in cal-core: VALARMs
  relative to start/end, absolute triggers, `REPEAT`/`DURATION`,
  `ACKNOWLEDGED` respected, `EMAIL`/`PROCEDURE` ignored; overrides without
  their own alarms use the series' alarms; `default_reminder` applies to timed
  events without alarms (not all-day ones).
- Delivered keys are stored in `$XDG_STATE_HOME/cal-tui/notified` and pruned
  after 3 days. Missed reminders are delivered while the event is still ahead,
  or if at most `--catch-up` (default 10 min) old. A failed delivery (no
  notification daemon) is retried on the next check.
- Sinks: desktop (D-Bus via `notify-rust`), `--print`, or `--command`.

- `cal-tui notify` runs as a systemd user service. It watches the vdirs (with
  `notify`), keeps an in-memory index of upcoming alarms for the next N hours,
  and sends desktop notifications through `notify-rust`.
- **Alarm sources:** each item's `VALARM`s (`TRIGGER` relative to start or
  absolute), plus per-calendar defaults from the config (for example 15 minutes
  before `uni` events, which have no `VALARM`).
- **Each alarm fires exactly once.** A small state file records sent
  `(calendar, uid, recurrence-id, trigger)` keys, and old keys are pruned.

## Config (`~/.config/cal-tui/config.toml`)

```toml
calendars_dir = "~/.local/share/calendars"
sync_command  = "vdirsyncer sync"
default_calendar = "personal"

[calendars.uni]
color = "cyan"
read_only = true
default_reminder = "15m"
hide_matching = ["^Tutorial group B"]   # optional title filter

[calendars.personal]
color = "green"
```

## Nix / home-manager

The flake exports `homeManagerModules.default` (also `homeModules.default`):
`programs.cal-tui.settings` is turned into `config.toml` with typed options for
the known keys and free-form pass-through for the rest. The generated file is
validated with `cal-tui check-config` at build time, so a typo or invalid value
(unknown key, time zone, colour, reminder, regex) fails the rebuild instead of
the app. The `notify` service (milestone 4) will be added to the module as
`services.cal-tui-notify`. Sync stays with home-manager's own vdirsyncer
module (`accounts.calendar` + `services.vdirsyncer`).

## Testing

- `cal-core` unit tests and fixture tests (`crates/cal-core/tests/fixtures`)
  cover parsing, time zones, recurrence and loading.
- CLI tests run the real binary against the fixtures at a fixed `--now` and
  compare full output with `crates/cal-tui/tests/snapshots/*.txt`.
- TUI tests render to an in-memory terminal and compare the whole screen with
  snapshots. `UPDATE_SNAPSHOTS=1 cargo test` rewrites snapshots after an
  intended change; review them with `git diff`. A missing snapshot fails.
- `nix flake check` runs the tests (in the package build), clippy, rustfmt
  and the home-manager module checks. `.githooks/pre-commit` runs the cargo
  part locally (`git config core.hooksPath .githooks`).

## Milestones

1. *(done)* **Core read path:** read vdirs leniently, expand recurrences with overrides
   and time zones, and make `agenda`, `today` and `next` work against a real
   Radicale vdir and the university feed. Tests use fixture `.ics` files.
2. *(done)* **TUI (read-only):** the classic layout, navigation, calendar colours and
   visibility toggles.
3. *(done)* **Editing:** add, edit and delete events and todos, with series vs.
   occurrence scope, atomic writes and conflict detection. Includes the CLI
   `add` and `todo` commands.
4. *(done)* **Notifications:** the `notify` daemon and a systemd unit.
5. *(done)* **Layout engine:** presets, in-app rearranging, saving to the state file
   (config-defined layouts take precedence).
6. **Packaging:** the Nix package and home-manager module exist already;
   remaining: the `notify` systemd unit in the module, release builds.
