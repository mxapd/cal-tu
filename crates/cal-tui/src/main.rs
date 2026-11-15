mod fmt;
mod notify;
mod output;
mod tui;

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
    /// Check the config file for mistakes and exit (non-zero if any).
    CheckConfig,
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

/// Find exactly one todo by UID or case-insensitive title substring.
fn find_todo(store: &Store, query: &str, completed: bool) -> Result<cal_core::Todo> {
    let todos: Vec<_> = store
        .todos()
        .into_iter()
        .filter(|t| t.completed == completed)
        .collect();
    if let Some(t) = todos.iter().find(|t| t.uid == query) {
        return Ok(t.clone());
    }
    let q = query.to_lowercase();
    let exact: Vec<_> = todos
        .iter()
        .filter(|t| t.summary.to_lowercase() == q)
        .collect();
    let matches: Vec<_> = if exact.len() == 1 {
        exact
    } else {
        todos
            .iter()
            .filter(|t| t.summary.to_lowercase().contains(&q))
            .collect()
    };
    match matches.as_slice() {
        [t] => Ok((*t).clone()),
        [] => bail!(
            "no {} todo matches '{query}'",
            if completed { "completed" } else { "open" }
        ),
        many => {
            let list: Vec<_> = many.iter().map(|t| format!("  {}", t.summary)).collect();
            bail!(
                "'{query}' matches {} todos; be more specific:\n{}",
                many.len(),
                list.join("\n")
            )
        }
    }
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
                    "calendars.{id}.color: unknown colour '{c}' (use a name like cyan or #rrggbb)"
                ));
            }
        }
    }
    problems
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let config_path = cli.config.clone().unwrap_or_else(Config::default_path);
    let mut config = Config::load(&config_path)?;
    if let Some(dir) = &cli.calendars_dir {
        config.calendars_dir = Some(dir.to_string_lossy().into_owned());
    }

    if matches!(cli.command, Some(Command::CheckConfig)) {
        let problems = check_config(&config);
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
            if !calendars.is_empty() {
                let wanted: Vec<usize> = calendars
                    .iter()
                    .map(|c| {
                        store
                            .calendar_index(c)
                            .with_context(|| format!("unknown calendar '{c}'"))
                    })
                    .collect::<Result<_>>()?;
                for (i, cal) in store.calendars.iter_mut().enumerate() {
                    cal.hidden = !wanted.contains(&i);
                }
            }
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
            let next = inst
                .iter()
                .filter(|i| !i.all_day && i.kind == cal_core::Kind::Event)
                .find(|i| i.start.instant(store.home) >= now);
            if let Some(i) = next {
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
            let q =
                cal_core::quick::parse_event(&text.join(" "), now).map_err(anyhow::Error::msg)?;
            let cal =
                store.target_calendar(calendar.as_deref(), config.default_calendar.as_deref())?;
            let reminder = remind
                .map(|r| {
                    cal_core::config::parse_offset(&r)
                        .with_context(|| format!("cannot parse reminder '{r}' (use e.g. 15m, 1h)"))
                })
                .transpose()?;
            let rrule = match repeat {
                Some(r) => cal_core::quick::parse_repeat(&r).map_err(anyhow::Error::msg)?,
                None => None,
            };
            let ev = cal_core::NewEvent {
                summary: q.summary,
                location,
                description,
                start: q.start,
                end: q.end,
                rrule,
                reminder,
            };
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
                let cal = store
                    .target_calendar(calendar.as_deref(), config.default_calendar.as_deref())?;
                let due = due
                    .map(|d| cal_core::quick::parse_due(&d, now).map_err(anyhow::Error::msg))
                    .transpose()?;
                let todo = cal_core::NewTodo {
                    summary: text.join(" "),
                    description,
                    due,
                    priority,
                };
                store.create_todo(cal, &todo, now.with_timezone(&Utc))?;
                println!(
                    "Added todo: {}",
                    output::describe_new_todo(&store, cal, &todo)
                );
            }
            Some(TodoAction::Done { query }) => {
                let t = find_todo(&store, &query.join(" "), false)?;
                store.set_todo_completed(t.object, true, now.with_timezone(&Utc))?;
                println!("Done: {}", t.summary);
            }
            Some(TodoAction::Undone { query }) => {
                let t = find_todo(&store, &query.join(" "), true)?;
                store.set_todo_completed(t.object, false, now.with_timezone(&Utc))?;
                println!("Not done: {}", t.summary);
            }
        },
        Command::Notify { .. } | Command::CheckConfig => unreachable!(),
    }

    output::report_warnings(&warnings, cli.verbose);
    Ok(())
}
