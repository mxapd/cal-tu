//! `cal-tui sync`: the real binary on a writable copy of the fixtures, with
//! its own state dir, printing the journal entries instead of posting them.

#[path = "support/snapshot.rs"]
mod snapshot;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use snapshot::assert_snapshot;

const NOW: &str = "2026-11-16T11:32";

struct Env {
    calendars: tempfile::TempDir,
    dir: tempfile::TempDir,
}

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

/// Fixtures and a config whose `sync_command` is `sync`.
fn setup(sync: &str, jarvis: &str) -> Env {
    let fixtures =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../cal-core/tests/fixtures/calendars");
    let calendars = tempfile::tempdir().unwrap();
    copy(&fixtures, calendars.path());
    let dir = tempfile::tempdir().unwrap();
    let env = Env { calendars, dir };
    env.config(sync, jarvis);
    env
}

impl Env {
    fn config(&self, sync: &str, jarvis: &str) {
        std::fs::write(
            self.dir.path().join("config.toml"),
            format!(
                "timezone = \"Europe/Stockholm\"\nsync_command = {sync:?}\n\
                 [calendars.uni]\nread_only = true\n[jarvis]\n{jarvis}\n"
            ),
        )
        .unwrap();
    }

    fn cmd(&self, now: &str, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_cal-tui"));
        c.arg("--config")
            .arg(self.dir.path().join("config.toml"))
            .arg("--calendars-dir")
            .arg(self.calendars.path())
            .args(["--now", now])
            .args(args)
            .env("XDG_STATE_HOME", self.dir.path().join("state"))
            .env_remove("JARVIS_URL")
            .current_dir(self.dir.path());
        c
    }

    fn run_at(&self, now: &str, args: &[&str]) -> Output {
        self.cmd(now, args).output().unwrap()
    }

    /// `sync --print`: (exit code, the entries printed).
    fn sync(&self, now: &str) -> (i32, String) {
        let out = self.run_at(now, &["sync", "--print"]);
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8(out.stdout).unwrap(),
        )
    }

    fn ok(&self, args: &[&str]) {
        let out = self.run_at(NOW, args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn file(&self, cal: &str, name: &str) -> PathBuf {
        self.calendars.path().join(cal).join(name)
    }
}

#[test]
fn reports_what_changed_since_the_last_sync() {
    let e = setup("true", "");
    // The first sync only remembers what is there.
    assert_eq!(e.sync(NOW), (0, String::new()));

    e.ok(&["add", "Dentist", "fri", "14:00", "1h", "-l", "Main St"]);
    e.ok(&["add", "Gym", "tue", "7:00", "--repeat", "weekly"]);
    e.ok(&["todo", "done", "rent"]);
    e.ok(&["todo", "add", "Call", "bank", "--due", "thu"]);
    // Lunch moves from 12:00 to 14:00 (as a sync from the phone would).
    let lunch = e.file("personal", "lunch.ics");
    let text = std::fs::read_to_string(&lunch).unwrap();
    std::fs::write(
        &lunch,
        text.replace("T120000", "T140000")
            .replace("T130000", "T150000"),
    )
    .unwrap();
    // The bike todo is deleted elsewhere.
    std::fs::remove_file(e.file("personal", "bike.ics")).unwrap();

    let (code, entry) = e.sync(NOW);
    assert_eq!(code, 0);
    assert_snapshot("sync_changes", &entry);
    // Nothing new since: nothing posted.
    assert_eq!(e.sync(NOW), (0, String::new()));
}

#[test]
fn the_window_rolling_on_is_not_news() {
    let e = setup("true", "");
    e.sync(NOW);
    // A month later, past events have left the window and new days have
    // come into it; none of that is a change.
    assert_eq!(e.sync("2026-12-20T09:00"), (0, String::new()));
}

#[test]
fn failures_alert_once_and_recovery_is_reported() {
    let e = setup(
        "echo error: cannot reach the server >&2; echo Use -vdebug for more >&2; false",
        "fail_after = 2",
    );
    let (code, out) = e.sync(NOW);
    assert_eq!(
        (code, out.as_str()),
        (0, ""),
        "the first failure is retried quietly"
    );
    let out = e.run_at(NOW, &["sync", "--print"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "the second in a row is the alert"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("failed 2 times in a row"), "{err}");
    assert!(err.contains("cannot reach the server"), "{err}");
    assert_eq!(
        e.sync(NOW).0,
        0,
        "an outage is reported once, not on every run"
    );

    e.config("true", "fail_after = 2");
    let (code, out) = e.sync(NOW);
    assert_eq!(code, 0);
    assert_eq!(
        out,
        "tags: resolved\n\nCalendar sync works again, after 3 failed attempts.\n"
    );
    assert_eq!(e.sync(NOW), (0, String::new()));
}

#[test]
fn syncs_take_turns() {
    // Two syncs at once (the timer and the TUI's S, say) must not run the
    // sync command side by side.
    let e = setup("echo start >> log; sleep 0.4; echo end >> log", "");
    let a = e.cmd(NOW, &["sync", "--print"]).spawn().unwrap();
    let b = e.cmd(NOW, &["sync", "--print"]).spawn().unwrap();
    for mut c in [a, b] {
        assert!(c.wait().unwrap().success());
    }
    let log = std::fs::read_to_string(e.dir.path().join("log")).unwrap();
    assert_eq!(log, "start\nend\nstart\nend\n");
}

#[test]
fn no_jarvis_section_means_no_journal() {
    // A plain standalone config: sync works, and nothing tries to post.
    let e = setup("true", "");
    std::fs::write(
        e.dir.path().join("config.toml"),
        "timezone = \"Europe/Stockholm\"\nsync_command = \"true\"\n",
    )
    .unwrap();
    let out = e.run_at(NOW, &["sync"]);
    assert!(out.status.success());
    e.ok(&["add", "Dentist", "fri", "14:00"]);
    let out = e.run_at(NOW, &["sync"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!String::from_utf8_lossy(&out.stderr).contains("journal"));
}
