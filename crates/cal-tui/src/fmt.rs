//! Formatting shared by the CLI output and the TUI.

use cal_core::{Instance, Kind, Moment};
use chrono::{Duration, NaiveTime};
use chrono_tz::Tz;

/// How an instance's time looks on one particular day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimeRange {
    AllDay,
    /// Due todo; optional time.
    Due(Option<String>),
    /// Start/end as `HH:MM`; `None` means it continues from/into another day.
    Span(Option<String>, Option<String>),
    /// Zero-length timed item.
    At(String),
}

impl TimeRange {
    /// Fixed-width (11 char) column, e.g. `08:15-10:00`.
    pub fn column(&self) -> String {
        match self {
            TimeRange::AllDay => "all day".into(),
            TimeRange::Due(None) => "due".into(),
            TimeRange::Due(Some(t)) => format!("due {t}"),
            TimeRange::At(t) => format!("{t}      "),
            TimeRange::Span(s, e) => format!(
                "{}-{}",
                s.as_deref().unwrap_or("   …"),
                e.as_deref().unwrap_or("…   ")
            ),
        }
    }
}

pub fn time_range(i: &Instance, day: chrono::NaiveDate, home: Tz) -> TimeRange {
    if i.kind == Kind::Todo {
        return TimeRange::Due(match i.start {
            Moment::DateTime(dt) => Some(dt.with_timezone(&home).format("%H:%M").to_string()),
            Moment::Date(_) => None,
        });
    }
    match (i.start, i.end) {
        (Moment::Date(_), _) | (_, Moment::Date(_)) => TimeRange::AllDay,
        (Moment::DateTime(s), Moment::DateTime(e)) => {
            let s = s.with_timezone(&home);
            let e = e.with_timezone(&home);
            let starts_before = s.date_naive() < day;
            // An end at exactly midnight belongs to the previous day.
            let ends_after = e.date_naive() > day
                && !(e.date_naive() == day + Duration::days(1) && e.time() == NaiveTime::MIN);
            let hm = |t: chrono::DateTime<Tz>| t.format("%H:%M").to_string();
            match (starts_before, ends_after) {
                (true, true) => TimeRange::AllDay,
                _ if s == e => TimeRange::At(hm(s)),
                (sb, ea) => TimeRange::Span((!sb).then(|| hm(s)), (!ea).then(|| hm(e))),
            }
        }
    }
}

/// Parse a calendar colour (`red`, `#rrggbb`) into RGB-or-name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rgb {
    Named(u8), // ANSI 0-7
    True(u8, u8, u8),
}

/// Muted colours that sit well on dark and light backgrounds. Usable by name
/// in the config, and used for calendars without a colour of their own.
pub const SOFT: [(&str, (u8, u8, u8)); 7] = [
    ("orange", (215, 135, 95)),
    ("teal", (95, 175, 175)),
    ("lavender", (175, 135, 215)),
    ("sage", (135, 175, 135)),
    ("sky", (95, 135, 175)),
    ("rose", (215, 135, 135)),
    ("sand", (215, 175, 95)),
];

pub fn parse_color(c: Option<&str>) -> Option<Rgb> {
    let c = c?.trim();
    if let Some(hex) = c.strip_prefix('#') {
        if hex.len() >= 6 {
            let p = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
            return Some(Rgb::True(p(0)?, p(2)?, p(4)?));
        }
        return None;
    }
    let lower = c.to_ascii_lowercase();
    if let Some((_, rgb)) = SOFT.iter().find(|(name, _)| *name == lower) {
        return Some(Rgb::True(rgb.0, rgb.1, rgb.2));
    }
    let n = match lower.as_str() {
        "black" => 0,
        "red" => 1,
        "green" => 2,
        "yellow" => 3,
        "blue" => 4,
        "magenta" => 5,
        "cyan" => 6,
        "white" => 7,
        _ => return None,
    };
    Some(Rgb::Named(n))
}

/// Human-friendly duration, e.g. `28m`, `2h05m`, `3d4h`.
pub fn humanize(d: Duration) -> String {
    let mins = d.num_minutes().max(0);
    match (mins / 1440, (mins % 1440) / 60, mins % 60) {
        (0, 0, m) => format!("{m}m"),
        (0, h, 0) => format!("{h}h"),
        (0, h, m) => format!("{h}h{m:02}m"),
        (d, h, _) => format!("{d}d{h}h"),
    }
}
