//! `cal-tui sync`: sync the calendars and say in the journal what changed.
//!
//! The timer, the TUI's `S` key and `serve` (after an edit) all sync through
//! [`run`], one at a time: two vdirsyncer runs at once can corrupt its
//! status. After a sync, the calendars are compared with what the last run
//! saw (`seen.json` in the state dir), and the differences become one journal
//! entry (#changed). Nothing changed, nothing posted.
//!
//! A failed sync is retried quietly: only the `fail_after`th failure in a
//! row makes `cal-tui sync` exit with an error, which Jarvis turns into one
//! #failed #critical entry; the first sync that works again posts #resolved.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use cal_core::{Config, Kind, Store};
use chrono::{DateTime, Duration, NaiveDate};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::report::Entry;

/// How far ahead changes are watched.
const WINDOW_DAYS: i64 = 180;
/// Lines per section of an entry.
const MAX_LINES: usize = 10;

/// Held while syncing or touching the snapshot; released on drop.
pub struct Lock(#[allow(dead_code)] File);

pub fn lock(dir: &Path) -> Result<Lock> {
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let path = dir.join("sync.lock");
    let f = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    f.lock()
        .with_context(|| format!("cannot lock {}", path.display()))?;
    Ok(Lock(f))
}

// --- what the last run saw ---------------------------------------------------

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
struct EventSeen {
    uid: String,
    summary: String,
    /// Start as `moment_json` writes it; compared for moves.
    start: String,
    end: String,
    /// The start's day (YYYY-MM-DD), for the window.
    day: String,
    /// When, for people: `Fri 20 Nov 2026 14:00-15:00`.
    when: String,
    location: Option<String>,
    calendar: String,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
struct TodoSeen {
    summary: String,
    due: Option<String>,
    priority: u8,
    completed: bool,
}

#[derive(Serialize, Deserialize, Default)]
struct Snapshot {
    /// The days `events` covers: [from, to).
    from: String,
    to: String,
    events: BTreeMap<String, EventSeen>,
    todos: BTreeMap<String, TodoSeen>,
}

fn snapshot(store: &Store, today: NaiveDate) -> Snapshot {
    let to = today + Duration::days(WINDOW_DAYS);
    let (inst, _) = store.instances(store.day_start(today), store.day_start(to));
    let events = inst
        .iter()
        .filter(|i| i.kind == Kind::Event)
        .map(|i| {
            (
                crate::ops::event_id(store, i),
                EventSeen {
                    uid: i.uid.clone(),
                    summary: i.summary.clone(),
                    start: crate::output::moment_json(&i.start, store.home),
                    end: crate::output::moment_json(&i.end, store.home),
                    day: i
                        .start
                        .instant(store.home)
                        .date_naive()
                        .format("%Y-%m-%d")
                        .to_string(),
                    when: crate::output::describe_span(&i.start, &i.end, store.home),
                    location: i.location.clone(),
                    calendar: store.calendars[i.calendar].name.clone(),
                },
            )
        })
        .collect();
    let todos = store
        .todos()
        .iter()
        .map(|t| {
            (
                t.uid.clone(),
                TodoSeen {
                    summary: t.summary.clone(),
                    due: t.due.map(|d| crate::report::due_text(&d, store.home)),
                    priority: t.priority,
                    completed: t.completed,
                },
            )
        })
        .collect();
    Snapshot {
        from: today.format("%Y-%m-%d").to_string(),
        to: to.format("%Y-%m-%d").to_string(),
        events,
        todos,
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec(value)?)
        .with_context(|| format!("cannot write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("cannot write {}", path.display()))?;
    Ok(())
}

// --- the differences, as a journal entry -------------------------------------

fn event_line(e: &EventSeen) -> String {
    let mut line = format!("- {} {}", e.when, e.summary);
    if let Some(loc) = &e.location {
        line.push_str(&format!(" · {loc}"));
    }
    line.push_str(&format!(" *({})*", e.calendar));
    line
}

/// Lines, at most [`MAX_LINES`] of them.
fn capped(mut lines: Vec<String>) -> Vec<String> {
    if lines.len() > MAX_LINES {
        let more = lines.len() - MAX_LINES;
        lines.truncate(MAX_LINES);
        lines.push(format!("- …and {more} more"));
    }
    lines
}

/// Several occurrences of one series that changed the same way become one
/// line.
fn grouped(items: Vec<(&EventSeen, String)>, verb: &str) -> Vec<String> {
    let mut by_uid: BTreeMap<&str, Vec<(&EventSeen, String)>> = BTreeMap::new();
    let mut order = Vec::new();
    for (e, line) in items {
        if !by_uid.contains_key(e.uid.as_str()) {
            order.push(e.uid.as_str());
        }
        by_uid.entry(e.uid.as_str()).or_default().push((e, line));
    }
    let mut out = Vec::new();
    for uid in order {
        let group = &by_uid[uid];
        if group.len() == 1 {
            out.push(group[0].1.clone());
        } else {
            let e = group[0].0;
            out.push(format!(
                "- {}: {} occurrences {verb}, from {} *({})*",
                e.summary,
                group.len(),
                e.when,
                e.calendar
            ));
        }
    }
    out
}

fn plural(n: usize, one: &str) -> String {
    format!("{n} {one}{}", if n == 1 { "" } else { "s" })
}

/// What changed from `old` to `new`, as an entry; None when nothing did.
fn changes(old: &Snapshot, new: &Snapshot) -> Option<Entry> {
    // Only days both runs looked at: days rolling in or out of the window
    // are not news.
    let lo = old.from.as_str().max(new.from.as_str());
    let hi = old.to.as_str().min(new.to.as_str());
    let inside = |e: &EventSeen| lo <= e.day.as_str() && e.day.as_str() < hi;

    let mut added = Vec::new();
    let mut moved = Vec::new();
    let mut changed = Vec::new();
    let mut removed = Vec::new();
    for (id, e) in new.events.iter().filter(|(_, e)| inside(e)) {
        match old.events.get(id) {
            None => added.push((e, event_line(e))),
            Some(o) if o.start != e.start || o.end != e.end => {
                let mut line = format!("- {}: {} → {}", e.summary, o.when, e.when);
                line.push_str(&format!(" *({})*", e.calendar));
                moved.push((e, line));
            }
            Some(o) if o != e => {
                let mut what = Vec::new();
                if o.summary != e.summary {
                    what.push(format!("renamed from {}", o.summary));
                }
                if o.location != e.location {
                    what.push(match &e.location {
                        Some(l) => format!("now at {l}"),
                        None => "no place".into(),
                    });
                }
                if o.calendar != e.calendar {
                    what.push(format!("moved to {}", e.calendar));
                }
                changed.push((
                    e,
                    format!("- {} {}: {}", e.when, e.summary, what.join(", ")),
                ));
            }
            Some(_) => {}
        }
    }
    for (id, o) in old.events.iter().filter(|(_, o)| inside(o)) {
        if new.events.get(id).is_some_and(inside) {
            continue;
        }
        match new.events.get(id) {
            Some(e) => moved.push((
                e,
                format!(
                    "- {}: {} → {} *({})*",
                    e.summary, o.when, e.when, e.calendar
                ),
            )),
            None => removed.push((o, event_line(o))),
        }
    }

    let mut todo_lines = Vec::new();
    let (mut todo_new, mut todo_done, mut todo_gone, mut todo_changed) = (0, 0, 0, 0);
    let uids: BTreeSet<&String> = old.todos.keys().chain(new.todos.keys()).collect();
    for uid in uids {
        match (old.todos.get(uid), new.todos.get(uid)) {
            (None, Some(t)) => {
                todo_new += 1;
                let due = t
                    .due
                    .as_ref()
                    .map(|d| format!(" (due {d})"))
                    .unwrap_or_default();
                todo_lines.push(format!("- new: {}{due}", t.summary));
            }
            (Some(t), None) => {
                todo_gone += 1;
                todo_lines.push(format!("- removed: {}", t.summary));
            }
            (Some(o), Some(t)) if o != t => {
                if !o.completed && t.completed {
                    todo_done += 1;
                    todo_lines.push(format!("- done: {}", t.summary));
                    continue;
                }
                todo_changed += 1;
                let mut what = Vec::new();
                if o.completed && !t.completed {
                    what.push("reopened".to_string());
                }
                if o.summary != t.summary {
                    what.push(format!("renamed from {}", o.summary));
                }
                if o.due != t.due {
                    what.push(match &t.due {
                        Some(d) => format!("due {d}"),
                        None => "no due date".into(),
                    });
                }
                if o.priority != t.priority {
                    what.push(match t.priority {
                        0 => "no priority".to_string(),
                        p => format!("priority {p}"),
                    });
                }
                todo_lines.push(format!("- {}: {}", t.summary, what.join(", ")));
            }
            _ => {}
        }
    }

    let sections = [
        ("New", grouped(added, "added")),
        ("Moved", grouped(moved, "moved")),
        ("Changed", grouped(changed, "changed")),
        ("Removed", grouped(removed, "removed")),
    ];
    let counts: Vec<usize> = sections.iter().map(|(_, l)| l.len()).collect();
    if counts.iter().all(|&n| n == 0) && todo_lines.is_empty() {
        return None;
    }

    // "2 new events, 1 event moved; 1 todo done"
    let mut parts = Vec::new();
    let mut event_words = Vec::new();
    for ((name, _), &n) in sections.iter().zip(&counts) {
        if n == 0 {
            continue;
        }
        event_words.push(match *name {
            "New" => format!("{n} new event{}", if n == 1 { "" } else { "s" }),
            other => format!("{} {}", plural(n, "event"), other.to_lowercase()),
        });
    }
    if !event_words.is_empty() {
        parts.push(event_words.join(", "));
    }
    let mut todo_words = Vec::new();
    if todo_new > 0 {
        todo_words.push(format!(
            "{todo_new} new todo{}",
            if todo_new == 1 { "" } else { "s" }
        ));
    }
    if todo_done > 0 {
        todo_words.push(format!("{} done", plural(todo_done, "todo")));
    }
    if todo_changed > 0 {
        todo_words.push(format!("{} changed", plural(todo_changed, "todo")));
    }
    if todo_gone > 0 {
        todo_words.push(format!("{} removed", plural(todo_gone, "todo")));
    }
    if !todo_words.is_empty() {
        parts.push(todo_words.join(", "));
    }

    let mut body = vec![format!("{}.", parts.join("; ")), String::new()];
    for (name, lines) in sections {
        if lines.is_empty() {
            continue;
        }
        body.push(format!("### {name}"));
        body.extend(capped(lines));
        body.push(String::new());
    }
    if !todo_lines.is_empty() {
        body.push("### Todos".into());
        body.extend(capped(todo_lines));
        body.push(String::new());
    }
    while body.last().is_some_and(String::is_empty) {
        body.pop();
    }
    Some(Entry {
        body: body.join("\n") + "\n",
        tags: vec!["changed"],
    })
}

// --- running it --------------------------------------------------------------

#[derive(Serialize, Deserialize, Default)]
struct SyncState {
    /// Failed syncs in a row.
    failures: u32,
    /// Whether this run of failures was reported (exit 1 once).
    alerted: bool,
}

pub struct Options {
    /// Run `sync_command` (else only compare).
    pub run_command: bool,
    /// Print entries instead of posting them.
    pub print: bool,
    /// Where state lives; default `Config::state_dir()`.
    pub state_dir: PathBuf,
}

impl Options {
    pub fn new() -> Options {
        Options {
            run_command: true,
            print: false,
            state_dir: Config::state_dir(),
        }
    }
}

/// How a sync went.
pub enum Outcome {
    /// Synced (or nothing to run), and compared.
    Synced,
    /// The sync failed; `alert` when this failure is the one to report.
    Failed { message: String, alert: bool },
}

/// Where entries go, if anywhere.
pub fn report_url(config: &Config) -> Option<String> {
    config
        .jarvis
        .as_ref()?
        .report_url(std::env::var("JARVIS_URL").ok())
}

/// Post `entry` (or print it); a failure to post is logged, not fatal.
pub fn publish(config: &Config, entry: &Entry, print: bool) {
    if print {
        println!("tags: {}\n", entry.tags.join(" "));
        print!("{}", entry.body);
        return;
    }
    if let Some(url) = report_url(config) {
        if let Err(e) = crate::report::post(&url, entry) {
            eprintln!("journal: {e:#}");
        }
    }
}

fn run_command(cmd: &str) -> Result<(), String> {
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .output()
        .map_err(|e| format!("cannot run '{cmd}': {e}"))?;
    // Into the unit's log.
    let text = String::from_utf8_lossy(&out.stdout) + String::from_utf8_lossy(&out.stderr);
    eprint!("{text}");
    if out.status.success() {
        return Ok(());
    }
    // The cause, for the alert: vdirsyncer's first error line (its last
    // one is a hint about -vdebug), else the last line.
    let why = text
        .lines()
        .find(|l| l.trim_start().to_ascii_lowercase().starts_with("error:"))
        .or_else(|| text.lines().rev().find(|l| !l.trim().is_empty()))
        .unwrap_or("")
        .trim();
    Err(format!("'{cmd}' failed ({}): {why}", out.status))
}

/// Sync, then compare and post what changed.
pub fn run(config: &Config, now: DateTime<Tz>, opts: &Options) -> Result<Outcome> {
    let _lock = lock(&opts.state_dir)?;
    let state_path = opts.state_dir.join("sync.json");
    let mut state: SyncState = read_json(&state_path).unwrap_or_default();

    let result = match (&config.sync_command, opts.run_command) {
        (Some(cmd), true) => run_command(cmd),
        _ => Ok(()),
    };
    if let Err(message) = result {
        state.failures += 1;
        let fail_after = config.jarvis.as_ref().map_or(6, |j| j.fail_after());
        let alert = state.failures >= fail_after && !state.alerted;
        state.alerted |= alert;
        write_json(&state_path, &state)?;
        eprintln!("sync failed ({} in a row): {message}", state.failures);
        return Ok(Outcome::Failed { message, alert });
    }
    if state.alerted {
        publish(
            config,
            &Entry {
                body: format!(
                    "Calendar sync works again, after {} failed attempts.\n",
                    state.failures
                ),
                tags: vec!["resolved"],
            },
            opts.print,
        );
    }
    if state.failures > 0 || state.alerted {
        write_json(&state_path, &SyncState::default())?;
    }

    let store = Store::load(config)?;
    let new = snapshot(&store, now.date_naive());
    let seen_path = opts.state_dir.join("seen.json");
    if let Some(old) = read_json::<Snapshot>(&seen_path) {
        if let Some(entry) = changes(&old, &new) {
            publish(config, &entry, opts.print);
        }
    }
    write_json(&seen_path, &new)?;
    Ok(Outcome::Synced)
}

/// After an edit made here (serve): take what is now true about `uid` into
/// the snapshot, so the next sync does not report the edit a second time.
pub fn note_edit(store: &Store, uid: &str, now: DateTime<Tz>, state_dir: &Path) -> Result<()> {
    let _lock = lock(state_dir)?;
    let seen_path = state_dir.join("seen.json");
    let Some(mut seen) = read_json::<Snapshot>(&seen_path) else {
        return Ok(()); // never synced: nothing to keep in step
    };
    let today = NaiveDate::parse_from_str(&seen.from, "%Y-%m-%d").unwrap_or(now.date_naive());
    let fresh = snapshot(store, today);
    seen.events.retain(|_, e| e.uid != uid);
    seen.events
        .extend(fresh.events.into_iter().filter(|(_, e)| e.uid == uid));
    seen.todos.remove(uid);
    if let Some(t) = fresh.todos.get(uid) {
        seen.todos.insert(uid.to_string(), t.clone());
    }
    write_json(&seen_path, &seen)
}
