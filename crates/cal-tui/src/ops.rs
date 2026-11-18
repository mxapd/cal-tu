//! What the shell commands and `serve` share: finding calendars, todos and
//! the next event, and turning user input into new events and todos.
//!
//! Errors the user can fix (a bad date, an unknown calendar, an ambiguous
//! todo) are [`UserError`]s, so that `serve` can answer them with 400.

use anyhow::Result;
use cal_core::quick::{parse_date, parse_length, parse_time};
use cal_core::Moment;
use cal_core::{Config, EditError, Instance, Kind, NewEvent, NewTodo, Store, Todo};
use chrono::{DateTime, Duration};
use chrono_tz::Tz;

/// A mistake in the request rather than a failure.
#[derive(Debug)]
pub struct UserError(pub String);

impl std::fmt::Display for UserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UserError {}

pub fn user(msg: impl Into<String>) -> anyhow::Error {
    UserError(msg.into()).into()
}

/// Whether the caller can fix `e` by asking differently.
pub fn is_user_error(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.is::<UserError>()
            || matches!(
                c.downcast_ref::<EditError>(),
                Some(EditError::ReadOnly(_) | EditError::Invalid(_) | EditError::Conflict(_))
            )
    })
}

/// Hide every calendar but `names` (folder or display name). Empty: all.
pub fn only_calendars(store: &mut Store, names: &[String]) -> Result<()> {
    if names.is_empty() {
        return Ok(());
    }
    let wanted: Vec<usize> = names
        .iter()
        .map(|c| {
            store
                .calendar_index(c)
                .ok_or_else(|| user(format!("unknown calendar '{c}'")))
        })
        .collect::<Result<_>>()?;
    for (i, cal) in store.calendars.iter_mut().enumerate() {
        cal.hidden = !wanted.contains(&i);
    }
    Ok(())
}

/// The first timed event that starts at or after `now`.
pub fn next_event<'a>(
    store: &Store,
    inst: &'a [Instance],
    now: DateTime<Tz>,
) -> Option<&'a Instance> {
    inst.iter()
        .filter(|i| !i.all_day && i.kind == Kind::Event)
        .find(|i| i.start.instant(store.home) >= now)
}

/// An event in the quick syntax of `cal-tui add`, and its options.
#[derive(Default)]
pub struct EventInput {
    pub text: String,
    pub calendar: Option<String>,
    pub remind: Option<String>,
    pub location: Option<String>,
    pub description: Option<String>,
    pub repeat: Option<String>,
}

/// The calendar and the event `input` describes, checked but not saved.
pub fn new_event(
    store: &Store,
    config: &Config,
    now: DateTime<Tz>,
    input: EventInput,
) -> Result<(usize, NewEvent)> {
    let q = cal_core::quick::parse_event(&input.text, now).map_err(user)?;
    let cal = store.target_calendar(
        input.calendar.as_deref(),
        config.default_calendar.as_deref(),
    )?;
    let reminder = input
        .remind
        .map(|r| {
            cal_core::config::parse_offset(&r)
                .ok_or_else(|| user(format!("cannot parse reminder '{r}' (use e.g. 15m, 1h)")))
        })
        .transpose()?;
    let rrule = match input.repeat {
        Some(r) => cal_core::quick::parse_repeat(&r).map_err(user)?,
        None => None,
    };
    Ok((
        cal,
        NewEvent {
            summary: q.summary,
            location: input.location,
            description: input.description,
            start: q.start,
            end: q.end,
            rrule,
            reminder,
        },
    ))
}

/// A todo and its options, as `cal-tui todo add` takes them.
#[derive(Default)]
pub struct TodoInput {
    pub summary: String,
    pub due: Option<String>,
    pub priority: u8,
    pub calendar: Option<String>,
    pub description: Option<String>,
}

/// The calendar and the todo `input` describes, checked but not saved.
pub fn new_todo(
    store: &Store,
    config: &Config,
    now: DateTime<Tz>,
    input: TodoInput,
) -> Result<(usize, NewTodo)> {
    if input.priority > 9 {
        return Err(user(format!(
            "priority {} is out of range: 1 (highest) – 9, or 0 for none",
            input.priority
        )));
    }
    let cal = store.target_calendar(
        input.calendar.as_deref(),
        config.default_calendar.as_deref(),
    )?;
    let due = input
        .due
        .map(|d| cal_core::quick::parse_due(&d, now).map_err(user))
        .transpose()?;
    Ok((
        cal,
        NewTodo {
            summary: input.summary,
            description: input.description,
            due,
            priority: input.priority,
        },
    ))
}

/// Find exactly one todo by UID or case-insensitive title substring.
pub fn find_todo(store: &Store, query: &str, completed: bool) -> Result<Todo> {
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
        [] => Err(user(format!(
            "no {} todo matches '{query}'",
            if completed { "completed" } else { "open" }
        ))),
        many => {
            let list: Vec<_> = many.iter().map(|t| format!("  {}", t.summary)).collect();
            Err(user(format!(
                "'{query}' matches {} todos; be more specific:\n{}",
                many.len(),
                list.join("\n")
            )))
        }
    }
}

/// Mark the todo `query` finds done (or, with `undo`, not done).
pub fn complete_todo(store: &Store, query: &str, undo: bool, now: DateTime<Tz>) -> Result<Todo> {
    let t = find_todo(store, query, undo)?;
    store.set_todo_completed(t.object, !undo, now.with_timezone(&chrono::Utc))?;
    Ok(t)
}

/// Start and end from a date, a start time and an end (a time, a length or a
/// last day), as the event form and `serve` take them. No start time: all day.
pub fn span(
    date_s: &str,
    start_s: &str,
    end_s: &str,
    now: DateTime<Tz>,
) -> Result<(Moment, Moment), String> {
    let home = now.timezone();
    let today = now.date_naive();
    let date =
        parse_date(date_s, today).ok_or_else(|| format!("Date: cannot understand '{date_s}'"))?;
    if start_s.is_empty() {
        // All day.
        let end = if end_s.is_empty() {
            date + Duration::days(1)
        } else if let Some(last) = parse_date(end_s, date) {
            if last < date {
                return Err("End time: the last day is before the first".into());
            }
            last + Duration::days(1)
        } else if let Some(len) = parse_length(end_s) {
            date + Duration::days(len.num_days().max(1))
        } else {
            return Err(format!(
                "End time: cannot understand '{end_s}' (a last day like 2026-12-24, or 3d)"
            ));
        };
        return Ok((Moment::Date(date), Moment::Date(end)));
    }
    let t =
        parse_time(start_s).ok_or_else(|| format!("Start time: cannot understand '{start_s}'"))?;
    let start = cal_core::time::local_to_zoned(home, date.and_time(t));
    let end = if end_s.is_empty() {
        start + Duration::hours(1)
    } else if let Some(et) = parse_time(end_s) {
        let d = if et <= t {
            date + Duration::days(1)
        } else {
            date
        };
        cal_core::time::local_to_zoned(home, d.and_time(et))
    } else if let Some(len) = parse_length(end_s) {
        start + len
    } else {
        let mut parts = end_s.split_whitespace();
        let (d, tm) = (parts.next(), parts.next());
        match (
            d.and_then(|d| parse_date(d, date)),
            tm.and_then(parse_time),
            parts.next(),
        ) {
            (Some(d), Some(tm), None) => cal_core::time::local_to_zoned(home, d.and_time(tm)),
            _ => {
                return Err(format!(
                    "End time: cannot understand '{end_s}' (15:30, 1h30 or 2026-12-25 10:00)"
                ))
            }
        }
    };
    if end < start {
        return Err("End time: before the start".into());
    }
    Ok((Moment::DateTime(start), Moment::DateTime(end)))
}

/// How an event is named from outside: its UID, plus `@` and the original
/// start of the occurrence (RECURRENCE-ID) for one occurrence of a series.
pub fn event_id(store: &Store, i: &Instance) -> String {
    match &i.recurrence_id {
        Some(rid) => format!("{}@{}", i.uid, crate::output::moment_json(rid, store.home)),
        None => i.uid.clone(),
    }
}

/// An event an [`event_id`] names, as it is now.
pub struct EventRef {
    pub obj: usize,
    /// The occurrence, for one occurrence of a series.
    pub rid: Option<Moment>,
    pub summary: String,
    /// Start and end of what was named: the occurrence, or the event.
    pub start: Moment,
    pub end: Moment,
}

impl EventRef {
    /// The whole series with `all`, else what was named.
    pub fn scope(&self, all: bool) -> cal_core::Scope {
        match (&self.rid, all) {
            (Some(rid), false) => cal_core::Scope::Occurrence(*rid),
            _ => cal_core::Scope::All,
        }
    }
}

/// A RECURRENCE-ID as [`event_id`] writes it.
fn parse_rid(s: &str, home: Tz) -> Option<Moment> {
    if let Ok(d) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Some(Moment::Date(d));
    }
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| Moment::DateTime(dt.with_timezone(&home)))
}

/// Find the event `id` names. UIDs may contain `@` themselves, so only a
/// suffix that reads as a date or time is taken as the occurrence.
pub fn find_event(store: &Store, id: &str) -> Result<EventRef> {
    let id = id.trim();
    let (uid, rid) = match id.rsplit_once('@') {
        Some((uid, r)) => match parse_rid(r, store.home) {
            Some(rid) => (uid, Some(rid)),
            None => (id, None),
        },
        None => (id, None),
    };
    let obj = store
        .objects
        .iter()
        .position(|o| o.kind == Kind::Event && o.uid == uid)
        .ok_or_else(|| user(format!("no event with id '{id}'")))?;
    let o = &store.objects[obj];
    let Some(rid) = rid else {
        let (start, end) = cal_core::recur::span(&o.master, store.home)
            .map_err(|e| anyhow::anyhow!("{id}: {e}"))?;
        return Ok(EventRef {
            obj,
            rid: None,
            summary: o
                .master
                .prop("SUMMARY")
                .map(|p| p.value.clone())
                .unwrap_or_default(),
            start,
            end,
        });
    };
    // The occurrence, as it is now. It is looked for around its original
    // day, and, when it has an override that moved it further, around the
    // day it was moved to.
    let home = store.home;
    let mut days = vec![rid.instant(home).date_naive()];
    for ov in &o.overrides {
        let same = ov
            .prop("RECURRENCE-ID")
            .and_then(|p| cal_core::time::parse_moment(p, home).ok())
            .is_some_and(|r| cal_core::recur::same_occurrence(&r, &rid, home));
        if let (true, Ok((start, _))) = (same, cal_core::recur::span(ov, home)) {
            days.push(start.instant(home).date_naive());
        }
    }
    let i = days
        .into_iter()
        .find_map(|day| {
            let (inst, _) = store.instances(
                store.day_start(day - Duration::days(1)),
                store.day_start(day + Duration::days(2)),
            );
            inst.into_iter().find(|i| {
                i.object == obj
                    && i.recurrence_id
                        .is_some_and(|r| cal_core::recur::same_occurrence(&r, &rid, home))
            })
        })
        .ok_or_else(|| {
            user(format!(
                "no occurrence '{id}' (deleted, or moved by a sync?)"
            ))
        })?;
    Ok(EventRef {
        obj,
        rid: Some(rid),
        summary: i.summary.clone(),
        start: i.start,
        end: i.end,
    })
}

/// Find a todo, open ones first, then done ones.
/// Done ones are only looked at when no open todo matches: a query that
/// matches several open todos stays an error.
pub fn find_any_todo(store: &Store, query: &str) -> Result<Todo> {
    let q = query.to_lowercase();
    let matches = |done: bool| {
        store.todos().iter().any(|t| {
            t.completed == done && (t.uid == query || t.summary.to_lowercase().contains(&q))
        })
    };
    if !matches(false) && matches(true) {
        find_todo(store, query, true)
    } else {
        find_todo(store, query, false)
    }
}
