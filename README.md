# cal-tui

Calcurse-style terminal calendar and todo manager in Rust, built on plain
iCalendar files (vdir) synced with CalDAV (e.g. Radicale) via vdirsyncer.

Status: milestone 3 (TUI and shell commands, with editing). Notifications and
configurable layouts are not implemented yet. See [DESIGN.md](DESIGN.md)
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

In the TUI: `a` add, `e` edit, `d` delete. For a repeating event you choose
"only this occurrence" or "all occurrences". In the todo panel `x` marks done
and `+`/`-` change the priority. Press `?` for all keys.

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
