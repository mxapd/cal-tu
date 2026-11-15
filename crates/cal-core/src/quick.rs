//! Parsing short human input: `Dentist fri 14:00 1h`, dates, times, lengths.
//!
//! Used by `cal-tui add`, `cal-tui todo add --due` and the TUI forms.

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, Weekday};
use chrono_tz::Tz;
use regex::Regex;

use crate::time::{local_to_zoned, Moment};

/// A date: `today`, `tomorrow`, a weekday (`fri`, `friday`: the next one,
/// today included), `+3d`/`+2w`, `YYYY-MM-DD`, or `MM-DD` (the next such
/// date, possibly next year).
pub fn parse_date(token: &str, today: NaiveDate) -> Option<NaiveDate> {
    let t = token.trim().to_ascii_lowercase();
    match t.as_str() {
        "today" | "tod" => return Some(today),
        "tomorrow" | "tmrw" | "tom" => return today.succ_opt(),
        "yesterday" => return today.pred_opt(),
        _ => {}
    }
    if let Some(wd) = weekday(&t) {
        let ahead = (wd.num_days_from_monday() as i64
            - today.weekday().num_days_from_monday() as i64)
            .rem_euclid(7);
        return Some(today + Duration::days(ahead));
    }
    if let Some(rest) = t.strip_prefix(['+', '-']) {
        let sign = if t.starts_with('-') { -1 } else { 1 };
        let (num, unit) = rest.split_at(
            rest.find(|c: char| !c.is_ascii_digit())
                .unwrap_or(rest.len()),
        );
        let n: i64 = num.parse().ok()?;
        let days = match unit {
            "" | "d" => n,
            "w" => n * 7,
            _ => return None,
        };
        return today.checked_add_signed(Duration::days(sign * days));
    }
    let parts: Vec<&str> = t.split('-').collect();
    match parts.as_slice() {
        [y, m, d] if y.len() == 4 => {
            NaiveDate::from_ymd_opt(y.parse().ok()?, m.parse().ok()?, d.parse().ok()?)
        }
        [m, d] if m.len() <= 2 && d.len() <= 2 => {
            let (m, d) = (m.parse().ok()?, d.parse().ok()?);
            let this = NaiveDate::from_ymd_opt(today.year(), m, d)?;
            if this >= today {
                Some(this)
            } else {
                NaiveDate::from_ymd_opt(today.year() + 1, m, d)
            }
        }
        _ => None,
    }
}

fn weekday(t: &str) -> Option<Weekday> {
    let names = [
        (Weekday::Mon, &["mon", "monday"][..]),
        (Weekday::Tue, &["tue", "tues", "tuesday"]),
        (Weekday::Wed, &["wed", "wednesday"]),
        (Weekday::Thu, &["thu", "thur", "thurs", "thursday"]),
        (Weekday::Fri, &["fri", "friday"]),
        (Weekday::Sat, &["sat", "saturday"]),
        (Weekday::Sun, &["sun", "sunday"]),
    ];
    names
        .iter()
        .find(|(_, ns)| ns.contains(&t))
        .map(|(w, _)| *w)
}

/// A time of day: `14:00`, `9:30`, `2pm`, `2:30pm`, `12am`.
pub fn parse_time(token: &str) -> Option<NaiveTime> {
    let t = token.trim().to_ascii_lowercase();
    let (body, pm) = if let Some(b) = t.strip_suffix("pm") {
        (b, Some(true))
    } else if let Some(b) = t.strip_suffix("am") {
        (b, Some(false))
    } else {
        (t.as_str(), None)
    };
    let (h, m) = match body.split_once(':') {
        Some((h, m)) if m.len() == 2 => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
        None if pm.is_some() => (body.parse::<u32>().ok()?, 0),
        _ => return None,
    };
    let h = match pm {
        Some(_) if !(1..=12).contains(&h) => return None,
        Some(true) => h % 12 + 12,
        Some(false) => h % 12,
        None => h,
    };
    NaiveTime::from_hms_opt(h, m, 0)
}

/// A length with units: `1h`, `30m`, `90min`, `1h30`, `1h30m`, `2d`.
pub fn parse_length(token: &str) -> Option<Duration> {
    let re = Regex::new(r"^(?:(\d+)d)?(?:(\d+)h)?(?:(\d+)(?:m|min|mins)?)?$").unwrap();
    let t = token.trim().to_ascii_lowercase();
    if !t.contains(['d', 'h', 'm']) {
        return None; // a bare number is not a length
    }
    let c = re.captures(&t)?;
    let n = |i: usize| c.get(i).map_or(Ok(0), |m| m.as_str().parse::<i64>());
    let total =
        Duration::days(n(1).ok()?) + Duration::hours(n(2).ok()?) + Duration::minutes(n(3).ok()?);
    (total > Duration::zero()).then_some(total)
}

/// Repeat shorthands: `none`, `daily`, `weekdays`, `weekly`, `biweekly`,
/// `monthly`, `yearly`, or a raw RRULE value (`FREQ=...`). `Ok(None)` means
/// "does not repeat".
pub fn parse_repeat(text: &str) -> Result<Option<String>, String> {
    let t = text.trim();
    let rule = match t.to_ascii_lowercase().as_str() {
        "" | "none" | "no" | "never" => return Ok(None),
        "daily" => "FREQ=DAILY",
        "weekdays" => "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR",
        "weekly" => "FREQ=WEEKLY",
        "biweekly" | "fortnightly" => "FREQ=WEEKLY;INTERVAL=2",
        "monthly" => "FREQ=MONTHLY",
        "yearly" | "annually" => "FREQ=YEARLY",
        l if l.starts_with("freq=") || l.starts_with("rrule:freq=") => {
            return Ok(Some(t.trim_start_matches("RRULE:").trim_start_matches("rrule:").to_string()))
        }
        _ => {
            return Err(format!(
                "unknown repeat '{t}' (none, daily, weekdays, weekly, biweekly, monthly, yearly, or FREQ=...)"
            ))
        }
    };
    Ok(Some(rule.to_string()))
}

/// The shorthand for an RRULE if there is one, else the rule itself.
pub fn describe_repeat(rule: Option<&str>) -> String {
    let Some(rule) = rule else {
        return "none".into();
    };
    for name in [
        "daily", "weekdays", "weekly", "biweekly", "monthly", "yearly",
    ] {
        if parse_repeat(name).ok().flatten().as_deref() == Some(rule.trim()) {
            return name.into();
        }
    }
    rule.trim().to_string()
}

/// Result of [`parse_event`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickEvent {
    pub summary: String,
    pub start: Moment,
    pub end: Moment,
}

/// Parse `Dentist fri 14:00 1h` style input.
///
/// Recognised anywhere in the text (the first of each kind): a date, a start
/// time or range (`14:00-15:30`), an end time after the start, a length,
/// `allday`. Everything else is the title; prefix a word with `\` to keep it
/// in the title (`\fri`). Defaults: today; no time means all day; a timed
/// event lasts one hour.
pub fn parse_event(text: &str, now: DateTime<Tz>) -> Result<QuickEvent, String> {
    let home = now.timezone();
    let today = now.date_naive();
    let (mut date, mut start, mut end, mut len) = (None, None, None, None);
    let mut words = Vec::new();
    for tok in text.split_whitespace() {
        if let Some(lit) = tok.strip_prefix('\\') {
            words.push(lit.to_string());
            continue;
        }
        if date.is_none() {
            if let Some(d) = parse_date(tok, today) {
                date = Some(d);
                continue;
            }
        }
        if start.is_none() {
            if let Some((a, b)) = tok.split_once('-') {
                if let (Some(a), Some(b)) = (parse_time(a), parse_time(b)) {
                    start = Some(a);
                    end = Some(b);
                    continue;
                }
            }
            if let Some(t) = parse_time(tok) {
                start = Some(t);
                continue;
            }
        } else if end.is_none() && len.is_none() {
            if let Some(t) = parse_time(tok) {
                end = Some(t);
                continue;
            }
        }
        if len.is_none() && end.is_none() {
            if let Some(l) = parse_length(tok) {
                len = Some(l);
                continue;
            }
        }
        if matches!(tok.to_ascii_lowercase().as_str(), "allday" | "all-day") && start.is_none() {
            continue;
        }
        if matches!(tok, "-" | "to" | "until") && start.is_some() && end.is_none() && len.is_none()
        {
            continue;
        }
        words.push(tok.to_string());
    }
    let summary = words.join(" ");
    if summary.trim().is_empty() {
        return Err("the title is empty".into());
    }
    let date = date.unwrap_or(today);
    let (start, end) = match start {
        None => {
            let days = len.map_or(1, |l| l.num_days().max(1));
            (
                Moment::Date(date),
                Moment::Date(date + Duration::days(days)),
            )
        }
        Some(t) => {
            let s = local_to_zoned(home, date.and_time(t));
            let e = match (end, len) {
                (Some(et), _) => {
                    let end_date = if et <= t {
                        date + Duration::days(1)
                    } else {
                        date
                    };
                    local_to_zoned(home, end_date.and_time(et))
                }
                (None, Some(l)) => s + l,
                (None, None) => s + Duration::hours(1),
            };
            (Moment::DateTime(s), Moment::DateTime(e))
        }
    };
    Ok(QuickEvent {
        summary,
        start,
        end,
    })
}

/// A due date with optional time: `fri`, `2026-11-20 17:00`, `tomorrow 9am`.
pub fn parse_due(text: &str, now: DateTime<Tz>) -> Result<Moment, String> {
    let today = now.date_naive();
    let (mut date, mut time) = (None, None);
    for tok in text.split_whitespace() {
        if date.is_none() {
            if let Some(d) = parse_date(tok, today) {
                date = Some(d);
                continue;
            }
        }
        if time.is_none() {
            if let Some(t) = parse_time(tok) {
                time = Some(t);
                continue;
            }
        }
        return Err(format!("cannot understand '{tok}' in due date '{text}'"));
    }
    match (date, time) {
        (None, None) => Err("empty due date".into()),
        (d, None) => Ok(Moment::Date(d.unwrap())),
        (d, Some(t)) => Ok(Moment::DateTime(local_to_zoned(
            now.timezone(),
            d.unwrap_or(today).and_time(t),
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use chrono_tz::Europe::Stockholm;

    // Monday 16 Nov 2026, 11:32.
    fn now() -> DateTime<Tz> {
        Stockholm.with_ymd_and_hms(2026, 11, 16, 11, 32, 0).unwrap()
    }

    fn d(m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, m, day).unwrap()
    }

    fn dt(m: u32, day: u32, h: u32, min: u32) -> Moment {
        Moment::DateTime(Stockholm.with_ymd_and_hms(2026, m, day, h, min, 0).unwrap())
    }

    #[test]
    fn dates() {
        let today = d(11, 16);
        assert_eq!(parse_date("today", today), Some(today));
        assert_eq!(parse_date("tomorrow", today), Some(d(11, 17)));
        assert_eq!(parse_date("mon", today), Some(today), "today counts");
        assert_eq!(parse_date("Friday", today), Some(d(11, 20)));
        assert_eq!(parse_date("sun", today), Some(d(11, 22)));
        assert_eq!(parse_date("+3d", today), Some(d(11, 19)));
        assert_eq!(parse_date("+2w", today), Some(d(11, 30)));
        assert_eq!(
            parse_date("2027-01-04", today),
            NaiveDate::from_ymd_opt(2027, 1, 4)
        );
        assert_eq!(parse_date("12-24", today), Some(d(12, 24)));
        assert_eq!(
            parse_date("01-10", today),
            NaiveDate::from_ymd_opt(2027, 1, 10),
            "past → next year"
        );
        assert_eq!(parse_date("dentist", today), None);
        assert_eq!(parse_date("2026-02-30", today), None);
    }

    #[test]
    fn times_and_lengths() {
        let t = |h, m| NaiveTime::from_hms_opt(h, m, 0);
        assert_eq!(parse_time("14:00"), t(14, 0));
        assert_eq!(parse_time("9:05"), t(9, 5));
        assert_eq!(parse_time("2pm"), t(14, 0));
        assert_eq!(parse_time("12am"), t(0, 0));
        assert_eq!(parse_time("12:30pm"), t(12, 30));
        assert_eq!(parse_time("14"), None, "bare numbers stay in the title");
        assert_eq!(parse_time("25:00"), None);
        assert_eq!(parse_length("1h"), Some(Duration::hours(1)));
        assert_eq!(parse_length("1h30"), Some(Duration::minutes(90)));
        assert_eq!(parse_length("90min"), Some(Duration::minutes(90)));
        assert_eq!(parse_length("2d"), Some(Duration::days(2)));
        assert_eq!(parse_length("42"), None);
        assert_eq!(parse_length("hm"), None);
    }

    #[test]
    fn events() {
        let e = parse_event("Dentist fri 14:00 1h", now()).unwrap();
        assert_eq!(e.summary, "Dentist");
        assert_eq!((e.start, e.end), (dt(11, 20, 14, 0), dt(11, 20, 15, 0)));

        let e = parse_event("Call mum 18:30-19:15", now()).unwrap();
        assert_eq!(e.summary, "Call mum");
        assert_eq!((e.start, e.end), (dt(11, 16, 18, 30), dt(11, 16, 19, 15)));

        let e = parse_event("Party sat 22:00 to 2:00", now()).unwrap();
        assert_eq!((e.start, e.end), (dt(11, 21, 22, 0), dt(11, 22, 2, 0)));

        let e = parse_event("Conference 12-01 3d", now()).unwrap();
        assert_eq!(e.start, Moment::Date(d(12, 1)));
        assert_eq!(e.end, Moment::Date(d(12, 4)));

        let e = parse_event("Room 14 meeting tomorrow 9am", now()).unwrap();
        assert_eq!(e.summary, "Room 14 meeting");
        assert_eq!(e.start, dt(11, 17, 9, 0));

        let e = parse_event(r"Thank \sun fri", now()).unwrap();
        assert_eq!(e.summary, "Thank sun");

        assert!(parse_event("fri 14:00", now()).is_err());
    }

    #[test]
    fn repeats() {
        assert_eq!(parse_repeat("weekly"), Ok(Some("FREQ=WEEKLY".into())));
        assert_eq!(parse_repeat("none"), Ok(None));
        assert_eq!(
            parse_repeat("FREQ=MONTHLY;BYDAY=1MO"),
            Ok(Some("FREQ=MONTHLY;BYDAY=1MO".into()))
        );
        assert!(parse_repeat("sometimes").is_err());
        assert_eq!(
            describe_repeat(Some("FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR")),
            "weekdays"
        );
        assert_eq!(
            describe_repeat(Some("FREQ=WEEKLY;BYDAY=MO,WE")),
            "FREQ=WEEKLY;BYDAY=MO,WE"
        );
        assert_eq!(describe_repeat(None), "none");
    }

    #[test]
    fn due() {
        assert_eq!(parse_due("fri", now()), Ok(Moment::Date(d(11, 20))));
        assert_eq!(parse_due("tomorrow 9am", now()), Ok(dt(11, 17, 9, 0)));
        assert_eq!(parse_due("17:00", now()), Ok(dt(11, 16, 17, 0)));
        assert!(parse_due("someday", now()).is_err());
    }
}
