//! End-to-end tests of the shell commands: run the real binary against the
//! fixture calendars at a fixed time and compare the full output.

#[path = "support/snapshot.rs"]
mod snapshot;

use std::path::{Path, PathBuf};
use std::process::Command;

use snapshot::assert_snapshot;

const NOW: &str = "2026-11-16T11:32";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../cal-core/tests/fixtures/calendars")
}

const CONFIG: &str = r#"
timezone = "Europe/Stockholm"
[calendars.uni]
read_only = true
hide_matching = ["^Tutorial group B"]
"#;

struct Out {
    status: i32,
    stdout: String,
    stderr: String,
}

fn run_with(config: &str, args: &[&str]) -> Out {
    run_in(&fixtures(), config, args)
}

fn run_in(calendars: &Path, config: &str, args: &[&str]) -> Out {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    std::fs::write(&cfg, config).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_cal-tui"))
        .arg("--config")
        .arg(&cfg)
        .arg("--calendars-dir")
        .arg(calendars)
        .arg("--now")
        .arg(NOW)
        .args(args)
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    Out {
        status: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8(out.stdout).unwrap(),
        // Paths in messages differ per machine; keep only file names.
        stderr: String::from_utf8(out.stderr)
            .unwrap()
            .replace(&calendars.display().to_string(), "<calendars>")
            .replace(&cfg.display().to_string(), "<config>"),
    }
}

fn run(args: &[&str]) -> Out {
    run_with(CONFIG, args)
}

/// Run, require success, snapshot stdout.
fn check(name: &str, args: &[&str]) -> Out {
    let out = run(args);
    assert_eq!(out.status, 0, "{args:?} failed:\n{}", out.stderr);
    assert_snapshot(name, &out.stdout);
    out
}

#[test]
fn today() {
    let out = check("cli_today", &["today"]);
    // broken.ics is reported on stderr, not mixed into the output.
    assert!(out.stderr.contains("warning:"), "{}", out.stderr);
}

#[test]
fn agenda_two_weeks() {
    check("cli_agenda_14", &["agenda", "--days", "14"]);
}

#[test]
fn agenda_around_overrides() {
    // Moved (28→29 Oct), excluded (21 Oct) and cancelled (4 Nov) climbing.
    check(
        "cli_agenda_overrides",
        &["agenda", "--from", "2026-10-19", "--days", "18"],
    );
}

#[test]
fn agenda_one_calendar() {
    check(
        "cli_agenda_uni",
        &["agenda", "--days", "7", "-c", "University"],
    );
}

#[test]
fn agenda_json() {
    let out = check("cli_agenda_json", &["agenda", "--days", "2", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert!(v.as_array().unwrap().len() >= 3);
}

#[test]
fn next_formats() {
    check("cli_next", &["next"]);
    check(
        "cli_next_format",
        &["next", "--format", "{summary}|{calendar}|{location}|{in}"],
    );
    check("cli_next_json", &["next", "--json"]);
}

#[test]
fn todos() {
    check("cli_todo", &["todo"]);
    check("cli_todo_all", &["todo", "--all"]);
    check("cli_todo_json", &["todo", "--all", "--json"]);
}

#[test]
fn verbose_lists_each_warning() {
    let out = run(&["-v", "today"]);
    assert!(out.stderr.contains("broken.ics"), "{}", out.stderr);
}

#[test]
fn unknown_calendar_is_an_error() {
    let out = run(&["agenda", "-c", "nope"]);
    assert_ne!(out.status, 0);
    assert!(
        out.stderr.contains("unknown calendar 'nope'"),
        "{}",
        out.stderr
    );
}

#[test]
fn check_config_accepts_good_config() {
    let out = run(&["check-config"]);
    assert_eq!(out.status, 0, "{}", out.stderr);
}

#[test]
fn check_config_reports_every_problem() {
    let out = run_with(
        r#"
        timezone = "Mars/Olympus"
        [calendars.uni]
        color = "chartreuse-ish"
        default_reminder = "soon"
        "#,
        &["check-config"],
    );
    assert_eq!(out.status, 1);
    assert_snapshot("cli_check_config_bad", &out.stderr);
}

#[test]
fn unknown_config_key_is_rejected() {
    let out = run_with("colour = \"red\"\n", &["check-config"]);
    assert_ne!(out.status, 0);
    assert!(
        out.stderr.contains("unknown field `colour`"),
        "{}",
        out.stderr
    );
}

// --- commands that write --------------------------------------------------------

/// A writable copy of the fixture calendars.
fn writable_copy() -> tempfile::TempDir {
    fn copy(from: &Path, to: &Path) {
        for e in std::fs::read_dir(from).unwrap() {
            let e = e.unwrap();
            let target = to.join(e.file_name());
            if e.path().is_dir() {
                std::fs::create_dir_all(&target).unwrap();
                copy(&e.path(), &target);
            } else {
                std::fs::copy(e.path(), &target).unwrap();
            }
        }
    }
    let dir = tempfile::tempdir().unwrap();
    copy(&fixtures(), dir.path());
    dir
}

fn count_ics(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|x| x == "ics")
        })
        .count()
}

#[test]
fn add_event_and_see_it_in_the_agenda() {
    let cals = writable_copy();
    let out = run_in(
        cals.path(),
        CONFIG,
        &[
            "add",
            "Dentist",
            "fri",
            "14:00",
            "1h",
            "-l",
            "Main St 1",
            "-r",
            "15m",
        ],
    );
    assert_eq!(out.status, 0, "{}", out.stderr);
    assert_eq!(
        out.stdout,
        "Added: Fri 20 Nov 2026 14:00-15:00 Dentist · Main St 1  [personal], reminder 15m before\n"
    );
    let out = run_in(
        cals.path(),
        CONFIG,
        &["agenda", "--from", "2026-11-20", "--days", "1"],
    );
    assert!(
        out.stdout
            .contains("14:00-15:00  Dentist · Main St 1  [personal]"),
        "{}",
        out.stdout
    );
}

#[test]
fn add_all_day_repeating_and_dry_run() {
    let cals = writable_copy();
    let before = count_ics(&cals.path().join("personal"));
    let out = run_in(
        cals.path(),
        CONFIG,
        &["add", "-n", "Conference", "12-01", "3d"],
    );
    assert_eq!(
        out.stdout,
        "Would add: Tue 01 Dec – Thu 03 Dec 2026 Conference  [personal]\n"
    );
    assert_eq!(
        count_ics(&cals.path().join("personal")),
        before,
        "dry run writes nothing"
    );

    let out = run_in(
        cals.path(),
        CONFIG,
        &["add", "Gym", "tue", "7:00", "--repeat", "weekly"],
    );
    assert_eq!(out.status, 0, "{}", out.stderr);
    assert!(out.stdout.contains("repeats weekly"), "{}", out.stdout);
    let out = run_in(
        cals.path(),
        CONFIG,
        &["agenda", "--from", "2026-12-01", "--days", "1"],
    );
    assert!(out.stdout.contains("07:00-08:00  Gym"), "{}", out.stdout);
}

#[test]
fn add_to_read_only_calendar_fails() {
    let cals = writable_copy();
    let out = run_in(cals.path(), CONFIG, &["add", "Party", "fri", "-c", "uni"]);
    assert_ne!(out.status, 0);
    assert!(
        out.stderr.contains("calendar 'University' is read-only"),
        "{}",
        out.stderr
    );
    assert_eq!(count_ics(&cals.path().join("uni")), 4);
}

#[test]
fn add_uses_default_calendar_from_config() {
    let cals = writable_copy();
    std::fs::create_dir(cals.path().join("work")).unwrap();
    // Top-level keys go before the first table.
    let config = format!("default_calendar = \"work\"\n{CONFIG}");
    let out = run_in(cals.path(), &config, &["add", "Standup", "9:00"]);
    assert_eq!(out.status, 0, "{}", out.stderr);
    assert!(out.stdout.contains("[work]"), "{}", out.stdout);
    assert_eq!(count_ics(&cals.path().join("work")), 1);
}

#[test]
fn todo_add_done_undone() {
    let cals = writable_copy();
    let out = run_in(
        cals.path(),
        CONFIG,
        &["todo", "add", "Buy", "milk", "--due", "tomorrow", "-p", "2"],
    );
    assert_eq!(out.status, 0, "{}", out.stderr);
    assert_eq!(
        out.stdout,
        "Added todo: Buy milk (priority 2), due Tue 17 Nov 2026  [personal]\n"
    );

    let list = run_in(cals.path(), CONFIG, &["todo"]).stdout;
    assert!(list.contains("[ ] 2 Buy milk  (due Tue 17 Nov)"), "{list}");

    let out = run_in(cals.path(), CONFIG, &["todo", "done", "milk"]);
    assert_eq!(out.stdout, "Done: Buy milk\n", "{}", out.stderr);
    assert!(!run_in(cals.path(), CONFIG, &["todo"])
        .stdout
        .contains("Buy milk"));

    let out = run_in(cals.path(), CONFIG, &["todo", "undone", "buy milk"]);
    assert_eq!(out.stdout, "Not done: Buy milk\n", "{}", out.stderr);
    assert!(run_in(cals.path(), CONFIG, &["todo"])
        .stdout
        .contains("Buy milk"));
}

#[test]
fn todo_done_needs_a_unique_match() {
    let cals = writable_copy();
    run_in(cals.path(), CONFIG, &["todo", "add", "Fix", "car"]);
    let out = run_in(cals.path(), CONFIG, &["todo", "done", "fix"]);
    assert_ne!(out.status, 0);
    assert!(out.stderr.contains("matches 2 todos"), "{}", out.stderr);
    let out = run_in(cals.path(), CONFIG, &["todo", "done", "nothing like this"]);
    assert!(
        out.stderr.contains("no open todo matches"),
        "{}",
        out.stderr
    );
}
