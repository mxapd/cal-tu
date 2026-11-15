//! Recurrence expansion: RRULE + RDATE − EXDATE, with RECURRENCE-ID overrides.

use std::str::FromStr;

use chrono::{DateTime, Duration, NaiveTime, TimeZone};
use chrono_tz::Tz;
use rrule::{RRule, RRuleSet, Unvalidated};

use crate::ical::Component;
use crate::time::{local_to_zoned, parse_duration, parse_moment, parse_moment_list, Moment};

/// One concrete occurrence produced by expansion.
#[derive(Debug, Clone)]
pub struct Occurrence<'a> {
    /// The component the data comes from (the master or an override).
    pub component: &'a Component,
    /// `Some` for occurrences of a recurring series (the original start).
    pub recurrence_id: Option<Moment>,
    pub start: Moment,
    pub end: Moment,
}

/// Start, end and duration of a VEVENT-like component.
pub fn span(comp: &Component, home: Tz) -> Result<(Moment, Moment), String> {
    let start = comp
        .prop("DTSTART")
        .ok_or("missing DTSTART")
        .and_then(|p| parse_moment(p, home).map_err(|_| "invalid DTSTART"))
        .map_err(str::to_string)?;
    let end = if let Some(p) = comp.prop("DTEND") {
        match parse_moment(p, home) {
            // Lenient: an end before the start is treated as no end.
            Ok(e) if moment_ge(&e, &start, home) => e,
            _ => default_end(start),
        }
    } else if let Some(d) = comp.prop("DURATION").and_then(|p| parse_duration(&p.value)) {
        start.add(d)
    } else {
        default_end(start)
    };
    Ok((start, end))
}

fn default_end(start: Moment) -> Moment {
    match start {
        Moment::Date(d) => Moment::Date(d + Duration::days(1)),
        Moment::DateTime(_) => start,
    }
}

fn moment_ge(a: &Moment, b: &Moment, home: Tz) -> bool {
    a.instant(home) >= b.instant(home)
}

pub(crate) fn duration_between(start: &Moment, end: &Moment, home: Tz) -> Duration {
    match (start, end) {
        (Moment::Date(s), Moment::Date(e)) => *e - *s,
        _ => end.instant(home) - start.instant(home),
    }
}

/// Whether two moments identify the same occurrence. A DATE matches any
/// date-time on that day (lenient handling of mismatched EXDATE types).
pub fn same_occurrence(a: &Moment, b: &Moment, home: Tz) -> bool {
    match (a, b) {
        (Moment::DateTime(x), Moment::DateTime(y)) => x == y,
        (Moment::Date(x), Moment::Date(y)) => x == y,
        (Moment::Date(d), m @ Moment::DateTime(_)) | (m @ Moment::DateTime(_), Moment::Date(d)) => {
            m.date_in(home) == *d
        }
    }
}

fn overlaps(start: &Moment, end: &Moment, from: DateTime<Tz>, to: DateTime<Tz>, home: Tz) -> bool {
    let s = start.instant(home);
    let e = end.instant(home);
    if s == e {
        s >= from && s < to
    } else {
        s < to && e > from
    }
}

/// Expand a master component and its overrides into the occurrences that
/// overlap `[from, to)`. Problems are reported through `warn`.
pub fn expand<'a>(
    master: &'a Component,
    overrides: &[&'a Component],
    from: DateTime<Tz>,
    to: DateTime<Tz>,
    home: Tz,
    warn: &mut dyn FnMut(String),
) -> Vec<Occurrence<'a>> {
    let mut out = Vec::new();
    let (start, end) = match span(master, home) {
        Ok(s) => s,
        Err(e) => {
            warn(e);
            return out;
        }
    };
    let dur = duration_between(&start, &end, home);

    // Overrides keyed by their RECURRENCE-ID.
    let mut override_ids: Vec<(Moment, &'a Component)> = Vec::new();
    for ov in overrides {
        match ov.prop("RECURRENCE-ID").map(|p| parse_moment(p, home)) {
            Some(Ok(rid)) => override_ids.push((rid, ov)),
            _ => warn("override with invalid RECURRENCE-ID ignored".into()),
        }
    }

    let rrules: Vec<&str> = master.props("RRULE").map(|p| p.value.as_str()).collect();
    let has_rdate = master.prop("RDATE").is_some();

    if rrules.is_empty() && !has_rdate && override_ids.is_empty() {
        if overlaps(&start, &end, from, to, home) {
            out.push(Occurrence {
                component: master,
                recurrence_id: None,
                start,
                end,
            });
        }
        return out;
    }

    // Collect occurrence starts in a widened window, so occurrences that began
    // before `from` but are still running are included.
    let window_start = from - dur.max(Duration::zero()) - Duration::days(1);
    let mut starts: Vec<Moment> = Vec::new();
    if rrules.is_empty() {
        starts.push(start);
    }
    for rule in rrules {
        match expand_rrule(rule, &start, window_start, to, home) {
            Ok(mut s) => starts.append(&mut s),
            Err(e) => {
                warn(format!("cannot expand RRULE '{rule}': {e}"));
                starts.push(start);
            }
        }
    }
    for p in master.props("RDATE") {
        for r in parse_moment_list(p, home) {
            match r {
                Ok(m) => starts.push(m),
                Err(e) => warn(format!("invalid RDATE: {e}")),
            }
        }
    }
    let exdates: Vec<Moment> = master
        .props("EXDATE")
        .flat_map(|p| parse_moment_list(p, home))
        .filter_map(|r| r.map_err(|e| warn(format!("invalid EXDATE: {e}"))).ok())
        .collect();

    starts.sort_by_key(|m| m.instant(home));
    starts.dedup_by(|a, b| same_occurrence(a, b, home));

    for s in starts {
        if exdates.iter().any(|x| same_occurrence(x, &s, home)) {
            continue;
        }
        if override_ids
            .iter()
            .any(|(rid, _)| same_occurrence(rid, &s, home))
        {
            continue; // handled below
        }
        let e = s.add(dur);
        if overlaps(&s, &e, from, to, home) {
            out.push(Occurrence {
                component: master,
                recurrence_id: Some(s),
                start: s,
                end: e,
            });
        }
    }

    for (rid, ov) in override_ids {
        if ov
            .text("STATUS")
            .is_some_and(|s| s.eq_ignore_ascii_case("CANCELLED"))
        {
            continue;
        }
        let ov_start = ov
            .prop("DTSTART")
            .and_then(|p| parse_moment(p, home).ok())
            .unwrap_or(rid);
        let ov_end = if ov.prop("DTEND").is_some() || ov.prop("DURATION").is_some() {
            span(ov, home).map(|(_, e)| e).unwrap_or(ov_start.add(dur))
        } else {
            ov_start.add(dur)
        };
        if overlaps(&ov_start, &ov_end, from, to, home) {
            out.push(Occurrence {
                component: ov,
                recurrence_id: Some(rid),
                start: ov_start,
                end: ov_end,
            });
        }
    }

    out.sort_by_key(|o| o.start.instant(home));
    out
}

/// Expand one RRULE into occurrence starts within `[window_start, to)`.
fn expand_rrule(
    rule: &str,
    dtstart: &Moment,
    window_start: DateTime<Tz>,
    to: DateTime<Tz>,
    home: Tz,
) -> Result<Vec<Moment>, String> {
    // UNTIL is handled here instead of by the rrule crate, which is strict
    // about its value type and time zone.
    let mut until_raw = None;
    let rest: Vec<&str> = rule
        .trim()
        .trim_start_matches("RRULE:")
        .split(';')
        .filter(|part| {
            let (k, v) = part.split_once('=').unwrap_or((part, ""));
            if k.trim().eq_ignore_ascii_case("UNTIL") {
                until_raw = Some(v.trim().to_string());
                false
            } else {
                !part.trim().is_empty()
            }
        })
        .collect();
    let parsed = RRule::<Unvalidated>::from_str(&rest.join(";")).map_err(|e| e.to_string())?;

    // Run the expansion in the zone of DTSTART; all-day series run in UTC.
    let (rr_start, zone): (DateTime<rrule::Tz>, Option<Tz>) = match dtstart {
        Moment::Date(d) => (
            rrule::Tz::UTC.from_utc_datetime(&d.and_time(NaiveTime::MIN)),
            None,
        ),
        Moment::DateTime(dt) => {
            let tz = dt.timezone();
            (dt.with_timezone(&rrule::Tz::Tz(tz)), Some(tz))
        }
    };

    let mut rrule = parsed;
    if let Some(u) = until_raw {
        let until_utc = until_to_utc(&u, dtstart, home)?;
        if until_utc < rr_start.with_timezone(&chrono::Utc) {
            return Ok(vec![]);
        }
        rrule = rrule.until(rrule::Tz::UTC.from_utc_datetime(&until_utc.naive_utc()));
    }
    let set: RRuleSet = rrule.build(rr_start).map_err(|e| e.to_string())?.limit();

    let mut out = Vec::new();
    for dt in &set {
        let m = match zone {
            None => Moment::Date(dt.date_naive()),
            Some(tz) => Moment::DateTime(dt.with_timezone(&tz)),
        };
        let inst = m.instant(home);
        if inst >= to {
            break;
        }
        if inst >= window_start {
            out.push(m);
        }
    }
    Ok(out)
}

/// Interpret an UNTIL value relative to DTSTART and return it in UTC.
fn until_to_utc(value: &str, dtstart: &Moment, home: Tz) -> Result<DateTime<chrono::Utc>, String> {
    let start_tz = match dtstart {
        Moment::DateTime(dt) => dt.timezone(),
        Moment::Date(_) => Tz::UTC,
    };
    let m = crate::time::parse_moment_value(value, None, None, start_tz)
        .or_else(|_| crate::time::parse_moment_value(value, None, None, home))?;
    Ok(match m {
        // A DATE UNTIL includes the whole day.
        Moment::Date(d) => {
            local_to_zoned(start_tz, d.and_hms_opt(23, 59, 59).unwrap()).with_timezone(&chrono::Utc)
        }
        Moment::DateTime(dt) => dt.with_timezone(&chrono::Utc),
    })
}

/// Check that an RRULE value can be expanded from `dtstart`.
pub fn check_rrule(rule: &str, dtstart: &Moment, home: Tz) -> Result<(), String> {
    let from = dtstart.instant(home);
    expand_rrule(rule, dtstart, from, from + Duration::days(1), home).map(|_| ())
}
