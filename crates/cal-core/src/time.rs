//! Dates, date-times, durations and time-zone resolution.

use chrono::{DateTime, Duration, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
use chrono_tz::Tz;

use crate::ical::Property;

/// A DTSTART/DTEND/DUE/RECURRENCE-ID/EXDATE value.
///
/// Floating times (no `Z`, no `TZID`) are interpreted in the user's home zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Moment {
    /// All-day value (`VALUE=DATE`).
    Date(NaiveDate),
    /// A point in time, in the zone the item was written in.
    DateTime(DateTime<Tz>),
}

impl Moment {
    pub fn is_date(&self) -> bool {
        matches!(self, Moment::Date(_))
    }

    /// The calendar date of this moment as seen in `home`.
    pub fn date_in(&self, home: Tz) -> NaiveDate {
        match self {
            Moment::Date(d) => *d,
            Moment::DateTime(dt) => dt.with_timezone(&home).date_naive(),
        }
    }

    /// The instant this moment starts at, with dates taken as midnight in `home`.
    pub fn instant(&self, home: Tz) -> DateTime<Tz> {
        match self {
            Moment::Date(d) => local_to_zoned(home, d.and_time(NaiveTime::MIN)),
            Moment::DateTime(dt) => dt.with_timezone(&home),
        }
    }

    /// Add a duration; for all-day values only whole days are applied.
    pub fn add(&self, d: Duration) -> Moment {
        match self {
            Moment::Date(date) => Moment::Date(*date + Duration::days(d.num_days())),
            Moment::DateTime(dt) => Moment::DateTime(*dt + d),
        }
    }
}

/// Parse a date/date-time property value, resolving its TZID.
pub fn parse_moment(prop: &Property, home: Tz) -> Result<Moment, String> {
    let value = prop.value.trim();
    // A property may hold a list (EXDATE, RDATE); callers split first.
    parse_moment_value(value, prop.param("VALUE"), prop.param("TZID"), home)
}

/// Parse every value of a list-valued property (EXDATE/RDATE).
pub fn parse_moment_list(prop: &Property, home: Tz) -> Vec<Result<Moment, String>> {
    prop.value
        .split(',')
        .filter(|v| !v.trim().is_empty())
        .map(|v| parse_moment_value(v.trim(), prop.param("VALUE"), prop.param("TZID"), home))
        .collect()
}

pub fn parse_moment_value(
    value: &str,
    value_type: Option<&str>,
    tzid: Option<&str>,
    home: Tz,
) -> Result<Moment, String> {
    let is_date = value_type.is_some_and(|v| v.eq_ignore_ascii_case("DATE"))
        || (value.len() == 8 && value.bytes().all(|b| b.is_ascii_digit()));
    if is_date {
        let date = NaiveDate::parse_from_str(&value[..value.len().min(8)], "%Y%m%d")
            .map_err(|e| format!("invalid date '{value}': {e}"))?;
        return Ok(Moment::Date(date));
    }

    let (body, utc) = match value.strip_suffix(['Z', 'z']) {
        Some(b) => (b, true),
        None => (value, false),
    };
    let naive = NaiveDateTime::parse_from_str(body, "%Y%m%dT%H%M%S")
        .or_else(|_| NaiveDateTime::parse_from_str(body, "%Y%m%dT%H%M"))
        .map_err(|e| format!("invalid date-time '{value}': {e}"))?;

    if utc {
        return Ok(Moment::DateTime(Tz::UTC.from_utc_datetime(&naive)));
    }
    let tz = match tzid {
        Some(id) => resolve_tzid(id).ok_or_else(|| format!("unknown time zone '{id}'"))?,
        None => home,
    };
    Ok(Moment::DateTime(local_to_zoned(tz, naive)))
}

/// Convert a wall-clock time to a zoned time, resolving DST gaps (shift
/// forward) and overlaps (earliest).
pub fn local_to_zoned(tz: Tz, naive: NaiveDateTime) -> DateTime<Tz> {
    match tz.from_local_datetime(&naive) {
        LocalResult::Single(dt) => dt,
        LocalResult::Ambiguous(a, _) => a,
        LocalResult::None => {
            // Inside a DST gap: move forward until the time exists.
            let mut t = naive;
            for _ in 0..4 {
                t += Duration::minutes(30);
                if let Some(dt) = tz.from_local_datetime(&t).earliest() {
                    return dt;
                }
            }
            tz.from_utc_datetime(&naive)
        }
    }
}

/// Resolve a TZID to an IANA zone. Accepts IANA names, Mozilla-style
/// prefixed names (`/mozilla.org/20050126_1/Europe/Berlin`), and common
/// Windows/Outlook zone names.
pub fn resolve_tzid(id: &str) -> Option<Tz> {
    let id = id.trim().trim_matches('"');
    if let Ok(tz) = id.parse::<Tz>() {
        return Some(tz);
    }
    // Strip leading path segments: "/mozilla.org/x/Europe/Berlin".
    let segments: Vec<&str> = id.split('/').filter(|s| !s.is_empty()).collect();
    for start in 1..segments.len() {
        if let Ok(tz) = segments[start..].join("/").parse::<Tz>() {
            return Some(tz);
        }
    }
    windows_zone(id)
}

/// Map Windows zone names (as produced by Outlook/Exchange) to IANA zones.
fn windows_zone(id: &str) -> Option<Tz> {
    use chrono_tz::*;
    let tz = match id {
        "UTC" | "Coordinated Universal Time" | "GMT" => UTC,
        "GMT Standard Time" => Europe::London,
        "Greenwich Standard Time" => Atlantic::Reykjavik,
        "W. Europe Standard Time" => Europe::Berlin,
        "Central Europe Standard Time" => Europe::Budapest,
        "Central European Standard Time" => Europe::Warsaw,
        "Romance Standard Time" => Europe::Paris,
        "E. Europe Standard Time" => Europe::Chisinau,
        "FLE Standard Time" => Europe::Helsinki,
        "GTB Standard Time" => Europe::Bucharest,
        "Russian Standard Time" => Europe::Moscow,
        "Eastern Standard Time" => America::New_York,
        "Central Standard Time" => America::Chicago,
        "Mountain Standard Time" => America::Denver,
        "Pacific Standard Time" => America::Los_Angeles,
        "Alaskan Standard Time" => America::Anchorage,
        "Hawaiian Standard Time" => Pacific::Honolulu,
        "Atlantic Standard Time" => America::Halifax,
        "India Standard Time" => Asia::Kolkata,
        "China Standard Time" => Asia::Shanghai,
        "Tokyo Standard Time" => Asia::Tokyo,
        "Korea Standard Time" => Asia::Seoul,
        "Singapore Standard Time" => Asia::Singapore,
        "AUS Eastern Standard Time" => Australia::Sydney,
        "New Zealand Standard Time" => Pacific::Auckland,
        _ => return None,
    };
    Some(tz)
}

/// Parse an iCalendar DURATION such as `PT1H30M`, `-P1D`, `P2W`.
pub fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    let (neg, s) = match s.as_bytes().first()? {
        b'-' => (true, &s[1..]),
        b'+' => (false, &s[1..]),
        _ => (false, s),
    };
    let s = s.strip_prefix(['P', 'p'])?;
    let mut total = Duration::zero();
    let mut num = String::new();
    let mut in_time = false;
    let mut any = false;
    for c in s.chars() {
        match c.to_ascii_uppercase() {
            'T' => in_time = true,
            d if d.is_ascii_digit() => num.push(d),
            unit => {
                let n: i64 = num.parse().ok()?;
                num.clear();
                any = true;
                total += match (unit, in_time) {
                    ('W', false) => Duration::weeks(n),
                    ('D', false) => Duration::days(n),
                    ('H', true) => Duration::hours(n),
                    ('M', true) => Duration::minutes(n),
                    ('S', true) => Duration::seconds(n),
                    _ => return None,
                };
            }
        }
    }
    if !any || !num.is_empty() {
        return None;
    }
    Some(if neg { -total } else { total })
}

/// The zone used for floating times and display: `$TZ`, then the system zone,
/// then UTC.
pub fn system_zone() -> Tz {
    std::env::var("TZ")
        .ok()
        .and_then(|z| resolve_tzid(z.trim_start_matches(':')))
        .or_else(|| {
            iana_time_zone::get_timezone()
                .ok()
                .and_then(|z| z.parse().ok())
        })
        .unwrap_or(Tz::UTC)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;
    use chrono_tz::Europe::Stockholm;

    #[test]
    fn parses_values() {
        let home = Stockholm;
        assert_eq!(
            parse_moment_value("20261116", None, None, home).unwrap(),
            Moment::Date(NaiveDate::from_ymd_opt(2026, 11, 16).unwrap())
        );
        let m = parse_moment_value("20261116T081500Z", None, None, home).unwrap();
        assert_eq!(m.instant(home).hour(), 9);
        let m = parse_moment_value(
            "20261116T081500",
            None,
            Some("W. Europe Standard Time"),
            home,
        )
        .unwrap();
        assert_eq!(m.instant(home).hour(), 8);
        let m = parse_moment_value(
            "20261116T081500",
            None,
            Some("/mozilla.org/20050126_1/Europe/London"),
            home,
        )
        .unwrap();
        assert_eq!(m.instant(home).hour(), 9);
        assert!(parse_moment_value("20261116T081500", None, Some("Mars/Base"), home).is_err());
    }

    #[test]
    fn dst_gap_shifts_forward() {
        // 2026-03-29 02:30 does not exist in Stockholm.
        let naive = NaiveDate::from_ymd_opt(2026, 3, 29)
            .unwrap()
            .and_hms_opt(2, 30, 0)
            .unwrap();
        assert_eq!(local_to_zoned(Stockholm, naive).hour(), 3);
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("PT1H30M"), Some(Duration::minutes(90)));
        assert_eq!(parse_duration("-PT15M"), Some(Duration::minutes(-15)));
        assert_eq!(parse_duration("P1W"), Some(Duration::days(7)));
        assert_eq!(parse_duration("P1DT2H"), Some(Duration::hours(26)));
        assert_eq!(parse_duration("P"), None);
        assert_eq!(parse_duration("PT5"), None);
    }
}
