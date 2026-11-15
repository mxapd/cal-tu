//! Editing tests: each works on a private copy of the fixture calendars,
//! changes something through the Store API, reloads from disk and checks the
//! result as the user would see it, plus the raw file where it matters.

use std::path::{Path, PathBuf};

use cal_core::{Config, EditError, EventEdit, Moment, NewEvent, NewTodo, Scope, Store, TodoEdit};
use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use chrono_tz::Europe::Stockholm;

struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
    config: Config,
}

impl Env {
    fn new() -> Env {
        let dir = tempfile::tempdir().unwrap();
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/calendars");
        copy_dir(&src, dir.path());
        let config = Config::from_toml(
            r#"
            [calendars.uni]
            read_only = true
            "#,
        )
        .unwrap();
        Env {
            root: dir.path().to_path_buf(),
            _dir: dir,
            config,
        }
    }

    fn store(&self) -> Store {
        Store::load_dir(&self.root, &self.config, Stockholm)
    }

    fn file(&self, rel: &str) -> String {
        std::fs::read_to_string(self.root.join(rel)).unwrap()
    }

    /// Titles and local start times on a day, e.g. `["18:30 Climbing"]`.
    fn day(&self, y: i32, m: u32, d: u32) -> Vec<String> {
        let store = self.store();
        let (items, _) = store.day(NaiveDate::from_ymd_opt(y, m, d).unwrap());
        items
            .iter()
            .map(|i| match i.start {
                Moment::Date(_) => format!("all-day {}", i.summary),
                Moment::DateTime(dt) => {
                    format!(
                        "{} {}",
                        dt.with_timezone(&Stockholm).format("%H:%M"),
                        i.summary
                    )
                }
            })
            .collect()
    }
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

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 16, 10, 32, 0).unwrap()
}

fn at(m: u32, d: u32, h: u32, min: u32) -> Moment {
    Moment::DateTime(Stockholm.with_ymd_and_hms(2026, m, d, h, min, 0).unwrap())
}

fn obj(store: &Store, uid: &str) -> usize {
    store.objects.iter().position(|o| o.uid == uid).unwrap()
}

fn cal(store: &Store, id: &str) -> usize {
    store.calendar_index(id).unwrap()
}

fn edit_error(r: anyhow::Result<()>) -> EditError {
    r.unwrap_err()
        .downcast::<EditError>()
        .expect("an EditError")
}

fn ics_files(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

// --- creating ---------------------------------------------------------------

#[test]
fn create_event_with_reminder_and_timezone() {
    let env = Env::new();
    let store = env.store();
    let uid = store
        .create_event(
            cal(&store, "personal"),
            &NewEvent {
                summary: "Dentist, check-up".into(),
                location: Some("Main St 1".into()),
                description: None,
                start: at(11, 20, 14, 0),
                end: at(11, 20, 15, 0),
                rrule: None,
                reminder: Some(Duration::minutes(15)),
            },
            now(),
        )
        .unwrap();
    assert!(env
        .day(2026, 11, 20)
        .contains(&"14:00 Dentist, check-up".to_string()));

    let text = std::fs::read_to_string(
        env.root
            .join("personal")
            .join(format!("{}.ics", uid.replace('@', "_"))),
    )
    .unwrap();
    for want in [
        "DTSTART;TZID=Europe/Stockholm:20261120T140000\r\n",
        "SUMMARY:Dentist\\, check-up\r\n",
        "BEGIN:VTIMEZONE\r\nTZID:Europe/Stockholm\r\n",
        "TRIGGER:-PT15M\r\n",
        "PRODID:-//cal-tui//",
    ] {
        assert!(text.contains(want), "missing {want:?} in\n{text}");
    }
}

#[test]
fn create_all_day_and_repeating() {
    let env = Env::new();
    let store = env.store();
    let d = |m, day| Moment::Date(NaiveDate::from_ymd_opt(2026, m, day).unwrap());
    store
        .create_event(
            cal(&store, "personal"),
            &NewEvent {
                summary: "Gym".into(),
                location: None,
                description: None,
                start: at(11, 17, 7, 0),
                end: at(11, 17, 8, 0),
                rrule: Some("FREQ=WEEKLY".into()),
                reminder: None,
            },
            now(),
        )
        .unwrap();
    store
        .create_event(
            cal(&store, "personal"),
            &NewEvent {
                summary: "Holiday".into(),
                location: None,
                description: None,
                start: d(12, 1),
                end: d(12, 3),
                rrule: None,
                reminder: None,
            },
            now(),
        )
        .unwrap();
    assert!(env.day(2026, 11, 24).contains(&"07:00 Gym".to_string()));
    assert!(env
        .day(2026, 12, 2)
        .contains(&"all-day Holiday".to_string()));
    assert!(!env
        .day(2026, 12, 3)
        .contains(&"all-day Holiday".to_string()));
}

#[test]
fn create_rejects_bad_input() {
    let env = Env::new();
    let store = env.store();
    let mut ev = NewEvent {
        summary: " ".into(),
        location: None,
        description: None,
        start: at(11, 20, 14, 0),
        end: at(11, 20, 15, 0),
        rrule: None,
        reminder: None,
    };
    let personal = cal(&store, "personal");
    assert!(
        store.create_event(personal, &ev, now()).is_err(),
        "empty title"
    );
    ev.summary = "X".into();
    ev.end = at(11, 20, 13, 0);
    assert!(
        store.create_event(personal, &ev, now()).is_err(),
        "end before start"
    );
    ev.end = at(11, 20, 15, 0);
    ev.rrule = Some("FREQ=SOMETIMES".into());
    assert!(
        store.create_event(personal, &ev, now()).is_err(),
        "bad rule"
    );
}

// --- safety -----------------------------------------------------------------

#[test]
fn read_only_calendar_refuses_every_change() {
    let env = Env::new();
    let store = env.store();
    let before = env.file("uni/lecture.ics");
    let lecture = obj(&store, "040000008200E00074C5B7101A82E008");
    let edit = EventEdit {
        summary: Some("Hacked".into()),
        ..Default::default()
    };
    let e = edit_error(store.update_event(lecture, Scope::All, at(11, 16, 8, 15), &edit, now()));
    assert_eq!(e, EditError::ReadOnly("University".into()));
    assert!(matches!(
        edit_error(store.delete(lecture, Scope::All, now())),
        EditError::ReadOnly(_)
    ));
    let new = NewTodo {
        summary: "x".into(),
        ..Default::default()
    };
    assert!(store.create_todo(cal(&store, "uni"), &new, now()).is_err());
    assert_eq!(env.file("uni/lecture.ics"), before);
    assert_eq!(ics_files(&env.root.join("uni")).len(), 6);
}

#[test]
fn change_on_disk_after_loading_is_a_conflict() {
    let env = Env::new();
    let store = env.store();
    // A sync rewrites the file after we loaded it.
    let synced = env
        .file("personal/lunch.ics")
        .replace("Café Lisa", "Café Nero");
    std::fs::write(env.root.join("personal/lunch.ics"), &synced).unwrap();

    let edit = EventEdit {
        summary: Some("Lunch".into()),
        ..Default::default()
    };
    let lunch = obj(&store, "lunch-1");
    let e = edit_error(store.update_event(lunch, Scope::All, at(11, 16, 12, 0), &edit, now()));
    assert!(matches!(e, EditError::Conflict(_)), "{e}");
    assert_eq!(
        env.file("personal/lunch.ics"),
        synced,
        "their change is kept"
    );

    // Deleted by a sync: also a conflict, nothing recreated.
    std::fs::remove_file(env.root.join("personal/lunch.ics")).unwrap();
    let e = edit_error(store.delete(lunch, Scope::All, now()));
    assert!(matches!(e, EditError::Conflict(_)));
    assert!(!env.root.join("personal/lunch.ics").exists());
}

#[test]
fn file_with_syntax_problems_is_not_rewritten() {
    let env = Env::new();
    let path = env.root.join("personal/odd.ics");
    let text = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:odd-1\r\nDTSTART:20261116T090000\r\n\
                SUMMARY:Odd\r\nthis line is garbage\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    std::fs::write(&path, text).unwrap();
    let store = env.store();
    let odd = obj(&store, "odd-1");
    assert!(
        env.day(2026, 11, 16).contains(&"09:00 Odd".to_string()),
        "still shown"
    );
    assert!(matches!(
        edit_error(store.can_edit(odd)),
        EditError::UnsafeFile(..)
    ));
    let edit = EventEdit {
        summary: Some("Fixed".into()),
        ..Default::default()
    };
    assert!(store
        .update_event(odd, Scope::All, at(11, 16, 9, 0), &edit, now())
        .is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
}

// --- editing events -----------------------------------------------------------

#[test]
fn edit_keeps_unknown_properties_and_bumps_sequence() {
    let env = Env::new();
    let store = env.store();
    let edit = EventEdit {
        summary: Some("Lunch with Anna & Bo".into()),
        location: Some(None),
        ..Default::default()
    };
    store
        .update_event(
            obj(&store, "lunch-1"),
            Scope::All,
            at(11, 16, 12, 0),
            &edit,
            now(),
        )
        .unwrap();
    let text = env.file("personal/lunch.ics");
    assert!(
        text.contains("X-PHONE-APP-FIELD;FOO=bar:keep me\r\n"),
        "{text}"
    );
    assert!(
        text.contains("PRODID:-//test//EN\r\n"),
        "other client's PRODID kept"
    );
    assert!(text.contains("SUMMARY:Lunch with Anna & Bo\r\n"));
    assert!(!text.contains("LOCATION"));
    assert!(
        text.contains("SEQUENCE:1\r\n"),
        "missing SEQUENCE counts as 0"
    );
    assert!(text.contains("DTSTAMP:20261116T103200Z\r\n"));
    // Time untouched.
    assert!(text.contains("DTSTART;TZID=Europe/Stockholm:20261116T120000\r\n"));
}

#[test]
fn move_single_event_to_another_day() {
    let env = Env::new();
    let store = env.store();
    let edit = EventEdit {
        time: Some((at(11, 18, 12, 30), at(11, 18, 13, 30))),
        ..Default::default()
    };
    store
        .update_event(
            obj(&store, "lunch-1"),
            Scope::All,
            at(11, 16, 12, 0),
            &edit,
            now(),
        )
        .unwrap();
    assert!(!env.day(2026, 11, 16).iter().any(|s| s.contains("Lunch")));
    assert!(env
        .day(2026, 11, 18)
        .contains(&"12:30 Lunch with Anna".to_string()));
}

#[test]
fn edit_one_occurrence_creates_an_override() {
    let env = Env::new();
    let store = env.store();
    let climb = obj(&store, "climb-1");
    let edit = EventEdit {
        summary: Some("Climbing with Sam".into()),
        time: Some((at(11, 16, 19, 0), at(11, 16, 21, 0))),
        ..Default::default()
    };
    store
        .update_event(
            climb,
            Scope::Occurrence(at(11, 16, 18, 30)),
            at(11, 16, 18, 30),
            &edit,
            now(),
        )
        .unwrap();
    assert!(env
        .day(2026, 11, 16)
        .contains(&"19:00 Climbing with Sam".to_string()));
    assert!(
        env.day(2026, 11, 18)
            .contains(&"18:30 Climbing".to_string()),
        "others unchanged"
    );
    assert!(env
        .day(2026, 11, 23)
        .contains(&"18:30 Climbing".to_string()));
    let text = env.file("personal/climbing.ics");
    assert!(text.contains("RECURRENCE-ID;TZID=Europe/Stockholm:20261116T183000\r\n"));
    assert_eq!(
        text.matches("BEGIN:VALARM").count(),
        2,
        "override keeps the alarm"
    );
    assert_eq!(
        text.matches("RRULE:FREQ=WEEKLY").count(),
        1,
        "override has no RRULE"
    );

    // Editing it again updates the same override.
    let store = env.store();
    let edit = EventEdit {
        location: Some(Some("Klätterverket".into())),
        ..Default::default()
    };
    store
        .update_event(
            obj(&store, "climb-1"),
            Scope::Occurrence(at(11, 16, 18, 30)),
            at(11, 16, 19, 0),
            &edit,
            now(),
        )
        .unwrap();
    let text = env.file("personal/climbing.ics");
    assert_eq!(
        text.matches("RECURRENCE-ID;TZID=Europe/Stockholm:20261116T183000")
            .count(),
        1
    );
    assert!(text.contains("LOCATION:Klätterverket"));
    assert!(env
        .day(2026, 11, 16)
        .contains(&"19:00 Climbing with Sam".to_string()));
}

#[test]
fn delete_one_occurrence_adds_exdate() {
    let env = Env::new();
    let store = env.store();
    store
        .delete(
            obj(&store, "climb-1"),
            Scope::Occurrence(at(11, 18, 18, 30)),
            now(),
        )
        .unwrap();
    assert!(!env.day(2026, 11, 18).iter().any(|s| s.contains("Climbing")));
    assert!(env
        .day(2026, 11, 16)
        .contains(&"18:30 Climbing".to_string()));
    assert!(env
        .day(2026, 11, 23)
        .contains(&"18:30 Climbing".to_string()));
    assert!(env
        .file("personal/climbing.ics")
        .contains("EXDATE;TZID=Europe/Stockholm:20261118T183000\r\n"));

    // Deleting the moved occurrence (28 → 29 Oct) removes its override too.
    let store = env.store();
    store
        .delete(
            obj(&store, "climb-1"),
            Scope::Occurrence(at(10, 28, 18, 30)),
            now(),
        )
        .unwrap();
    assert!(
        env.day(2026, 10, 29).is_empty()
            || !env.day(2026, 10, 29).iter().any(|s| s.contains("Climbing"))
    );
    assert!(!env
        .file("personal/climbing.ics")
        .contains("Climbing (moved)"));
}

#[test]
fn series_time_change_moves_exceptions_along() {
    let env = Env::new();
    let store = env.store();
    // Seen on Mon 16 Nov 18:30–20:00; move the series to 19:00–20:30.
    let edit = EventEdit {
        time: Some((at(11, 16, 19, 0), at(11, 16, 20, 30))),
        ..Default::default()
    };
    store
        .update_event(
            obj(&store, "climb-1"),
            Scope::All,
            at(11, 16, 18, 30),
            &edit,
            now(),
        )
        .unwrap();
    assert!(env
        .day(2026, 11, 16)
        .contains(&"19:00 Climbing".to_string()));
    // Before the DST change too: same wall-clock time.
    assert!(env
        .day(2026, 10, 19)
        .contains(&"19:00 Climbing".to_string()));
    // EXDATE on 21 Oct still excludes.
    assert!(!env.day(2026, 10, 21).iter().any(|s| s.contains("Climbing")));
    // The cancelled 4 Nov occurrence stays cancelled.
    assert!(!env.day(2026, 11, 4).iter().any(|s| s.contains("Climbing")));
    // The user-moved occurrence keeps its own time and day.
    assert_eq!(
        env.day(2026, 10, 29),
        vec!["19:00 Climbing (moved)".to_string()]
    );
    assert!(!env.day(2026, 10, 28).iter().any(|s| s.contains("Climbing")));
}

#[test]
fn series_on_fixed_weekdays_cannot_move_to_another_day() {
    let env = Env::new();
    let store = env.store();
    let before = env.file("personal/climbing.ics");
    let edit = EventEdit {
        time: Some((at(11, 17, 18, 30), at(11, 17, 20, 0))),
        ..Default::default()
    };
    let e = edit_error(store.update_event(
        obj(&store, "climb-1"),
        Scope::All,
        at(11, 16, 18, 30),
        &edit,
        now(),
    ));
    assert!(matches!(e, EditError::Invalid(_)), "{e}");
    assert_eq!(env.file("personal/climbing.ics"), before);

    // With a new rule it is fine.
    let edit = EventEdit {
        time: Some((at(11, 17, 18, 30), at(11, 17, 20, 0))),
        rrule: Some(Some("FREQ=WEEKLY;BYDAY=TU,TH".into())),
        ..Default::default()
    };
    store
        .update_event(
            obj(&store, "climb-1"),
            Scope::All,
            at(11, 16, 18, 30),
            &edit,
            now(),
        )
        .unwrap();
    assert!(env
        .day(2026, 11, 17)
        .contains(&"18:30 Climbing".to_string()));
    assert!(env
        .day(2026, 11, 19)
        .contains(&"18:30 Climbing".to_string()));
    assert!(!env.day(2026, 11, 16).iter().any(|s| s.contains("Climbing")));
}

#[test]
fn series_title_change_reaches_unmodified_overrides_only() {
    let env = Env::new();
    let store = env.store();
    let edit = EventEdit {
        summary: Some("Bouldering".into()),
        ..Default::default()
    };
    store
        .update_event(
            obj(&store, "climb-1"),
            Scope::All,
            at(11, 16, 18, 30),
            &edit,
            now(),
        )
        .unwrap();
    assert!(env
        .day(2026, 11, 16)
        .contains(&"18:30 Bouldering".to_string()));
    assert_eq!(
        env.day(2026, 10, 29),
        vec!["19:00 Climbing (moved)".to_string()]
    );
    let text = env.file("personal/climbing.ics");
    assert_eq!(
        text.matches("SUMMARY:Bouldering").count(),
        2,
        "master + cancelled override\n{text}"
    );
    assert!(
        text.contains("DESCRIPTION:Climbing"),
        "alarm text untouched"
    );
}

#[test]
fn stop_repeating_drops_exceptions() {
    let env = Env::new();
    let store = env.store();
    let edit = EventEdit {
        rrule: Some(None),
        ..Default::default()
    };
    store
        .update_event(
            obj(&store, "climb-1"),
            Scope::All,
            at(11, 16, 18, 30),
            &edit,
            now(),
        )
        .unwrap();
    let text = env.file("personal/climbing.ics");
    assert!(
        !text.contains("RRULE:FREQ=WEEKLY")
            && !text.contains("EXDATE")
            && !text.contains("RECURRENCE-ID"),
        "{text}"
    );
    // The occurrence the user was looking at is the one that remains.
    assert!(env
        .day(2026, 11, 16)
        .contains(&"18:30 Climbing".to_string()));
    assert!(!env.day(2026, 11, 18).iter().any(|s| s.contains("Climbing")));
    assert!(!env.day(2026, 10, 5).iter().any(|s| s.contains("Climbing")));
}

#[test]
fn delete_whole_item_removes_the_file() {
    let env = Env::new();
    let store = env.store();
    store
        .delete(obj(&store, "climb-1"), Scope::All, now())
        .unwrap();
    assert!(!env.root.join("personal/climbing.ics").exists());
    assert!(!env.day(2026, 11, 16).iter().any(|s| s.contains("Climbing")));
}

#[test]
fn occurrence_edit_keeps_outlook_timezone() {
    // Make the university calendar writable for this test.
    let mut env = Env::new();
    env.config = Config::default();
    let store = env.store();
    let lecture = obj(&store, "040000008200E00074C5B7101A82E008");
    let edit = EventEdit {
        location: Some(Some("Hall B".into())),
        ..Default::default()
    };
    store
        .update_event(
            lecture,
            Scope::Occurrence(at(11, 23, 8, 15)),
            at(11, 23, 8, 15),
            &edit,
            now(),
        )
        .unwrap();
    let text = env.file("uni/lecture.ics");
    assert!(
        text.contains("RECURRENCE-ID;TZID=W. Europe Standard Time:20261123T081500\r\n"),
        "{text}"
    );
    assert!(
        text.contains("SUMMARY;LANGUAGE=en-US:Linear Algebra\\, lecture"),
        "params kept"
    );
    assert!(
        !text.contains("TZID:Europe/Stockholm"),
        "no extra VTIMEZONE needed"
    );
    let store = env.store();
    let (items, _) = store.day(NaiveDate::from_ymd_opt(2026, 11, 23).unwrap());
    assert_eq!(items[0].location.as_deref(), Some("Hall B"));
    let (items, _) = store.day(NaiveDate::from_ymd_opt(2026, 11, 30).unwrap());
    assert_eq!(items[0].location.as_deref(), Some("Hall A"));
}

// --- todos ----------------------------------------------------------------------

#[test]
fn todo_lifecycle() {
    let env = Env::new();
    let store = env.store();
    let personal = cal(&store, "personal");
    let uid = store
        .create_todo(
            personal,
            &NewTodo {
                summary: "Buy milk".into(),
                due: Some(Moment::Date(NaiveDate::from_ymd_opt(2026, 11, 17).unwrap())),
                priority: 3,
                ..Default::default()
            },
            now(),
        )
        .unwrap();

    let store = env.store();
    let t = store.todos().into_iter().find(|t| t.uid == uid).unwrap();
    assert_eq!((t.priority, t.completed), (3, false));
    assert!(env.day(2026, 11, 17).iter().any(|s| s.contains("Buy milk")));

    let o = obj(&store, &uid);
    store
        .update_todo(
            o,
            &TodoEdit {
                summary: Some("Buy oat milk".into()),
                due: Some(None),
                priority: Some(0),
                ..Default::default()
            },
            now(),
        )
        .unwrap();
    let store = env.store();
    let t = store.todos().into_iter().find(|t| t.uid == uid).unwrap();
    assert_eq!(
        (t.summary.as_str(), t.priority, t.due),
        ("Buy oat milk", 0, None)
    );

    store.set_todo_completed(o, true, now()).unwrap();
    let store = env.store();
    assert!(
        store
            .todos()
            .into_iter()
            .find(|t| t.uid == uid)
            .unwrap()
            .completed
    );
    store.set_todo_completed(o, false, now()).unwrap();
    let store = env.store();
    assert!(
        !store
            .todos()
            .into_iter()
            .find(|t| t.uid == uid)
            .unwrap()
            .completed
    );

    store.delete(o, Scope::All, now()).unwrap();
    assert!(env.store().todos().iter().all(|t| t.uid != uid));
}

#[test]
fn completing_keeps_other_todo_fields() {
    let env = Env::new();
    let store = env.store();
    store
        .set_todo_completed(obj(&store, "todo-rent"), true, now())
        .unwrap();
    let text = env.file("personal/rent.ics");
    for want in [
        "STATUS:COMPLETED\r\n",
        "COMPLETED:20261116T103200Z\r\n",
        "PERCENT-COMPLETE:100\r\n",
        "DUE;VALUE=DATE:20261118\r\n",
        "PRIORITY:1\r\n",
    ] {
        assert!(text.contains(want), "missing {want:?}:\n{text}");
    }
    assert_eq!(text.matches("STATUS").count(), 1);
}
