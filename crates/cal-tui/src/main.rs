mod fmt;
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
#[command(version, about)]
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
    /// Add an event, e.g. `add "Dentist fri 14:00 1h"` (milestone 3).
    Add {
        text: String,
        #[arg(long, short = 'c')]
        calendar: Option<String>,
        #[arg(long)]
        remind: Option<String>,
    },
    /// List todos.
    Todo {
        /// Include completed todos.
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Run the reminder daemon (milestone 4).
    Notify,
    /// Check the config file for mistakes and exit (non-zero if any).
    CheckConfig,
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
    if matches!(command, Command::Add { .. } | Command::Notify) {
        bail!("not implemented yet; see DESIGN.md milestones");
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
        Command::Todo { all, json } => output::todos(&store, all, json, color)?,
        Command::Add { .. } | Command::Notify | Command::CheckConfig => unreachable!(),
    }

    output::report_warnings(&warnings, cli.verbose);
    Ok(())
}
