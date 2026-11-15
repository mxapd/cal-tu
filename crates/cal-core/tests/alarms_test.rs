//! Which reminders fire when.

use std::path::Path;

use cal_core::{Config, Store};
use chrono::{DateTime, TimeZone};
use chrono_tz::{Europe::Stockholm, Tz};

fn at(m: u32, d: u32, h: u32, min: u32) -> DateTime<Tz> {
    Stockholm.with_ymd_and_hms(2026, m, d, h, min, 0).unwrap()
}

fn copy_dir(from: &Path, to: &Path) {
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let target = to.join(e.file_name());
        if e.path().is_dir() {
            std::fs::create_dir_all(&target).unwrap();
            copy_dir(&e.path(), &target);
        } else {
            std::fs::copy(e.path(), &target).unwrap();
        }
    }
}

const EXTRA: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
BEGIN:VEVENT\r\nUID:special\r\nDTSTART;TZID=Europe/Stockholm:20261117T100000\r\n\
DTEND;TZID=Europe/Stockholm:20261117T120000\r\nSUMMARY:Special\r\n\
BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER;VALUE=DATE-TIME:20261116T200000Z\r\nEND:VALARM\r\n\
BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER;RELATED=END:-PT10M\r\nEND:VALARM\r\n\
BEGIN:VALARM\r\nACTION:AUDIO\r\nTRIGGER:-PT1H\r\nREPEAT:2\r\nDURATION:PT5M\r\nEND:VALARM\r\n\
BEGIN:VALARM\r\nACTION:EMAIL\r\nTRIGGER:-PT2H\r\nEND:VALARM\r\n\
BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT3H\r\nACKNOWLEDGED:20261117T060000Z\r\nEND:VALARM\r\n\
END:VEVENT\r\n\
BEGIN:VTODO\r\nUID:todo-alarm\r\nSUMMARY:Submit report\r\nDUE;TZID=Europe/Stockholm:20261117T170000\r\n\
BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER;RELATED=END:-PT30M\r\nEND:VALARM\r\nEND:VTODO\r\n\
END:VCALENDAR\r\n";

fn store(config: &str) -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/calendars"),
        dir.path(),
    );
    std::fs::write(dir.path().join("personal/special.ics"), EXTRA).unwrap();
    let config = Config::from_toml(config).unwrap();
    let store = Store::load_dir(dir.path(), &config, Stockholm);
    (dir, store)
}

/// `"HH:MM summary"` for every alarm in the window, in local time.
fn list(store: &Store, from: DateTime<Tz>, to: DateTime<Tz>) -> Vec<String> {
    let (alarms, _) = store.alarms(from, to);
    alarms
        .iter()
        .map(|a| {
            format!(
                "{} {}",
                a.at.with_timezone(&Stockholm).format("%m-%d %H:%M"),
                a.summary
            )
        })
        .collect()
}

#[test]
fn series_alarms_follow_exceptions_and_overrides() {
    let (_d, s) = store("");
    let got = list(&s, at(10, 19, 0, 0), at(11, 6, 0, 0));
    let climbing: Vec<_> = got
        .iter()
        .filter(|l| l.contains("Climbing"))
        .cloned()
        .collect();
    assert_eq!(
        climbing,
        vec![
            "10-19 18:00 Climbing",
            // 21 Oct: EXDATE
            "10-26 18:00 Climbing",
            // 28 Oct moved to 29 Oct 19:00; the override has no alarm of
            // its own, so the series' 30 minutes apply.
            "10-29 18:30 Climbing (moved)",
            "11-02 18:00 Climbing",
            // 4 Nov: cancelled
        ]
    );
}

#[test]
fn trigger_kinds() {
    let (_d, s) = store("");
    let got = list(&s, at(11, 16, 12, 0), at(11, 18, 0, 0));
    let special: Vec<_> = got
        .iter()
        .filter(|l| l.contains("Special") || l.contains("Submit"))
        .cloned()
        .collect();
    assert_eq!(
        special,
        vec![
            "11-16 21:00 Special", // absolute, 20:00Z
            // 07:00 (-3h) was acknowledged; 08:00 email skipped
            "11-17 09:00 Special", // -1h, audio …
            "11-17 09:05 Special", // … repeated twice, 5 min apart
            "11-17 09:10 Special",
            "11-17 11:50 Special", // 10 min before the end
            "11-17 16:30 Submit report",
        ],
        "{got:#?}"
    );
}

#[test]
fn default_reminder_for_timed_events_without_alarms() {
    let (_d, s) = store(
        r#"
        [calendars.uni]
        default_reminder = "15m"
        "#,
    );
    let got = list(&s, at(11, 16, 0, 0), at(11, 17, 0, 0));
    assert!(
        got.contains(&"11-16 08:00 Linear Algebra, lecture".to_string()),
        "{got:#?}"
    );
    // Lunch (personal, no alarm, no default) has none.
    assert!(!got.iter().any(|l| l.contains("Lunch")));
    let (alarms, _) = s.alarms(at(11, 16, 0, 0), at(11, 17, 0, 0));
    assert!(
        alarms
            .iter()
            .find(|a| a.summary.starts_with("Linear"))
            .unwrap()
            .default
    );
}

#[test]
fn hidden_and_filtered_calendars_are_silent() {
    let (_d, s) = store(
        r#"
        [calendars.personal]
        hidden = true
        [calendars.uni]
        default_reminder = "15m"
        hide_matching = ["^Linear"]
        "#,
    );
    let got = list(&s, at(10, 1, 0, 0), at(12, 1, 0, 0));
    assert!(
        got.iter()
            .all(|l| !l.contains("Climbing") && !l.contains("Linear")),
        "{got:#?}"
    );
}

#[test]
fn keys_are_unique_and_stable() {
    let (_d, s) = store("");
    let (a, _) = s.alarms(at(10, 1, 0, 0), at(12, 1, 0, 0));
    let (b, _) = s.alarms(at(10, 1, 0, 0), at(12, 1, 0, 0));
    assert_eq!(a, b);
    let mut keys: Vec<_> = a.iter().map(|x| x.key.clone()).collect();
    let n = keys.len();
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), n);
    assert!(
        a[0].key.starts_with("personal|climb-1|20261005T163000Z|"),
        "{}",
        a[0].key
    );
}

#[test]
fn completed_todos_are_silent() {
    let (dir, _) = store("");
    let p = dir.path().join("personal/special.ics");
    let text = std::fs::read_to_string(&p).unwrap().replace(
        "SUMMARY:Submit report\r\n",
        "SUMMARY:Submit report\r\nSTATUS:COMPLETED\r\n",
    );
    std::fs::write(&p, text).unwrap();
    let s = Store::load_dir(dir.path(), &Config::default(), Stockholm);
    assert!(!list(&s, at(11, 16, 0, 0), at(11, 18, 0, 0))
        .iter()
        .any(|l| l.contains("Submit")));
}
