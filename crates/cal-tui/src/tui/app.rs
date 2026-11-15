//! TUI state and key handling (no drawing).

use cal_core::store::Fingerprint;
use cal_core::{Config, EditError, Instance, Kind, Scope, Store, Todo, Warning};
use chrono::{DateTime, Datelike, Duration, Months, NaiveDate, Utc};
use chrono_tz::Tz;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use super::form::{Form, Submit};
use super::layout::{Node, PanelKind};
use cal_core::layout::{LayoutConfig, BUILTIN};
use ratatui::layout::Rect;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalView {
    Month,
    Week,
}

/// An edit or delete waiting for the user to pick a scope or confirm.
#[derive(Debug, Clone)]
pub enum Pending {
    Edit(Instance),
    Delete(Instance),
    DeleteTodo(Todo),
}

#[derive(Debug, Clone)]
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
    /// Add/edit form.
    Form(Box<Form>),
    /// "This occurrence or all?" for repeating events.
    Scope(Pending),
    /// "Delete …? y/n".
    Confirm(Pending),
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
    /// The layout in use (possibly changed in layout mode).
    pub layout: Node,
    /// Preset it came from, and whether it was changed since.
    pub layout_name: String,
    pub layout_modified: bool,
    /// Layout mode (`w`): keys rearrange panels.
    pub layout_mode: bool,
    layout_cfg: LayoutConfig,
    layout_state: LayoutConfig,
    layout_state_path: PathBuf,
    /// Set while drawing: the main area and the panels shown in it.
    pub main_area: Rect,
    pub shown_panels: Vec<PanelKind>,
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
    fingerprint: Fingerprint,
}

impl App {
    pub fn new(config: Config, store: Store) -> App {
        Self::with_layout_state(config, store, Config::state_dir().join("layout.toml"))
    }

    /// Like `new`, with layouts saved from the TUI kept at `state_path`.
    pub fn with_layout_state(config: Config, store: Store, state_path: PathBuf) -> App {
        let today = Utc::now().with_timezone(&store.home).date_naive();
        let mut startup_message = None;
        // Invalid layout settings must not stop the app: fall back and say so.
        let layout_cfg = config.layout().unwrap_or_else(|e| {
            startup_message = Some(format!("Ignoring [layout] in config: {e}"));
            LayoutConfig::default()
        });
        let layout_state = LayoutConfig::load_state(&state_path).unwrap_or_else(|e| {
            startup_message = Some(format!("Ignoring saved layout: {e}"));
            LayoutConfig::default()
        });
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
            message: startup_message,
            layout: Node::builtin("classic").expect("built-in"),
            layout_name: "classic".into(),
            layout_modified: false,
            layout_mode: false,
            layout_cfg,
            layout_state,
            layout_state_path: state_path,
            main_area: Rect::default(),
            shown_panels: Vec::new(),
            now_override: None,
            day_items: Vec::new(),
            todos: Vec::new(),
            marks: BTreeMap::new(),
            warnings: Vec::new(),
            next_up: None,
            marks_range: None,
            fingerprint: Vec::new(),
        };
        let active = app
            .layout_cfg
            .active
            .clone()
            .or_else(|| app.layout_state.active.clone())
            .unwrap_or_else(|| "classic".into());
        app.set_preset(&active);
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

    /// State of the calendar folders on disk (see `Store::fingerprint`).
    fn compute_fingerprint(&self) -> Fingerprint {
        self.store.fingerprint(&self.config.calendars_dir())
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
        if self.layout_mode {
            self.handle_layout_key(key);
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
            KeyCode::Char('w') => self.layout_mode = true,
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
            KeyCode::Char('a') => self.start_add(),
            KeyCode::Char('e') => self.start_edit(),
            KeyCode::Char('d') => self.start_delete(),
            KeyCode::Char('x') | KeyCode::Char(' ') => self.toggle_todo_done(),
            KeyCode::Char('+') => self.bump_priority(true),
            KeyCode::Char('-') => self.bump_priority(false),
            _ => {}
        }
        Action::None
    }

    // --- editing -------------------------------------------------------------

    /// The selected item as a todo: from the todo panel, or a due todo in the
    /// appointments panel.
    fn target_todo(&self) -> Option<Todo> {
        match self.focus {
            PanelKind::Todo => self.selected_todo().cloned(),
            PanelKind::Appointments => {
                let i = self.selected_instance()?;
                (i.kind == Kind::Todo)
                    .then(|| {
                        self.store
                            .todos()
                            .into_iter()
                            .find(|t| t.object == i.object)
                    })
                    .flatten()
            }
            PanelKind::Calendar => None,
        }
    }

    /// The selected event occurrence (appointments panel).
    fn target_event(&self) -> Option<Instance> {
        (self.focus == PanelKind::Appointments)
            .then(|| self.selected_instance().cloned())
            .flatten()
            .filter(|i| i.kind == Kind::Event)
    }

    /// Check the item can be edited; sets a message if not.
    fn editable(&mut self, object: usize) -> bool {
        match self.store.can_edit(object) {
            Ok(()) => true,
            Err(e) => {
                self.message = Some(format!("Cannot edit: {e:#}"));
                false
            }
        }
    }

    fn default_calendar(&self) -> Option<usize> {
        self.config
            .default_calendar
            .as_deref()
            .and_then(|c| self.store.calendar_index(c))
    }

    fn start_add(&mut self) {
        if self.store.writable_calendars().is_empty() {
            self.message =
                Some("No writable calendar: create a folder in the calendars directory".into());
            return;
        }
        let form = if self.focus == PanelKind::Todo {
            Form::new_todo(&self.store, self.default_calendar())
        } else {
            Form::new_event(&self.store, self.date, self.default_calendar())
        };
        self.popup = Some(Popup::Form(Box::new(form)));
    }

    fn start_edit(&mut self) {
        if let Some(t) = self.target_todo() {
            if self.editable(t.object) {
                self.popup = Some(Popup::Form(Box::new(Form::edit_todo(&self.store, &t))));
            }
        } else if let Some(i) = self.target_event() {
            if !self.editable(i.object) {
                return;
            }
            self.popup = Some(if i.recurrence_id.is_some() {
                Popup::Scope(Pending::Edit(i))
            } else {
                Popup::Form(Box::new(Form::edit_event(&self.store, &i, Scope::All)))
            });
        }
    }

    fn start_delete(&mut self) {
        if let Some(t) = self.target_todo() {
            if self.editable(t.object) {
                self.popup = Some(Popup::Confirm(Pending::DeleteTodo(t)));
            }
        } else if let Some(i) = self.target_event() {
            if !self.editable(i.object) {
                return;
            }
            self.popup = Some(if i.recurrence_id.is_some() {
                Popup::Scope(Pending::Delete(i))
            } else {
                Popup::Confirm(Pending::Delete(i))
            });
        }
    }

    /// Continue an edit/delete after the scope was chosen or confirmed.
    fn run_pending(&mut self, pending: Pending, scope: Scope) {
        match pending {
            Pending::Edit(i) => {
                self.popup = Some(Popup::Form(Box::new(Form::edit_event(
                    &self.store,
                    &i,
                    scope,
                ))));
            }
            Pending::Delete(i) => {
                let what = match scope {
                    Scope::All if i.recurrence_id.is_some() => "Deleted all occurrences",
                    Scope::All => "Deleted",
                    Scope::Occurrence(_) => "Deleted this occurrence",
                };
                let _ = self.write(what, |s, now| s.delete(i.object, scope, now));
            }
            Pending::DeleteTodo(t) => {
                let _ = self.write("Deleted todo", |s, now| s.delete(t.object, Scope::All, now));
            }
        }
    }

    fn toggle_todo_done(&mut self) {
        let Some(t) = self.target_todo() else { return };
        if !self.editable(t.object) {
            return;
        }
        let done = !t.completed;
        let msg = if done {
            "Marked done"
        } else {
            "Marked not done"
        };
        let _ = self.write(msg, |s, now| s.set_todo_completed(t.object, done, now));
    }

    /// `+` raises the priority (towards 1), `-` lowers it (towards none).
    fn bump_priority(&mut self, up: bool) {
        let Some(t) = self.target_todo() else { return };
        if !self.editable(t.object) {
            return;
        }
        let p = match (up, t.priority) {
            (true, 0) => 9,
            (true, p) => p.saturating_sub(1).max(1),
            (false, 0) | (false, 9) => 0,
            (false, p) => p + 1,
        };
        if p == t.priority {
            return;
        }
        let edit = cal_core::TodoEdit {
            priority: Some(p),
            ..Default::default()
        };
        let msg = if p == 0 {
            "Priority: none".to_string()
        } else {
            format!("Priority: {p}")
        };
        let _ = self.write(&msg, |s, now| s.update_todo(t.object, &edit, now));
    }

    /// Save a submitted form. On error the form stays open with the message.
    fn submit(&mut self, form: &Form) -> Result<(), String> {
        match form.submit(self.now())? {
            Submit::Nothing => {
                self.message = Some("No changes".into());
                Ok(())
            }
            Submit::NewEvent(cal, ev) => {
                self.write("Added", |s, now| s.create_event(cal, &ev, now).map(|_| ()))
            }
            Submit::EditEvent {
                object,
                scope,
                seen_start,
                edit,
            } => self.write("Saved", |s, now| {
                s.update_event(object, scope, seen_start, &edit, now)
            }),
            Submit::NewTodo(cal, t) => self.write("Added todo", |s, now| {
                s.create_todo(cal, &t, now).map(|_| ())
            }),
            Submit::EditTodo { object, edit } => {
                self.write("Saved", |s, now| s.update_todo(object, &edit, now))
            }
        }
    }

    /// Run a store change, then reload. A conflict (the file changed on disk)
    /// reloads and asks the user to retry; other errors are returned.
    fn write(
        &mut self,
        ok: &str,
        f: impl FnOnce(&Store, DateTime<Utc>) -> anyhow::Result<()>,
    ) -> Result<(), String> {
        let now = self.now().with_timezone(&Utc);
        match f(&self.store, now) {
            Ok(()) => {
                self.reload();
                self.message = Some(ok.to_string());
                Ok(())
            }
            Err(e) => match e.downcast_ref::<EditError>() {
                Some(EditError::Conflict(_)) => {
                    self.reload();
                    self.message = Some(
                        "The item changed on disk (sync?). Reloaded; please make your change again."
                            .into(),
                    );
                    Ok(())
                }
                _ => {
                    let msg = format!("{e:#}");
                    self.message = Some(msg.clone());
                    Err(msg)
                }
            },
        }
    }

    // --- layouts ---------------------------------------------------------------

    /// Preset names in cycling order: built-in, then saved, then config.
    pub fn preset_names(&self) -> Vec<String> {
        let mut names: Vec<String> = BUILTIN.iter().map(|s| s.to_string()).collect();
        for n in self
            .layout_state
            .presets
            .keys()
            .chain(self.layout_cfg.presets.keys())
        {
            if !names.contains(n) {
                names.push(n.clone());
            }
        }
        names
    }

    /// A preset by name: config beats saved beats built-in.
    pub fn preset(&self, name: &str) -> Option<Node> {
        self.layout_cfg
            .presets
            .get(name)
            .or_else(|| self.layout_state.presets.get(name))
            .cloned()
            .or_else(|| Node::builtin(name))
    }

    fn set_preset(&mut self, name: &str) {
        match self.preset(name) {
            Some(n) => {
                self.layout = n;
                self.layout_name = name.to_string();
            }
            None => {
                self.message = Some(format!("No layout preset named '{name}'; using classic"));
                self.layout = Node::builtin("classic").expect("built-in");
                self.layout_name = "classic".into();
            }
        }
        self.layout_modified = false;
    }

    /// The layout to draw in a main area `width` cells wide.
    pub fn displayed_layout(&self, width: u16) -> Node {
        let (below, preset) = self
            .layout_cfg
            .narrow
            .clone()
            .or_else(|| self.layout_state.narrow.clone())
            .unwrap_or((70, "stacked".into()));
        if !self.layout_mode && width < below {
            if let Some(n) = self.preset(&preset) {
                return n;
            }
        }
        self.layout.clone()
    }

    fn handle_layout_key(&mut self, key: KeyEvent) {
        use super::layout as l;
        let panel = self.focus;
        let area = self.main_area;
        let changed = match key.code {
            KeyCode::Esc | KeyCode::Char('w') | KeyCode::Char('q') => {
                self.layout_mode = false;
                return;
            }
            KeyCode::Tab => {
                self.cycle_focus(1);
                return;
            }
            KeyCode::BackTab => {
                self.cycle_focus(-1);
                return;
            }
            KeyCode::Right | KeyCode::Char('n') | KeyCode::Left | KeyCode::Char('p') => {
                let names = self.preset_names();
                let i = names
                    .iter()
                    .position(|n| *n == self.layout_name)
                    .unwrap_or(0) as i32;
                let step = if matches!(key.code, KeyCode::Left | KeyCode::Char('p')) {
                    -1
                } else {
                    1
                };
                let next = names[(i + step).rem_euclid(names.len() as i32) as usize].clone();
                self.set_preset(&next);
                self.fix_focus();
                return;
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                l::resize(&mut self.layout, panel, true, 5, area)
            }
            KeyCode::Char('-') => l::resize(&mut self.layout, panel, true, -5, area),
            KeyCode::Char('>') => l::resize(&mut self.layout, panel, false, 5, area),
            KeyCode::Char('<') => l::resize(&mut self.layout, panel, false, -5, area),
            KeyCode::Char('s') => l::swap(&mut self.layout, panel),
            KeyCode::Char('r') => l::rotate(&mut self.layout, panel),
            KeyCode::Char('x') => {
                let ok = l::hide(&mut self.layout, panel);
                self.fix_focus();
                ok
            }
            KeyCode::Char('u') => l::unhide(&mut self.layout),
            KeyCode::Enter => {
                self.save_layout();
                return;
            }
            _ => false,
        };
        if changed {
            self.layout_modified = true;
        }
    }

    /// Keep the focus on a panel that is in the layout.
    fn fix_focus(&mut self) {
        let panels = self.layout.panels();
        if !panels.contains(&self.focus) {
            self.focus = panels.first().copied().unwrap_or(PanelKind::Appointments);
        }
    }

    /// Save the current layout to the state file and make it the default.
    fn save_layout(&mut self) {
        let name = if self.layout_modified || !self.preset_names().contains(&self.layout_name) {
            "custom".to_string()
        } else {
            self.layout_name.clone()
        };
        let mut state = self.layout_state.clone();
        if self.layout_modified {
            state.presets.insert(name.clone(), self.layout.clone());
        }
        state.active = Some(name.clone());
        match state.save_state(&self.layout_state_path) {
            Ok(()) => {
                self.layout_state = state;
                self.layout_name = name.clone();
                self.layout_modified = false;
                self.layout_mode = false;
                self.message = Some(match &self.layout_cfg.active {
                    Some(a) => format!(
                        "Saved layout '{name}', but your config sets layout.active = \"{a}\", which takes precedence at startup"
                    ),
                    None => format!("Saved layout '{name}'"),
                });
            }
            Err(e) => {
                self.message = Some(format!(
                    "Cannot save layout to {}: {e}",
                    self.layout_state_path.display()
                ))
            }
        }
    }

    fn cycle_focus(&mut self, dir: i32) {
        let all: Vec<PanelKind> = if self.shown_panels.is_empty() {
            PanelKind::ALL.to_vec()
        } else {
            self.shown_panels.clone()
        };
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
            && !matches!(popup, Popup::Goto { .. } | Popup::Form(_));
        if close || (key.code == KeyCode::Esc) {
            if let Popup::Calendars { .. } = popup {
                self.apply_visibility();
                self.refresh();
            }
            return;
        }
        self.popup = Some(match popup {
            Popup::Form(mut form) => {
                let ctrl_s =
                    key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s');
                if key.code == KeyCode::Enter || ctrl_s {
                    match self.submit(&form) {
                        Ok(()) => return,
                        Err(e) => form.error = Some(e),
                    }
                } else {
                    form.handle_key(key);
                }
                Popup::Form(form)
            }
            Popup::Scope(pending) => {
                let scope = match (key.code, &pending) {
                    (KeyCode::Char('o' | 'O'), Pending::Edit(i) | Pending::Delete(i)) => {
                        i.recurrence_id.map(Scope::Occurrence)
                    }
                    (KeyCode::Char('a' | 'A'), _) => Some(Scope::All),
                    _ => None,
                };
                match scope {
                    Some(scope) => {
                        self.run_pending(pending, scope);
                        return;
                    }
                    None => Popup::Scope(pending),
                }
            }
            Popup::Confirm(pending) => match key.code {
                KeyCode::Char('y' | 'Y') => {
                    self.run_pending(pending, Scope::All);
                    return;
                }
                KeyCode::Char('n' | 'N') => return,
                _ => Popup::Confirm(pending),
            },
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
