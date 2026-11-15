//! Reading calendars from a vdir and querying them.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, NaiveTime};
use chrono_tz::Tz;
use regex::Regex;

use crate::config::Config;
use crate::ical::{self, Component};
use crate::recur;
use crate::time::{local_to_zoned, parse_moment, Moment};

/// One calendar (a vdir collection folder).
#[derive(Debug, Clone)]
pub struct Calendar {
    /// Folder name; stable identifier.
    pub id: String,
    /// Display name.
    pub name: String,
    pub path: PathBuf,
    /// Colour name or `#rrggbb`.
    pub color: Option<String>,
    pub read_only: bool,
    pub hidden: bool,
    /// Reminder for timed events without their own alarm.
    pub default_reminder: Option<chrono::Duration>,
    hide_matching: Vec<Regex>,
}

impl Calendar {
    pub(crate) fn hides(&self, summary: &str) -> bool {
        self.hide_matching.iter().any(|r| r.is_match(summary))
    }
}

/// A problem found while loading; loading always continues.
#[derive(Debug, Clone)]
pub struct Warning {
    pub path: Option<PathBuf>,
    pub message: String,
}

impl std::fmt::Display for Warning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.path {
            Some(p) => write!(f, "{}: {}", p.display(), self.message),
            None => write!(f, "{}", self.message),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Event,
    Todo,
}

/// A logical calendar object: all components sharing one UID in one file.
#[derive(Debug, Clone)]
pub struct Object {
    pub calendar: usize,
    pub path: PathBuf,
    pub uid: String,
    pub kind: Kind,
    /// The main component (no RECURRENCE-ID). For a file holding only
    /// overrides, the first override acts as master.
    pub master: Component,
    pub overrides: Vec<Component>,
}

/// One occurrence of an event (or a todo on its due date) to display.
#[derive(Debug, Clone)]
pub struct Instance {
    pub calendar: usize,
    pub object: usize,
    pub kind: Kind,
    pub uid: String,
    /// Set for occurrences of a recurring series.
    pub recurrence_id: Option<Moment>,
    pub summary: String,
    pub location: Option<String>,
    pub description: Option<String>,
    pub start: Moment,
    pub end: Moment,
    pub all_day: bool,
    pub tentative: bool,
}

/// A todo.
#[derive(Debug, Clone)]
pub struct Todo {
    pub calendar: usize,
    pub object: usize,
    pub uid: String,
    pub summary: String,
    pub description: Option<String>,
    /// 1 (highest) – 9 (lowest); 0 = undefined.
    pub priority: u8,
    pub due: Option<Moment>,
    pub completed: bool,
}

/// All loaded calendars and objects.
#[derive(Debug)]
pub struct Store {
    pub home: Tz,
    pub calendars: Vec<Calendar>,
    pub objects: Vec<Object>,
    pub warnings: Vec<Warning>,
    /// Content hash of every file as it was loaded, to detect changes made
    /// by others (a sync) before writing.
    pub(crate) file_hashes: HashMap<PathBuf, u64>,
    /// Files with syntax problems: shown, but never rewritten, because the
    /// lenient parser may have skipped parts of them.
    pub(crate) unsafe_files: HashMap<PathBuf, String>,
}

impl Store {
    /// Load every calendar folder under the configured directory.
    pub fn load(config: &Config) -> anyhow::Result<Store> {
        let home = config.home_zone()?;
        Ok(Self::load_dir(&config.calendars_dir(), config, home))
    }

    /// Load from an explicit directory and zone (used by tests).
    pub fn load_dir(dir: &Path, config: &Config, home: Tz) -> Store {
        let mut store = Store {
            home,
            calendars: Vec::new(),
            objects: Vec::new(),
            warnings: Vec::new(),
            file_hashes: HashMap::new(),
            unsafe_files: HashMap::new(),
        };
        let mut folders: Vec<PathBuf> = match std::fs::read_dir(dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect(),
            Err(e) => {
                store.warn(Some(dir), format!("cannot read calendars directory: {e}"));
                return store;
            }
        };
        folders.sort();
        for folder in folders {
            store.load_calendar(&folder, config);
        }
        store
    }

    fn warn(&mut self, path: Option<&Path>, message: String) {
        self.warnings.push(Warning {
            path: path.map(Path::to_path_buf),
            message,
        });
    }

    fn load_calendar(&mut self, folder: &Path, config: &Config) {
        let id = folder
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if id.starts_with('.') {
            return;
        }
        let cc = config.calendars.get(&id).cloned().unwrap_or_default();
        let meta = |name: &str| {
            std::fs::read_to_string(folder.join(name))
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        };
        let mut hide_matching = Vec::new();
        for pat in &cc.hide_matching {
            match Regex::new(pat) {
                Ok(r) => hide_matching.push(r),
                Err(e) => self.warn(
                    None,
                    format!("calendar {id}: bad hide_matching '{pat}': {e}"),
                ),
            }
        }
        let cal_idx = self.calendars.len();
        self.calendars.push(Calendar {
            name: cc
                .name
                .or_else(|| meta("displayname"))
                .unwrap_or_else(|| id.clone()),
            color: cc.color.or_else(|| meta("color")),
            read_only: cc.read_only,
            hidden: cc.hidden,
            default_reminder: cc
                .default_reminder
                .as_deref()
                .and_then(crate::config::parse_offset),
            hide_matching,
            path: folder.to_path_buf(),
            id,
        });

        let mut files: Vec<PathBuf> = match std::fs::read_dir(folder) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("ics")))
                .collect(),
            Err(e) => {
                self.warn(Some(folder), format!("cannot read calendar: {e}"));
                return;
            }
        };
        files.sort();
        for file in files {
            self.load_file(cal_idx, &file);
        }
    }

    fn load_file(&mut self, cal: usize, path: &Path) {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => return self.warn(Some(path), format!("cannot read: {e}")),
        };
        self.file_hashes
            .insert(path.to_path_buf(), crate::fsio::content_hash(&bytes));
        let text = match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(e) => {
                self.unsafe_files
                    .insert(path.to_path_buf(), "not valid UTF-8".into());
                String::from_utf8_lossy(e.as_bytes()).into_owned()
            }
        };
        let parsed = ical::parse(&text);
        if let Some(w) = parsed.warnings.first() {
            self.unsafe_files
                .entry(path.to_path_buf())
                .or_insert_with(|| format!("line {}: {}", w.line, w.message));
        }
        for w in &parsed.warnings {
            self.warn(Some(path), format!("line {}: {}", w.line, w.message));
        }

        // Group VEVENT/VTODO components by UID, preserving file order.
        let mut groups: BTreeMap<String, (Kind, Vec<Component>)> = BTreeMap::new();
        let mut order: Vec<String> = Vec::new();
        for vcal in parsed.components.iter().filter(|c| c.name == "VCALENDAR") {
            for comp in &vcal.children {
                let kind = match comp.name.as_str() {
                    "VEVENT" => Kind::Event,
                    "VTODO" => Kind::Todo,
                    _ => continue,
                };
                let uid = comp
                    .text("UID")
                    .filter(|u| !u.trim().is_empty())
                    .unwrap_or_else(|| format!("{}#{}", path.display(), order.len()));
                let entry = groups.entry(uid.clone()).or_insert_with(|| {
                    order.push(uid.clone());
                    (kind, Vec::new())
                });
                entry.1.push(comp.clone());
            }
        }
        if order.is_empty() {
            self.warn(Some(path), "no events or todos found".into());
        }
        for uid in order {
            let (kind, comps) = groups.remove(&uid).unwrap();
            let (mut masters, overrides): (Vec<_>, Vec<_>) = comps
                .into_iter()
                .partition(|c| c.prop("RECURRENCE-ID").is_none());
            let master = if !masters.is_empty() {
                if masters.len() > 1 {
                    self.warn(
                        Some(path),
                        format!("UID {uid} defined {} times; using the first", masters.len()),
                    );
                }
                masters.swap_remove(0)
            } else {
                // Orphaned overrides: show them as standalone items.
                let mut first = overrides[0].clone();
                first.properties.retain(|p| p.name != "RECURRENCE-ID");
                first
            };
            self.objects.push(Object {
                calendar: cal,
                path: path.to_path_buf(),
                uid,
                kind,
                master,
                overrides,
            });
        }
    }

    /// Midnight at the start of `date` in the home zone.
    pub fn day_start(&self, date: NaiveDate) -> DateTime<Tz> {
        local_to_zoned(self.home, date.and_time(NaiveTime::MIN))
    }

    /// Event occurrences and due todos overlapping `[from, to)`, sorted with
    /// all-day items first, then by start time.
    pub fn instances(&self, from: DateTime<Tz>, to: DateTime<Tz>) -> (Vec<Instance>, Vec<Warning>) {
        let mut out = Vec::new();
        let mut warnings = Vec::new();
        for (idx, obj) in self.objects.iter().enumerate() {
            let cal = &self.calendars[obj.calendar];
            if cal.hidden {
                continue;
            }
            match obj.kind {
                Kind::Event => {
                    let overrides: Vec<&Component> = obj.overrides.iter().collect();
                    let mut warn = |m: String| {
                        warnings.push(Warning {
                            path: Some(obj.path.clone()),
                            message: m,
                        })
                    };
                    let occ =
                        recur::expand(&obj.master, &overrides, from, to, self.home, &mut warn);
                    for o in occ {
                        let c = o.component;
                        if c.text("STATUS")
                            .is_some_and(|s| s.eq_ignore_ascii_case("CANCELLED"))
                        {
                            continue;
                        }
                        let summary = c
                            .text("SUMMARY")
                            .or_else(|| obj.master.text("SUMMARY"))
                            .unwrap_or_else(|| "(no title)".into());
                        if cal.hides(&summary) {
                            continue;
                        }
                        out.push(Instance {
                            calendar: obj.calendar,
                            object: idx,
                            kind: Kind::Event,
                            uid: obj.uid.clone(),
                            recurrence_id: o.recurrence_id,
                            location: c.text("LOCATION").filter(|s| !s.is_empty()),
                            description: c.text("DESCRIPTION").filter(|s| !s.is_empty()),
                            all_day: o.start.is_date(),
                            tentative: c
                                .text("STATUS")
                                .is_some_and(|s| s.eq_ignore_ascii_case("TENTATIVE")),
                            summary,
                            start: o.start,
                            end: o.end,
                        });
                    }
                }
                Kind::Todo => {
                    let Some(todo) = self.todo_from(idx) else {
                        continue;
                    };
                    let Some(due) = todo.due else { continue };
                    if todo.completed {
                        continue;
                    }
                    let at = due.instant(self.home);
                    let in_range = match due {
                        Moment::Date(d) => {
                            self.day_start(d) < to
                                && self.day_start(d.succ_opt().unwrap_or(d)) > from
                        }
                        Moment::DateTime(_) => at >= from && at < to,
                    };
                    if in_range && !cal.hides(&todo.summary) {
                        out.push(Instance {
                            calendar: obj.calendar,
                            object: idx,
                            kind: Kind::Todo,
                            uid: obj.uid.clone(),
                            recurrence_id: None,
                            summary: todo.summary,
                            location: None,
                            description: todo.description,
                            start: due,
                            end: due,
                            all_day: due.is_date(),
                            tentative: false,
                        });
                    }
                }
            }
        }
        out.sort_by(|a, b| {
            let key = |i: &Instance| {
                (
                    i.start.date_in(self.home),
                    !i.all_day,
                    i.start.instant(self.home),
                )
            };
            key(a).cmp(&key(b)).then_with(|| a.summary.cmp(&b.summary))
        });
        (out, warnings)
    }

    /// Instances overlapping the given calendar day.
    pub fn day(&self, date: NaiveDate) -> (Vec<Instance>, Vec<Warning>) {
        let next = date.succ_opt().unwrap_or(date);
        self.instances(self.day_start(date), self.day_start(next))
    }

    /// Whether an instance overlaps the given calendar day (home zone).
    pub fn occurs_on(&self, i: &Instance, day: NaiveDate) -> bool {
        let next = day.succ_opt().unwrap_or(day);
        let (from, to) = (self.day_start(day), self.day_start(next));
        let s = i.start.instant(self.home);
        let e = i.end.instant(self.home);
        if s == e {
            s >= from && s < to
        } else {
            s < to && e > from
        }
    }

    fn todo_from(&self, idx: usize) -> Option<Todo> {
        let obj = &self.objects[idx];
        if obj.kind != Kind::Todo {
            return None;
        }
        let c = &obj.master;
        let status = c.text("STATUS").unwrap_or_default();
        Some(Todo {
            calendar: obj.calendar,
            object: idx,
            uid: obj.uid.clone(),
            summary: c.text("SUMMARY").unwrap_or_else(|| "(no title)".into()),
            description: c.text("DESCRIPTION").filter(|s| !s.is_empty()),
            priority: c
                .text("PRIORITY")
                .and_then(|p| p.trim().parse::<u8>().ok())
                .filter(|p| *p <= 9)
                .unwrap_or(0),
            due: c.prop("DUE").and_then(|p| parse_moment(p, self.home).ok()),
            completed: status.eq_ignore_ascii_case("COMPLETED") || c.prop("COMPLETED").is_some(),
        })
    }

    /// All todos in visible calendars, open first, then by priority
    /// (undefined last) and due date.
    pub fn todos(&self) -> Vec<Todo> {
        let mut todos: Vec<Todo> = (0..self.objects.len())
            .filter_map(|i| self.todo_from(i))
            .filter(|t| !self.calendars[t.calendar].hidden)
            .collect();
        todos.sort_by_key(|t| {
            (
                t.completed,
                if t.priority == 0 { 10 } else { t.priority },
                t.due.map(|d| d.instant(self.home)),
                t.summary.clone(),
            )
        });
        todos
    }

    /// Find a calendar by folder name or display name.
    pub fn calendar_index(&self, name: &str) -> Option<usize> {
        self.calendars
            .iter()
            .position(|c| c.id == name)
            .or_else(|| {
                self.calendars
                    .iter()
                    .position(|c| c.name.eq_ignore_ascii_case(name))
            })
    }
}

/// A cheap summary of the calendar folders' state on disk, to notice changes
/// made by a sync or another program without re-reading every file.
pub type Fingerprint = Vec<(PathBuf, Option<std::time::SystemTime>, usize)>;

impl Store {
    /// Folder modification times plus, per calendar, the number of `.ics`
    /// files and their newest modification time (catches in-place writes,
    /// which do not change the folder's mtime).
    pub fn fingerprint(&self, root: &Path) -> Fingerprint {
        let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
        let mut fp = vec![(root.to_path_buf(), mtime(root), 0)];
        for cal in &self.calendars {
            let mut newest = mtime(&cal.path);
            let mut count = 0;
            if let Ok(rd) = std::fs::read_dir(&cal.path) {
                for e in rd.filter_map(|e| e.ok()) {
                    let p = e.path();
                    if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("ics")) {
                        count += 1;
                        newest = newest.max(e.metadata().and_then(|m| m.modified()).ok());
                    }
                }
            }
            fp.push((cal.path.clone(), newest, count));
        }
        fp
    }
}
