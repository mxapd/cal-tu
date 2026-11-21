mod fmt;
#[cfg(feature = "jarvis")]
mod journal;
mod notify;
mod ops;
mod output;
#[cfg(feature = "jarvis")]
mod panels;
#[cfg(feature = "jarvis")]
mod report;
#[cfg(feature = "jarvis")]
mod serve;
mod sync;
mod tui;
mod weather;

use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use cal_core::{Config, Store};
use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, Utc};
use chrono_tz::Tz;
use clap::{Parser, Subcommand};

/// Calcurse-style terminal calendar and todo manager on vdir/CalDAV.
#[derive(Parser)]
#[command(version, about, args_override_self = true)]
struct Cli {
    /// Config file [default: ~/.config/cal-tui/config.toml]
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Calendars directory, overriding the config.
    #[arg(long, global = true)]
    calendars_dir: Option<PathBuf>,
    /// Print every loading warning instead of a summary.
    #[arg(long, short, global = true)]
    verbose: bool,
    /// Pretend it is this time (`YYYY-MM-DDTHH:MM` in the home zone, or
    /// RFC 3339). Used by tests.
    #[arg(long, global = true, hide = true)]
    now: Option<String>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Show today's items.
    Today {
        #[arg(long)]
        json: bool,
    },
    /// Show upcoming items, grouped by day.
    Agenda {
        /// First day (YYYY-MM-DD) [default: today]
        #[arg(long)]
        from: Option<NaiveDate>,
        #[arg(long, default_value_t = 7)]
        days: u32,
        /// Only these calendars (folder or display name); repeatable.
        #[arg(long = "calendar", short = 'c')]
        calendars: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Print the next upcoming timed event on one line (for status bars).
    Next {
        /// Placeholders: {time} {summary} {in} {calendar} {location}
        #[arg(long, default_value = "{time} {summary} (in {in})")]
        format: String,
        /// How far ahead to look, in days.
        #[arg(long, default_value_t = 7)]
        within: u32,
        #[arg(long)]
        json: bool,
    },
    /// Add an event: `add Dentist fri 14:00 1h`, `add Call mum 18:30-19:00`,
    /// `add Conference 12-01 3d` (no time = all day).
    ///
    /// Dates: today, tomorrow, mon…sun (next one), +3d, MM-DD, YYYY-MM-DD.
    /// Times: 14:00, 2pm, 14:00-15:30. Lengths: 1h, 30m, 1h30, 2d.
    /// Prefix a word with \ to keep it in the title.
    #[command(verbatim_doc_comment)]
    Add {
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
        /// Calendar (folder or display name) [default: config default_calendar]
        #[arg(long, short = 'c')]
        calendar: Option<String>,
        /// Reminder before the start, e.g. 15m, 1h, 1d.
        #[arg(long, short = 'r')]
        remind: Option<String>,
        #[arg(long, short = 'l')]
        location: Option<String>,
        /// Notes.
        #[arg(long, short = 'd')]
        description: Option<String>,
        /// daily, weekdays, weekly, biweekly, monthly, yearly, or FREQ=...
        #[arg(long)]
        repeat: Option<String>,
        /// Show what would be added without saving.
        #[arg(long, short = 'n')]
        dry_run: bool,
    },
    /// List todos, or add / complete one.
    Todo {
        #[command(subcommand)]
        action: Option<TodoAction>,
        /// Include completed todos.
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Reminder service: sends a desktop notification for each reminder
    /// (VALARMs, plus the calendar default_reminder for timed events without
    /// one). Runs until stopped; see --once and --upcoming.
    Notify {
        /// Check once, deliver what is due, and exit.
        #[arg(long)]
        once: bool,
        /// Print reminders to stdout instead of showing notifications.
        #[arg(long, conflicts_with = "command")]
        print: bool,
        /// Run this shell command per reminder instead; details are in
        /// $CAL_TUI_SUMMARY, $CAL_TUI_BODY, $CAL_TUI_START, $CAL_TUI_CALENDAR,
        /// $CAL_TUI_LOCATION, $CAL_TUI_UID.
        #[arg(long)]
        command: Option<String>,
        /// List the reminders due within this time (e.g. 24h, 7d) and exit.
        #[arg(long)]
        upcoming: Option<String>,
        /// Deliver missed reminders up to this old even if the event already
        /// started (reminders for events still ahead are always delivered).
        #[arg(long, default_value = "10m")]
        catch_up: String,
        /// Where delivered reminders are recorded.
        #[arg(long, default_value_os_t = notify::default_state_path())]
        state: PathBuf,
    },
    /// Post the day at a glance to a journal such as Jarvis: today's and
    /// tomorrow's events and the open todos, as markdown (#agenda, and
    /// #overdue when a todo is late).
    #[cfg(feature = "jarvis")]
    Report {
        /// Where to POST it [default: $JARVIS_URL/report/<service>, or
        /// http://127.0.0.1:8485/report/<service>]
        #[arg(long)]
        url: Option<String>,
        /// The service name the entry is filed under [default: jarvis.service
        /// in the config, or calendar]
        #[arg(long)]
        service: Option<String>,
        /// Print the entry instead of posting it.
        #[arg(long)]
        print: bool,
    },
    /// Answer questions about the calendars over HTTP: the Jarvis Service
    /// API (GET /describe, POST /<operation> with JSON), so that Jarvis can
    /// offer them to AIs as MCP tools.
    ///
    /// Listens on --listen or --socket; with neither, on the socket systemd
    /// passes (socket activation).
    #[cfg(feature = "jarvis")]
    #[command(verbatim_doc_comment)]
    Serve {
        /// TCP address, e.g. 127.0.0.1:8487 (port 0: any free port).
        #[arg(long, conflicts_with = "socket")]
        listen: Option<String>,
        /// Unix socket path.
        #[arg(long)]
        socket: Option<PathBuf>,
        /// File holding the bearer token every request must carry. Required
        /// when --listen is not a loopback address.
        #[arg(long, env = "CAL_TUI_TOKEN_FILE")]
        token_file: Option<PathBuf>,
        /// The service name /describe gives.
        #[arg(long, default_value = "calendar")]
        service: String,
        /// Exit after this many seconds without a request (0: never).
        #[arg(long, default_value_t = 0)]
        idle: u64,
    },
    /// Sync the calendars (sync_command, e.g. vdirsyncer), one at a time
    /// with the TUI's `S`.
    #[cfg_attr(
        feature = "jarvis",
        doc = "\nWith [jarvis] in the config, post to the journal what changed since\n\
               the last sync. A failed sync exits 0 until it has failed\n\
               jarvis.fail_after times in a row (default 6)."
    )]
    Sync {
        /// Print the entries instead of posting them (what was seen is
        /// still remembered, so the next sync does not report it again).
        #[cfg(feature = "jarvis")]
        #[arg(long)]
        print: bool,
        /// Only compare with the last sync; do not run sync_command.
        #[cfg(feature = "jarvis")]
        #[arg(long)]
        no_sync: bool,
    },
    /// Check the config file for mistakes and exit (non-zero if any).
    CheckConfig,
    /// Print a shell completion script: `cal-tui completions zsh`.
    Completions { shell: clap_complete::Shell },
}

#[derive(Subcommand)]
enum TodoAction {
    /// Add a todo: `todo add Pay rent --due fri -p 1`.
    Add {
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
        /// Due date, optionally with time: fri, 12-24, "tomorrow 9am".
        #[arg(long)]
        due: Option<String>,
        /// Priority 1 (highest) – 9; 0 = none.
        #[arg(long, short = 'p', default_value_t = 0, value_parser = clap::value_parser!(u8).range(0..=9))]
        priority: u8,
        #[arg(long, short = 'c')]
        calendar: Option<String>,
        #[arg(long, short = 'd')]
        description: Option<String>,
    },
    /// Mark a todo done, found by UID or by (part of) its title.
    Done {
        #[arg(required = true, num_args = 1..)]
        query: Vec<String>,
    },
    /// Mark a completed todo as not done.
    Undone {
        #[arg(required = true, num_args = 1..)]
        query: Vec<String>,
    },
}

/// Parse `--now`: RFC 3339, or a local `YYYY-MM-DDTHH:MM[:SS]` in `home`.
fn parse_now(s: &str, home: Tz) -> Result<DateTime<Tz>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&home));
    }
    for f in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M"] {
        if let Ok(n) = NaiveDateTime::parse_from_str(s, f) {
            return Ok(cal_core::time::local_to_zoned(home, n));
        }
    }
    bail!("cannot parse --now '{s}'")
}

/// Every problem in the config, including colours (which only the UI knows).
fn check_config(config: &Config) -> Vec<String> {
    let mut problems = config.validate();
    for (id, cal) in &config.calendars {
        if let Some(c) = &cal.color {
            if fmt::parse_color(Some(c)).is_none() {
                problems.push(format!(
                    "calendars.{id}.color: unknown colour '{c}' (use orange, teal, lavender, sage, sky, rose, sand, an ANSI name like cyan, or #rrggbb)"
                ));
            }
        }
    }
    problems
}

/// `cal-tui sync`'s exit: an error for the failure that is to be reported.
fn sync_exit(outcome: sync::Outcome) -> Result<()> {
    match outcome {
        sync::Outcome::Failed {
            message,
            failures,
            alert: true,
        } => {
            if failures > 1 {
                bail!("calendar sync has failed {failures} times in a row: {message}")
            }
            bail!("calendar sync failed: {message}")
        }
        _ => Ok(()),
    }
}

/// `[jarvis]` in a build without the feature: worth a note, not an error,
/// so one config serves both builds.
fn feature_notes(config: &Config) -> Vec<String> {
    if cfg!(feature = "jarvis") || config.jarvis.is_none() {
        return Vec::new();
    }
    vec!["note: [jarvis] is ignored: cal-tui was built without the jarvis feature".into()]
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Some(Command::Completions { shell }) = cli.command {
        use clap::CommandFactory;
        clap_complete::generate(
            shell,
            &mut Cli::command(),
            "cal-tui",
            &mut std::io::stdout(),
        );
        return Ok(());
    }
    let config_path = cli.config.clone().unwrap_or_else(Config::default_path);
    let mut config = Config::load(&config_path)?;
    if let Some(dir) = &cli.calendars_dir {
        config.calendars_dir = Some(dir.to_string_lossy().into_owned());
    }

    if matches!(cli.command, Some(Command::CheckConfig)) {
        let problems = check_config(&config);
        for n in feature_notes(&config) {
            eprintln!("{}: {n}", config_path.display());
        }
        if problems.is_empty() {
            println!("{}: ok", config_path.display());
            return Ok(());
        }
        for p in &problems {
            eprintln!("{}: {p}", config_path.display());
        }
        std::process::exit(1);
    }

    let home = config.home_zone()?;
    let now = match &cli.now {
        Some(s) => parse_now(s, home)?,
        None => Utc::now().with_timezone(&home),
    };

    let Some(command) = cli.command else {
        return tui::run(config, cli.now.as_ref().map(|_| now));
    };
    if let Command::Notify {
        once,
        print,
        command,
        upcoming,
        catch_up,
        state,
    } = command
    {
        let offset = |s: &str, what: &str| {
            cal_core::config::parse_offset(s)
                .with_context(|| format!("cannot parse {what} '{s}' (use e.g. 10m, 24h, 7d)"))
        };
        if let Some(within) = upcoming {
            let within = offset(&within, "--upcoming")?;
            let store = Store::load(&config)?;
            notify::upcoming(&store, now, within);
            return Ok(());
        }
        let sink = match (print, command) {
            (true, _) => notify::Sink::Print,
            (_, Some(c)) => notify::Sink::Command(c),
            _ => notify::Sink::Desktop,
        };
        let opts = notify::Options {
            once,
            sink,
            catch_up: offset(&catch_up, "--catch-up")?,
            state,
        };
        return notify::run(config, cli.now.as_ref().map(|_| now), opts);
    }

    #[cfg(feature = "jarvis")]
    if let Command::Serve {
        listen,
        socket,
        token_file,
        service,
        idle,
    } = command
    {
        let opts = serve::Options {
            listen,
            socket,
            token_file,
            service,
            idle: std::time::Duration::from_secs(idle),
            now: cli.now.as_ref().map(|_| now),
        };
        return serve::run(config, opts);
    }

    #[cfg(feature = "jarvis")]
    if let Command::Sync { print, no_sync } = command {
        let opts = sync::Options {
            run_command: !no_sync,
            print,
            may_alert: true,
            ..sync::Options::new()
        };
        return sync_exit(sync::run(&config, now, &opts)?);
    }
    #[cfg(not(feature = "jarvis"))]
    if let Command::Sync {} = command {
        let opts = sync::Options {
            may_alert: true,
            ..sync::Options::new()
        };
        return sync_exit(sync::run(&config, now, &opts)?);
    }

    let mut store = Store::load(&config)?;
    let color = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let today = now.date_naive();
    let mut warnings = store.warnings.clone();

    match command {
        Command::Today { json } => {
            let (inst, w) = store.instances(
                store.day_start(today),
                store.day_start(today + Duration::days(1)),
            );
            warnings.extend(w);
            output::agenda(&store, &inst, today, 1, json, color)?;
        }
        Command::Agenda {
            from,
            days,
            calendars,
            json,
        } => {
            ops::only_calendars(&mut store, &calendars)?;
            let from = from.unwrap_or(today);
            let days = days.max(1);
            let (inst, w) = store.instances(
                store.day_start(from),
                store.day_start(from + Duration::days(days as i64)),
            );
            warnings.extend(w);
            output::agenda(&store, &inst, from, days, json, color)?;
        }
        Command::Next {
            format,
            within,
            json,
        } => {
            let (inst, w) = store.instances(now, now + Duration::days(within as i64));
            warnings.extend(w);
            if let Some(i) = ops::next_event(&store, &inst, now) {
                output::next(&store, i, now, &format, json)?;
            }
        }
        Command::Add {
            text,
            calendar,
            remind,
            location,
            description,
            repeat,
            dry_run,
        } => {
            let (cal, ev) = ops::new_event(
                &store,
                &config,
                now,
                ops::EventInput {
                    text: text.join(" "),
                    calendar,
                    remind,
                    location,
                    description,
                    repeat,
                },
            )?;
            let line = output::describe_new_event(&store, cal, &ev);
            if dry_run {
                println!("Would add: {line}");
            } else {
                store.create_event(cal, &ev, now.with_timezone(&Utc))?;
                println!("Added: {line}");
            }
        }
        Command::Todo { action, all, json } => match action {
            None => output::todos(&store, all, json, color)?,
            Some(TodoAction::Add {
                text,
                due,
                priority,
                calendar,
                description,
            }) => {
                let (cal, todo) = ops::new_todo(
                    &store,
                    &config,
                    now,
                    ops::TodoInput {
                        summary: text.join(" "),
                        due,
                        priority,
                        calendar,
                        description,
                    },
                )?;
                store.create_todo(cal, &todo, now.with_timezone(&Utc))?;
                println!(
                    "Added todo: {}",
                    output::describe_new_todo(&store, cal, &todo)
                );
            }
            Some(TodoAction::Done { query }) => {
                let t = ops::complete_todo(&store, &query.join(" "), false, now)?;
                println!("Done: {}", t.summary);
            }
            Some(TodoAction::Undone { query }) => {
                let t = ops::complete_todo(&store, &query.join(" "), true, now)?;
                println!("Not done: {}", t.summary);
            }
        },
        #[cfg(feature = "jarvis")]
        Command::Report {
            url,
            service,
            print,
        } => {
            let entry = report::build(&store, now);
            if print {
                println!("tags: {}\n", entry.tags.join(" "));
                print!("{}", entry.body);
            } else {
                let mut jarvis = config.jarvis.clone().unwrap_or_default();
                if let Some(service) = service {
                    jarvis.service = Some(service);
                }
                // --url, else [jarvis] report_url, else $JARVIS_URL/report/<service>,
                // else the same on Jarvis's default address.
                let url = url
                    .or_else(|| jarvis.report_url(std::env::var("JARVIS_URL").ok()))
                    .unwrap_or_else(|| report::default_url(jarvis.service()));
                report::post(&url, &entry)?;
                println!(
                    "Posted to {url}: {}",
                    entry.body.lines().next().unwrap_or("")
                );
            }
        }
        // Notify, Serve, Sync, CheckConfig and Completions returned above.
        _ => unreachable!(),
    }

    output::report_warnings(&warnings, cli.verbose);
    Ok(())
}
