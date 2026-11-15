//! Creating, changing and deleting events and todos.
//!
//! Every change follows the same steps (DESIGN.md, "Reliability rules"):
//!
//! 1. Refuse if the calendar is read-only.
//! 2. Re-read the file and compare it with what was loaded; if a sync changed
//!    it in the meantime, stop with [`EditError::Conflict`].
//! 3. Refuse files that had syntax problems, since the lenient parser may have
//!    skipped parts of them and writing would lose data.
//! 4. Change only the properties the user touched on the parsed components;
//!    everything else (unknown `X-` properties, alarms, attendees) is kept.
//! 5. Write atomically (temp file, fsync, rename).
//!
//! The store is not updated in place: reload it after a successful edit.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, Duration, NaiveDateTime, NaiveTime, Utc};
use chrono_tz::Tz;

use crate::ical::{self, escape_text, Component, Param, Property};
use crate::recur::{self, same_occurrence};
use crate::store::{Kind, Store};
use crate::time::{local_to_zoned, parse_moment, parse_moment_list, resolve_tzid, Moment};
use crate::{fsio, vtimezone};

/// Why an edit was refused. Wrapped in `anyhow::Error`; use
/// `err.downcast_ref::<EditError>()` to react to a specific case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    /// The calendar is marked read-only.
    ReadOnly(String),
    /// The file changed on disk since it was loaded (or was deleted).
    Conflict(PathBuf),
    /// The file has syntax problems; rewriting it could lose data.
    UnsafeFile(PathBuf, String),
    /// The requested change is not valid.
    Invalid(String),
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EditError::ReadOnly(c) => write!(f, "calendar '{c}' is read-only"),
            EditError::Conflict(p) => write!(
                f,
                "{} changed on disk since it was loaded (sync?); reload and try again",
                p.display()
            ),
            EditError::UnsafeFile(p, why) => write!(
                f,
                "{} has syntax problems ({why}); not editing it to avoid losing data",
                p.display()
            ),
            EditError::Invalid(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for EditError {}

fn invalid(m: impl Into<String>) -> anyhow::Error {
    EditError::Invalid(m.into()).into()
}

/// Which part of a repeating event an edit or delete applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The whole item (every occurrence of a series).
    All,
    /// One occurrence, identified by its RECURRENCE-ID (its original start).
    Occurrence(Moment),
}

/// A new event.
#[derive(Debug, Clone)]
pub struct NewEvent {
    pub summary: String,
    pub location: Option<String>,
    pub description: Option<String>,
    pub start: Moment,
    pub end: Moment,
    /// RRULE value, e.g. `FREQ=WEEKLY`.
    pub rrule: Option<String>,
    /// Display alarm this long before the start.
    pub reminder: Option<Duration>,
}

/// Changes to an event; `None` fields are left as they are.
#[derive(Debug, Clone, Default)]
pub struct EventEdit {
    pub summary: Option<String>,
    /// `Some(None)` removes the location.
    pub location: Option<Option<String>>,
    pub description: Option<Option<String>>,
    /// New start and end of the occurrence being edited. For a whole series
    /// the series is shifted by the same amount.
    pub time: Option<(Moment, Moment)>,
    /// New RRULE (`Some(None)` stops repeating). Whole-series edits only.
    pub rrule: Option<Option<String>>,
}

/// A new todo.
#[derive(Debug, Clone, Default)]
pub struct NewTodo {
    pub summary: String,
    pub description: Option<String>,
    pub due: Option<Moment>,
    /// 1 (highest) – 9 (lowest); 0 = none.
    pub priority: u8,
}

/// Changes to a todo; `None` fields are left as they are.
#[derive(Debug, Clone, Default)]
pub struct TodoEdit {
    pub summary: Option<String>,
    pub description: Option<Option<String>>,
    pub due: Option<Option<Moment>>,
    pub priority: Option<u8>,
}

// ---------------------------------------------------------------------------
// Writing date/time values

/// How an existing date-time property is written, so edits keep its form.
#[derive(Debug, Clone)]
enum Style {
    Floating,
    Utc,
    /// `TZID` as written in the file (may be a Windows name) and its zone.
    Zone(String, Tz),
}

fn default_style(home: Tz) -> Style {
    if home == Tz::UTC {
        Style::Utc
    } else {
        Style::Zone(home.name().to_string(), home)
    }
}

fn style_of(p: Option<&Property>, home: Tz) -> Style {
    let Some(p) = p else {
        return default_style(home);
    };
    if p.value.trim().ends_with(['Z', 'z']) {
        return Style::Utc;
    }
    match p.param("TZID") {
        Some(id) => match resolve_tzid(id) {
            Some(tz) => Style::Zone(id.to_string(), tz),
            None => default_style(home),
        },
        None => match parse_moment(p, home) {
            Ok(Moment::DateTime(_)) => Style::Floating,
            _ => default_style(home),
        },
    }
}

fn moment_prop(name: &str, m: Moment, style: &Style, home: Tz) -> Property {
    let mut p = Property::new(name, "");
    match m {
        Moment::Date(d) => {
            p.params.push(Param {
                name: "VALUE".into(),
                values: vec!["DATE".into()],
            });
            p.value = d.format("%Y%m%d").to_string();
        }
        Moment::DateTime(dt) => match style {
            Style::Utc => p.value = dt.with_timezone(&Utc).format("%Y%m%dT%H%M%SZ").to_string(),
            Style::Floating => {
                p.value = dt.with_timezone(&home).format("%Y%m%dT%H%M%S").to_string()
            }
            Style::Zone(id, tz) => {
                p.params.push(Param {
                    name: "TZID".into(),
                    values: vec![id.clone()],
                });
                p.value = dt.with_timezone(tz).format("%Y%m%dT%H%M%S").to_string();
            }
        },
    }
    p
}

fn utc_stamp(now: DateTime<Utc>) -> String {
    now.format("%Y%m%dT%H%M%SZ").to_string()
}

/// iCalendar DURATION text, e.g. `-PT15M`, `P1D`.
pub fn format_duration(d: Duration) -> String {
    let neg = d < Duration::zero();
    let mut secs = d.num_seconds().abs();
    let days = secs / 86400;
    secs %= 86400;
    let mut s = String::from(if neg { "-P" } else { "P" });
    if days > 0 {
        s.push_str(&format!("{days}D"));
    }
    if secs > 0 || days == 0 {
        s.push('T');
        let (h, m, sec) = (secs / 3600, (secs % 3600) / 60, secs % 60);
        if h > 0 {
            s.push_str(&format!("{h}H"));
        }
        if m > 0 || (h == 0 && sec == 0) {
            s.push_str(&format!("{m}M"));
        }
        if sec > 0 {
            s.push_str(&format!("{sec}S"));
        }
    }
    s
}

// ---------------------------------------------------------------------------
// Component helpers

fn is(c: &Component, name: &str) -> bool {
    c.name.eq_ignore_ascii_case(name)
}

fn is_item(c: &Component) -> bool {
    is(c, "VEVENT") || is(c, "VTODO")
}

fn uid_of(c: &Component) -> Option<String> {
    c.text("UID").map(|u| u.trim().to_string())
}

fn has_uid(c: &Component, uid: &str) -> bool {
    is_item(c) && uid_of(c).as_deref() == Some(uid)
}

fn remove_prop(c: &mut Component, name: &str) {
    c.properties.retain(|p| !p.name.eq_ignore_ascii_case(name));
}

/// Replace the first property of that name in place (dropping duplicates),
/// or append it.
fn set_prop(c: &mut Component, p: Property) {
    match c
        .properties
        .iter()
        .position(|q| q.name.eq_ignore_ascii_case(&p.name))
    {
        Some(i) => {
            let name = p.name.clone();
            c.properties[i] = p;
            let mut seen = false;
            c.properties.retain(|q| {
                if !q.name.eq_ignore_ascii_case(&name) {
                    return true;
                }
                let keep = !seen;
                seen = true;
                keep
            });
        }
        None => c.properties.push(p),
    }
}

/// Set a TEXT property; `None` or empty removes it. Parameters such as
/// `LANGUAGE` are kept; `ALTREP` is dropped because it would be stale.
fn set_text(c: &mut Component, name: &str, value: Option<&str>) {
    let value = value.map(str::trim).filter(|v| !v.is_empty());
    if c.text(name).as_deref().map(str::trim) == value {
        return;
    }
    match value {
        None => remove_prop(c, name),
        Some(v) => match c
            .properties
            .iter_mut()
            .find(|p| p.name.eq_ignore_ascii_case(name))
        {
            Some(p) => {
                p.value = escape_text(v);
                p.params.retain(|q| !q.name.eq_ignore_ascii_case("ALTREP"));
            }
            None => c.properties.push(Property::new(name, &escape_text(v))),
        },
    }
}

/// Record a modification: DTSTAMP, LAST-MODIFIED, SEQUENCE.
fn touch(c: &mut Component, now: DateTime<Utc>) {
    set_prop(c, Property::new("DTSTAMP", &utc_stamp(now)));
    set_prop(c, Property::new("LAST-MODIFIED", &utc_stamp(now)));
    let seq = c
        .text("SEQUENCE")
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0)
        + 1;
    set_prop(c, Property::new("SEQUENCE", &seq.to_string()));
}

fn set_span(c: &mut Component, start: Moment, end: Moment, style: &Style, home: Tz) {
    set_prop(c, moment_prop("DTSTART", start, style, home));
    remove_prop(c, "DURATION");
    set_prop(c, moment_prop("DTEND", end, style, home));
}

fn is_recurring(c: &Component) -> bool {
    c.prop("RRULE").is_some() || c.prop("RDATE").is_some()
}

fn master_idx(vcal: &Component, uid: &str) -> Option<usize> {
    vcal.children
        .iter()
        .position(|c| has_uid(c, uid) && c.prop("RECURRENCE-ID").is_none())
}

fn override_idxs(vcal: &Component, uid: &str) -> Vec<usize> {
    (0..vcal.children.len())
        .filter(|&i| {
            let c = &vcal.children[i];
            has_uid(c, uid) && c.prop("RECURRENCE-ID").is_some()
        })
        .collect()
}

fn rid_of(c: &Component, home: Tz) -> Option<Moment> {
    c.prop("RECURRENCE-ID")
        .and_then(|p| parse_moment(p, home).ok())
}

/// Add generated VTIMEZONEs for IANA TZIDs that have none in the file.
fn ensure_vtimezones(vcal: &mut Component, year: i32) {
    fn collect(c: &Component, ids: &mut BTreeSet<String>) {
        for p in &c.properties {
            if let Some(id) = p.param("TZID") {
                ids.insert(id.to_string());
            }
        }
        for ch in &c.children {
            collect(ch, ids);
        }
    }
    let mut ids = BTreeSet::new();
    for c in vcal.children.iter().filter(|c| !is(c, "VTIMEZONE")) {
        collect(c, &mut ids);
    }
    for id in ids {
        let present = vcal
            .children
            .iter()
            .any(|c| is(c, "VTIMEZONE") && c.text("TZID").as_deref() == Some(id.as_str()));
        if present {
            continue;
        }
        if let Ok(tz) = id.parse::<Tz>() {
            let at = vcal
                .children
                .iter()
                .position(|c| !is(c, "VTIMEZONE"))
                .unwrap_or(vcal.children.len());
            vcal.children
                .insert(at, vtimezone::vtimezone(&id, tz, year));
        }
    }
}

fn new_vcalendar() -> Component {
    let mut v = Component::new("VCALENDAR");
    v.properties.push(Property::new("VERSION", "2.0"));
    v.properties.push(Property::new(
        "PRODID",
        &format!("-//cal-tui//cal-tui {}//EN", crate::VERSION),
    ));
    v
}

fn new_uid(now: DateTime<Utc>) -> String {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let salt = now.timestamp_subsec_nanos() ^ std::process::id().rotate_left(16);
    format!("{}-{salt:08x}{n:04x}@cal-tui", now.format("%Y%m%dT%H%M%SZ"))
}

fn file_name_for(uid: &str) -> String {
    let safe: String = uid
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{safe}.ics")
}

fn serialize(comps: &[Component]) -> String {
    comps.iter().map(Component::to_ical).collect()
}

fn check_span(start: &Moment, end: &Moment, home: Tz) -> Result<()> {
    match (start, end) {
        (Moment::Date(s), Moment::Date(e)) if e <= s => {
            Err(invalid("an all-day item must end after the day it starts"))
        }
        (Moment::Date(_), Moment::DateTime(_)) | (Moment::DateTime(_), Moment::Date(_)) => Err(
            invalid("start and end must both be dates or both be date-times"),
        ),
        _ if end.instant(home) < start.instant(home) => Err(invalid("the end is before the start")),
        _ => Ok(()),
    }
}

/// RRULE parts that fix the days of occurrences; moving the series to another
/// day would not move those.
fn rule_pins_days(rule: &str) -> bool {
    let r = rule.to_ascii_uppercase();
    ["BYDAY=", "BYMONTHDAY=", "BYYEARDAY=", "BYWEEKNO="]
        .iter()
        .any(|k| r.contains(k))
}

/// Moves moments by the wall-clock difference between the occurrence the user
/// edited and its new start (in the home zone), converting between all-day
/// and timed values when the type changes.
struct Shift {
    delta: Duration,
    days: i64,
    to_date: bool,
    home: Tz,
}

impl Shift {
    fn new(seen: Moment, new: Moment, home: Tz) -> Shift {
        let a = local_naive(seen, home);
        let b = local_naive(new, home);
        Shift {
            delta: b - a,
            days: (b.date() - a.date()).num_days(),
            to_date: new.is_date(),
            home,
        }
    }

    fn apply(&self, m: Moment) -> Moment {
        let moved = local_naive(m, self.home) + self.delta;
        if self.to_date {
            return Moment::Date(moved.date());
        }
        let zoned = local_to_zoned(self.home, moved);
        match m {
            Moment::DateTime(dt) => Moment::DateTime(zoned.with_timezone(&dt.timezone())),
            Moment::Date(_) => Moment::DateTime(zoned),
        }
    }
}

fn local_naive(m: Moment, home: Tz) -> NaiveDateTime {
    match m {
        Moment::Date(d) => d.and_time(NaiveTime::MIN),
        Moment::DateTime(dt) => dt.with_timezone(&home).naive_local(),
    }
}

fn apply_text_fields(c: &mut Component, edit: &EventEdit) {
    if let Some(s) = &edit.summary {
        set_text(c, "SUMMARY", Some(s));
    }
    if let Some(l) = &edit.location {
        set_text(c, "LOCATION", l.as_deref());
    }
    if let Some(d) = &edit.description {
        set_text(c, "DESCRIPTION", d.as_deref());
    }
}

// ---------------------------------------------------------------------------
// Store API

impl Store {
    fn check_writable(&self, cal: usize) -> Result<()> {
        let c = self
            .calendars
            .get(cal)
            .ok_or_else(|| invalid("unknown calendar"))?;
        if c.read_only {
            return Err(EditError::ReadOnly(c.name.clone()).into());
        }
        Ok(())
    }

    /// Calendars that accept edits.
    pub fn writable_calendars(&self) -> Vec<usize> {
        (0..self.calendars.len())
            .filter(|&i| !self.calendars[i].read_only)
            .collect()
    }

    /// The calendar new items go into: `wanted` (folder or display name),
    /// else the configured default, else the first writable calendar.
    pub fn target_calendar(&self, wanted: Option<&str>, default: Option<&str>) -> Result<usize> {
        let idx = match wanted.or(default) {
            Some(name) => self
                .calendar_index(name)
                .ok_or_else(|| invalid(format!("unknown calendar '{name}'")))?,
            None => *self.writable_calendars().first().ok_or_else(|| {
                invalid(
                    "no writable calendar: create a folder in the calendars directory \
                     (or run your sync) first",
                )
            })?,
        };
        self.check_writable(idx)?;
        Ok(idx)
    }

    /// Whether the object can be edited, and if not, why.
    pub fn can_edit(&self, obj: usize) -> Result<()> {
        let o = self
            .objects
            .get(obj)
            .ok_or_else(|| invalid("no such item"))?;
        self.check_writable(o.calendar)?;
        if let Some(why) = self.unsafe_files.get(&o.path) {
            return Err(EditError::UnsafeFile(o.path.clone(), why.clone()).into());
        }
        Ok(())
    }

    /// Read a file for editing, making sure it is the version we loaded.
    fn load_for_edit(&self, path: &Path) -> Result<Vec<Component>> {
        if let Some(why) = self.unsafe_files.get(path) {
            return Err(EditError::UnsafeFile(path.to_path_buf(), why.clone()).into());
        }
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(EditError::Conflict(path.to_path_buf()).into())
            }
            Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
        };
        if self.file_hashes.get(path) != Some(&fsio::content_hash(&bytes)) {
            return Err(EditError::Conflict(path.to_path_buf()).into());
        }
        let text = String::from_utf8(bytes)
            .map_err(|_| EditError::UnsafeFile(path.to_path_buf(), "not valid UTF-8".into()))?;
        let parsed = ical::parse(&text);
        if let Some(w) = parsed.warnings.first() {
            return Err(EditError::UnsafeFile(
                path.to_path_buf(),
                format!("line {}: {}", w.line, w.message),
            )
            .into());
        }
        Ok(parsed.components)
    }

    /// Write the file back, or delete it when no event or todo is left.
    fn save(&self, path: &Path, comps: &[Component]) -> Result<()> {
        let has_items = comps.iter().any(|c| c.children.iter().any(is_item));
        if has_items {
            fsio::write_atomic(path, serialize(comps).as_bytes())
        } else {
            fsio::remove(path)
        }
        .with_context(|| format!("cannot write {}", path.display()))
    }

    /// Load the object's file, let `f` change the VCALENDAR holding it, save.
    fn edit_object(
        &self,
        obj: usize,
        f: impl FnOnce(&mut Component, &str) -> Result<()>,
    ) -> Result<()> {
        self.can_edit(obj)?;
        let o = &self.objects[obj];
        let mut comps = self.load_for_edit(&o.path)?;
        let vcal = comps
            .iter_mut()
            .find(|c| is(c, "VCALENDAR") && c.children.iter().any(|ch| has_uid(ch, &o.uid)))
            .ok_or_else(|| invalid("this item has no UID, so it cannot be edited safely"))?;
        f(vcal, &o.uid)?;
        self.save(&o.path, &comps)
    }

    fn create_item(&self, cal: usize, item: Component, now: DateTime<Utc>) -> Result<String> {
        self.check_writable(cal)?;
        let uid = uid_of(&item).expect("new items have a UID");
        let mut vcal = new_vcalendar();
        vcal.children.push(item);
        ensure_vtimezones(&mut vcal, now.year());
        let path = self.calendars[cal].path.join(file_name_for(&uid));
        fsio::create_atomic(&path, vcal.to_ical().as_bytes())
            .with_context(|| format!("cannot create {}", path.display()))?;
        Ok(uid)
    }

    /// Create an event in calendar `cal`. Returns its UID.
    pub fn create_event(&self, cal: usize, ev: &NewEvent, now: DateTime<Utc>) -> Result<String> {
        self.check_writable(cal)?;
        if ev.summary.trim().is_empty() {
            return Err(invalid("the title is empty"));
        }
        check_span(&ev.start, &ev.end, self.home)?;
        if let Some(r) = &ev.rrule {
            recur::check_rrule(r, &ev.start, self.home)
                .map_err(|e| invalid(format!("invalid repeat rule '{r}': {e}")))?;
        }
        let style = default_style(self.home);
        let mut e = Component::new("VEVENT");
        let uid = new_uid(now);
        e.properties.push(Property::new("UID", &uid));
        e.properties.push(Property::new("DTSTAMP", &utc_stamp(now)));
        e.properties.push(Property::new("CREATED", &utc_stamp(now)));
        e.properties
            .push(Property::new("LAST-MODIFIED", &utc_stamp(now)));
        e.properties.push(Property::new("SEQUENCE", "0"));
        set_span(&mut e, ev.start, ev.end, &style, self.home);
        set_text(&mut e, "SUMMARY", Some(&ev.summary));
        set_text(&mut e, "LOCATION", ev.location.as_deref());
        set_text(&mut e, "DESCRIPTION", ev.description.as_deref());
        if let Some(r) = &ev.rrule {
            e.properties.push(Property::new("RRULE", r.trim()));
        }
        if let Some(before) = ev.reminder {
            let mut a = Component::new("VALARM");
            a.properties.push(Property::new("ACTION", "DISPLAY"));
            a.properties
                .push(Property::new("TRIGGER", &format_duration(-before)));
            a.properties.push(Property::new(
                "DESCRIPTION",
                &escape_text(ev.summary.trim()),
            ));
            e.children.push(a);
        }
        self.create_item(cal, e, now)
    }

    /// Change an event. `seen_start` is the start of the occurrence the user
    /// edited (for a whole-series time change, the series moves by the same
    /// amount).
    pub fn update_event(
        &self,
        obj: usize,
        scope: Scope,
        seen_start: Moment,
        edit: &EventEdit,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let home = self.home;
        if let Some(s) = &edit.summary {
            if s.trim().is_empty() {
                return Err(invalid("the title is empty"));
            }
        }
        if let Some((s, e)) = &edit.time {
            check_span(s, e, home)?;
        }
        self.edit_object(obj, |vcal, uid| {
            let mi = master_idx(vcal, uid)
                .ok_or_else(|| invalid("the main part of this series is missing"))?;
            match scope {
                Scope::Occurrence(rid) if is_recurring(&vcal.children[mi]) => {
                    edit_occurrence(vcal, mi, uid, rid, edit, home, now)?
                }
                _ => edit_series(vcal, mi, uid, seen_start, edit, home, now)?,
            }
            ensure_vtimezones(vcal, now.year());
            Ok(())
        })
    }

    /// Delete an item, or one occurrence of a repeating event.
    pub fn delete(&self, obj: usize, scope: Scope, now: DateTime<Utc>) -> Result<()> {
        let home = self.home;
        self.edit_object(obj, |vcal, uid| {
            let master = master_idx(vcal, uid);
            match (scope, master) {
                (Scope::Occurrence(rid), Some(mi)) if is_recurring(&vcal.children[mi]) => {
                    vcal.children.retain(|c| {
                        !(has_uid(c, uid)
                            && rid_of(c, home).is_some_and(|r| same_occurrence(&r, &rid, home)))
                    });
                    let mi = master_idx(vcal, uid).expect("master kept");
                    let m = &mut vcal.children[mi];
                    let style = style_of(m.prop("DTSTART"), home);
                    m.properties.push(moment_prop("EXDATE", rid, &style, home));
                    touch(m, now);
                }
                _ => vcal.children.retain(|c| !has_uid(c, uid)),
            }
            Ok(())
        })
    }

    /// Create a todo in calendar `cal`. Returns its UID.
    pub fn create_todo(&self, cal: usize, t: &NewTodo, now: DateTime<Utc>) -> Result<String> {
        self.check_writable(cal)?;
        if t.summary.trim().is_empty() {
            return Err(invalid("the title is empty"));
        }
        let mut c = Component::new("VTODO");
        let uid = new_uid(now);
        c.properties.push(Property::new("UID", &uid));
        c.properties.push(Property::new("DTSTAMP", &utc_stamp(now)));
        c.properties.push(Property::new("CREATED", &utc_stamp(now)));
        c.properties
            .push(Property::new("LAST-MODIFIED", &utc_stamp(now)));
        c.properties.push(Property::new("SEQUENCE", "0"));
        set_text(&mut c, "SUMMARY", Some(&t.summary));
        set_text(&mut c, "DESCRIPTION", t.description.as_deref());
        if let Some(due) = t.due {
            c.properties.push(moment_prop(
                "DUE",
                due,
                &default_style(self.home),
                self.home,
            ));
        }
        if t.priority > 0 {
            c.properties
                .push(Property::new("PRIORITY", &t.priority.min(9).to_string()));
        }
        c.properties.push(Property::new("STATUS", "NEEDS-ACTION"));
        self.create_item(cal, c, now)
    }

    /// Change a todo.
    pub fn update_todo(&self, obj: usize, edit: &TodoEdit, now: DateTime<Utc>) -> Result<()> {
        let home = self.home;
        if self.objects.get(obj).map(|o| o.kind) != Some(Kind::Todo) {
            return Err(invalid("not a todo"));
        }
        if let Some(s) = &edit.summary {
            if s.trim().is_empty() {
                return Err(invalid("the title is empty"));
            }
        }
        self.edit_object(obj, |vcal, uid| {
            let mi = master_idx(vcal, uid).ok_or_else(|| invalid("todo not found"))?;
            let c = &mut vcal.children[mi];
            if let Some(s) = &edit.summary {
                set_text(c, "SUMMARY", Some(s));
            }
            if let Some(d) = &edit.description {
                set_text(c, "DESCRIPTION", d.as_deref());
            }
            if let Some(due) = edit.due {
                match due {
                    Some(m) => {
                        let style = style_of(c.prop("DUE"), home);
                        set_prop(c, moment_prop("DUE", m, &style, home));
                    }
                    None => remove_prop(c, "DUE"),
                }
            }
            if let Some(p) = edit.priority {
                match p.min(9) {
                    0 => remove_prop(c, "PRIORITY"),
                    p => set_prop(c, Property::new("PRIORITY", &p.to_string())),
                }
            }
            touch(c, now);
            ensure_vtimezones(vcal, now.year());
            Ok(())
        })
    }

    /// Mark a todo done or not done.
    pub fn set_todo_completed(&self, obj: usize, done: bool, now: DateTime<Utc>) -> Result<()> {
        if self.objects.get(obj).map(|o| o.kind) != Some(Kind::Todo) {
            return Err(invalid("not a todo"));
        }
        self.edit_object(obj, |vcal, uid| {
            let mi = master_idx(vcal, uid).ok_or_else(|| invalid("todo not found"))?;
            let c = &mut vcal.children[mi];
            if done {
                set_prop(c, Property::new("STATUS", "COMPLETED"));
                set_prop(c, Property::new("COMPLETED", &utc_stamp(now)));
                set_prop(c, Property::new("PERCENT-COMPLETE", "100"));
            } else {
                set_prop(c, Property::new("STATUS", "NEEDS-ACTION"));
                remove_prop(c, "COMPLETED");
                remove_prop(c, "PERCENT-COMPLETE");
            }
            touch(c, now);
            Ok(())
        })
    }
}

/// Edit one occurrence: update its override, creating it from the master.
fn edit_occurrence(
    vcal: &mut Component,
    mi: usize,
    uid: &str,
    rid: Moment,
    edit: &EventEdit,
    home: Tz,
    now: DateTime<Utc>,
) -> Result<()> {
    let master = vcal.children[mi].clone();
    let style = style_of(master.prop("DTSTART"), home);
    let existing = override_idxs(vcal, uid).into_iter().find(|&i| {
        rid_of(&vcal.children[i], home).is_some_and(|r| same_occurrence(&r, &rid, home))
    });
    let idx = match existing {
        Some(i) => i,
        None => {
            let (ms, me) = recur::span(&master, home).map_err(invalid)?;
            let dur = recur::duration_between(&ms, &me, home);
            let mut ov = master.clone();
            for n in ["RRULE", "RDATE", "EXDATE", "EXRULE"] {
                remove_prop(&mut ov, n);
            }
            set_span(&mut ov, rid, rid.add(dur), &style, home);
            set_prop(&mut ov, moment_prop("RECURRENCE-ID", rid, &style, home));
            let last = (0..vcal.children.len())
                .rev()
                .find(|&i| has_uid(&vcal.children[i], uid))
                .unwrap_or(mi);
            vcal.children.insert(last + 1, ov);
            last + 1
        }
    };
    let ov = &mut vcal.children[idx];
    apply_text_fields(ov, edit);
    if let Some((s, e)) = edit.time {
        let st = style_of(ov.prop("DTSTART"), home);
        set_span(ov, s, e, &st, home);
    }
    touch(ov, now);
    Ok(())
}

/// Edit a whole series (or a non-repeating item).
fn edit_series(
    vcal: &mut Component,
    mi: usize,
    uid: &str,
    seen_start: Moment,
    edit: &EventEdit,
    home: Tz,
    now: DateTime<Utc>,
) -> Result<()> {
    let old = vcal.children[mi].clone();
    let overrides = override_idxs(vcal, uid);

    // Text: the master, plus overrides that still had the series' value.
    for (name, new) in [
        ("SUMMARY", edit.summary.as_ref().map(|s| Some(s.as_str()))),
        ("LOCATION", edit.location.as_ref().map(|l| l.as_deref())),
        (
            "DESCRIPTION",
            edit.description.as_ref().map(|d| d.as_deref()),
        ),
    ] {
        let Some(new) = new else { continue };
        let old_val = old.text(name);
        set_text(&mut vcal.children[mi], name, new);
        for &i in &overrides {
            if vcal.children[i].text(name) == old_val {
                set_text(&mut vcal.children[i], name, new);
            }
        }
    }

    // Repeat rule.
    let rule_after: Option<String> = match &edit.rrule {
        Some(r) => r.clone(),
        None => old.prop("RRULE").map(|p| p.value.clone()),
    };
    if let Some(r) = &edit.rrule {
        let m = &mut vcal.children[mi];
        match r {
            Some(rule) => set_prop(m, Property::new("RRULE", rule.trim())),
            None => remove_prop(m, "RRULE"),
        }
    }
    let recurring_after = rule_after.is_some() || old.prop("RDATE").is_some();
    if !recurring_after && is_recurring(&old) {
        // No longer repeating: exceptions are meaningless.
        remove_prop(&mut vcal.children[mi], "EXDATE");
        vcal.children
            .retain(|c| !(has_uid(c, uid) && c.prop("RECURRENCE-ID").is_some()));
    }
    let mi = master_idx(vcal, uid).expect("master kept");

    // Time.
    let (old_start, _) = recur::span(&old, home).map_err(invalid)?;
    let style = style_of(old.prop("DTSTART"), home);
    let mut new_start = old_start;
    // Stopping a series keeps the occurrence the user was looking at.
    let time = edit.time.or_else(|| {
        (is_recurring(&old) && !recurring_after).then(|| {
            let dur = recur::span(&old, home)
                .map(|(s, e)| recur::duration_between(&s, &e, home))
                .unwrap_or_default();
            (seen_start, seen_start.add(dur))
        })
    });
    if let Some((ns, ne)) = time {
        if !is_recurring(&old) || !recurring_after {
            set_span(&mut vcal.children[mi], ns, ne, &style, home);
            new_start = ns;
        } else {
            let shift = Shift::new(seen_start, ns, home);
            if shift.days != 0
                && edit.rrule.is_none()
                && rule_after.as_deref().is_some_and(rule_pins_days)
            {
                return Err(invalid(
                    "this series repeats on fixed days; moving all occurrences to another day \
                     would not match its repeat rule. Change the repeat rule too, or edit \
                     only this occurrence",
                ));
            }
            let dur = recur::duration_between(&ns, &ne, home);
            new_start = shift.apply(old_start);
            set_span(
                &mut vcal.children[mi],
                new_start,
                new_start.add(dur),
                &style,
                home,
            );
            // Exceptions and overrides follow the series.
            let exdates: Vec<Moment> = old
                .props("EXDATE")
                .flat_map(|p| parse_moment_list(p, home))
                .filter_map(|r| r.ok())
                .map(|m| shift.apply(m))
                .collect();
            let m = &mut vcal.children[mi];
            remove_prop(m, "EXDATE");
            for x in exdates {
                m.properties.push(moment_prop("EXDATE", x, &style, home));
            }
            for i in override_idxs(vcal, uid) {
                let ov = &mut vcal.children[i];
                let Some(rid) = rid_of(ov, home) else {
                    continue;
                };
                let new_rid = shift.apply(rid);
                let ov_span = recur::span(ov, home).ok();
                let moved = ov_span.is_some_and(|(s, _)| s.instant(home) != rid.instant(home));
                if !moved {
                    // Not moved by the user: keep it in its (shifted) slot.
                    let ov_dur = ov_span
                        .map(|(s, e)| recur::duration_between(&s, &e, home))
                        .unwrap_or(dur);
                    let st = style_of(ov.prop("DTSTART"), home);
                    set_span(ov, new_rid, new_rid.add(ov_dur), &st, home);
                }
                set_prop(ov, moment_prop("RECURRENCE-ID", new_rid, &style, home));
            }
        }
    }
    if let Some(r) = &rule_after {
        if edit.rrule.is_some() || edit.time.is_some() {
            recur::check_rrule(r, &new_start, home)
                .map_err(|e| invalid(format!("invalid repeat rule '{r}': {e}")))?;
        }
    }
    touch(&mut vcal.children[mi], now);
    Ok(())
}
