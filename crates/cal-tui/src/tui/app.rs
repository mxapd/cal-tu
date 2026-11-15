//! TUI state and key handling (no drawing).

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::SystemTime;

use cal_core::{Config, Instance, Kind, Store, Todo, Warning};
use chrono::{DateTime, Datelike, Duration, Months, NaiveDate, Utc};
use chrono_tz::Tz;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::layout::{Node, PanelKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalView {
    Month,
    Week,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Popup {
    Help,
    /// Details of the selected appointment or todo.
    Details,
    Calendars {
        sel: usize,
    },
    Goto {
        input: String,
        error: Option<String>,
    },
    Warnings {
        scroll: usize,
    },
}

/// What the run loop has to do after a key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    Quit,
    /// Suspend the TUI, run the sync command, reload.
    Sync,
}

pub struct App {
    pub config: Config,
    pub store: Store,
    /// Calendars hidden with the toggle (by folder name).
    pub toggled_off: BTreeSet<String>,
    pub date: NaiveDate,
    pub focus: PanelKind,
    pub cal_view: CalView,
    pub show_completed: bool,
    pub appt_sel: usize,
    pub todo_sel: usize,
    pub popup: Option<Popup>,
    pub message: Option<String>,
    pub layout: Node,
    /// Fixed "now" for tests.
    pub now_override: Option<DateTime<Tz>>,

    // Derived data, rebuilt by `refresh`.
    pub day_items: Vec<Instance>,
    pub todos: Vec<Todo>,
    /// Calendars with items on each day of the visible calendar range.
    pub marks: BTreeMap<NaiveDate, Vec<usize>>,
    pub warnings: Vec<Warning>,
    pub next_up: Option<(String, DateTime<Tz>)>,
    marks_range: Option<(NaiveDate, NaiveDate)>,
    fingerprint: Vec<(PathBuf, Option<SystemTime>)>,
}

impl App {
    pub fn new(config: Config, store: Store) -> App {
        let today = Utc::now().with_timezone(&store.home).date_naive();
        let mut app = App {
            config,
            store,
            toggled_off: BTreeSet::new(),
            date: today,
            focus: PanelKind::Appointments,
            cal_view: CalView::Month,
            show_completed: false,
            appt_sel: 0,
            todo_sel: 0,
            popup: None,
            message: None,
            layout: Node::classic(),
            now_override: None,
            day_items: Vec::new(),
            todos: Vec::new(),
            marks: BTreeMap::new(),
            warnings: Vec::new(),
            next_up: None,
            marks_range: None,
            fingerprint: Vec::new(),
        };
        app.fingerprint = app.compute_fingerprint();
        app.apply_visibility();
        app.refresh();
        app
    }

    pub fn now(&self) -> DateTime<Tz> {
        self.now_override
            .unwrap_or_else(|| Utc::now().with_timezone(&self.store.home))
    }

    pub fn today(&self) -> NaiveDate {
        self.now().date_naive()
    }

    fn apply_visibility(&mut self) {
        for cal in &mut self.store.calendars {
            let cfg_hidden = self.config.calendars.get(&cal.id).is_some_and(|c| c.hidden);
            cal.hidden = cfg_hidden || self.toggled_off.contains(&cal.id);
        }
        self.marks_range = None;
    }

    /// First and last day shown by the calendar panel.
    pub fn visible_range(&self) -> (NaiveDate, NaiveDate) {
        match self.cal_view {
            CalView::Month => {
                let first = self.date.with_day(1).unwrap();
                let start = first - Duration::days(first.weekday().num_days_from_monday() as i64);
                (start, start + Duration::days(41))
            }
            CalView::Week => {
                let start =
                    self.date - Duration::days(self.date.weekday().num_days_from_monday() as i64);
                (start, start + Duration::days(6))
            }
        }
    }

    /// Recompute derived data after the date, visibility or store changed.
    pub fn refresh(&mut self) {
        let (items, mut warnings) = self.store.day(self.date);
        self.day_items = items;
        self.appt_sel = self.appt_sel.min(self.day_items.len().saturating_sub(1));

        self.todos = self
            .store
            .todos()
            .into_iter()
            .filter(|t| self.show_completed || !t.completed)
            .collect();
        self.todo_sel = self.todo_sel.min(self.todos.len().saturating_sub(1));

        let range = self.visible_range();
        if self.marks_range != Some(range) {
            let (from, to) = range;
            let (inst, w) = self.store.instances(
                self.store.day_start(from),
                self.store.day_start(to + Duration::days(1)),
            );
            warnings.extend(w);
            let mut marks: BTreeMap<NaiveDate, Vec<usize>> = BTreeMap::new();
            for i in &inst {
                let mut d = i.start.date_in(self.store.home).max(from);
                let last = i.end.date_in(self.store.home).min(to);
                while d <= last {
                    if self.store.occurs_on(i, d) {
                        let cals = marks.entry(d).or_default();
                        if !cals.contains(&i.calendar) {
                            cals.push(i.calendar);
                        }
                    }
                    d += Duration::days(1);
                }
            }
            self.marks = marks;
            self.marks_range = Some(range);
        }

        self.warnings = self.store.warnings.clone();
        self.warnings.extend(warnings);
        self.warnings
            .dedup_by(|a, b| a.path == b.path && a.message == b.message);
        self.update_next_up();
    }

    /// Next timed event starting within 24 hours, for the status bar.
    pub fn update_next_up(&mut self) {
        let now = self.now();
        let (inst, _) = self.store.instances(now, now + Duration::days(1));
        self.next_up = inst
            .into_iter()
            .filter(|i| !i.all_day && i.kind == Kind::Event)
            .map(|i| (i.summary, i.start.instant(self.store.home)))
            .find(|(_, start)| *start >= now);
    }

    /// Reload everything from disk.
    pub fn reload(&mut self) {
        match Store::load(&self.config) {
            Ok(store) => {
                self.store = store;
                self.fingerprint = self.compute_fingerprint();
                self.apply_visibility();
                self.refresh();
            }
            Err(e) => self.message = Some(format!("reload failed: {e:#}")),
        }
    }

    /// Modification times of the calendars directory and each calendar
    /// folder. vdirsyncer writes files atomically (rename), which updates the
    /// folder's mtime.
    fn compute_fingerprint(&self) -> Vec<(PathBuf, Option<SystemTime>)> {
        let mtime = |p: &std::path::Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
        let root = self.config.calendars_dir();
        let mut fp = vec![(root.clone(), mtime(&root))];
        for cal in &self.store.calendars {
            fp.push((cal.path.clone(), mtime(&cal.path)));
        }
        fp
    }

    /// Reload if the calendar folders changed on disk. Returns true if so.
    pub fn check_for_changes(&mut self) -> bool {
        if self.compute_fingerprint() != self.fingerprint {
            self.reload();
            self.message = Some("Reloaded: calendars changed on disk".into());
            true
        } else {
            false
        }
    }

    fn set_date(&mut self, d: NaiveDate) {
        if d != self.date {
            self.date = d;
            self.appt_sel = 0;
            self.refresh();
        }
    }

    fn shift_days(&mut self, n: i64) {
        self.set_date(self.date + Duration::days(n));
    }

    fn shift_months(&mut self, n: i32) {
        let d = if n >= 0 {
            self.date.checked_add_months(Months::new(n as u32))
        } else {
            self.date.checked_sub_months(Months::new((-n) as u32))
        };
        if let Some(d) = d {
            self.set_date(d);
        }
    }

    pub fn selected_instance(&self) -> Option<&Instance> {
        self.day_items.get(self.appt_sel)
    }

    pub fn selected_todo(&self) -> Option<&Todo> {
        self.todos.get(self.todo_sel)
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Action {
        self.message = None;
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Action::Quit;
        }
        if self.popup.is_some() {
            self.handle_popup_key(key);
            return Action::None;
        }
        match key.code {
            KeyCode::Char('q') => return Action::Quit,
            KeyCode::Char('?') => self.popup = Some(Popup::Help),
            KeyCode::Tab => self.cycle_focus(1),
            KeyCode::BackTab => self.cycle_focus(-1),
            KeyCode::Char('h') | KeyCode::Left => self.shift_days(-1),
            KeyCode::Char('l') | KeyCode::Right => self.shift_days(1),
            KeyCode::Char('H') => self.shift_days(-7),
            KeyCode::Char('L') => self.shift_days(7),
            KeyCode::Char('[') => self.shift_months(-1),
            KeyCode::Char(']') => self.shift_months(1),
            KeyCode::Char('j') | KeyCode::Down => self.move_vertical(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_vertical(-1),
            KeyCode::Char('t') => self.set_date(self.today()),
            KeyCode::Char('g') => {
                self.popup = Some(Popup::Goto {
                    input: String::new(),
                    error: None,
                })
            }
            KeyCode::Char('c') => self.popup = Some(Popup::Calendars { sel: 0 }),
            KeyCode::Char('v') => {
                self.cal_view = match self.cal_view {
                    CalView::Month => CalView::Week,
                    CalView::Week => CalView::Month,
                };
                self.refresh();
            }
            KeyCode::Char('C') => {
                self.show_completed = !self.show_completed;
                self.refresh();
            }
            KeyCode::Char('W') => self.popup = Some(Popup::Warnings { scroll: 0 }),
            KeyCode::Char('R') => {
                self.reload();
                self.message = Some("Reloaded".into());
            }
            KeyCode::Char('S') => return Action::Sync,
            KeyCode::Enter => {
                let has = match self.focus {
                    PanelKind::Todo => self.selected_todo().is_some(),
                    _ => self.selected_instance().is_some(),
                };
                if has {
                    self.popup = Some(Popup::Details);
                }
            }
            KeyCode::Char('a' | 'e' | 'd') => {
                self.message = Some("Editing comes in milestone 3".into());
            }
            _ => {}
        }
        Action::None
    }

    fn cycle_focus(&mut self, dir: i32) {
        let all = PanelKind::ALL;
        let i = all.iter().position(|p| *p == self.focus).unwrap_or(0) as i32;
        self.focus = all[(i + dir).rem_euclid(all.len() as i32) as usize];
    }

    fn move_vertical(&mut self, dir: i32) {
        match self.focus {
            PanelKind::Calendar => self.shift_days(7 * dir as i64),
            PanelKind::Appointments => {
                self.appt_sel = step(self.appt_sel, dir, self.day_items.len());
            }
            PanelKind::Todo => {
                self.todo_sel = step(self.todo_sel, dir, self.todos.len());
            }
        }
    }

    fn handle_popup_key(&mut self, key: KeyEvent) {
        let Some(popup) = self.popup.take() else {
            return;
        };
        let close = matches!(key.code, KeyCode::Esc | KeyCode::Char('q'))
            && !matches!(popup, Popup::Goto { .. });
        if close || (key.code == KeyCode::Esc) {
            if let Popup::Calendars { .. } = popup {
                self.apply_visibility();
                self.refresh();
            }
            return;
        }
        self.popup = Some(match popup {
            Popup::Help | Popup::Details => {
                if matches!(key.code, KeyCode::Enter | KeyCode::Char('?')) {
                    return;
                }
                popup
            }
            Popup::Warnings { scroll } => match key.code {
                KeyCode::Char('j') | KeyCode::Down => Popup::Warnings { scroll: scroll + 1 },
                KeyCode::Char('k') | KeyCode::Up => Popup::Warnings {
                    scroll: scroll.saturating_sub(1),
                },
                KeyCode::Enter => return,
                _ => popup,
            },
            Popup::Calendars { sel } => {
                let n = self.store.calendars.len();
                match key.code {
                    KeyCode::Char('j') | KeyCode::Down => Popup::Calendars {
                        sel: step(sel, 1, n),
                    },
                    KeyCode::Char('k') | KeyCode::Up => Popup::Calendars {
                        sel: step(sel, -1, n),
                    },
                    KeyCode::Char(' ') | KeyCode::Enter => {
                        if let Some(cal) = self.store.calendars.get(sel) {
                            let id = cal.id.clone();
                            if !self.toggled_off.remove(&id) {
                                self.toggled_off.insert(id);
                            }
                            self.apply_visibility();
                            self.refresh();
                        }
                        Popup::Calendars { sel }
                    }
                    _ => popup,
                }
            }
            Popup::Goto { mut input, .. } => match key.code {
                KeyCode::Enter => match parse_goto(&input, self.date, self.today()) {
                    Some(d) => {
                        self.set_date(d);
                        return;
                    }
                    None => Popup::Goto {
                        error: Some(format!("Cannot understand '{input}'")),
                        input,
                    },
                },
                KeyCode::Backspace => {
                    input.pop();
                    Popup::Goto { input, error: None }
                }
                KeyCode::Char(c) => {
                    input.push(c);
                    Popup::Goto { input, error: None }
                }
                _ => Popup::Goto { input, error: None },
            },
        });
    }
}

fn step(cur: usize, dir: i32, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    (cur as i64 + dir as i64).clamp(0, len as i64 - 1) as usize
}

/// Parse a goto target: `YYYY-MM-DD`, `MM-DD`, `DD`, `+N`/`-N` (days),
/// `today`.
pub fn parse_goto(input: &str, current: NaiveDate, today: NaiveDate) -> Option<NaiveDate> {
    let s = input.trim();
    if s.eq_ignore_ascii_case("today") || s.is_empty() {
        return Some(today);
    }
    if let Some(n) = s.strip_prefix('+').and_then(|n| n.parse::<i64>().ok()) {
        return current.checked_add_signed(Duration::days(n));
    }
    if let Some(n) = s.strip_prefix('-').and_then(|n| n.parse::<i64>().ok()) {
        return current.checked_sub_signed(Duration::days(n));
    }
    let parts: Vec<&str> = s.split(['-', '/', '.']).collect();
    let num = |p: &str| p.parse::<u32>().ok();
    match parts.as_slice() {
        [y, m, d] => NaiveDate::from_ymd_opt(y.parse().ok()?, num(m)?, num(d)?),
        [m, d] => NaiveDate::from_ymd_opt(current.year(), num(m)?, num(d)?),
        [d] => NaiveDate::from_ymd_opt(current.year(), current.month(), num(d)?),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goto_parsing() {
        let cur = NaiveDate::from_ymd_opt(2026, 11, 16).unwrap();
        let today = NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();
        let d = |y, m, d| NaiveDate::from_ymd_opt(y, m, d);
        assert_eq!(parse_goto("2027-01-04", cur, today), d(2027, 1, 4));
        assert_eq!(parse_goto("12-24", cur, today), d(2026, 12, 24));
        assert_eq!(parse_goto("3", cur, today), d(2026, 11, 3));
        assert_eq!(parse_goto("+10", cur, today), d(2026, 11, 26));
        assert_eq!(parse_goto("-16", cur, today), d(2026, 10, 31));
        assert_eq!(parse_goto("today", cur, today), Some(today));
        assert_eq!(parse_goto("2026-02-30", cur, today), None);
        assert_eq!(parse_goto("soon", cur, today), None);
    }
}
