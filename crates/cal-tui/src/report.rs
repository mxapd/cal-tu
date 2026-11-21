//! `cal-tui report`: the day at a glance as a markdown entry for a journal
//! such as Jarvis (`POST <url>?tags=a,b`, the body is markdown and its first
//! line the summary).

use std::time::Duration as StdDuration;

use anyhow::{Context, Result};
use cal_core::{Instance, Kind, Moment, Store, Todo};
use chrono::{DateTime, Duration, NaiveDate};
use chrono_tz::Tz;

/// Where entries go when neither --url nor $JARVIS_URL says otherwise:
/// Jarvis's own default address.
pub const DEFAULT_SERVER: &str = "http://127.0.0.1:8485";

pub struct Entry {
    pub body: String,
    pub tags: Vec<&'static str>,
}

fn plural(n: usize, one: &str) -> String {
    format!("{n} {one}{}", if n == 1 { "" } else { "s" })
}

pub fn due_text(due: &Moment, home: Tz) -> String {
    match due {
        Moment::Date(d) => d.format("%a %d %b").to_string(),
        Moment::DateTime(dt) => dt.with_timezone(&home).format("%a %d %b %H:%M").to_string(),
    }
}

pub fn is_overdue(due: &Moment, now: DateTime<Tz>) -> bool {
    match due {
        Moment::Date(d) => *d < now.date_naive(),
        Moment::DateTime(dt) => dt.with_timezone(&now.timezone()) < now,
    }
}

fn is_due_today(due: &Moment, now: DateTime<Tz>, home: Tz) -> bool {
    due.instant(home).date_naive() == now.date_naive() && !is_overdue(due, now)
}

/// The events of one day, all-day ones first, as markdown list lines.
fn day_lines(store: &Store, inst: &[Instance], day: NaiveDate) -> Vec<String> {
    item_lines(store, inst, day, false)
}

/// The items of one day, all-day ones first, as markdown list lines; todos
/// (marked as such) only `with_todos`.
pub fn item_lines(
    store: &Store,
    inst: &[Instance],
    day: NaiveDate,
    with_todos: bool,
) -> Vec<String> {
    let mut items: Vec<&Instance> = inst
        .iter()
        .filter(|i| (with_todos || i.kind == Kind::Event) && store.occurs_on(i, day))
        .collect();
    items.sort_by_key(|i| (!i.all_day, i.start.instant(store.home)));
    items
        .iter()
        .map(|i| {
            let time = crate::fmt::time_range(i, day, store.home).column();
            let todo = if i.kind == Kind::Todo { "todo: " } else { "" };
            let mut line = format!("- {} {todo}{}", time.trim(), i.summary);
            if let Some(loc) = &i.location {
                line.push_str(&format!(" · {loc}"));
            }
            line.push_str(&format!(" *({})*", store.calendars[i.calendar].name));
            line
        })
        .collect()
}

pub fn todo_line(store: &Store, t: &Todo, now: DateTime<Tz>) -> String {
    let mut line = "- ".to_string();
    if t.due.is_some_and(|d| is_overdue(&d, now)) {
        line.push_str("**overdue** ");
    }
    line.push_str(&t.summary);
    if let Some(d) = &t.due {
        line.push_str(&format!(" (due {})", due_text(d, store.home)));
    }
    if t.priority > 0 {
        line.push_str(&format!(" · priority {}", t.priority));
    }
    line
}

/// Today's events, tomorrow's, and the open todos.
pub fn build(store: &Store, now: DateTime<Tz>) -> Entry {
    let today = now.date_naive();
    let tomorrow = today + Duration::days(1);
    let (inst, _) = store.instances(
        store.day_start(today),
        store.day_start(today + Duration::days(2)),
    );
    let today_lines = day_lines(store, &inst, today);
    let tomorrow_lines = day_lines(store, &inst, tomorrow);

    let open: Vec<Todo> = store.todos().into_iter().filter(|t| !t.completed).collect();
    let overdue = open
        .iter()
        .filter(|t| t.due.is_some_and(|d| is_overdue(&d, now)))
        .count();
    let due_today = open
        .iter()
        .filter(|t| t.due.is_some_and(|d| is_due_today(&d, now, store.home)))
        .count();
    // Overdue first, then as `todos()` orders them.
    let mut todos: Vec<&Todo> = open.iter().collect();
    todos.sort_by_key(|t| !t.due.is_some_and(|d| is_overdue(&d, now)));

    let mut summary = if today_lines.is_empty() {
        "Nothing scheduled today".to_string()
    } else {
        format!("{} today", plural(today_lines.len(), "event"))
    };
    if due_today > 0 {
        summary.push_str(&format!(", {} due", plural(due_today, "todo")));
    }
    if overdue > 0 {
        summary.push_str(&format!(", {overdue} overdue"));
    }
    summary.push('.');

    let mut body = vec![summary, String::new()];
    body.push(format!("### Today · {}", today.format("%a %d %b")));
    if today_lines.is_empty() {
        body.push("Nothing scheduled.".into());
    }
    body.extend(today_lines);
    if !tomorrow_lines.is_empty() {
        body.push(String::new());
        body.push(format!("### Tomorrow · {}", tomorrow.format("%a %d %b")));
        body.extend(tomorrow_lines);
    }
    if !todos.is_empty() {
        body.push(String::new());
        body.push(format!("### Todos ({})", todos.len()));
        body.extend(todos.iter().map(|t| todo_line(store, t, now)));
    }

    let mut tags = vec!["agenda"];
    if overdue > 0 {
        tags.push("overdue");
    }
    Entry {
        body: body.join("\n") + "\n",
        tags,
    }
}

/// Where `service`'s entries go on Jarvis's default address.
pub fn default_url(service: &str) -> String {
    format!("{DEFAULT_SERVER}/report/{service}")
}

pub fn post(url: &str, entry: &Entry) -> Result<()> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        // A Jarvis that is down should not hold up a sync for long.
        .timeout_connect(Some(StdDuration::from_secs(3)))
        .timeout_global(Some(StdDuration::from_secs(15)))
        .build()
        .into();
    agent
        .post(url)
        .query("tags", entry.tags.join(","))
        .header("Content-Type", "text/markdown; charset=utf-8")
        .send(entry.body.as_bytes())
        .with_context(|| format!("cannot post the report to {url}"))?;
    Ok(())
}
