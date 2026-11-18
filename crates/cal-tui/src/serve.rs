//! `cal-tui serve`: the calendars over HTTP, as the Jarvis Service API v1
//! (jarvis/docs/service-api.md) has it, so that Jarvis can route to them and
//! offer them to AIs as MCP tools (`calendar__agenda`, ...):
//!
//!     GET  /describe       {"service", "api": 1, "operations": {...}}
//!     POST /<operation>    JSON parameters in, a JSON object out, with a
//!                          markdown `text` for people (and AIs).
//!                          400 {"error"}: the caller can fix it; 404: no such
//!                          operation; 500 {"error"}: it failed.
//!
//! The calendars are loaded again for every request, so a sync in between is
//! always seen. Requests are answered one at a time.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::fd::{FromRawFd, IntoRawFd};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::time::{Duration as StdDuration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use cal_core::{Config, Store};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use chrono_tz::Tz;
use serde_json::{json, Map, Value};
use tiny_http::{Header, Method, Request, Response, Server};

use crate::ops::{self, is_user_error, user};
use crate::output;
use crate::report;

pub struct Options {
    pub listen: Option<String>,
    pub socket: Option<PathBuf>,
    pub token_file: Option<PathBuf>,
    pub service: String,
    /// Exit after this long without a request; zero: never.
    pub idle: StdDuration,
    /// A fixed "now", for tests.
    pub now: Option<DateTime<Tz>>,
}

const MAX_BODY: u64 = 1 << 20;
const MAX_AGENDA_DAYS: i64 = 62;
const MAX_NEXT_DAYS: i64 = 366;
/// The fd systemd passes the first socket on.
const SD_LISTEN_FDS_START: i32 = 3;

struct Op {
    name: &'static str,
    kind: &'static str,
    description: &'static str,
    params: fn() -> Value,
}

const OPERATIONS: &[Op] = &[
    Op {
        name: "today",
        kind: "read",
        description: "Today's events (and todos due today), all-day ones first.",
        params: || json!({"type": "object", "properties": {}}),
    },
    Op {
        name: "agenda",
        kind: "read",
        description: "Events and due todos, grouped by day, from a day on.",
        params: || {
            json!({"type": "object", "properties": {
                "from": {"type": "string",
                         "description": "first day: YYYY-MM-DD, MM-DD, today, tomorrow, mon…sun (the next one) or +3d; default today"},
                "days": {"type": "integer", "description": "how many days, 1–62; default 7", "default": 7},
                "calendars": {"type": "array", "items": {"type": "string"},
                              "description": "only these calendars (folder or display name); default all"}
            }})
        },
    },
    Op {
        name: "next",
        kind: "read",
        description: "The next timed event that has not started yet.",
        params: || {
            json!({"type": "object", "properties": {
                "within": {"type": "integer", "description": "how many days ahead to look, 1–366; default 7", "default": 7}
            }})
        },
    },
    Op {
        name: "todos",
        kind: "read",
        description: "Open todos, overdue ones marked, by priority and due date.",
        params: || {
            json!({"type": "object", "properties": {
                "all": {"type": "boolean", "description": "include completed todos", "default": false}
            }})
        },
    },
    Op {
        name: "calendars",
        kind: "read",
        description: "The calendars: id, name, whether they can be written, and which one new items go to by default.",
        params: || json!({"type": "object", "properties": {}}),
    },
    Op {
        name: "add_event",
        kind: "command",
        description: "Add an event from one line: title, then date, time and length, e.g. \"Dentist fri 14:00 1h\", \"Call mum 18:30-19:00\", \"Conference 12-01 3d\" (no time: all day).",
        params: || {
            json!({"type": "object", "properties": {
                "text": {"type": "string",
                         "description": "title then when. Dates: today, tomorrow, mon…sun, +3d, MM-DD, YYYY-MM-DD. Times: 14:00, 2pm, 14:00-15:30. Lengths: 1h, 30m, 1h30, 2d. A word starting with \\ stays in the title."},
                "calendar": {"type": "string", "description": "calendar id or name; default: the default calendar"},
                "location": {"type": "string"},
                "description": {"type": "string", "description": "notes"},
                "remind": {"type": "string", "description": "a reminder this long before the start, e.g. 15m, 1h, 1d"},
                "repeat": {"type": "string", "description": "daily, weekdays, weekly, biweekly, monthly, yearly, or an RRULE (FREQ=...)"},
                "dry_run": {"type": "boolean", "description": "only say what would be added", "default": false}
            }, "required": ["text"]})
        },
    },
    Op {
        name: "add_todo",
        kind: "command",
        description: "Add a todo.",
        params: || {
            json!({"type": "object", "properties": {
                "summary": {"type": "string", "description": "the title"},
                "due": {"type": "string", "description": "due date, optionally with a time: fri, 12-24, \"tomorrow 9am\""},
                "priority": {"type": "integer", "description": "1 (highest) – 9; 0 for none", "default": 0},
                "calendar": {"type": "string", "description": "calendar id or name; default: the default calendar"},
                "description": {"type": "string", "description": "notes"}
            }, "required": ["summary"]})
        },
    },
    Op {
        name: "complete_todo",
        kind: "command",
        description: "Mark a todo done, found by its uid or (part of) its title; with undo, mark a done one open again.",
        params: || {
            json!({"type": "object", "properties": {
                "query": {"type": "string", "description": "the todo's uid, or (part of) its title"},
                "undo": {"type": "boolean", "description": "mark it not done instead", "default": false}
            }, "required": ["query"]})
        },
    },
];

fn describe(service: &str) -> Value {
    let ops: Map<String, Value> = OPERATIONS
        .iter()
        .map(|op| {
            (
                op.name.to_string(),
                json!({"kind": op.kind, "description": op.description, "params": (op.params)()}),
            )
        })
        .collect();
    json!({"service": service, "api": 1, "operations": ops})
}

pub fn run(config: Config, opts: Options) -> Result<()> {
    if opts.service.is_empty()
        || !opts
            .service
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        bail!("--service '{}': use a-z, 0-9 and -", opts.service);
    }
    let home = config.home_zone()?;
    let token = opts.token_file.as_deref().map(read_token).transpose()?;
    let (server, shown) = listen(&opts, token.is_some())?;
    println!("cal-tui serve: {} on {shown}", opts.service);
    std::io::stdout().flush().ok();

    let app = App {
        config,
        home,
        service: opts.service,
        token,
        now: opts.now,
        warnings: Default::default(),
    };
    loop {
        let req = if opts.idle.is_zero() {
            Some(server.recv()?)
        } else {
            server.recv_timeout(opts.idle)?
        };
        let Some(req) = req else {
            eprintln!(
                "idle for {}s, exiting; systemd starts me again on the next request",
                opts.idle.as_secs()
            );
            // Not tiny_http's Drop: it deletes the socket file, which under
            // socket activation is systemd's (and panics on an abstract one).
            std::mem::forget(server);
            return Ok(());
        };
        app.handle(req);
    }
}

fn read_token(path: &Path) -> Result<String> {
    let token = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read the token file {}", path.display()))?
        .trim()
        .to_string();
    if token.is_empty() {
        bail!("the token file {} is empty", path.display());
    }
    Ok(token)
}

fn server_from(l: impl Into<tiny_http::Listener>) -> Result<Server> {
    Server::from_listener(l, None).map_err(|e| anyhow!("cannot serve: {e}"))
}

fn tcp_server(l: TcpListener, has_token: bool) -> Result<(Server, String)> {
    let local = l.local_addr()?;
    if !local.ip().is_loopback() && !has_token {
        bail!("{local} can be reached from other machines: give --token-file");
    }
    Ok((server_from(l)?, format!("http://{local}")))
}

/// The listener `opts` asks for: TCP, a unix socket, or the one systemd
/// passes.
fn listen(opts: &Options, has_token: bool) -> Result<(Server, String)> {
    if let Some(addr) = &opts.listen {
        let l = TcpListener::bind(addr).with_context(|| format!("cannot listen on {addr}"))?;
        return tcp_server(l, has_token);
    }
    if let Some(path) = &opts.socket {
        // A socket left over from an earlier run.
        if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_socket()) {
            std::fs::remove_file(path).ok();
        }
        let l = UnixListener::bind(path)
            .with_context(|| format!("cannot listen on {}", path.display()))?;
        return Ok((server_from(l)?, path.display().to_string()));
    }
    let ours = std::env::var("LISTEN_PID")
        .map(|p| p == std::process::id().to_string())
        .unwrap_or(true);
    let fds: u32 = std::env::var("LISTEN_FDS")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    if !ours || fds < 1 {
        bail!("give --listen ADDRESS or --socket PATH (or start it from a systemd socket)");
    }
    // SAFETY: systemd passes the listening socket as fd 3, and nothing else
    // in this process uses it.
    let unix = unsafe { UnixListener::from_raw_fd(SD_LISTEN_FDS_START) };
    if unix.local_addr().is_ok() {
        return Ok((server_from(unix)?, "the socket from systemd".into()));
    }
    // Not a unix socket: a TCP one.
    let tcp = unsafe { TcpListener::from_raw_fd(unix.into_raw_fd()) };
    tcp_server(tcp, has_token)
}

struct App {
    config: Config,
    home: Tz,
    service: String,
    token: Option<String>,
    now: Option<DateTime<Tz>>,
    /// How many loading problems were last reported.
    warnings: std::cell::Cell<usize>,
}

fn error(msg: impl std::fmt::Display) -> Value {
    json!({"error": msg.to_string()})
}

/// Compare without stopping at the first difference.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

impl App {
    fn handle(&self, mut req: Request) {
        let started = Instant::now();
        let method = req.method().clone();
        let path = req.url().split('?').next().unwrap_or("").to_string();
        let (status, body) = self.answer(&mut req, &method, &path);
        let mut resp = Response::from_data(serde_json::to_vec(&body).unwrap_or_default())
            .with_status_code(status)
            .with_header(
                Header::from_bytes("Content-Type", "application/json; charset=utf-8").unwrap(),
            );
        if status == 401 {
            resp.add_header(Header::from_bytes("WWW-Authenticate", "Bearer").unwrap());
        }
        if let Err(e) = req.respond(resp) {
            eprintln!("{method} {path}: cannot answer: {e}");
        }
        eprintln!(
            "{method} {path} {status} {}ms",
            started.elapsed().as_millis()
        );
    }

    fn authorized(&self, req: &Request) -> bool {
        let Some(token) = &self.token else {
            return true;
        };
        req.headers()
            .iter()
            .find(|h| h.field.equiv("Authorization"))
            .and_then(|h| h.value.as_str().strip_prefix("Bearer "))
            .is_some_and(|got| same(got.trim().as_bytes(), token.as_bytes()))
    }

    fn answer(&self, req: &mut Request, method: &Method, path: &str) -> (u16, Value) {
        if !self.authorized(req) {
            return (401, error("missing or wrong bearer token"));
        }
        let name = path.trim_start_matches('/');
        let op = OPERATIONS.iter().find(|o| o.name == name);
        match (method, name, op) {
            (Method::Get, "describe", _) => (200, describe(&self.service)),
            (Method::Get, "health", _) => (200, json!({"ok": true})),
            (Method::Post, _, Some(op)) => {
                let params = match read_params(req, op) {
                    Ok(p) => p,
                    Err(e) => return (400, error(e)),
                };
                match self.call(op.name, &params) {
                    Ok(v) => (200, v),
                    Err(e) if is_user_error(&e) => (400, error(e)),
                    Err(e) => {
                        eprintln!("{}: {e:#}", op.name);
                        (500, error(format!("{e:#}")))
                    }
                }
            }
            (_, "describe" | "health", _) | (_, _, Some(_)) => {
                (405, error(format!("{method} is not used on {path}")))
            }
            _ => (404, error(format!("no operation '{name}'"))),
        }
    }

    fn now(&self) -> DateTime<Tz> {
        self.now
            .unwrap_or_else(|| Utc::now().with_timezone(&self.home))
    }

    fn call(&self, op: &str, p: &Params) -> Result<Value> {
        let now = self.now();
        let today = now.date_naive();
        let mut store = Store::load(&self.config)?;
        // Once, not on every request.
        if self.warnings.replace(store.warnings.len()) != store.warnings.len() {
            output::report_warnings(&store.warnings, false);
        }
        match op {
            "today" => Ok(agenda(&store, today, 1)),
            "agenda" => {
                let from = match p.str("from")? {
                    Some(s) => cal_core::quick::parse_date(s.trim(), today).ok_or_else(|| {
                        user(format!(
                            "from: cannot read '{s}' as a day (YYYY-MM-DD, MM-DD, today, tomorrow, mon…sun, +3d)"
                        ))
                    })?,
                    None => today,
                };
                let days = p.int_in("days", 7, 1, MAX_AGENDA_DAYS)?;
                ops::only_calendars(&mut store, &p.strings("calendars")?)?;
                Ok(agenda(&store, from, days))
            }
            "next" => {
                let within = p.int_in("within", 7, 1, MAX_NEXT_DAYS)?;
                let (inst, _) = store.instances(now, now + Duration::days(within));
                Ok(match ops::next_event(&store, &inst, now) {
                    Some(i) => {
                        let mut text = output::next_line(
                            &store,
                            i,
                            now,
                            "{time} {summary} (in {in}) *({calendar})*",
                        );
                        if let Some(loc) = &i.location {
                            text.push_str(&format!(" · {loc}"));
                        }
                        json!({"item": output::json_item(&store, i), "text": text})
                    }
                    None => {
                        json!({"item": null, "text": format!("Nothing in the next {within} days.")})
                    }
                })
            }
            "todos" => Ok(todos(&store, p.bool("all")?, now)),
            "calendars" => Ok(self.calendars(&store)),
            "add_event" => {
                let (cal, ev) = ops::new_event(
                    &store,
                    &self.config,
                    now,
                    ops::EventInput {
                        text: p.required("text")?,
                        calendar: p.str("calendar")?,
                        remind: p.str("remind")?,
                        location: p.str("location")?,
                        description: p.str("description")?,
                        repeat: p.str("repeat")?,
                    },
                )?;
                let line = output::describe_new_event(&store, cal, &ev);
                let event = |uid: Option<&str>| {
                    json!({
                        "uid": uid,
                        "calendar": store.calendars[cal].id,
                        "summary": ev.summary,
                        "start": output::moment_json(&ev.start, store.home),
                        "end": output::moment_json(&ev.end, store.home),
                        "repeat": ev.rrule,
                    })
                };
                if p.bool("dry_run")? {
                    return Ok(
                        json!({"added": null, "would_add": event(None), "text": format!("Would add: {line}")}),
                    );
                }
                let uid = store.create_event(cal, &ev, now.with_timezone(&Utc))?;
                Ok(json!({"added": event(Some(&uid)), "text": format!("Added: {line}")}))
            }
            "add_todo" => {
                let priority = p.int_in("priority", 0, 0, 9)? as u8;
                let (cal, todo) = ops::new_todo(
                    &store,
                    &self.config,
                    now,
                    ops::TodoInput {
                        summary: p.required("summary")?,
                        due: p.str("due")?,
                        priority,
                        calendar: p.str("calendar")?,
                        description: p.str("description")?,
                    },
                )?;
                let uid = store.create_todo(cal, &todo, now.with_timezone(&Utc))?;
                Ok(json!({
                    "added": {
                        "uid": uid,
                        "calendar": store.calendars[cal].id,
                        "summary": todo.summary,
                        "due": todo.due.map(|d| output::moment_json(&d, store.home)),
                        "priority": todo.priority,
                    },
                    "text": format!("Added todo: {}", output::describe_new_todo(&store, cal, &todo)),
                }))
            }
            "complete_todo" => {
                let undo = p.bool("undo")?;
                let t = ops::complete_todo(&store, &p.required("query")?, undo, now)?;
                Ok(json!({
                    "uid": t.uid,
                    "summary": t.summary,
                    "completed": !undo,
                    "text": format!("{}: {}", if undo { "Not done" } else { "Done" }, t.summary),
                }))
            }
            _ => unreachable!("{op} is in OPERATIONS"),
        }
    }

    fn calendars(&self, store: &Store) -> Value {
        let default = store
            .target_calendar(None, self.config.default_calendar.as_deref())
            .ok();
        let mut lines = vec![format!("### Calendars ({})", store.calendars.len())];
        let cals: Vec<Value> = store
            .calendars
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let mut line = format!("- {}", c.name);
                if c.name != c.id {
                    line.push_str(&format!(" (`{}`)", c.id));
                }
                if c.read_only {
                    line.push_str(" · read-only");
                }
                if default == Some(i) {
                    line.push_str(" · default");
                }
                lines.push(line);
                json!({"id": c.id, "name": c.name, "read_only": c.read_only, "default": default == Some(i)})
            })
            .collect();
        json!({"calendars": cals, "text": lines.join("\n") + "\n"})
    }
}

/// Items from `from` on for `days` days, and as markdown by day.
fn agenda(store: &Store, from: NaiveDate, days: i64) -> Value {
    let (inst, _) = store.instances(
        store.day_start(from),
        store.day_start(from + Duration::days(days)),
    );
    let mut text = Vec::new();
    for n in 0..days {
        let day = from + Duration::days(n);
        let lines = report::item_lines(store, &inst, day, true);
        if lines.is_empty() && days > 1 {
            continue;
        }
        if !text.is_empty() {
            text.push(String::new());
        }
        text.push(format!("### {}", day.format("%a %d %b %Y")));
        if lines.is_empty() {
            text.push("Nothing scheduled.".into());
        }
        text.extend(lines);
    }
    if text.is_empty() {
        let last = from + Duration::days(days - 1);
        text.push(format!(
            "Nothing scheduled from {} to {}.",
            from.format("%a %d %b"),
            last.format("%a %d %b %Y")
        ));
    }
    json!({
        "from": from.format("%Y-%m-%d").to_string(),
        "days": days,
        "items": output::json_items(store, &inst),
        "text": text.join("\n") + "\n",
    })
}

/// Open todos (`all`: done ones too), with `overdue`.
fn todos(store: &Store, all: bool, now: DateTime<Tz>) -> Value {
    let todos: Vec<_> = store
        .todos()
        .into_iter()
        .filter(|t| all || !t.completed)
        .collect();
    let overdue =
        |t: &cal_core::Todo| !t.completed && t.due.is_some_and(|d| report::is_overdue(&d, now));
    let items: Vec<Value> = todos
        .iter()
        .map(|t| {
            let mut v = serde_json::to_value(output::json_todo(store, t)).unwrap_or_default();
            v["overdue"] = json!(overdue(t));
            v
        })
        .collect();
    let open = todos.iter().filter(|t| !t.completed).count();
    let text = if todos.is_empty() {
        "No open todos.\n".to_string()
    } else {
        let mut lines = vec![format!("### Todos ({open} open)")];
        lines.extend(todos.iter().map(|t| {
            if t.completed {
                format!("- ~~{}~~ (done)", t.summary)
            } else {
                report::todo_line(store, t, now)
            }
        }));
        lines.join("\n") + "\n"
    };
    json!({"todos": items, "text": text})
}

/// An operation's parameters, checked against its schema's properties.
struct Params(Map<String, Value>);

fn read_params(req: &mut Request, op: &Op) -> Result<Params> {
    let mut raw = Vec::new();
    req.as_reader()
        .take(MAX_BODY + 1)
        .read_to_end(&mut raw)
        .context("cannot read the body")?;
    if raw.len() as u64 > MAX_BODY {
        bail!("the body is over {MAX_BODY} bytes");
    }
    let map = if raw.iter().all(u8::is_ascii_whitespace) {
        Map::new()
    } else {
        match serde_json::from_slice(&raw).context("the body is not JSON")? {
            Value::Object(m) => m,
            _ => bail!("the body should be a JSON object of parameters"),
        }
    };
    let schema = (op.params)();
    let props = schema["properties"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    for key in map.keys() {
        if !props.contains_key(key) {
            let known: Vec<_> = props.keys().map(String::as_str).collect();
            bail!(
                "{} takes no parameter '{key}' (it takes: {})",
                op.name,
                if known.is_empty() {
                    "none".into()
                } else {
                    known.join(", ")
                }
            );
        }
    }
    Ok(Params(map))
}

impl Params {
    fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key).filter(|v| !v.is_null())
    }

    fn str(&self, key: &str) -> Result<Option<String>> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(user(format!("{key} should be a string"))),
        }
    }

    fn required(&self, key: &str) -> Result<String> {
        self.str(key)?
            .ok_or_else(|| user(format!("{key} is required")))
    }

    fn bool(&self, key: &str) -> Result<bool> {
        match self.get(key) {
            None => Ok(false),
            Some(Value::Bool(b)) => Ok(*b),
            Some(_) => Err(user(format!("{key} should be true or false"))),
        }
    }

    fn int_in(&self, key: &str, default: i64, min: i64, max: i64) -> Result<i64> {
        let n = match self.get(key) {
            None => return Ok(default),
            Some(v) => v
                .as_i64()
                .or_else(|| v.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64))
                .ok_or_else(|| user(format!("{key} should be a whole number")))?,
        };
        if !(min..=max).contains(&n) {
            return Err(user(format!("{key} should be {min}–{max}, not {n}")));
        }
        Ok(n)
    }

    /// A list of strings; one string is a list of one.
    fn strings(&self, key: &str) -> Result<Vec<String>> {
        match self.get(key) {
            None => Ok(Vec::new()),
            Some(Value::String(s)) => Ok(vec![s.clone()]),
            Some(Value::Array(a)) => a
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(String::from)
                        .ok_or_else(|| user(format!("{key} should be a list of strings")))
                })
                .collect(),
            Some(_) => Err(user(format!("{key} should be a list of strings"))),
        }
    }
}
