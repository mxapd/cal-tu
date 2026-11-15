//! Generate `VTIMEZONE` components from the IANA database.
//!
//! RFC 5545 requires a VTIMEZONE for every TZID used in a file. cal-tui itself
//! resolves IANA names directly, but other clients (phones, Thunderbird,
//! Outlook) may rely on the definition, so new items include one.
//!
//! The yearly rules are derived from the transitions in a reference year,
//! expressed as "nth / last weekday of month", which covers the common DST
//! schemes (EU, North America, …).

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, Offset, TimeZone, Utc};
use chrono_tz::{OffsetComponents, Tz};

use crate::ical::{Component, Property};

fn offset_secs(tz: Tz, at: DateTime<Utc>) -> i32 {
    tz.offset_from_utc_datetime(&at.naive_utc())
        .fix()
        .local_minus_utc()
}

fn is_dst(tz: Tz, at: DateTime<Utc>) -> bool {
    !tz.offset_from_utc_datetime(&at.naive_utc())
        .dst_offset()
        .is_zero()
}

fn fmt_offset(secs: i32) -> String {
    let sign = if secs < 0 { '-' } else { '+' };
    let s = secs.abs();
    format!("{sign}{:02}{:02}", s / 3600, (s % 3600) / 60)
}

/// Instants in `year` where the UTC offset changes, to the minute.
fn transitions(tz: Tz, year: i32) -> Vec<DateTime<Utc>> {
    let start = Utc.with_ymd_and_hms(year, 1, 1, 0, 0, 0).unwrap();
    let end = Utc.with_ymd_and_hms(year + 1, 1, 1, 0, 0, 0).unwrap();
    let mut out = Vec::new();
    let mut t = start;
    let mut off = offset_secs(tz, t);
    while t < end {
        let next = t + Duration::hours(1);
        let o = offset_secs(tz, next);
        if o != off {
            // Narrow down to the minute.
            let (mut lo, mut hi) = (t, next);
            while hi - lo > Duration::minutes(1) {
                let mid = lo + (hi - lo) / 2;
                if offset_secs(tz, mid) == off {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            out.push(hi);
            off = o;
        }
        t = next;
    }
    out
}

/// `BYDAY` value such as `-1SU` or `2SU` for a date.
fn byday(date: NaiveDate) -> String {
    let wd = match date.weekday() {
        chrono::Weekday::Mon => "MO",
        chrono::Weekday::Tue => "TU",
        chrono::Weekday::Wed => "WE",
        chrono::Weekday::Thu => "TH",
        chrono::Weekday::Fri => "FR",
        chrono::Weekday::Sat => "SA",
        chrono::Weekday::Sun => "SU",
    };
    let last = (date + Duration::days(7)).month() != date.month();
    if last {
        format!("-1{wd}")
    } else {
        format!("{}{wd}", (date.day() - 1) / 7 + 1)
    }
}

/// First date in `year` matching the month/BYDAY of `date`.
fn same_rule_in(date: NaiveDate, year: i32) -> NaiveDate {
    let rule = byday(date);
    let mut d = NaiveDate::from_ymd_opt(year, date.month(), 1).unwrap();
    while d.month() == date.month() {
        if d.weekday() == date.weekday() && byday(d) == rule {
            return d;
        }
        // `-1` rules are found by stepping through the month.
        d += Duration::days(1);
    }
    date
}

/// A VTIMEZONE for `tzid` (written as given) based on the rules of `tz`
/// around `year`.
pub fn vtimezone(tzid: &str, tz: Tz, year: i32) -> Component {
    let mut vtz = Component::new("VTIMEZONE");
    vtz.properties.push(Property::new("TZID", tzid));
    let trans = transitions(tz, year);
    let mid_year = Utc.with_ymd_and_hms(year, 1, 1, 0, 0, 0).unwrap();

    if trans.is_empty() {
        let off = fmt_offset(offset_secs(tz, mid_year));
        let mut std = Component::new("STANDARD");
        std.properties
            .push(Property::new("DTSTART", "19700101T000000"));
        std.properties.push(Property::new("TZOFFSETFROM", &off));
        std.properties.push(Property::new("TZOFFSETTO", &off));
        vtz.children.push(std);
        return vtz;
    }

    for t in trans {
        let from = offset_secs(tz, t - Duration::minutes(1));
        let to = offset_secs(tz, t);
        // Local wall-clock time of the transition, in the old offset.
        let local: NaiveDateTime = t.naive_utc() + Duration::seconds(from as i64);
        let name = if is_dst(tz, t) {
            "DAYLIGHT"
        } else {
            "STANDARD"
        };
        let mut c = Component::new(name);
        let first = same_rule_in(local.date(), 1970).and_time(local.time());
        c.properties.push(Property::new(
            "DTSTART",
            &first.format("%Y%m%dT%H%M%S").to_string(),
        ));
        c.properties.push(Property::new(
            "RRULE",
            &format!(
                "FREQ=YEARLY;BYMONTH={};BYDAY={}",
                local.month(),
                byday(local.date())
            ),
        ));
        c.properties
            .push(Property::new("TZOFFSETFROM", &fmt_offset(from)));
        c.properties
            .push(Property::new("TZOFFSETTO", &fmt_offset(to)));
        let abbr = tz.offset_from_utc_datetime(&t.naive_utc()).to_string();
        if !abbr.starts_with(['+', '-']) {
            c.properties.push(Property::new("TZNAME", &abbr));
        }
        vtz.children.push(c);
    }
    vtz
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stockholm_has_eu_rules() {
        let v = vtimezone("Europe/Stockholm", chrono_tz::Europe::Stockholm, 2026).to_ical();
        assert!(v.contains("BEGIN:DAYLIGHT\r\nDTSTART:19700329T020000\r\nRRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU\r\nTZOFFSETFROM:+0100\r\nTZOFFSETTO:+0200\r\nTZNAME:CEST"), "{v}");
        assert!(v.contains("BEGIN:STANDARD\r\nDTSTART:19701025T030000\r\nRRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU\r\nTZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100"), "{v}");
    }

    #[test]
    fn new_york_uses_nth_sunday() {
        let v = vtimezone("America/New_York", chrono_tz::America::New_York, 2026).to_ical();
        assert!(v.contains("BYMONTH=3;BYDAY=2SU"), "{v}");
        assert!(v.contains("BYMONTH=11;BYDAY=1SU"), "{v}");
    }

    #[test]
    fn zone_without_dst() {
        let v = vtimezone("Asia/Tokyo", chrono_tz::Asia::Tokyo, 2026).to_ical();
        assert!(v.contains("TZOFFSETFROM:+0900\r\nTZOFFSETTO:+0900"), "{v}");
    }
}
