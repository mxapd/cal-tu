//! Reminders: when alarms fire.
//!
//! Sources:
//! - `VALARM`s on events and todos. A `TRIGGER` is relative to the start
//!   (default) or end (`RELATED=END`) of each occurrence, or absolute.
//!   `REPEAT`/`DURATION` add repetitions. Alarms that a client acknowledged
//!   (`ACKNOWLEDGED`, RFC 9074) are skipped, as are `EMAIL`/`PROCEDURE`.
//! - The calendar's `default_reminder` for timed events with no alarm.
//!
//! An override (one changed occurrence) without alarms of its own uses the
//! series' alarms, so a reminder is never lost by editing one occurrence.

use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;

use crate::ical::Component;
use crate::recur;
use crate::store::{Kind, Store, Warning};
use crate::time::{parse_duration, parse_moment, Moment};

/// Occurrences starting this far after the window can still have alarms in it.
const LOOKAHEAD: i64 = 8;
/// …and this far before (alarms after the start or relative to the end).
const LOOKBACK: i64 = 2;

/// One reminder to deliver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alarm {
    /// Stable identity of this reminder, for "fire exactly once".
    pub key: String,
    /// When to notify.
    pub at: DateTime<Tz>,
    pub calendar: usize,
    pub uid: String,
    pub kind: Kind,
    pub summary: String,
    pub location: Option<String>,
    /// Start of the occurrence (todos: due, or start if set).
    pub start: Moment,
    pub end: Moment,
    pub all_day: bool,
    /// From the calendar's `default_reminder` rather than the item.
    pub default: bool,
}

fn utc(dt: DateTime<Tz>) -> String {
    dt.with_timezone(&Utc).format("%Y%m%dT%H%M%SZ").to_string()
}

fn moment_key(m: Option<Moment>) -> String {
    match m {
        None => "-".into(),
        Some(Moment::Date(d)) => d.format("%Y%m%d").to_string(),
        Some(Moment::DateTime(dt)) => utc(dt),
    }
}

/// Trigger times of one VALARM for an item spanning `start`–`end`.
/// Relative triggers need `relative`; absolute ones are returned only when
/// `absolute` is set (they belong to the item, not to each occurrence).
fn trigger_times(
    alarm: &Component,
    start: Moment,
    end: Moment,
    home: Tz,
    relative: bool,
    absolute: bool,
) -> Vec<DateTime<Tz>> {
    let action = alarm
        .text("ACTION")
        .unwrap_or_default()
        .to_ascii_uppercase();
    if matches!(action.trim(), "EMAIL" | "PROCEDURE" | "NONE") {
        return Vec::new();
    }
    let Some(trig) = alarm.prop("TRIGGER") else {
        return Vec::new();
    };
    let is_abs = trig
        .param("VALUE")
        .is_some_and(|v| v.eq_ignore_ascii_case("DATE-TIME"))
        || trig.value.trim().starts_with(|c: char| c.is_ascii_digit());
    let first = if is_abs {
        if !absolute {
            return Vec::new();
        }
        match parse_moment(trig, home) {
            Ok(m) => m.instant(home),
            Err(_) => return Vec::new(),
        }
    } else {
        if !relative {
            return Vec::new();
        }
        let Some(offset) = parse_duration(&trig.value) else {
            return Vec::new();
        };
        let base = if trig
            .param("RELATED")
            .is_some_and(|r| r.eq_ignore_ascii_case("END"))
        {
            end
        } else {
            start
        };
        base.instant(home) + offset
    };
    let mut out = vec![first];
    let repeat = alarm
        .text("REPEAT")
        .and_then(|r| r.trim().parse::<u32>().ok())
        .unwrap_or(0)
        .min(100);
    if let Some(step) = alarm.text("DURATION").and_then(|d| parse_duration(&d)) {
        if step > Duration::zero() {
            for k in 1..=repeat {
                out.push(first + step * k as i32);
            }
        }
    }
    let acked = alarm
        .prop("ACKNOWLEDGED")
        .and_then(|p| parse_moment(p, home).ok())
        .map(|m| m.instant(home));
    out.retain(|t| acked.is_none_or(|a| *t > a));
    out
}

impl Store {
    /// Reminders that fire in `[from, to)`, sorted by time.
    pub fn alarms(&self, from: DateTime<Tz>, to: DateTime<Tz>) -> (Vec<Alarm>, Vec<Warning>) {
        let home = self.home;
        let mut out = Vec::new();
        let mut warnings = Vec::new();
        let occ_from = from - Duration::days(LOOKBACK);
        let occ_to = to + Duration::days(LOOKAHEAD);

        for obj in &self.objects {
            let cal = &self.calendars[obj.calendar];
            if cal.hidden {
                continue;
            }
            let mut push = |at: DateTime<Tz>,
                            rid: Option<Moment>,
                            c: &Component,
                            start: Moment,
                            end: Moment,
                            default: bool| {
                if at < from || at >= to {
                    return;
                }
                let summary = c
                    .text("SUMMARY")
                    .or_else(|| obj.master.text("SUMMARY"))
                    .unwrap_or_else(|| "(no title)".into());
                if cal.hides(&summary) {
                    return;
                }
                out.push(Alarm {
                    key: format!("{}|{}|{}|{}", cal.id, obj.uid, moment_key(rid), utc(at)),
                    at,
                    calendar: obj.calendar,
                    uid: obj.uid.clone(),
                    kind: obj.kind,
                    summary,
                    location: c.text("LOCATION").filter(|s| !s.is_empty()),
                    start,
                    end,
                    all_day: start.is_date(),
                    default,
                });
            };

            match obj.kind {
                Kind::Event => {
                    let overrides: Vec<&Component> = obj.overrides.iter().collect();
                    let mut warn = |m: String| {
                        warnings.push(Warning {
                            path: Some(obj.path.clone()),
                            message: m,
                        })
                    };
                    let occs =
                        recur::expand(&obj.master, &overrides, occ_from, occ_to, home, &mut warn);
                    for o in occs {
                        let c = o.component;
                        if c.text("STATUS")
                            .is_some_and(|s| s.eq_ignore_ascii_case("CANCELLED"))
                        {
                            continue;
                        }
                        let own: Vec<&Component> = c.children_named("VALARM").collect();
                        let valarms = if own.is_empty() {
                            obj.master.children_named("VALARM").collect()
                        } else {
                            own
                        };
                        if valarms.is_empty() {
                            if let (Some(d), false) = (cal.default_reminder, o.start.is_date()) {
                                let at = o.start.instant(home) - d;
                                push(at, o.recurrence_id, c, o.start, o.end, true);
                            }
                            continue;
                        }
                        for a in valarms {
                            for at in trigger_times(a, o.start, o.end, home, true, false) {
                                push(at, o.recurrence_id, c, o.start, o.end, false);
                            }
                        }
                    }
                    // Absolute triggers fire once for the item.
                    if let Ok((s, e)) = recur::span(&obj.master, home) {
                        for a in obj.master.children_named("VALARM") {
                            for at in trigger_times(a, s, e, home, false, true) {
                                push(at, None, &obj.master, s, e, false);
                            }
                        }
                    }
                }
                Kind::Todo => {
                    let c = &obj.master;
                    let status = c.text("STATUS").unwrap_or_default();
                    if status.eq_ignore_ascii_case("COMPLETED")
                        || status.eq_ignore_ascii_case("CANCELLED")
                        || c.prop("COMPLETED").is_some()
                    {
                        continue;
                    }
                    let due = c.prop("DUE").and_then(|p| parse_moment(p, home).ok());
                    let dtstart = c.prop("DTSTART").and_then(|p| parse_moment(p, home).ok());
                    let (Some(start), Some(end)) = (dtstart.or(due), due.or(dtstart)) else {
                        continue;
                    };
                    for a in c.children_named("VALARM") {
                        for at in trigger_times(a, start, end, home, true, true) {
                            push(at, None, c, start, end, false);
                        }
                    }
                }
            }
        }
        out.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.summary.cmp(&b.summary)));
        out.dedup_by(|a, b| a.key == b.key);
        (out, warnings)
    }
}
