# cal-tui

Calcurse-style terminal calendar and todo manager in Rust, built on plain
iCalendar files (vdir) synced with CalDAV (e.g. Radicale) via vdirsyncer.

Status: all six planned milestones done (TUI with editing and rearrangeable
layouts, shell commands, reminders, Nix packaging). Next: styling, and testing
against real calendars. See [DESIGN.md](DESIGN.md)
for the plan.

## Usage

```sh
cal-tui                                    # full-screen TUI (press ? for keys)
cal-tui today                              # today's items
cal-tui agenda --days 14 -c uni            # upcoming items, one calendar
cal-tui next --format '{time} {summary}'   # for status bars
cal-tui todo --all                         # todos, including completed
cal-tui agenda --json                      # machine-readable output

cal-tui add Dentist fri 14:00 1h -r 15m    # add an event (reminder 15 min before)
cal-tui add Conference 12-01 3d            # no time = all day
cal-tui add Gym tue 7:00 --repeat weekly   # repeating
cal-tui add -n Call mum 18:30-19:00        # dry run: show what would be added
cal-tui todo add Pay rent --due fri -p 1   # add a todo
cal-tui todo done rent                     # complete it (by part of the title)
```

The mouse works too: click a day (or a line of the agenda under the month) to go
there, click an item to select it and again to open it, scroll over the
calendar to change month or use the ‹ › arrows by its title, and drag the lines
between panels to resize them (saved when you let go). Set `mouse = false` in the config to turn this off;
while it is on, hold Shift to select text in the terminal.

In the TUI: `a` add, `e` edit, `d` delete. For a repeating event you choose
"only this occurrence" or "all occurrences". In the todo panel `x` marks done
and `+`/`-` change the priority. Press `?` for all keys.

## Layouts

Press `w` in the TUI for layout mode: `←`/`→` cycle presets (`classic`,
`classic-left`, `three-columns`, `stacked`, plus your own), `Tab` picks a panel,
`+`/`-` resize it, `<`/`>` resize its column, `s` swaps it with its neighbour, `r`
turns a row into a column, `x` hides it, `u` brings hidden panels back, `Enter`
saves (to `~/.local/state/cal-tui/layout.toml`) and `Esc` leaves.

Layouts can also be declared in the config (or in Nix via
`programs.cal-tui.settings.layout`); those take precedence over saved ones:

```toml
[layout]
active = "mine"
narrow = { below_width = 70, preset = "stacked" }   # small terminals

[layout.presets.mine]
split = "columns"
children = [
  { panel = "appointments", size = "60%" },
  { split = "rows", children = [ { panel = "calendar", size = 9 }, { panel = "todo" } ] },
]
```

## Weather

Optional: the forecast next to the date in the appointments panel (today with
the current temperature, and up to 16 days ahead):

```toml
[weather]
location = "Stockholm"          # or latitude = 59.33, longitude = 18.07
# units = "fahrenheit"
```

Data comes from [Open-Meteo](https://open-meteo.com) (free, no account); only the
location is sent. It refreshes hourly in the background and is cached in
`~/.cache/cal-tui/`, so it also shows offline.

## Reminders

`cal-tui notify` runs in the background and shows a desktop notification for
each reminder: the alarms set on events and todos, plus `default_reminder` from
a calendar's config for timed events that have none (handy for a subscribed
feed). Each reminder is shown once, also across restarts. Reminders missed
while the computer was off are still shown if the event has not started yet.

```sh
cal-tui notify --upcoming 24h    # what will fire in the next 24 hours
cal-tui notify                   # run the service (normally via systemd)
cal-tui notify --command 'notify-send "$CAL_TUI_SUMMARY" "$CAL_TUI_BODY"'
```

With home-manager: `services.cal-tui-notify.enable = true;` (a systemd user
service started with your graphical session).

## Jarvis

cal-tui can be a [Jarvis](http://gitea.yggdrasil.com/BMSwahn/jarvis) service
while staying a normal standalone TUI. Both use the same synced calendars:

- **`cal-tui sync`** syncs (`sync_command`, e.g. vdirsyncer) and then posts to
  the Jarvis journal what changed since the last sync: new, moved, changed and
  removed events, and todos new, done or removed (`#changed`; nothing when
  nothing changed). The TUI's `S` key syncs the same way, and syncs never run
  side by side.
- **`cal-tui serve`** answers Jarvis (below). Every change made through it is
  journaled (`#edited`) and pushed to the server at once.
- **`cal-tui report`** posts the day at a glance: today's and tomorrow's
  events and the open todos (`#agenda`, `#overdue` when a todo is late).

Where entries go is the config; without `[jarvis]` nothing is posted:

```toml
[jarvis]
service = "calendar"                                   # default
report_url = "http://127.0.0.1:8485/report/calendar"   # default: $JARVIS_URL/report/<service>
fail_after = 6                                         # default
```

A sync that fails (the server unreachable) is retried quietly: only the
`fail_after`th failure in a row makes `cal-tui sync` exit with an error, which
Jarvis reports once as `#failed #critical`, and the first sync that works
again posts `#resolved`.

```sh
cal-tui sync --print                   # sync, print the entry instead of posting it (still remembered)
cal-tui sync --no-sync --print         # only compare with the last sync
cal-tui report --print                 # the day at a glance, printed
```

On NixOS, `nixosModules.default` runs all of this as system units for one
user (so Jarvis can watch them, logged in or not):

```nix
imports = [ inputs.cal-tui.nixosModules.default ];
services.cal-tui.jarvis = { enable = true; user = "alice"; };   # syncAt, digestAt, socket, ...
jarvis.endpoints.calendar = {
  socket = "/run/cal-tui/api.sock";
  socketGroup = "calendar-api";
  units = [ "cal-tui-sync.service" "cal-tui-api.service" "cal-tui-digest.service" ];
  timer = "cal-tui-sync.timer";
  maxAge = "40m";
  allow.ai = [ "today" "agenda" "next" "todos" "calendars" ];
};
```

That is `cal-tui-sync.timer` (every 5 minutes), `cal-tui-digest.timer` (07:00)
and `cal-tui-api.socket`, which starts `cal-tui serve` on Jarvis's first
request. They use the user's own `~/.config/cal-tui/config.toml`, calendars
and vdirsyncer config, so drop any sync timer of your own.

### Asking the calendar: `cal-tui serve`

`cal-tui serve` answers questions about the calendars over HTTP. It speaks
Jarvis's Service API (`GET /describe`, `POST /<operation>` with JSON in and
out), so Jarvis can be pointed at it, and Jarvis's MCP endpoint then offers its
operations to AIs as tools (`calendar__agenda`, ...). Each answer is JSON
(the same items as `--json`), with a markdown `text` for people and AIs.

| Operation | Kind | Parameters |
|---|---|---|
| `today` | read | |
| `agenda` | read | `from` (2026-11-20, tomorrow, fri, +3d), `days` (1–62), `calendars` |
| `next` | read | `within` (days) |
| `todos` | read | `all` |
| `calendars` | read | |
| `add_event` | command | `text` (as `cal-tui add`: "Dentist fri 14:00 1h"), `calendar`, `location`, `description`, `remind`, `repeat`, `dry_run` |
| `add_todo` | command | `summary`, `due`, `priority`, `calendar`, `description` |
| `complete_todo` | command | `query` (uid or part of the title), `undo` |
| `edit_event` | command | `id`, `summary`, `location`, `description`, `date`, `time` (14:00-15:30, 14:00, all day), `all` |
| `delete_event` | command | `id`, `all` |
| `edit_todo` | command | `query`, `summary`, `due`, `priority`, `description` (`""` clears) |
| `delete_todo` | command | `query` |
| `panel_month` | panel | `month` (2026-12, next, -1) |
| `panel_week` | panel | `from`, `days` |
| `panel_day` | panel | `day` |
| `panel_todos` | panel | `all` |

Every event in the answers has an `id`: its UID, plus `@<start>` for one
occurrence of a repeating event. `edit_event` and `delete_event` with such an
id change only that occurrence, unless `all` is set.

The panels are the calendar's tabs in `jarvis ls`:

- **Month**: a grid of the month, a dot per event, `[n]` for today.
- **Week**: today, the next event and the todos as tiles, then the events by
  day.
- **Day**: one day in full (place, notes, repeat rule, reminders), then its
  events.
- **Todos**: the todos to tick off.

Events are changed where they are listed, in Week and Day: enter on a Day,
Time, Event or Where cell edits it (a new day moves the event, an emptied cell
clears it), and a row's actions delete the occurrence or the whole series.
Todos are changed in the Todos table the same way (title, due, priority;
Done, Not done, Delete). Every change is one of the commands above, and Jarvis
asks you first; `=` changes a panel's range. Panels are for you only: Jarvis
never offers them to AIs.

```sh
head -c 32 /dev/urandom | base64 > ~/.config/cal-tui/token
cal-tui serve --listen 100.64.0.5:8487 --token-file ~/.config/cal-tui/token
cal-tui serve --listen 127.0.0.1:8487                # loopback: no token needed
cal-tui serve --socket /run/user/1000/cal-tui.sock   # a unix socket
jarvis check http://100.64.0.5:8487 --token-file ~/.config/cal-tui/token
```

With a token, every request needs `Authorization: Bearer <token>`. An address
that other machines can reach needs a token, or `serve` refuses to start.
Without `--listen` or `--socket`, it uses the socket that systemd passes
(socket activation), and `--idle 600` makes it exit after ten idle minutes. The
calendars are read again on every request, so a sync is seen immediately.

On the Jarvis host, the token goes in a secret:

```nix
jarvis.endpoints.calendar = {
  url = "http://100.64.0.5:8487";
  tokenFile = config.sops.secrets.cal-tui-token.path;
  # you: everything; ai: only these. Add the commands once you trust it.
  allow.ai = [ "today" "agenda" "next" "todos" "calendars" ];
};
```

Where Jarvis runs, use the NixOS module above (a socket, no token) instead.

## Editing

Edits are written atomically. If a sync changed the file since it was loaded,
the edit is refused and the calendars are reloaded instead of overwriting the
other change. Calendars marked `read_only` can never be changed.

Calendars are read from `~/.local/share/calendars/<calendar>/*.ics` (one folder
per calendar, as written by vdirsyncer). Optional config in
`~/.config/cal-tui/config.toml`:

```toml
timezone = "Europe/Stockholm"        # default: system zone

[calendars.uni]
color = "cyan"                       # default: vdir `color` file
read_only = true
hide_matching = ["^Tutorial group B"]
```

Check a config without starting the app: `cal-tui check-config`.

## Nix / home-manager

```nix
# flake inputs
cal-tui.url = "path:/home/xam/Projects/cal-tui";   # or a git URL later
cal-tui.inputs.nixpkgs.follows = "nixpkgs";

# home-manager configuration
imports = [ inputs.cal-tui.homeManagerModules.default ];
programs.cal-tui = {
  enable = true;
  settings = {
    timezone = "Europe/Stockholm";
    default_calendar = "personal";
    sync_command = "vdirsyncer sync";
    calendars.uni = {
      color = "cyan";
      read_only = true;
      hide_matching = [ "^Tutorial group B" ];
    };
  };
};
```

Add `services.cal-tui-notify.enable = true;` for reminders, and
`services.cal-tui-serve = { enable = true; listen = "100.64.0.5:8487"; tokenFile = "/path/to/token"; };`
to run `cal-tui serve` as a user service. Shell completions
(bash, zsh, fish) come with the package. To use it as `pkgs.cal-tui` instead,
add `inputs.cal-tui.overlays.default` to your nixpkgs overlays.

This installs `cal-tui` and writes `~/.config/cal-tui/config.toml`. The file is
checked with `cal-tui check-config` during the build, so mistakes fail the
rebuild with a message naming the bad key.

## Development

```sh
nix develop
cargo test                       # all tests
UPDATE_SNAPSHOTS=1 cargo test    # accept intended output changes, then git diff
nix flake check                  # tests, clippy, rustfmt, home-manager module
git config core.hooksPath .githooks   # optional: run checks before each commit
```

See `examples/vdirsyncer.config` for syncing with Radicale and subscribing to a
read-only `.ics` feed.
