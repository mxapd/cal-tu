//! Jarvis panels: the calendar's tabs in `jarvis ls`, drawn live by
//! `cal-tui serve` (the operations `panel_month`, `panel_week`, `panel_day`
//! and `panel_todos`). The format is jarvis_panel.py's: a title, an order
//! among the tabs and blocks (stats tiles, tables you can act on).
//!
//! Events are changed in the Week and Day tables and todos in the Todos
//! table: an editable cell or a row action is an operation of this same
//! service (`edit_event`, `delete_event`, `edit_todo`, ...), and Jarvis asks
//! before it runs one. An emptied cell sends no value, so each cell edit also
//! names its `field`, which tells the command to clear it.
//!
//! Panels are for you: Jarvis never offers them over MCP.

use cal_core::{Instance, Kind, Moment, Store, Todo};
use chrono::{DateTime, Datelike, Duration, NaiveDate};
use chrono_tz::Tz;
use serde_json::{json, Value};

use crate::report::is_overdue;

/// (operation, tab title, order among the service's tabs)
pub const PANELS: &[(&str, &str, i64)] = &[
    ("panel_month", "Month", 5),
    ("panel_week", "Week", 10),
    ("panel_day", "Day", 15),
    ("panel_todos", "Todos", 20),
];

/// A panel's title and order, as /describe gives them.
fn head(op: &str) -> (&'static str, i64) {
    let (_, title, order) = PANELS.iter().find(|p| p.0 == op).expect("a panel");
    (title, *order)
}

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

/// The events on `day`, all-day ones first.
fn events_on<'a>(store: &Store, inst: &'a [Instance], day: NaiveDate) -> Vec<&'a Instance> {
    let mut items: Vec<&Instance> = inst
        .iter()
        .filter(|i| i.kind == Kind::Event && store.occurs_on(i, day))
        .collect();
    items.sort_by_key(|i| (!i.all_day, i.start.instant(store.home)));
    items
}

/// A cell edit: `edit_event` (or `edit_todo`) with the typed value as `field`.
fn edit(operation: &str, by: (&str, &str), field: &str) -> Value {
    json!({"operation": operation,
           "params": {by.0: by.1, field: "{value}", "field": field}})
}

/// The events of `days` days from `from` on, one row per event and day, as
/// a table whose cells change them.
fn event_table(
    store: &Store,
    inst: &[Instance],
    from: NaiveDate,
    days: i64,
    title: String,
) -> Value {
    let mut rows = Vec::new();
    for n in 0..days {
        let day = from + Duration::days(n);
        for i in events_on(store, inst, day) {
            let id = crate::ops::event_id(store, i);
            rows.push(json!({
                "row": format!("{id}|{day}"),
                "id": id,
                "day": date_cell(day),
                "time": crate::fmt::time_range(i, day, store.home).column().trim(),
                "summary": i.summary,
                "location": i.location.as_deref().unwrap_or(""),
                "calendar": store.calendars[i.calendar].name,
            }));
        }
    }
    let by = ("id", "{id}");
    json!({"type": "table", "id": "events", "title": title, "row_key": "row",
           "columns": [
               {"key": "day", "label": "Day", "type": "date", "edit": edit("edit_event", by, "date")},
               {"key": "time", "label": "Time", "edit": edit("edit_event", by, "time")},
               {"key": "summary", "label": "Event", "edit": edit("edit_event", by, "summary")},
               {"key": "location", "label": "Where", "edit": edit("edit_event", by, "location")},
               {"key": "calendar", "label": "Calendar"},
           ],
           "rows": rows,
           "actions": [
               {"label": "Delete", "operation": "delete_event", "params": {"id": "{id}"}},
               {"label": "Delete every occurrence", "operation": "delete_event",
                "params": {"id": "{id}", "all": true}},
           ],
           "add": {"label": "Add event", "operation": "add_event"}})
}

/// Tiles about now: today, the next event, the todos.
fn now_tiles(store: &Store, now: DateTime<Tz>) -> (Vec<Value>, String) {
    let today = now.date_naive();
    let (today_inst, _) = store.instances(
        store.day_start(today),
        store.day_start(today + Duration::days(1)),
    );
    let today_events = events_on(store, &today_inst, today).len();
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
    (tiles, summary)
}

/// The events of `days` days from `from` on: tiles about now, then the
/// events by day, to change in place.
pub fn week(store: &Store, from: NaiveDate, days: i64, now: DateTime<Tz>) -> Value {
    let (inst, _) = store.instances(
        store.day_start(from),
        store.day_start(from + Duration::days(days)),
    );
    let (tiles, summary) = now_tiles(store, now);
    let last = from + Duration::days(days - 1);
    let (title, order) = head("panel_week");
    json!({
        "title": title,
        "order": order,
        "summary": summary,
        "params": {"from": from.format("%Y-%m-%d").to_string(), "days": days},
        "updated": now.format("%Y-%m-%d %H:%M").to_string(),
        "blocks": [
            {"type": "stats", "items": tiles},
            event_table(store, &inst, from, days,
                        format!("{} – {}", from.format("%a %d %b"), last.format("%a %d %b"))),
        ],
    })
}

/// How long before `i` its reminders fire: its own VALARMs, or the
/// calendar's default reminder.
fn reminders(store: &Store, i: &Instance) -> Vec<String> {
    let start = i.start.instant(store.home);
    let (alarms, _) = store.alarms(start - Duration::days(8), start + Duration::minutes(1));
    let mut out: Vec<String> = alarms
        .iter()
        .filter(|a| a.uid == i.uid && a.start.instant(store.home) == start)
        .map(|a| {
            let before = start - a.at;
            let what = if before <= Duration::zero() {
                "at the start".to_string()
            } else {
                format!("{} before", crate::fmt::humanize(before))
            };
            if a.default {
                format!("{what} (the calendar's default)")
            } else {
                what
            }
        })
        .collect();
    out.dedup();
    out
}

/// One day in full: each event with its notes, repeat rule and reminders,
/// then the same table as Week.
pub fn day(store: &Store, day: NaiveDate, now: DateTime<Tz>) -> Value {
    let (inst, _) = store.instances(
        store.day_start(day),
        store.day_start(day + Duration::days(1)),
    );
    let events = events_on(store, &inst, day);
    let mut text = Vec::new();
    for i in &events {
        let cal = &store.calendars[i.calendar];
        let time = crate::fmt::time_range(i, day, store.home).column();
        text.push(format!("#### {} {}", time.trim(), i.summary));
        let mut facts = vec![format!("*{}*", cal.name)];
        if let Some(loc) = &i.location {
            facts.push(format!("at {loc}"));
        }
        let rule = store.objects[i.object]
            .master
            .prop("RRULE")
            .map(|p| p.value.clone());
        if rule.is_some() {
            facts.push(format!(
                "repeats {}",
                cal_core::quick::describe_repeat(rule.as_deref())
            ));
        }
        if cal.read_only {
            facts.push("read-only".into());
        }
        text.push(facts.join(" · "));
        let rem = reminders(store, i);
        if !rem.is_empty() {
            text.push(format!("Reminder: {}", rem.join(", ")));
        }
        if let Some(d) = i.description.as_deref().filter(|d| !d.trim().is_empty()) {
            text.push(String::new());
            text.extend(d.lines().map(|l| format!("> {l}")));
        }
        text.push(String::new());
    }
    if events.is_empty() {
        text.push("Nothing scheduled.".into());
    }
    let (title, order) = head("panel_day");
    json!({
        "title": title,
        "order": order,
        "summary": format!("{}: {}", day.format("%a %d %b"), plural(events.len(), "event")),
        "params": {"day": day.format("%Y-%m-%d").to_string()},
        "updated": now.format("%Y-%m-%d %H:%M").to_string(),
        "blocks": [
            {"type": "markdown", "text": format!("### {}\n\n{}", day.format("%A %d %B %Y"), text.join("\n").trim_end())},
            event_table(store, &inst, day, 1, day.format("%a %d %b").to_string()),
        ],
    })
}

/// The first day of a month: `YYYY-MM`, `this`, `next`, `last`, `+2`, `-1`.
pub fn parse_month(s: &str, today: NaiveDate) -> Option<NaiveDate> {
    let this = today.with_day(1)?;
    let shift = |n: i32| {
        let m = this.year() * 12 + this.month0() as i32 + n;
        NaiveDate::from_ymd_opt(m.div_euclid(12), m.rem_euclid(12) as u32 + 1, 1)
    };
    match s.trim() {
        "" | "this" => Some(this),
        "next" => shift(1),
        "last" | "prev" => shift(-1),
        t if t.starts_with(['+', '-']) => shift(t.parse().ok()?),
        t => NaiveDate::parse_from_str(&format!("{t}-01"), "%Y-%m-%d").ok(),
    }
}

/// A month as a grid: a row a week, a cell a day with a dot an event.
pub fn month(store: &Store, first: NaiveDate, now: DateTime<Tz>) -> Value {
    let today = now.date_naive();
    let next = parse_month("+1", first).expect("a month");
    let (inst, _) = store.instances(store.day_start(first), store.day_start(next));
    let count = |d: NaiveDate| events_on(store, &inst, d).len();

    const DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
    let mut rows = Vec::new();
    let mut busiest: Option<(NaiveDate, usize)> = None;
    let mut monday = first - Duration::days(first.weekday().num_days_from_monday() as i64);
    while monday < next {
        let mut row = json!({"week": monday.iso_week().week()});
        for (k, key) in DAYS.iter().enumerate() {
            let d = monday + Duration::days(k as i64);
            if d < first || d >= next {
                continue;
            }
            let n = count(d);
            if n > busiest.map_or(0, |b| b.1) {
                busiest = Some((d, n));
            }
            let day = if d == today {
                format!("[{}]", d.day())
            } else {
                d.day().to_string()
            };
            let dots = if n > 4 {
                "●●●●+".to_string()
            } else {
                "●".repeat(n)
            };
            row[key] = json!({"v": d.format("%Y-%m-%d").to_string(),
                              "t": format!("{day} {dots}").trim_end().to_string()});
        }
        rows.push(row);
        monday += Duration::days(7);
    }

    let events = inst.iter().filter(|i| i.kind == Kind::Event).count();
    let due_here = open_todos(store)
        .iter()
        .filter(|t| {
            t.due.is_some_and(|d| {
                let d = d.instant(store.home).date_naive();
                d >= first && d < next
            })
        })
        .count();
    let mut tiles = vec![json!({"label": "Events", "value": events})];
    tiles.push(match busiest {
        Some((d, n)) => json!({"label": "Busiest day", "value": d.format("%a %d").to_string(),
                               "note": plural(n, "event")}),
        None => json!({"label": "Busiest day", "value": "–"}),
    });
    tiles.push(json!({"label": "Todos due", "value": due_here}));

    let mut columns = vec![json!({"key": "week", "label": "Wk", "type": "number"})];
    columns.extend(DAYS.iter().map(|k| {
        let mut label = k.to_string();
        label[..1].make_ascii_uppercase();
        json!({"key": k, "label": label})
    }));
    let (title, order) = head("panel_month");
    json!({
        "title": title,
        "order": order,
        "summary": format!("{}: {}", first.format("%B %Y"), plural(events, "event")),
        "params": {"month": first.format("%Y-%m").to_string()},
        "updated": now.format("%Y-%m-%d %H:%M").to_string(),
        "blocks": [
            {"type": "stats", "items": tiles},
            {"type": "table", "id": "month", "title": first.format("%B %Y").to_string(),
             "row_key": "week", "columns": columns, "rows": rows},
            {"type": "markdown",
             "text": "A dot per event; [n] is today. Change events in Week or Day (`=` picks the days)."},
        ],
    })
}

/// The todos (`all`: done ones too) as a table to tick off and change.
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
    let by = ("query", "{uid}");
    let (title, order) = head("panel_todos");
    json!({
        "title": title,
        "order": order,
        "summary": summary,
        "params": {"all": all},
        "updated": now.format("%Y-%m-%d %H:%M").to_string(),
        "blocks": [
            {"type": "stats", "items": tiles},
            {"type": "table", "id": "todos", "row_key": "uid",
             "columns": [
                 {"key": "summary", "label": "Todo", "edit": edit("edit_todo", by, "summary")},
                 {"key": "due", "label": "Due", "type": "date", "edit": edit("edit_todo", by, "due")},
                 {"key": "priority", "label": "Prio", "type": "number",
                  "edit": edit("edit_todo", by, "priority")},
                 {"key": "status", "label": "Status"},
                 {"key": "calendar", "label": "Calendar"},
             ],
             "rows": rows,
             "actions": [
                 {"label": "Done", "operation": "complete_todo", "params": {"query": "{uid}"}},
                 {"label": "Not done", "operation": "complete_todo",
                  "params": {"query": "{uid}", "undo": true}},
                 {"label": "Delete", "operation": "delete_todo", "params": {"query": "{uid}"}},
             ],
             "add": {"label": "Add todo", "operation": "add_todo"}},
        ],
    })
}
