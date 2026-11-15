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
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    std::fs::write(&cfg, config).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_cal-tui"))
        .arg("--config")
        .arg(&cfg)
        .arg("--calendars-dir")
        .arg(fixtures())
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
            .replace(&fixtures().display().to_string(), "<fixtures>")
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
