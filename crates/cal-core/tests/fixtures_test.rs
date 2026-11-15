//! Integration tests against the fixture vdir in `tests/fixtures/calendars`.

use cal_core::{Config, Kind, Moment, Store};
use chrono::{NaiveDate, Timelike};
use chrono_tz::Europe::Stockholm;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn store() -> Store {
    let config: Config = toml::from_str(
        r#"
        [calendars.uni]
        read_only = true
        hide_matching = ["^Tutorial group B"]
        "#,
    )
    .unwrap();
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/calendars");
    Store::load_dir(dir.as_ref(), &config, Stockholm)
}

/// (local start time "HH:MM" or "all-day", summary) for one day.
fn day(store: &Store, date: NaiveDate) -> Vec<(String, String)> {
    let (inst, warnings) = store.day(date);
    assert!(warnings.is_empty(), "{warnings:?}");
    inst.into_iter()
        .map(|i| {
            let t = match i.start {
                Moment::Date(_) => "all-day".to_string(),
                Moment::DateTime(dt) => {
                    let l = dt.with_timezone(&Stockholm);
                    format!("{:02}:{:02}", l.hour(), l.minute())
                }
            };
            (t, i.summary)
        })
        .collect()
}

fn s(v: &[(&str, &str)]) -> Vec<(String, String)> {
    v.iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

#[test]
fn loads_calendars_and_metadata() {
    let st = store();
    let names: Vec<_> = st
        .calendars
        .iter()
        .map(|c| (c.id.as_str(), c.name.as_str()))
        .collect();
    assert_eq!(names, vec![("personal", "personal"), ("uni", "University")]);
    assert_eq!(st.calendars[1].color.as_deref(), Some("#00aaff"));
    assert!(st.calendars[1].read_only);
    assert!(!st.calendars[0].read_only);
}

#[test]
fn broken_file_is_a_warning_not_a_failure() {
    let st = store();
    assert!(st
        .warnings
        .iter()
        .any(|w| w.path.as_ref().is_some_and(|p| p.ends_with("broken.ics"))));
    // Everything else still loaded.
    assert!(st.objects.len() >= 10);
}

#[test]
fn monday_mixes_calendars_and_hides_filtered_items() {
    let st = store();
    assert_eq!(
        day(&st, d(2026, 11, 16)),
        s(&[
            ("08:15", "Linear Algebra, lecture"),
            ("12:00", "Lunch with Anna"),
            ("18:30", "Climbing"),
        ])
    );
}

#[test]
fn recurrence_exdate_override_and_cancel() {
    let st = store();
    // Regular Wednesday.
    assert_eq!(day(&st, d(2026, 10, 14)), s(&[("18:30", "Climbing")]));
    // EXDATE.
    assert_eq!(day(&st, d(2026, 10, 21)), s(&[]));
    // Overridden: moved from Wed 28th to Thu 29th.
    assert_eq!(day(&st, d(2026, 10, 28)), s(&[]));
    assert_eq!(
        day(&st, d(2026, 10, 29)),
        s(&[("19:00", "Climbing (moved)")])
    );
    // Cancelled occurrence.
    assert_eq!(day(&st, d(2026, 11, 4)), s(&[]));
}

#[test]
fn recurrence_keeps_wall_clock_time_across_dst() {
    let st = store();
    // DST ends 2026-10-25; both sides stay 18:30 local and 08:15 local.
    assert_eq!(
        day(&st, d(2026, 10, 19)),
        s(&[("08:15", "Linear Algebra, lecture"), ("18:30", "Climbing")])
    );
    assert_eq!(
        day(&st, d(2026, 10, 26)),
        s(&[("08:15", "Linear Algebra, lecture"), ("18:30", "Climbing")])
    );
}

#[test]
fn until_is_respected() {
    let st = store();
    // Lecture UNTIL 2026-12-31: last on Mon 28 Dec, none on Mon 4 Jan.
    assert!(day(&st, d(2026, 12, 28))
        .iter()
        .any(|(_, s)| s.starts_with("Linear")));
    assert_eq!(day(&st, d(2027, 1, 4)), s(&[("18:30", "Climbing")]));
    // All-day series with a date-time UNTIL (lenient): Nov 1, 8, 15.
    let reading: Vec<_> = (1..=22)
        .filter(|n| {
            day(&st, d(2026, 11, *n))
                .iter()
                .any(|(_, s)| s == "Reading week")
        })
        .collect();
    assert_eq!(reading, vec![1, 8, 15]);
}

#[test]
fn count_and_multi_day_all_day_events() {
    let st = store();
    let exam: Vec<_> = (13..=17)
        .filter(|n| {
            day(&st, d(2026, 12, *n))
                .iter()
                .any(|(_, s)| s == "Exam week")
        })
        .collect();
    assert_eq!(exam, vec![14, 15, 16]);
    // DTEND is exclusive: 20, 21, 22 but not 23.
    let trip: Vec<_> = (19..=23)
        .filter(|n| {
            day(&st, d(2026, 11, *n))
                .iter()
                .any(|(_, s)| s == "Weekend trip")
        })
        .collect();
    assert_eq!(trip, vec![20, 21, 22]);
    assert_eq!(day(&st, d(2026, 11, 21)), s(&[("all-day", "Weekend trip")]));
}

#[test]
fn floating_time_uses_home_zone() {
    let st = store();
    assert_eq!(
        day(&st, d(2026, 11, 17)),
        s(&[("09:00", "Floating standup")])
    );
}

#[test]
fn due_todo_appears_on_its_day() {
    let st = store();
    let (inst, _) = st.day(d(2026, 11, 18));
    let rent: Vec<_> = inst.iter().filter(|i| i.kind == Kind::Todo).collect();
    assert_eq!(rent.len(), 1);
    assert_eq!(rent[0].summary, "Pay rent");
}

#[test]
fn todos_sorted_open_first_by_priority() {
    let st = store();
    let todos: Vec<_> = st
        .todos()
        .into_iter()
        .map(|t| (t.summary, t.completed))
        .collect();
    assert_eq!(
        todos,
        vec![
            ("Pay rent".to_string(), false),
            ("Fix bike".to_string(), false),
            ("Old task".to_string(), true),
        ]
    );
}

#[test]
fn unknown_properties_survive_roundtrip() {
    let st = store();
    let lunch = st.objects.iter().find(|o| o.uid == "lunch-1").unwrap();
    let out = lunch.master.to_ical();
    assert!(out.contains("X-PHONE-APP-FIELD;FOO=bar:keep me"));
    let climb = st.objects.iter().find(|o| o.uid == "climb-1").unwrap();
    assert_eq!(climb.overrides.len(), 2);
    assert!(climb.master.to_ical().contains("BEGIN:VALARM"));
}
