//! What the shell commands and `serve` share: finding calendars, todos and
//! the next event, and turning user input into new events and todos.
//!
//! Errors the user can fix (a bad date, an unknown calendar, an ambiguous
//! todo) are [`UserError`]s, so that `serve` can answer them with 400.

use anyhow::Result;
use cal_core::{Config, EditError, Instance, Kind, NewEvent, NewTodo, Store, Todo};
use chrono::DateTime;
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
