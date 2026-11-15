//! Add/edit forms: text inputs, prefilling from an item, and turning the
//! input into core edit requests that contain only what the user changed.

use cal_core::quick::{describe_repeat, parse_date, parse_length, parse_repeat, parse_time};
use cal_core::{EventEdit, Instance, Moment, NewEvent, NewTodo, Scope, Store, Todo, TodoEdit};
use chrono::{DateTime, Duration, NaiveDate, NaiveTime};
use chrono_tz::Tz;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A single-line text input with a cursor (in chars).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Input {
    pub text: String,
    pub cursor: usize,
}

impl Input {
    pub fn new(text: impl Into<String>) -> Input {
        let text = text.into();
        Input {
            cursor: text.chars().count(),
            text,
        }
    }

    fn byte_at(&self, char_idx: usize) -> usize {
        self.text
            .char_indices()
            .nth(char_idx)
            .map_or(self.text.len(), |(i, _)| i)
    }

    /// Apply an editing key; returns false if the key is not for the input.
    pub fn handle(&mut self, key: KeyEvent) -> bool {
        let len = self.text.chars().count();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('u') if ctrl => {
                let b = self.byte_at(self.cursor);
                self.text.replace_range(..b, "");
                self.cursor = 0;
            }
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = len,
            KeyCode::Char(c) if !ctrl => {
                let b = self.byte_at(self.cursor);
                self.text.insert(b, c);
                self.cursor += 1;
            }
            KeyCode::Backspace if self.cursor > 0 => {
                let b = self.byte_at(self.cursor - 1);
                self.text.remove(b);
                self.cursor -= 1;
            }
            KeyCode::Delete if self.cursor < len => {
                let b = self.byte_at(self.cursor);
                self.text.remove(b);
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(len),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = len,
            KeyCode::Backspace | KeyCode::Delete => {}
            _ => return false,
        }
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldId {
    Title,
    Date,
    Start,
    End,
    Location,
    Repeat,
    Reminder,
    Due,
    Priority,
    Notes,
    Calendar,
}

impl FieldId {
    pub fn label(self) -> &'static str {
        match self {
            FieldId::Title => "Title",
            FieldId::Date => "Date",
            FieldId::Start => "Start time",
            FieldId::End => "End time",
            FieldId::Location => "Location",
            FieldId::Repeat => "Repeat",
            FieldId::Reminder => "Reminder",
            FieldId::Due => "Due",
            FieldId::Priority => "Priority",
            FieldId::Notes => "Notes",
            FieldId::Calendar => "Calendar",
        }
    }

    /// Shown dimmed in an empty field.
    pub fn placeholder(self) -> &'static str {
        match self {
            FieldId::Title => "what is it?",
            FieldId::Date => "e.g. 2026-12-24, fri, tomorrow",
            FieldId::Start => "e.g. 14:00 — empty = all day",
            FieldId::End => "e.g. 15:30 or 1h30 — empty = 1 hour",
            FieldId::Location => "optional",
            FieldId::Reminder => "e.g. 15m — empty = no reminder",
            FieldId::Due => "e.g. fri 17:00 — empty = no due date",
            FieldId::Notes => "optional",
            _ => "",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            FieldId::Date => "The day: 2026-12-24, 12-24, fri, tomorrow, +3d",
            FieldId::Start => {
                "When it starts, e.g. 14:00 or 2pm. Leave empty for an all-day event."
            }
            FieldId::End => "Ends at 15:30, or lasts 1h30; empty = 1 hour. All day: last day/3d",
            FieldId::Repeat => "none, daily, weekdays, weekly, biweekly, monthly, yearly, FREQ=…",
            FieldId::Reminder => "15m, 1h, 1d; empty = none",
            FieldId::Due => "fri, 12-24 17:00, tomorrow 9am; empty = none",
            FieldId::Priority => "1 (highest) – 9, 0 = none",
            FieldId::Notes => "\\n for a new line",
            FieldId::Calendar => "←/→ to choose",
            _ => "",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub id: FieldId,
    pub input: Input,
    /// Prefilled value, to tell what the user changed.
    pub original: String,
}

impl Field {
    fn new(id: FieldId, value: impl Into<String>) -> Field {
        let v = value.into();
        Field {
            id,
            input: Input::new(v.clone()),
            original: v,
        }
    }

    fn changed(&self) -> bool {
        self.input.text != self.original
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    NewEvent,
    EditEvent {
        object: usize,
        scope: Scope,
        seen_start: Moment,
    },
    NewTodo,
    EditTodo {
        object: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Form {
    pub title: String,
    pub target: Target,
    pub fields: Vec<Field>,
    pub focus: usize,
    pub error: Option<String>,
    /// Writable calendars to choose from (new items only).
    pub calendars: Vec<usize>,
    pub calendar: usize,
}

/// What a submitted form asks the store to do.
#[derive(Debug, Clone)]
pub enum Submit {
    NewEvent(usize, NewEvent),
    EditEvent {
        object: usize,
        scope: Scope,
        seen_start: Moment,
        edit: EventEdit,
    },
    NewTodo(usize, NewTodo),
    EditTodo {
        object: usize,
        edit: TodoEdit,
    },
    /// Nothing changed.
    Nothing,
}

fn notes_to_field(s: Option<&str>) -> String {
    s.unwrap_or("").replace('\\', "\\\\").replace('\n', "\\n")
}

fn notes_from_field(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some(o) => out.push(o),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn hm(t: NaiveTime) -> String {
    t.format("%H:%M").to_string()
}

fn ymd(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

impl Form {
    fn calendar_choice(store: &Store, preferred: Option<usize>) -> (Vec<usize>, usize) {
        let cals = store.writable_calendars();
        let pos = preferred
            .and_then(|p| cals.iter().position(|c| *c == p))
            .unwrap_or(0);
        (cals, pos)
    }

    pub fn new_event(store: &Store, date: NaiveDate, default_cal: Option<usize>) -> Form {
        let (calendars, calendar) = Self::calendar_choice(store, default_cal);
        Form {
            title: "New event".into(),
            target: Target::NewEvent,
            fields: vec![
                Field::new(FieldId::Title, ""),
                Field::new(FieldId::Date, ymd(date)),
                Field::new(FieldId::Start, ""),
                Field::new(FieldId::End, ""),
                Field::new(FieldId::Location, ""),
                Field::new(FieldId::Repeat, "none"),
                Field::new(FieldId::Reminder, ""),
                Field::new(FieldId::Notes, ""),
                Field::new(FieldId::Calendar, ""),
            ],
            focus: 0,
            error: None,
            calendars,
            calendar,
        }
    }

    pub fn edit_event(store: &Store, inst: &Instance, scope: Scope) -> Form {
        let home = store.home;
        let date = inst.start.date_in(home);
        let (start, end) = match (inst.start, inst.end) {
            (Moment::Date(s), Moment::Date(e)) => {
                let last = (e - Duration::days(1)).max(s);
                (
                    String::new(),
                    if last == s { String::new() } else { ymd(last) },
                )
            }
            _ => {
                let s = inst.start.instant(home);
                let e = inst.end.instant(home);
                let end = if e.date_naive() == s.date_naive() {
                    hm(e.time())
                } else {
                    format!("{} {}", ymd(e.date_naive()), hm(e.time()))
                };
                (hm(s.time()), end)
            }
        };
        let obj = &store.objects[inst.object];
        let rule = obj.master.prop("RRULE").map(|p| p.value.clone());
        let mut fields = vec![
            Field::new(FieldId::Title, inst.summary.clone()),
            Field::new(FieldId::Date, ymd(date)),
            Field::new(FieldId::Start, start),
            Field::new(FieldId::End, end),
            Field::new(FieldId::Location, inst.location.clone().unwrap_or_default()),
        ];
        if scope == Scope::All {
            fields.push(Field::new(
                FieldId::Repeat,
                describe_repeat(rule.as_deref()),
            ));
        }
        fields.push(Field::new(
            FieldId::Notes,
            notes_to_field(inst.description.as_deref()),
        ));
        let title = match scope {
            Scope::Occurrence(_) => "Edit this occurrence",
            Scope::All if inst.recurrence_id.is_some() => "Edit all occurrences",
            Scope::All => "Edit event",
        };
        Form {
            title: title.into(),
            target: Target::EditEvent {
                object: inst.object,
                scope,
                seen_start: inst.start,
            },
            fields,
            focus: 0,
            error: None,
            calendars: Vec::new(),
            calendar: 0,
        }
    }

    pub fn new_todo(store: &Store, default_cal: Option<usize>) -> Form {
        let (calendars, calendar) = Self::calendar_choice(store, default_cal);
        Form {
            title: "New todo".into(),
            target: Target::NewTodo,
            fields: vec![
                Field::new(FieldId::Title, ""),
                Field::new(FieldId::Due, ""),
                Field::new(FieldId::Priority, "0"),
                Field::new(FieldId::Notes, ""),
                Field::new(FieldId::Calendar, ""),
            ],
            focus: 0,
            error: None,
            calendars,
            calendar,
        }
    }

    pub fn edit_todo(store: &Store, t: &Todo) -> Form {
        let due = match t.due {
            None => String::new(),
            Some(Moment::Date(d)) => ymd(d),
            Some(Moment::DateTime(dt)) => {
                let l = dt.with_timezone(&store.home);
                format!("{} {}", ymd(l.date_naive()), hm(l.time()))
            }
        };
        Form {
            title: "Edit todo".into(),
            target: Target::EditTodo { object: t.object },
            fields: vec![
                Field::new(FieldId::Title, t.summary.clone()),
                Field::new(FieldId::Due, due),
                Field::new(FieldId::Priority, t.priority.to_string()),
                Field::new(FieldId::Notes, notes_to_field(t.description.as_deref())),
            ],
            focus: 0,
            error: None,
            calendars: Vec::new(),
            calendar: 0,
        }
    }

    pub fn focused(&self) -> FieldId {
        self.fields[self.focus].id
    }

    /// Handle a key that is not submit/cancel.
    pub fn handle_key(&mut self, key: KeyEvent) {
        self.error = None;
        let n = self.fields.len();
        match key.code {
            KeyCode::Tab | KeyCode::Down => self.focus = (self.focus + 1) % n,
            KeyCode::BackTab | KeyCode::Up => self.focus = (self.focus + n - 1) % n,
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')
                if self.focused() == FieldId::Calendar && !self.calendars.is_empty() =>
            {
                let m = self.calendars.len();
                self.calendar = if key.code == KeyCode::Left {
                    (self.calendar + m - 1) % m
                } else {
                    (self.calendar + 1) % m
                };
            }
            _ if self.focused() == FieldId::Calendar => {}
            _ => {
                self.fields[self.focus].input.handle(key);
            }
        }
    }

    fn get(&self, id: FieldId) -> Option<&Field> {
        self.fields.iter().find(|f| f.id == id)
    }

    fn text(&self, id: FieldId) -> String {
        self.get(id)
            .map(|f| f.input.text.trim().to_string())
            .unwrap_or_default()
    }

    fn changed(&self, id: FieldId) -> bool {
        self.get(id).is_some_and(Field::changed)
    }

    fn optional(&self, id: FieldId) -> Option<String> {
        Some(self.text(id)).filter(|s| !s.is_empty())
    }

    /// Start and end from the Date/Start/End fields.
    fn span(&self, now: DateTime<Tz>) -> Result<(Moment, Moment), String> {
        let home = now.timezone();
        let today = now.date_naive();
        let date_s = self.text(FieldId::Date);
        let date = parse_date(&date_s, today)
            .ok_or_else(|| format!("Date: cannot understand '{date_s}'"))?;
        let start_s = self.text(FieldId::Start);
        let end_s = self.text(FieldId::End);
        if start_s.is_empty() {
            // All day.
            let end = if end_s.is_empty() {
                date + Duration::days(1)
            } else if let Some(last) = parse_date(&end_s, date) {
                if last < date {
                    return Err("End time: the last day is before the first".into());
                }
                last + Duration::days(1)
            } else if let Some(len) = parse_length(&end_s) {
                date + Duration::days(len.num_days().max(1))
            } else {
                return Err(format!(
                    "End time: cannot understand '{end_s}' (a last day like 2026-12-24, or 3d)"
                ));
            };
            return Ok((Moment::Date(date), Moment::Date(end)));
        }
        let t = parse_time(&start_s)
            .ok_or_else(|| format!("Start time: cannot understand '{start_s}'"))?;
        let start = cal_core::time::local_to_zoned(home, date.and_time(t));
        let end = if end_s.is_empty() {
            start + Duration::hours(1)
        } else if let Some(et) = parse_time(&end_s) {
            let d = if et <= t {
                date + Duration::days(1)
            } else {
                date
            };
            cal_core::time::local_to_zoned(home, d.and_time(et))
        } else if let Some(len) = parse_length(&end_s) {
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

    fn notes(&self) -> Option<String> {
        self.optional(FieldId::Notes).map(|n| notes_from_field(&n))
    }

    fn priority(&self) -> Result<u8, String> {
        let p = self.text(FieldId::Priority);
        if p.is_empty() {
            return Ok(0);
        }
        p.parse::<u8>()
            .ok()
            .filter(|p| *p <= 9)
            .ok_or_else(|| format!("Priority: '{p}' is not a number from 0 to 9"))
    }

    fn due(&self, now: DateTime<Tz>) -> Result<Option<Moment>, String> {
        let d = self.text(FieldId::Due);
        if d.is_empty() {
            return Ok(None);
        }
        cal_core::quick::parse_due(&d, now)
            .map(Some)
            .map_err(|e| format!("Due: {e}"))
    }

    /// Validate the form and build the store request.
    pub fn submit(&self, now: DateTime<Tz>) -> Result<Submit, String> {
        let title = self.text(FieldId::Title);
        if title.is_empty() {
            return Err("Title: required".into());
        }
        let calendar = self.calendars.get(self.calendar).copied();
        match &self.target {
            Target::NewEvent => {
                let (start, end) = self.span(now)?;
                let rrule = parse_repeat(&self.text(FieldId::Repeat))
                    .map_err(|e| format!("Repeat: {e}"))?;
                let reminder = match self.optional(FieldId::Reminder) {
                    None => None,
                    Some(r) => Some(
                        cal_core::config::parse_offset(&r)
                            .ok_or_else(|| format!("Reminder: cannot understand '{r}'"))?,
                    ),
                };
                let cal = calendar.ok_or("No writable calendar to add to")?;
                Ok(Submit::NewEvent(
                    cal,
                    NewEvent {
                        summary: title,
                        location: self.optional(FieldId::Location),
                        description: self.notes(),
                        start,
                        end,
                        rrule,
                        reminder,
                    },
                ))
            }
            Target::EditEvent {
                object,
                scope,
                seen_start,
            } => {
                let mut edit = EventEdit::default();
                if self.changed(FieldId::Title) {
                    edit.summary = Some(title);
                }
                if self.changed(FieldId::Location) {
                    edit.location = Some(self.optional(FieldId::Location));
                }
                if self.changed(FieldId::Notes) {
                    edit.description = Some(self.notes());
                }
                if [FieldId::Date, FieldId::Start, FieldId::End]
                    .iter()
                    .any(|f| self.changed(*f))
                {
                    edit.time = Some(self.span(now)?);
                }
                if self.changed(FieldId::Repeat) {
                    edit.rrule = Some(
                        parse_repeat(&self.text(FieldId::Repeat))
                            .map_err(|e| format!("Repeat: {e}"))?,
                    );
                }
                let nothing = edit.summary.is_none()
                    && edit.location.is_none()
                    && edit.description.is_none()
                    && edit.time.is_none()
                    && edit.rrule.is_none();
                if nothing {
                    return Ok(Submit::Nothing);
                }
                Ok(Submit::EditEvent {
                    object: *object,
                    scope: *scope,
                    seen_start: *seen_start,
                    edit,
                })
            }
            Target::NewTodo => Ok(Submit::NewTodo(
                calendar.ok_or("No writable calendar to add to")?,
                NewTodo {
                    summary: title,
                    description: self.notes(),
                    due: self.due(now)?,
                    priority: self.priority()?,
                },
            )),
            Target::EditTodo { object } => {
                let mut edit = TodoEdit::default();
                if self.changed(FieldId::Title) {
                    edit.summary = Some(title);
                }
                if self.changed(FieldId::Notes) {
                    edit.description = Some(self.notes());
                }
                if self.changed(FieldId::Due) {
                    edit.due = Some(self.due(now)?);
                }
                if self.changed(FieldId::Priority) {
                    edit.priority = Some(self.priority()?);
                }
                if edit.summary.is_none()
                    && edit.description.is_none()
                    && edit.due.is_none()
                    && edit.priority.is_none()
                {
                    return Ok(Submit::Nothing);
                }
                Ok(Submit::EditTodo {
                    object: *object,
                    edit,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    #[test]
    fn input_editing() {
        let mut i = Input::new("héllo");
        i.handle(key(KeyCode::Left));
        i.handle(key(KeyCode::Backspace));
        assert_eq!(i.text, "hélo");
        i.handle(key(KeyCode::Home));
        i.handle(key(KeyCode::Char('¡')));
        assert_eq!((i.text.as_str(), i.cursor), ("¡hélo", 1));
        i.handle(key(KeyCode::Delete));
        assert_eq!(i.text, "¡élo");
        i.handle(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!((i.text.as_str(), i.cursor), ("élo", 0));
    }

    #[test]
    fn notes_escape_roundtrip() {
        for s in ["one\ntwo", "a\\nb literal", "back\\slash", ""] {
            assert_eq!(notes_from_field(&notes_to_field(Some(s))), s);
        }
    }
}
