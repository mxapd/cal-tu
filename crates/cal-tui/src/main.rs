use anyhow::{bail, Result};
use clap::{Parser, Subcommand};

/// Calcurse-style terminal calendar and todo manager on vdir/CalDAV.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
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
    /// Show upcoming items.
    Agenda {
        #[arg(long, default_value_t = 7)]
        days: u32,
        #[arg(long = "calendar", short = 'c')]
        calendars: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Print the next upcoming item on one line (for status bars).
    Next {
        #[arg(long)]
        format: Option<String>,
    },
    /// Add an event, e.g. `add "Dentist fri 14:00 1h"`.
    Add {
        text: String,
        #[arg(long, short = 'c')]
        calendar: Option<String>,
        #[arg(long)]
        remind: Option<String>,
    },
    /// Manage todos.
    Todo,
    /// Run the reminder daemon.
    Notify,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        None => bail!("TUI not implemented yet (milestone 2)"),
        Some(_) => bail!("command not implemented yet (milestone 1)"),
    }
}
