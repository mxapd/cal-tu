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
    let fixtures =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../cal-core/tests/fixtures/calendars");
    let calendars = tempfile::tempdir().unwrap();
    copy(&fixtures, calendars.path());
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    std::fs::write(&cfg, CONFIG).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cal-tui"));
    cmd.arg("--config")
        .arg(&cfg)
        .arg("--calendars-dir")
        .arg(calendars.path())
        .args(["--now", NOW, "serve", "--listen", "127.0.0.1:0"])
        .env_remove("CAL_TUI_TOKEN_FILE")
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
