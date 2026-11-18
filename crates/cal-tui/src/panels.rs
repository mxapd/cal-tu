//! Jarvis panels: tabs of the calendar service in `jarvis ls`, drawn live by
//! `cal-tui serve` (the operations `panel_week` and `panel_todos`). The
//! format is jarvis_panel.py's: a title, an order among the tabs and blocks
//! (stats tiles, tables you can act on). A table's actions and its add form
//! are operations of this same service, so ticking off a todo in the panel
//! is `complete_todo`, and Jarvis asks before it runs one.
//!
//! Panels are for you: Jarvis never offers them over MCP.

use cal_core::{Instance, Kind, Moment, Store, Todo};
use chrono::{DateTime, Duration, NaiveDate};
use chrono_tz::Tz;
use serde_json::{json, Value};

use crate::report::is_overdue;

/// (operation, tab title, order among the service's tabs)
pub const PANELS: &[(&str, &str, i64)] =
    &[("panel_week", "Week", 10), ("panel_todos", "Todos", 20)];

/// A date cell: sorts by `v`, shows `t`.
fn date_cell(d: NaiveDate) -> Value {
    json!({"v": d.format("%Y-%m-%d").to_string(), "t": d.format("%a %d %b").to_string()})
}

fn due_cell(due: &Moment, home: Tz) -> Value {
    match due {
        Moment::Date(d) => date_cell(*d),
        Moment::DateTime(dt) => {
            let dt = dt.with_timezone(&home);
            json!({"v": dt.format("%Y-%m-%d %H:%M").to_string(),
                   "t": dt.format("%a %d %b %H:%M").to_string()})
        }
    }
}

fn plural(n: usize, one: &str) -> String {
    format!("{n} {one}{}", if n == 1 { "" } else { "s" })
}

fn open_todos(store: &Store) -> Vec<Todo> {
    store.todos().into_iter().filter(|t| !t.completed).collect()
}

/// The events of `days` days from `from` on: tiles for today, the next event
/// and the todos, then a table by day.
pub fn week(store: &Store, from: NaiveDate, days: i64, now: DateTime<Tz>) -> Value {
    let today = now.date_naive();
    let (inst, _) = store.instances(
        store.day_start(from),
        store.day_start(from + Duration::days(days)),
    );
    let mut rows = Vec::new();
    for n in 0..days {
        let day = from + Duration::days(n);
        let mut items: Vec<&Instance> = inst
            .iter()
            .filter(|i| i.kind == Kind::Event && store.occurs_on(i, day))
            .collect();
        items.sort_by_key(|i| (!i.all_day, i.start.instant(store.home)));
        for i in items {
            let time = crate::fmt::time_range(i, day, store.home).column();
            rows.push(json!({
                "id": format!("{}|{}|{}", i.uid, day, i.start.instant(store.home).to_rfc3339()),
                "day": date_cell(day),
                "time": time.trim(),
                "summary": i.summary,
                "location": i.location.as_deref().unwrap_or(""),
                "calendar": store.calendars[i.calendar].name,
            }));
        }
    }

    // Tiles: about now, whatever range the table shows.
    let (today_inst, _) = store.instances(
        store.day_start(today),
        store.day_start(today + Duration::days(1)),
    );
    let today_events = today_inst
        .iter()
        .filter(|i| i.kind == Kind::Event && store.occurs_on(i, today))
        .count();
    let (soon, _) = store.instances(now, now + Duration::days(7));
    let next = crate::ops::next_event(store, &soon, now);
    let open = open_todos(store);
    let overdue = open
        .iter()
        .filter(|t| t.due.is_some_and(|d| is_overdue(&d, now)))
        .count();

    let mut tiles = vec![json!({"label": "Today", "value": plural(today_events, "event")})];
    tiles.push(match next {
        Some(i) => {
            let start = i.start.instant(store.home);
            let when = if start.date_naive() == today {
                start.format("%H:%M").to_string()
            } else {
                start.format("%a %H:%M").to_string()
            };
            json!({"label": "Next", "value": format!("{when} {}", i.summary),
                   "note": format!("in {}", crate::fmt::humanize(start - now))})
        }
        None => json!({"label": "Next", "value": "nothing this week"}),
    });
    tiles.push(json!({"label": "Open todos", "value": open.len()}));
    tiles.push(json!({"label": "Overdue", "value": overdue,
                      "tone": if overdue > 0 { "bad" } else { "good" }}));

    let last = from + Duration::days(days - 1);
    let mut summary = format!("{} today", plural(today_events, "event"));
    if let Some(i) = next {
        summary.push_str(&format!(
            "; next {} {}",
            i.start.instant(store.home).format("%H:%M"),
            i.summary
        ));
    }
    if overdue > 0 {
        summary.push_str(&format!("; {overdue} overdue"));
    }
    json!({
        "title": "Week",
        "order": 10,
        "summary": summary,
        "params": {"from": from.format("%Y-%m-%d").to_string(), "days": days},
        "updated": now.format("%Y-%m-%d %H:%M").to_string(),
        "blocks": [
            {"type": "stats", "items": tiles},
            {"type": "table", "id": "agenda",
             "title": format!("{} – {}", from.format("%a %d %b"), last.format("%a %d %b")),
             "row_key": "id",
             "columns": [
                 {"key": "day", "label": "Day", "type": "date"},
                 {"key": "time", "label": "Time"},
                 {"key": "summary", "label": "Event"},
                 {"key": "location", "label": "Where"},
                 {"key": "calendar", "label": "Calendar"},
             ],
             "rows": rows,
             "add": {"label": "Add event", "operation": "add_event"}},
        ],
    })
}

/// The todos (`all`: done ones too) as a table to tick off.
pub fn todos(store: &Store, all: bool, now: DateTime<Tz>) -> Value {
    let todos: Vec<Todo> = store
        .todos()
        .into_iter()
        .filter(|t| all || !t.completed)
        .collect();
    let today = now.date_naive();
    let is_late = |t: &Todo| !t.completed && t.due.is_some_and(|d| is_overdue(&d, now));
    let due_today = |t: &Todo| {
        !t.completed
            && !is_late(t)
            && t.due
                .is_some_and(|d| d.instant(store.home).date_naive() == today)
    };
    let rows: Vec<Value> = todos
        .iter()
        .map(|t| {
            let status = if t.completed {
                "done"
            } else if is_late(t) {
                "overdue"
            } else if due_today(t) {
                "due today"
            } else {
                "open"
            };
            json!({
                "uid": t.uid,
                "summary": t.summary,
                "due": t.due.map(|d| due_cell(&d, store.home)),
                "priority": if t.priority == 0 { Value::Null } else { json!(t.priority) },
                "calendar": store.calendars[t.calendar].name,
                "status": status,
            })
        })
        .collect();
    let open = todos.iter().filter(|t| !t.completed).count();
    let overdue = todos.iter().filter(|t| is_late(t)).count();
    let today_n = todos.iter().filter(|t| due_today(t)).count();

    let mut tiles = vec![
        json!({"label": "Open", "value": open}),
        json!({"label": "Due today", "value": today_n, "tone": if today_n > 0 { "warn" } else { "good" }}),
        json!({"label": "Overdue", "value": overdue, "tone": if overdue > 0 { "bad" } else { "good" }}),
    ];
    if all {
        tiles.push(json!({"label": "Done", "value": todos.len() - open}));
    }
    let mut summary = format!("{open} open");
    if overdue > 0 {
        summary.push_str(&format!(", {overdue} overdue"));
    }
    json!({
        "title": "Todos",
        "order": 20,
        "summary": summary,
        "params": {"all": all},
        "updated": now.format("%Y-%m-%d %H:%M").to_string(),
        "blocks": [
            {"type": "stats", "items": tiles},
            {"type": "table", "id": "todos", "row_key": "uid",
             "columns": [
                 {"key": "summary", "label": "Todo"},
                 {"key": "due", "label": "Due", "type": "date"},
                 {"key": "priority", "label": "Prio", "type": "number"},
                 {"key": "status", "label": "Status"},
                 {"key": "calendar", "label": "Calendar"},
             ],
             "rows": rows,
             "actions": [
                 {"label": "Done", "operation": "complete_todo", "params": {"query": "{uid}"}},
                 {"label": "Not done", "operation": "complete_todo",
                  "params": {"query": "{uid}", "undo": true}},
             ],
             "add": {"label": "Add todo", "operation": "add_todo"}},
        ],
    })
}
