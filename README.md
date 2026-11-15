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

Add `services.cal-tui-notify.enable = true;` for reminders. Shell completions
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
