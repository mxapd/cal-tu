//! `cal-tui serve`: the real binary on a free port, against a writable copy of
//! the fixture calendars at a fixed time, asked over HTTP as Jarvis asks it.

#[path = "support/snapshot.rs"]
mod snapshot;

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};

use serde_json::{json, Value};
use snapshot::assert_snapshot;

const NOW: &str = "2026-11-16T11:32";

const CONFIG: &str = r#"
timezone = "Europe/Stockholm"
[calendars.uni]
read_only = true
hide_matching = ["^Tutorial group B"]
"#;

struct Server {
    child: Child,
    url: String,
    token: Option<String>,
    calendars: tempfile::TempDir,
    _dir: tempfile::TempDir,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
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

fn start(token: Option<&str>) -> Server {
    start_with(token, CONFIG)
}

fn start_with(token: Option<&str>, config: &str) -> Server {
    let fixtures =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../cal-core/tests/fixtures/calendars");
    let calendars = tempfile::tempdir().unwrap();
    copy(&fixtures, calendars.path());
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    std::fs::write(&cfg, config).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cal-tui"));
    cmd.arg("--config")
        .arg(&cfg)
        .arg("--calendars-dir")
        .arg(calendars.path())
        .args(["--now", NOW, "serve", "--listen", "127.0.0.1:0"])
        .env_remove("CAL_TUI_TOKEN_FILE")
        .env_remove("JARVIS_URL")
        // Never the real ~/.local/state.
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(t) = token {
        let path = dir.path().join("token");
        std::fs::write(&path, format!("{t}\n")).unwrap();
        cmd.arg("--token-file").arg(path);
    }
    let mut child = cmd.spawn().unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let url = line
        .trim()
        .rsplit_once(" on ")
        .unwrap_or_else(|| panic!("no address in {line:?}"))
        .1
        .to_string();
    Server {
        child,
        url,
        token: token.map(String::from),
        calendars,
        _dir: dir,
    }
}

impl Server {
    fn agent() -> ureq::Agent {
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into()
    }

    fn auth(&self) -> Option<String> {
        self.token.as_ref().map(|t| format!("Bearer {t}"))
    }

    fn get(&self, path: &str) -> (u16, Value) {
        let mut req = Self::agent().get(format!("{}{path}", self.url));
        if let Some(a) = self.auth() {
            req = req.header("Authorization", a);
        }
        let mut resp = req.call().unwrap();
        (resp.status().as_u16(), resp.body_mut().read_json().unwrap())
    }

    fn post(&self, op: &str, params: Value) -> (u16, Value) {
        let mut req = Self::agent().post(format!("{}/{op}", self.url));
        if let Some(a) = self.auth() {
            req = req.header("Authorization", a);
        }
        let mut resp = req.send_json(params).unwrap();
        (resp.status().as_u16(), resp.body_mut().read_json().unwrap())
    }

    /// POST, require 200, return the result.
    fn ok(&self, op: &str, params: Value) -> Value {
        let (status, v) = self.post(op, params.clone());
        assert_eq!(status, 200, "{op} {params}: {v}");
        v
    }
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap() + "\n"
}

#[test]
fn describe_follows_the_service_api() {
    let s = start(None);
    let (status, d) = s.get("/describe");
    assert_eq!(status, 200);
    assert_eq!(d["service"], "calendar");
    assert_eq!(d["api"], 1);
    let ops = d["operations"].as_object().unwrap();
    for (name, op) in ops {
        let kind = op["kind"].as_str().unwrap();
        assert!(
            ["read", "command", "panel"].contains(&kind),
            "{name}: {kind}"
        );
        assert!(!op["description"].as_str().unwrap().is_empty());
        assert_eq!(op["params"]["type"], "object", "{name}");
        // A panel is named panel_<name>, and nothing else is.
        assert_eq!(kind == "panel", name.starts_with("panel_"), "{name}");
        // Jarvis's check: every read without required params answers {}.
        let v = match kind {
            "command" => continue,
            _ => s.ok(name, json!({})),
        };
        if kind == "panel" {
            assert_eq!(op["panel"], name.trim_start_matches("panel_"));
            assert!(
                op["title"].is_string() && op["order"].is_i64(),
                "{name}: {op}"
            );
            // jarvis_panel.validate_panel: only these keys, a title, blocks.
            for key in v.as_object().unwrap().keys() {
                assert!(
                    ["title", "order", "summary", "params", "updated", "blocks"]
                        .contains(&key.as_str()),
                    "{name}: {key}"
                );
            }
            assert!(v["blocks"].is_array(), "{name}: {v}");
        } else {
            assert!(v["text"].is_string(), "{name}: {v}");
        }
    }
    assert_snapshot("serve_describe", &pretty(&d));
}

#[test]
fn reads() {
    let s = start(None);
    assert_snapshot("serve_today", &pretty(&s.ok("today", json!({}))));
    assert_snapshot(
        "serve_agenda_uni",
        &pretty(&s.ok(
            "agenda",
            json!({"from": "2026-11-16", "days": 3, "calendars": ["uni"]}),
        )),
    );
    assert_snapshot("serve_next", &pretty(&s.ok("next", json!({}))));
    assert_snapshot(
        "serve_todos_all",
        &pretty(&s.ok("todos", json!({"all": true}))),
    );
    assert_snapshot("serve_calendars", &pretty(&s.ok("calendars", json!({}))));
    assert_snapshot("serve_panel_week", &pretty(&s.ok("panel_week", json!({}))));
    assert_snapshot(
        "serve_panel_month",
        &pretty(&s.ok("panel_month", json!({}))),
    );
    assert_snapshot("serve_panel_day", &pretty(&s.ok("panel_day", json!({}))));
    assert_snapshot(
        "serve_panel_todos_all",
        &pretty(&s.ok("panel_todos", json!({"all": true}))),
    );
    // Relative days, as an AI would say them.
    assert_eq!(
        s.ok("agenda", json!({"from": "tomorrow", "days": 1}))["from"],
        "2026-11-17"
    );
}

#[test]
fn commands_change_the_calendars() {
    let s = start(None);
    let dry = s.ok(
        "add_event",
        json!({"text": "Dentist fri 14:00 1h", "dry_run": true}),
    );
    assert_eq!(
        dry["text"],
        "Would add: Fri 20 Nov 2026 14:00-15:00 Dentist  [personal]"
    );
    assert!(dry["added"].is_null());

    let added = s.ok(
        "add_event",
        json!({"text": "Dentist fri 14:00 1h", "location": "Main St", "remind": "1h"}),
    );
    assert_eq!(
        added["text"],
        "Added: Fri 20 Nov 2026 14:00-15:00 Dentist · Main St  [personal], reminder 1h before"
    );
    assert_eq!(added["added"]["start"], "2026-11-20T14:00:00+01:00");
    let fri = s.ok("agenda", json!({"from": "2026-11-20", "days": 1}));
    assert!(
        fri["text"]
            .as_str()
            .unwrap()
            .contains("14:00-15:00 Dentist · Main St"),
        "{fri}"
    );

    let todo = s.ok(
        "add_todo",
        json!({"summary": "Buy milk", "due": "tomorrow", "priority": 2}),
    );
    assert_eq!(todo["added"]["due"], "2026-11-17");
    let done = s.ok("complete_todo", json!({"query": "milk"}));
    assert_eq!(done["text"], "Done: Buy milk");
    let open = s.ok("todos", json!({}));
    assert!(!open["text"].as_str().unwrap().contains("Buy milk"));
    let undone = s.ok("complete_todo", json!({"query": "milk", "undo": true}));
    assert_eq!(undone["text"], "Not done: Buy milk");

    // The files are really there.
    let personal = s.calendars.path().join("personal");
    let files: Vec<String> = std::fs::read_dir(personal)
        .unwrap()
        .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap_or_default())
        .collect();
    assert!(files.iter().any(|f| f.contains("SUMMARY:Dentist")));
    assert!(files.iter().any(|f| f.contains("SUMMARY:Buy milk")));
}

#[test]
fn mistakes_are_400_and_unknown_operations_404() {
    let s = start(None);
    let cases = [
        ("agenda", json!({"from": "someday"})),
        ("agenda", json!({"days": 0})),
        ("agenda", json!({"calendars": ["nope"]})),
        ("agenda", json!({"no_such_parameter_": 1})),
        ("todos", json!({"all": "yes"})),
        ("add_event", json!({})),
        ("add_event", json!({"text": "Party fri", "calendar": "uni"})),
        ("add_todo", json!({"summary": "x", "priority": 12})),
        ("complete_todo", json!({"query": "zzz"})),
    ];
    for (op, params) in cases {
        let (status, v) = s.post(op, params.clone());
        assert_eq!(status, 400, "{op} {params}: {v}");
        assert!(v["error"].is_string(), "{op} {params}: {v}");
    }
    let (status, _) = s.post("no_such_operation_", json!({}));
    assert_eq!(status, 404);
    let (status, v) = s.post("add_event", json!({"text": "Party fri", "calendar": "uni"}));
    assert_eq!(
        (status, v["error"].as_str().unwrap()),
        (400, "calendar 'University' is read-only")
    );
}

#[test]
fn a_token_is_required_when_set() {
    let s = start(Some("s3cret"));
    assert_eq!(s.get("/describe").0, 200);
    let mut wrong = start(Some("s3cret"));
    wrong.token = Some("guess".into());
    assert_eq!(wrong.get("/describe").0, 401);
    wrong.token = None;
    let (status, v) = wrong.post("today", json!({}));
    assert_eq!(status, 401, "{v}");
}

#[test]
fn refuses_an_open_address_without_a_token() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    std::fs::write(&cfg, CONFIG).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_cal-tui"))
        .arg("--config")
        .arg(&cfg)
        .args(["--now", NOW, "serve", "--listen", "0.0.0.0:0"])
        .env_remove("CAL_TUI_TOKEN_FILE")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("give --token-file"), "{err}");
}

#[test]
fn panel_actions_reach_the_commands() {
    let s = start(None);
    let panel = s.ok("panel_todos", json!({}));
    let table = &panel["blocks"][1];
    let done = &table["actions"][0];
    assert_eq!(done["operation"], "complete_todo");
    // What jarvis ls sends for the row of "Pay rent": "{uid}" filled in.
    let uid = table["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["summary"] == "Pay rent")
        .unwrap()["uid"]
        .clone();
    assert_eq!(done["params"]["query"], "{uid}");
    let v = s.ok(done["operation"].as_str().unwrap(), json!({"query": uid}));
    assert_eq!(v["text"], "Done: Pay rent");
    let after = s.ok("panel_todos", json!({}));
    assert!(!after.to_string().contains("Pay rent"));
}

/// What `jarvis ls` sends for a cell edit or a row action
/// (jarvis_panel.fill): "{col}" is the row's field, "{value}" what was
/// typed, and a param whose value is missing is left out.
fn fill(params: &Value, row: &Value, value: Option<&str>) -> Value {
    let mut out = serde_json::Map::new();
    for (k, v) in params.as_object().unwrap() {
        let filled = match v.as_str() {
            Some("{value}") => value.map(|x| json!(x)),
            Some(t) if t.starts_with('{') && t.ends_with('}') => {
                let f = &row[&t[1..t.len() - 1]];
                let f = f.get("v").unwrap_or(f);
                (!f.is_null()).then(|| f.clone())
            }
            _ => Some(v.clone()),
        };
        if let Some(f) = filled {
            out.insert(k.clone(), f);
        }
    }
    Value::Object(out)
}

/// The events table of a panel, and its row for `summary` on `day`.
fn event_row(panel: &Value, summary: &str, day: &str) -> (Value, Value) {
    let table = panel["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["id"] == "events")
        .unwrap()
        .clone();
    let row = table["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["summary"] == summary && r["day"]["v"] == day)
        .unwrap_or_else(|| panic!("no {summary} on {day}"))
        .clone();
    (table, row)
}

fn column<'a>(table: &'a Value, key: &str) -> &'a Value {
    table["columns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["key"] == key)
        .unwrap()
}

fn action<'a>(table: &'a Value, label: &str) -> &'a Value {
    table["actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["label"] == label)
        .unwrap()
}

/// Run a panel op (a column's `edit` or an action) on `row`.
fn run_op(s: &Server, op: &Value, row: &Value, value: Option<&str>) -> Value {
    s.ok(
        op["operation"].as_str().unwrap(),
        fill(&op["params"], row, value),
    )
}

fn agenda_text(s: &Server, from: &str, days: i64) -> String {
    s.ok("agenda", json!({"from": from, "days": days}))["text"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn edit_events_from_the_week_panel() {
    let s = start(None);
    let week = s.ok("panel_week", json!({}));

    // A one-off event: rename, move, retime, clear its place.
    let (table, row) = event_row(&week, "Lunch with Anna", "2026-11-16");
    let edit = &column(&table, "summary")["edit"];
    let v = run_op(&s, edit, &row, Some("Lunch with Anna and Bo"));
    assert_eq!(v["text"], "Updated Lunch with Anna and Bo");
    run_op(&s, &column(&table, "day")["edit"], &row, Some("2026-11-17"));
    let v = run_op(&s, &column(&table, "time")["edit"], &row, Some("12:30"));
    assert_eq!(
        v["text"],
        "Updated Lunch with Anna and Bo: Tue 17 Nov 2026 12:30-13:30"
    );
    // An emptied cell sends no value: the field is cleared.
    run_op(&s, &column(&table, "location")["edit"], &row, None);
    let text = agenda_text(&s, "2026-11-16", 2);
    assert!(
        text.contains("- 12:30-13:30 Lunch with Anna and Bo *(personal)*"),
        "{text}"
    );

    // One occurrence of a repeating event: only that day changes.
    let (table, row) = event_row(&week, "Climbing", "2026-11-18");
    assert!(row["id"].as_str().unwrap().starts_with("climb-1@"));
    run_op(
        &s,
        &column(&table, "time")["edit"],
        &row,
        Some("19:00-21:00"),
    );
    let text = agenda_text(&s, "2026-11-16", 3);
    assert!(text.contains("- 18:30-20:00 Climbing"), "{text}");
    assert!(text.contains("- 19:00-21:00 Climbing"), "{text}");

    // The whole series, by its plain uid.
    let v = s.ok(
        "edit_event",
        json!({"id": "climb-1@2026-11-16T18:30:00+01:00",
                                      "summary": "Bouldering", "all": true}),
    );
    assert_eq!(v["text"], "Updated all occurrences of Bouldering");
    let text = agenda_text(&s, "2026-11-16", 10);
    assert!(!text.contains("Climbing"), "{text}");
    assert!(text.matches("Bouldering").count() >= 4, "{text}");
}

#[test]
fn delete_events_from_the_day_panel() {
    let s = start(None);
    let day = s.ok("panel_day", json!({"day": "2026-11-18"}));
    let (table, row) = event_row(&day, "Climbing", "2026-11-18");
    let v = run_op(&s, action(&table, "Delete"), &row, None);
    assert_eq!(
        v["text"],
        "Deleted this occurrence: Climbing (Wed 18 Nov 2026 18:30-20:00)"
    );
    let text = agenda_text(&s, "2026-11-16", 10);
    assert!(!text.contains("Wed 18 Nov 2026\n- 18:30"), "{text}");
    assert_eq!(text.matches("Climbing").count(), 3, "{text}");

    let day = s.ok("panel_day", json!({"day": "2026-11-23"}));
    let (table, row) = event_row(&day, "Climbing", "2026-11-23");
    let v = run_op(&s, action(&table, "Delete every occurrence"), &row, None);
    assert_eq!(v["text"], "Deleted all occurrences: Climbing");
    assert!(!agenda_text(&s, "2026-11-16", 14).contains("Climbing"));
}

#[test]
fn edit_and_delete_todos_from_the_todos_panel() {
    let s = start(None);
    let panel = s.ok("panel_todos", json!({}));
    let table = &panel["blocks"][1];
    let row = table["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["summary"] == "Pay rent")
        .unwrap()
        .clone();
    let v = run_op(&s, &column(table, "priority")["edit"], &row, Some("3"));
    assert_eq!(v["text"], "Updated todo: Pay rent (priority 3)");
    let v = run_op(&s, &column(table, "due")["edit"], &row, Some("fri"));
    assert_eq!(v["text"], "Updated todo: Pay rent (due Fri 20 Nov)");
    let v = run_op(&s, &column(table, "due")["edit"], &row, None);
    assert_eq!(v["text"], "Updated todo: Pay rent (no due date)");
    let todos = s.ok("todos", json!({}));
    assert!(todos["text"]
        .as_str()
        .unwrap()
        .contains("- Pay rent · priority 3\n"));

    let v = run_op(&s, action(table, "Delete"), &row, None);
    assert_eq!(v["text"], "Deleted todo: Pay rent");
    assert!(!s.ok("todos", json!({}))["text"]
        .as_str()
        .unwrap()
        .contains("Pay rent"));
}

#[test]
fn edits_that_cannot_be_done_are_400() {
    let s = start(None);
    let cases = [
        ("edit_event", json!({"id": "nope", "summary": "x"})),
        ("edit_event", json!({"id": "lunch-1"})),
        ("edit_event", json!({"id": "lunch-1", "time": "25:99"})),
        ("edit_event", json!({"id": "lunch-1", "field": "summary"})),
        (
            "delete_event",
            json!({"id": "climb-1@2026-11-17T18:30:00+01:00"}),
        ),
        ("edit_todo", json!({"query": "rent", "priority": 12})),
        ("delete_todo", json!({"query": "zzz"})),
        ("panel_month", json!({"month": "someday"})),
    ];
    for (op, params) in cases {
        let (status, v) = s.post(op, params.clone());
        assert_eq!(status, 400, "{op} {params}: {v}");
    }
    // The university calendar is read-only.
    let id = s.ok("today", json!({}))["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["calendar"] == "uni")
        .unwrap()["id"]
        .clone();
    let (status, v) = s.post("delete_event", json!({"id": id}));
    assert_eq!(
        (status, v["error"].as_str().unwrap()),
        (400, "calendar 'University' is read-only")
    );
}

#[test]
fn months() {
    let s = start(None);
    let m = |month: &str| s.ok("panel_month", json!({"month": month}))["params"]["month"].clone();
    assert_eq!(m("next"), "2026-12");
    assert_eq!(m("-11"), "2025-12");
    assert_eq!(m("2027-02"), "2027-02");
}

/// A stand-in for Jarvis's /report: every POST's path and body.
fn journal() -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/report/calendar", listener.local_addr().unwrap());
    let got = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = got.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            // Headers, then Content-Length bytes of body.
            loop {
                let n = stream.read(&mut chunk).unwrap();
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf).to_string();
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let len: usize = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse().unwrap())
                        })
                        .unwrap_or(0);
                    if body.len() >= len || n == 0 {
                        let path = head.lines().next().unwrap_or("").to_string();
                        seen.lock().unwrap().push(format!("{path}\n{body}"));
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .ok();
        }
    });
    (url, got)
}

#[test]
fn an_edit_is_journaled_once() {
    let (url, posts) = journal();
    let config = format!("{CONFIG}\n[jarvis]\nreport_url = \"{url}\"\n");
    let s = start_with(None, &config);
    // The first sync remembers what is there.
    let sync = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_cal-tui"))
            .arg("--config")
            .arg(s._dir.path().join("config.toml"))
            .arg("--calendars-dir")
            .arg(s.calendars.path())
            .args(["--now", NOW, "sync", "--no-sync"])
            .args(args)
            .env("XDG_STATE_HOME", s._dir.path().join("state"))
            .env_remove("JARVIS_URL")
            .output()
            .unwrap()
    };
    assert!(sync(&[]).status.success());

    let week = s.ok("panel_week", json!({}));
    let (table, row) = event_row(&week, "Climbing", "2026-11-18");
    run_op(&s, action(&table, "Delete"), &row, None);

    // The edit is in the journal, tagged #edited ...
    std::thread::sleep(std::time::Duration::from_millis(500));
    let got = posts.lock().unwrap().clone();
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(
        got[0].starts_with("POST /report/calendar?tags=edited "),
        "{got:?}"
    );
    assert!(
        got[0].ends_with("\nDeleted this occurrence: Climbing (Wed 18 Nov 2026 18:30-20:00)\n"),
        "{got:?}"
    );
    // ... and the next sync does not report it again.
    let out = sync(&["--print"]);
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");

    // A dry run changes nothing, and posts nothing.
    s.ok(
        "add_event",
        json!({"text": "Dentist fri 14:00", "dry_run": true}),
    );
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(posts.lock().unwrap().len(), 1);
}

#[test]
fn an_occurrence_moved_far_can_still_be_edited() {
    let s = start(None);
    let id = "climb-1@2026-11-18T18:30:00+01:00";
    // Wed to Sat: well away from the day it belongs to.
    s.ok("edit_event", json!({"id": id, "date": "2026-11-21"}));
    let v = s.ok(
        "edit_event",
        json!({"id": id, "summary": "Climbing outdoors"}),
    );
    assert_eq!(v["text"], "Updated this occurrence of Climbing outdoors");
    let text = agenda_text(&s, "2026-11-21", 1);
    assert!(text.contains("18:30-20:00 Climbing outdoors"), "{text}");
    s.ok("delete_event", json!({"id": id}));
    assert!(!agenda_text(&s, "2026-11-21", 1).contains("Climbing"));
}

#[test]
fn an_unclear_todo_is_not_looked_for_among_the_done_ones() {
    let s = start(None);
    for t in ["Buy milk", "Milk the cow", "milk"] {
        s.ok("add_todo", json!({"summary": t}));
    }
    s.ok("complete_todo", json!({"query": "milk"}));
    // Now two open todos match, and one done one.
    let (status, v) = s.post("delete_todo", json!({"query": "milk"}));
    assert_eq!(status, 400, "{v}");
    assert!(
        v["error"].as_str().unwrap().contains("matches 2 todos"),
        "{v}"
    );
    let all = s.ok("todos", json!({"all": true}));
    assert!(all["text"].as_str().unwrap().contains("milk"), "{all}");
    // When no open one matches, the done ones are looked at.
    s.ok("delete_todo", json!({"query": "Buy milk"}));
    s.ok("delete_todo", json!({"query": "Milk the cow"}));
    let v = s.ok("delete_todo", json!({"query": "milk"}));
    assert_eq!(v["text"], "Deleted todo: milk");
}

#[test]
fn an_all_day_event_given_a_time_lasts_an_hour() {
    let s = start(None);
    let v = s.ok("edit_event", json!({"id": "trip-1", "time": "10:00"}));
    assert!(v["text"].as_str().unwrap().ends_with("10:00-11:00"), "{v}");
}

#[test]
fn a_failed_push_leaves_the_alert_to_the_timer() {
    // sync_command goes before the tables.
    let config = format!("sync_command = \"false\"\n{CONFIG}\n[jarvis]\nfail_after = 1\n");
    let s = start_with(None, &config);
    // The push after this edit fails: that is the first failure ...
    s.ok("add_event", json!({"text": "Dentist fri 14:00"}));
    std::thread::sleep(std::time::Duration::from_millis(500));
    // ... and the timer's sync, failing too, is the one that alerts.
    let out = Command::new(env!("CARGO_BIN_EXE_cal-tui"))
        .arg("--config")
        .arg(s._dir.path().join("config.toml"))
        .arg("--calendars-dir")
        .arg(s.calendars.path())
        .args(["--now", NOW, "sync"])
        .env("XDG_STATE_HOME", s._dir.path().join("state"))
        .env_remove("JARVIS_URL")
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{err}");
    assert!(err.contains("has failed 2 times in a row"), "{err}");
}
