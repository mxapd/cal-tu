# cal-tui

Calcurse-style terminal calendar and todo manager in Rust, built on plain
iCalendar files (vdir) synced with CalDAV (e.g. Radicale) via vdirsyncer.

Status: early scaffolding. See [DESIGN.md](DESIGN.md) for the plan.

## Development

```sh
nix develop
cargo build
cargo test
```

See `examples/vdirsyncer.config` for syncing with Radicale and subscribing to a
read-only `.ics` feed.
