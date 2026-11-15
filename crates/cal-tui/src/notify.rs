//! `cal-tui notify`: the reminder service.
//!
//! Every few seconds it checks the calendar folders for changes (reloading
//! when a sync or edit happened), works out which reminders are due and
//! delivers those not delivered before. Delivered reminders are recorded in a
//! state file, so each fires exactly once, also across restarts.
//!
//! Missed reminders (the computer was off or asleep) are still delivered if
//! the event has not started yet, or if they are at most `catch_up` old.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use cal_core::{Alarm, Config, Kind, Moment, Store};
use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;

use crate::fmt::humanize;

/// How often to check.
const TICK: std::time::Duration = std::time::Duration::from_secs(20);
/// How far back to look for missed reminders.
const MISSED_LOOKBACK: i64 = 1; // days
/// How long delivered reminders are remembered after they were due.
const KEEP_DAYS: i64 = 3;

/// Where reminders go.
pub enum Sink {
    /// Desktop notification (freedesktop D-Bus).
    Desktop,
    /// One line per reminder on stdout.
    Print,
    /// A shell command, with the details in `CAL_TUI_*` variables.
    Command(String),
}

pub struct Options {
    pub once: bool,
    pub sink: Sink,
    pub catch_up: Duration,
    pub state: PathBuf,
}

pub fn default_state_path() -> PathBuf {
    Config::state_dir().join("notified")
}

/// Keys of delivered reminders and when they were due (unix seconds).
struct State {
    path: PathBuf,
    fired: BTreeMap<String, i64>,
}

impl State {
    fn load(path: &Path) -> State {
        let fired = std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let (k, t) = l.rsplit_once('\t')?;
                Some((k.to_string(), t.parse().ok()?))
            })
            .collect();
        State {
            path: path.to_path_buf(),
            fired,
        }
    }

    fn save(&self) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("cannot create {}", dir.display()))?;
        }
        let text: String = self
            .fired
            .iter()
            .map(|(k, t)| format!("{k}\t{t}\n"))
            .collect();
        cal_core::fsio::write_atomic(&self.path, text.as_bytes())
            .with_context(|| format!("cannot write {}", self.path.display()))
    }

    fn prune(&mut self, now: DateTime<Tz>) {
        let cutoff = (now - Duration::days(KEEP_DAYS)).timestamp();
        self.fired.retain(|_, t| *t >= cutoff);
    }
}

/// Title and body of the notification for `a`, as seen at `now`.
pub fn message(store: &Store, a: &Alarm, now: DateTime<Tz>) -> (String, String) {
    let home = store.home;
    let today = now.date_naive();
    let day = |d: chrono::NaiveDate| {
        if d == today {
            "today".to_string()
        } else if d == today + Duration::days(1) {
            "tomorrow".to_string()
        } else {
            d.format("%a %d %b").to_string()
        }
    };
    let mut parts = Vec::new();
    match (a.kind, a.end) {
        (Kind::Todo, Moment::Date(d)) => parts.push(format!("due {}", day(d))),
        (Kind::Todo, due) => {
            let d = due.instant(home);
            parts.push(format!("due {} {}", day(d.date_naive()), d.format("%H:%M")));
        }
        (Kind::Event, _) if a.all_day => {
            parts.push(format!("{} (all day)", day(a.start.date_in(home))))
        }
        (Kind::Event, end) => {
            let s = a.start.instant(home);
            let e = end.instant(home);
            let until = s - now;
            parts.push(if until > Duration::minutes(1) {
                format!("in {}", humanize(until + Duration::seconds(30)))
            } else if until >= -Duration::minutes(1) {
                "now".into()
            } else {
                format!("started {} ago", humanize(-until + Duration::seconds(30)))
            });
            let date = if s.date_naive() == today {
                String::new()
            } else {
                format!("{} ", day(s.date_naive()))
            };
            parts.push(format!("{date}{}–{}", s.format("%H:%M"), e.format("%H:%M")));
        }
    }
    if let Some(l) = &a.location {
        parts.push(l.clone());
    }
    parts.push(format!("[{}]", store.calendars[a.calendar].name));
    (a.summary.clone(), parts.join(" · "))
}

fn deliver(store: &Store, a: &Alarm, now: DateTime<Tz>, sink: &Sink) -> Result<()> {
    let (title, body) = message(store, a, now);
    match sink {
        Sink::Print => {
            println!(
                "[{}] {title}: {body}",
                a.at.with_timezone(&store.home).format("%Y-%m-%d %H:%M")
            );
            Ok(())
        }
        Sink::Desktop => notify_rust::Notification::new()
            .appname("cal-tui")
            .summary(&title)
            .body(&body)
            .icon("x-office-calendar")
            .show()
            .map(|_| ())
            .context("cannot show desktop notification (is a notification daemon running?)"),
        Sink::Command(cmd) => {
            let start = match a.start {
                Moment::Date(d) => d.format("%Y-%m-%d").to_string(),
                Moment::DateTime(dt) => dt.with_timezone(&store.home).to_rfc3339(),
            };
            let status = std::process::Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .env("CAL_TUI_SUMMARY", &title)
                .env("CAL_TUI_BODY", &body)
                .env("CAL_TUI_START", start)
                .env("CAL_TUI_CALENDAR", &store.calendars[a.calendar].name)
                .env("CAL_TUI_LOCATION", a.location.as_deref().unwrap_or(""))
                .env("CAL_TUI_UID", &a.uid)
                .status()
                .with_context(|| format!("cannot run '{cmd}'"))?;
            anyhow::ensure!(status.success(), "'{cmd}' failed ({status})");
            Ok(())
        }
    }
}

/// One check: deliver what is due and not yet delivered. Returns whether the
/// state changed.
fn check(store: &Store, state: &mut State, now: DateTime<Tz>, opts: &Options) -> bool {
    let (alarms, _) = store.alarms(
        now - Duration::days(MISSED_LOOKBACK),
        now + Duration::seconds(1),
    );
    let mut changed = false;
    for a in alarms {
        if state.fired.contains_key(&a.key) {
            continue;
        }
        let still_ahead = a.start.instant(store.home) > now;
        let recent = a.at >= now - opts.catch_up;
        if recent || still_ahead {
            if let Err(e) = deliver(store, &a, now, &opts.sink) {
                // Not recorded: retried on the next check.
                eprintln!("cal-tui notify: {e:#}");
                continue;
            }
        }
        // Delivered, or too old to be useful: never consider it again.
        state.fired.insert(a.key.clone(), a.at.timestamp());
        changed = true;
    }
    changed
}

pub fn run(config: Config, now: Option<DateTime<Tz>>, opts: Options) -> Result<()> {
    let started = Instant::now();
    let home = config.home_zone()?;
    let clock = || match now {
        Some(n) => n + Duration::from_std(started.elapsed()).unwrap_or_default(),
        None => Utc::now().with_timezone(&home),
    };
    let root = config.calendars_dir();
    let mut store = Store::load(&config)?;
    let mut fp = store.fingerprint(&root);
    let mut state = State::load(&opts.state);
    loop {
        let now = clock();
        let current = store.fingerprint(&root);
        if current != fp {
            match Store::load(&config) {
                Ok(s) => {
                    store = s;
                    fp = store.fingerprint(&root);
                }
                Err(e) => eprintln!("cal-tui notify: reload failed: {e:#}"),
            }
        }
        if check(&store, &mut state, now, &opts) {
            state.prune(now);
            if let Err(e) = state.save() {
                eprintln!("cal-tui notify: {e:#}");
            }
        }
        if opts.once {
            return Ok(());
        }
        std::thread::sleep(TICK);
    }
}

/// `--upcoming`: list reminders due in `[now, now + within)`.
pub fn upcoming(store: &Store, now: DateTime<Tz>, within: Duration) {
    let (alarms, _) = store.alarms(now, now + within);
    if alarms.is_empty() {
        println!("no reminders");
    }
    for a in alarms {
        let (title, body) = message(store, &a, a.at);
        let src = if a.default { " (calendar default)" } else { "" };
        println!(
            "{}  {title}: {body}{src}",
            a.at.with_timezone(&store.home).format("%a %d %b %H:%M")
        );
    }
}
