//! Full-screen terminal UI.

mod app;
mod form;
mod layout;
mod ui;

#[cfg(test)]
#[path = "../../tests/support/snapshot.rs"]
mod snapshot;

use std::time::{Duration, Instant};

use anyhow::Result;
use cal_core::{Config, Store};
use chrono::Timelike;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind,
};
use ratatui::crossterm::execute;
use ratatui::DefaultTerminal;

use app::{Action, App};

/// How often to check the calendar folders for changes made by a sync.
const CHANGE_CHECK: Duration = Duration::from_secs(3);

pub fn run(config: Config, now: Option<chrono::DateTime<chrono_tz::Tz>>) -> Result<()> {
    let store = Store::load(&config)?;
    let mut app = App::new(config, store);
    if let Some(now) = now {
        app.now_override = Some(now);
        app.date = now.date_naive();
        app.refresh();
    }
    app.weather = app
        .config
        .weather
        .as_ref()
        .map(|w| crate::weather::Weather::new(w, Some(crate::weather::cache_path())));
    let mouse = app.config.mouse.unwrap_or(true);
    if mouse {
        // Stop mouse reporting if we crash, or the shell gets mouse codes.
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = execute!(std::io::stdout(), DisableMouseCapture);
            previous(info);
        }));
    }
    let mut terminal = enter(mouse);
    let result = event_loop(&mut terminal, &mut app);
    leave(mouse);
    result
}

/// Start full-screen mode (and mouse reporting).
fn enter(mouse: bool) -> DefaultTerminal {
    let terminal = ratatui::init();
    if mouse {
        let _ = execute!(std::io::stdout(), EnableMouseCapture);
    }
    terminal
}

fn leave(mouse: bool) {
    if mouse {
        let _ = execute!(std::io::stdout(), DisableMouseCapture);
    }
    ratatui::restore();
}

fn event_loop(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    let mut last_check = Instant::now();
    let mut last_minute = app.now().minute();
    loop {
        terminal.draw(|f| ui::draw(f, app))?;

        if event::poll(Duration::from_millis(500))? {
            let action = match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => app.handle_key(key),
                Event::Mouse(m) => app.handle_mouse(m),
                _ => Action::None,
            };
            match action {
                Action::Quit => return Ok(()),
                Action::Sync => run_sync(terminal, app)?,
                Action::None => {}
            }
        }

        if let Some(w) = &mut app.weather {
            w.tick();
        }
        if last_check.elapsed() >= CHANGE_CHECK {
            app.check_for_changes();
            last_check = Instant::now();
        }
        let minute = app.now().minute();
        if minute != last_minute {
            last_minute = minute;
            app.update_next_up();
        }
    }
}

/// Leave the TUI, run the configured sync command in the terminal, come back.
fn run_sync(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    let Some(cmd) = app.config.sync_command.clone() else {
        app.message = Some("No sync_command set in the config".into());
        return Ok(());
    };
    let mouse = app.config.mouse.unwrap_or(true);
    leave(mouse);
    println!("Running: {cmd}\n");
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .status();
    let ok = matches!(&status, Ok(s) if s.success());
    if !ok {
        match &status {
            Ok(s) => println!("\nSync failed ({s})."),
            Err(e) => println!("\nCould not run sync command: {e}"),
        }
        println!("Press Enter to return.");
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
    }
    *terminal = enter(mouse);
    app.reload();
    app.message = Some(if ok {
        "Sync finished".into()
    } else {
        "Sync failed".into()
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, TimeZone};
    use chrono_tz::Europe::Stockholm;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::Terminal;
    use std::path::Path;

    const FIXTURES: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../cal-core/tests/fixtures/calendars"
    );

    fn app() -> App {
        app_at(FIXTURES)
    }

    /// An app on a private, writable copy of the fixtures.
    fn writable_app() -> (tempfile::TempDir, App) {
        fn copy(from: &std::path::Path, to: &std::path::Path) {
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
        copy(FIXTURES.as_ref(), dir.path());
        let app = app_at(dir.path().to_str().unwrap());
        (dir, app)
    }

    fn app_at(dir: &str) -> App {
        // A state path that never exists, so a user's saved layout can't leak in.
        app_with(dir, "", Path::new("/nonexistent/cal-tui/layout.toml"))
    }

    /// `extra` is appended to the config (put tables after the uni table).
    fn app_with(dir: &str, extra: &str, layout_state: &Path) -> App {
        let config: Config = toml::from_str(&format!(
            r#"
            calendars_dir = "{dir}"
            timezone = "Europe/Stockholm"
            [calendars.uni]
            read_only = true
            hide_matching = ["^Tutorial group B"]
            {extra}
            "#
        ))
        .unwrap();
        let store = Store::load_dir(dir.as_ref(), &config, Stockholm);
        let mut app = App::with_layout_state(config, store, layout_state.to_path_buf());
        app.now_override = Some(Stockholm.with_ymd_and_hms(2026, 11, 16, 11, 32, 0).unwrap());
        app.date = NaiveDate::from_ymd_opt(2026, 11, 16).unwrap();
        app.refresh();
        app
    }

    fn render(app: &mut App) -> String {
        render_size(app, 110, 32)
    }

    fn render_size(app: &mut App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| ui::draw(f, app)).unwrap();
        let buf = term.backend().buffer();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn press(app: &mut App, c: char) {
        app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }

    #[test]
    fn renders_day_calendar_todo_and_status() {
        let mut app = app();
        let screen = render(&mut app);
        for want in [
            "Monday, November 16, 2026",
            "08:15 -> 10:00",
            "Linear Algebra, lecture · Hall A",
            "Lunch with Anna",
            "November 2026",
            "Pay rent",
            "Fix bike",
            "next: Lunch with Anna in 28m",
        ] {
            assert!(screen.contains(want), "missing {want:?} in:\n{screen}");
        }
        assert!(!screen.contains("Tutorial group B"));
        assert!(
            !screen.contains("Old task"),
            "completed todo hidden by default"
        );
        snapshot::assert_snapshot("tui_main", &screen);
    }

    #[test]
    fn navigation_and_calendar_toggle() {
        let mut app = app();
        press(&mut app, 'l');
        assert!(render(&mut app).contains("Floating standup"));
        press(&mut app, 'h');

        // Hide the University calendar via the calendars popup.
        press(&mut app, 'c');
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        press(&mut app, ' ');
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let screen = render(&mut app);
        assert!(!screen.contains("Linear Algebra"), "{screen}");
        assert!(screen.contains("Lunch with Anna"));
    }

    #[test]
    fn goto_and_details_popup() {
        let mut app = app();
        press(&mut app, 'g');
        for c in "10-29".chars() {
            press(&mut app, c);
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.date, NaiveDate::from_ymd_opt(2026, 10, 29).unwrap());
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let screen = render(&mut app);
        assert!(screen.contains("Climbing (moved)"));
        assert!(screen.contains("yes (part of a series)"), "{screen}");
        snapshot::assert_snapshot("tui_details_override", &screen);
    }

    fn key(app: &mut App, code: KeyCode) {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn typ(app: &mut App, s: &str) {
        for c in s.chars() {
            press(app, c);
        }
    }

    fn clear_field(app: &mut App) {
        app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    }

    #[test]
    fn add_event_with_the_form() {
        let (_dir, mut app) = writable_app();
        press(&mut app, 'a');
        typ(&mut app, "Dentist");
        key(&mut app, KeyCode::Tab); // date: prefilled with the selected day
        key(&mut app, KeyCode::Tab);
        typ(&mut app, "14:00");
        key(&mut app, KeyCode::Tab);
        typ(&mut app, "45m");
        snapshot::assert_snapshot("tui_form_new_event", &render(&mut app));
        key(&mut app, KeyCode::Enter);
        assert!(app.popup.is_none(), "form closed");
        let screen = render(&mut app);
        assert!(screen.contains("14:00 -> 14:45"), "{screen}");
        assert!(screen.contains("Dentist  [personal]"), "{screen}");
        assert!(screen.contains("Added"), "{screen}");
    }

    #[test]
    fn form_errors_keep_the_form_open() {
        let (_dir, mut app) = writable_app();
        press(&mut app, 'a');
        key(&mut app, KeyCode::Enter);
        let screen = render(&mut app);
        assert!(screen.contains("Title: required"), "{screen}");
        typ(&mut app, "Thing");
        key(&mut app, KeyCode::Tab);
        key(&mut app, KeyCode::Tab);
        typ(&mut app, "25:99");
        key(&mut app, KeyCode::Enter);
        assert!(render(&mut app).contains("Start: cannot understand '25:99'"));
        // Typing q in a form is text, not "close".
        press(&mut app, 'q');
        assert!(matches!(app.popup, Some(app::Popup::Form(_))));
        key(&mut app, KeyCode::Esc);
        assert!(app.popup.is_none());
    }

    #[test]
    fn edit_one_occurrence_of_a_series() {
        let (_dir, mut app) = writable_app();
        // Items on Mon 16 Nov: lecture, lunch, climbing.
        press(&mut app, 'j');
        press(&mut app, 'j');
        press(&mut app, 'e');
        snapshot::assert_snapshot("tui_scope_prompt", &render(&mut app));
        press(&mut app, 'o');
        clear_field(&mut app);
        typ(&mut app, "Climbing with Sam");
        snapshot::assert_snapshot("tui_form_edit_occurrence", &render(&mut app));
        key(&mut app, KeyCode::Enter);
        assert!(render(&mut app).contains("Climbing with Sam"));
        press(&mut app, 'l');
        press(&mut app, 'l');
        let wed = render(&mut app);
        assert!(wed.contains("Climbing  [personal]"), "{wed}");
        assert!(!wed.contains("Climbing with Sam"), "{wed}");
    }

    #[test]
    fn delete_asks_first() {
        let (_dir, mut app) = writable_app();
        press(&mut app, 'j'); // lunch
        press(&mut app, 'd');
        snapshot::assert_snapshot("tui_confirm_delete", &render(&mut app));
        press(&mut app, 'n');
        assert!(render(&mut app).contains("Lunch with Anna"));
        press(&mut app, 'd');
        press(&mut app, 'y');
        let screen = render(&mut app);
        assert!(!screen.contains("Lunch with Anna"), "{screen}");
        assert!(screen.contains("Deleted"));
    }

    #[test]
    fn delete_one_occurrence() {
        let (_dir, mut app) = writable_app();
        press(&mut app, 'j');
        press(&mut app, 'j'); // climbing
        press(&mut app, 'd');
        press(&mut app, 'o');
        let today = |app: &App| app.day_items.iter().any(|i| i.summary == "Climbing");
        assert!(!today(&app));
        press(&mut app, 'l');
        press(&mut app, 'l');
        assert!(today(&app), "next one remains");
    }

    #[test]
    fn read_only_items_cannot_be_edited() {
        let (_dir, mut app) = writable_app();
        press(&mut app, 'e'); // lecture, University (read-only)
        assert!(app.popup.is_none());
        let screen = render(&mut app);
        assert!(
            screen.contains("Cannot edit: calendar 'University' is read-only"),
            "{screen}"
        );
        press(&mut app, 'd');
        assert!(app.popup.is_none());
    }

    #[test]
    fn todo_keys() {
        let (_dir, mut app) = writable_app();
        key(&mut app, KeyCode::Tab);
        key(&mut app, KeyCode::Tab);
        assert_eq!(app.focus, layout::PanelKind::Todo);
        press(&mut app, 'x'); // Pay rent done
        let screen = render(&mut app);
        assert!(!screen.contains("Pay rent"), "{screen}");
        press(&mut app, '+'); // Fix bike 5 -> 4
        assert!(render(&mut app).contains("4. Fix bike"));
        press(&mut app, 'a');
        typ(&mut app, "Buy milk");
        key(&mut app, KeyCode::Tab);
        typ(&mut app, "tomorrow");
        key(&mut app, KeyCode::Tab);
        clear_field(&mut app);
        typ(&mut app, "2");
        key(&mut app, KeyCode::Enter);
        let screen = render(&mut app);
        assert!(screen.contains("2. Buy milk 17 Nov"), "{screen}");
    }

    #[test]
    fn conflict_reloads_instead_of_overwriting() {
        let (dir, mut app) = writable_app();
        press(&mut app, 'j'); // lunch
        press(&mut app, 'e');
        clear_field(&mut app);
        typ(&mut app, "Lunch (mine)");
        // Meanwhile a sync changes the file.
        let path = dir.path().join("personal/lunch.ics");
        let theirs = std::fs::read_to_string(&path)
            .unwrap()
            .replace("Lunch with Anna", "Lunch (theirs)");
        std::fs::write(&path, &theirs).unwrap();
        key(&mut app, KeyCode::Enter);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), theirs);
        let screen = render(&mut app);
        assert!(screen.contains("Lunch (theirs)"), "{screen}");
        assert!(screen.contains("changed on disk"), "{screen}");
    }

    #[test]
    fn layout_mode_presets_and_arranging() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("layout.toml");
        let mut app = app_with(FIXTURES, "", &state);
        render(&mut app);
        press(&mut app, 'w');
        key(&mut app, KeyCode::Right);
        assert_eq!(app.layout_name, "classic-left");
        snapshot::assert_snapshot("tui_layout_mode", &render(&mut app));
        key(&mut app, KeyCode::Right);
        assert_eq!(app.layout_name, "three-columns");
        snapshot::assert_snapshot("tui_layout_three_columns", &render(&mut app));

        // Back to classic; hide the todo panel, make appointments narrower.
        key(&mut app, KeyCode::Left);
        key(&mut app, KeyCode::Left);
        assert_eq!(app.layout_name, "classic");
        render(&mut app);
        key(&mut app, KeyCode::Tab);
        key(&mut app, KeyCode::Tab);
        assert_eq!(app.focus, layout::PanelKind::Todo);
        press(&mut app, 'x');
        assert_eq!(
            app.focus,
            layout::PanelKind::Appointments,
            "focus moves off a hidden panel"
        );
        press(&mut app, '-');
        assert!(app.layout_modified);
        let screen = render(&mut app);
        assert!(
            !app.shown_panels.contains(&layout::PanelKind::Todo),
            "{screen}"
        );
        assert!(screen.contains("classic (changed)"));

        // Save: becomes "custom" and the default next time.
        key(&mut app, KeyCode::Enter);
        assert!(!app.layout_mode);
        assert!(render(&mut app).contains("Saved layout 'custom'"));
        let saved = std::fs::read_to_string(&state).unwrap();
        assert!(saved.contains("active = \"custom\""), "{saved}");
        let mut again = app_with(FIXTURES, "", &state);
        assert_eq!(again.layout_name, "custom");
        render(&mut again);
        assert!(!again.shown_panels.contains(&layout::PanelKind::Todo));
    }

    #[test]
    fn config_layout_beats_saved_layout() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("layout.toml");
        std::fs::write(&state, "[layout]\nactive = \"three-columns\"\n").unwrap();
        // Saved layout used when the config says nothing.
        assert_eq!(app_with(FIXTURES, "", &state).layout_name, "three-columns");
        // A config preset (e.g. from Nix) wins.
        let cfg = r#"
            [layout]
            active = "agenda"
            [layout.presets.agenda]
            split = "rows"
            children = [ { panel = "appointments" }, { panel = "todo", size = 8 } ]
        "#;
        let mut app = app_with(FIXTURES, cfg, &state);
        assert_eq!(app.layout_name, "agenda");
        let screen = render(&mut app);
        assert!(
            !app.shown_panels.contains(&layout::PanelKind::Calendar),
            "no calendar panel:\n{screen}"
        );
        snapshot::assert_snapshot("tui_config_layout", &screen);
        // Saving says that the config still decides.
        press(&mut app, 'w');
        key(&mut app, KeyCode::Enter);
        assert!(render(&mut app).contains("takes precedence"));
    }

    #[test]
    fn broken_layout_settings_do_not_stop_the_app() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("layout.toml");
        std::fs::write(&state, "[layout]\nactive = 42\n").unwrap();
        let mut app = app_with(FIXTURES, "", &state);
        assert_eq!(app.layout_name, "classic");
        assert!(render(&mut app).contains("Ignoring saved layout"));
    }

    #[test]
    fn narrow_preset_from_config() {
        let cfg = r#"
            [layout]
            narrow = { below_width = 120, preset = "three-columns" }
        "#;
        let mut app = app_with(FIXTURES, cfg, Path::new("/nonexistent/x.toml"));
        // 110 columns is "narrow" here, so three-columns is used.
        let screen = render(&mut app);
        assert_eq!(app.shown_panels[0], layout::PanelKind::Calendar, "{screen}");
    }

    fn mouse(app: &mut App, kind: MouseEventKind, x: u16, y: u16) {
        app.handle_mouse(MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        });
    }

    /// Click the middle of the region registered for `hit`.
    fn click_on(app: &mut App, hit: app::Hit) -> ratatui::layout::Rect {
        let (r, _) = *app
            .hits
            .iter()
            .rev()
            .find(|(_, h)| *h == hit)
            .unwrap_or_else(|| panic!("{hit:?} not on screen"));
        mouse(
            app,
            MouseEventKind::Down(MouseButton::Left),
            r.x + r.width / 2,
            r.y,
        );
        r
    }

    fn day(m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, m, d).unwrap()
    }

    #[test]
    fn click_days_in_the_month_grid() {
        let mut app = app();
        let screen = render(&mut app);
        let rows: Vec<&str> = screen.lines().collect();
        // Every day's click region covers its number on screen.
        for (r, h) in app.hits.clone() {
            if let app::Hit::Day(d) = h {
                let row: String = rows[r.y as usize]
                    .chars()
                    .skip(r.x as usize)
                    .take(r.width as usize)
                    .collect();
                assert_eq!(row.trim(), d.format("%-d").to_string(), "{d}");
            }
        }
        click_on(&mut app, app::Hit::Day(day(11, 20)));
        assert_eq!(
            (app.date, app.focus),
            (day(11, 20), layout::PanelKind::Calendar)
        );
        // A dimmed day of the next month switches month.
        render(&mut app);
        click_on(&mut app, app::Hit::Day(day(12, 3)));
        assert_eq!(app.date, day(12, 3));
        assert!(render(&mut app).contains("December 2026"));
    }

    #[test]
    fn click_agenda_line_goes_to_that_day() {
        let mut app = app();
        render(&mut app);
        click_on(&mut app, app::Hit::AgendaDay(day(11, 20)));
        assert_eq!(
            (app.date, app.focus),
            (day(11, 20), layout::PanelKind::Appointments)
        );
        assert!(render(&mut app).contains("Friday, November 20, 2026"));
    }

    #[test]
    fn click_items_select_then_open() {
        let mut app = app();
        render(&mut app);
        click_on(&mut app, app::Hit::Appointment(2));
        assert_eq!(app.appt_sel, 2);
        assert!(app.popup.is_none());
        render(&mut app);
        click_on(&mut app, app::Hit::Appointment(2));
        assert!(matches!(app.popup, Some(app::Popup::Details)));
        // A click outside the details pop-up closes it.
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), 0, 0);
        assert!(app.popup.is_none());

        render(&mut app);
        click_on(&mut app, app::Hit::Todo(1));
        assert_eq!((app.focus, app.todo_sel), (layout::PanelKind::Todo, 1));
    }

    #[test]
    fn scroll_wheel() {
        let mut app = app();
        render(&mut app);
        let (cal, _) = *app
            .hits
            .iter()
            .find(|(_, h)| *h == app::Hit::Panel(layout::PanelKind::Calendar))
            .unwrap();
        mouse(&mut app, MouseEventKind::ScrollDown, cal.x + 2, cal.y + 2);
        assert_eq!(app.date, day(12, 16));
        mouse(&mut app, MouseEventKind::ScrollUp, cal.x + 2, cal.y + 2);
        assert_eq!(app.date, day(11, 16));
        let (appts, _) = app.hits[0];
        mouse(
            &mut app,
            MouseEventKind::ScrollDown,
            appts.x + 2,
            appts.y + 3,
        );
        assert_eq!(app.appt_sel, 1);
    }

    #[test]
    fn click_in_popups() {
        let mut app = app();
        press(&mut app, 'c');
        render(&mut app);
        click_on(&mut app, app::Hit::CalendarToggle(1)); // University
        assert!(app.store.calendars[1].hidden);
        assert!(matches!(app.popup, Some(app::Popup::Calendars { .. })));
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), 0, 0);
        assert!(app.popup.is_none());
        assert!(!render(&mut app).contains("Linear Algebra"));

        // Forms are not closed by a stray click.
        press(&mut app, 'a');
        render(&mut app);
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), 0, 0);
        assert!(matches!(app.popup, Some(app::Popup::Form(_))));
    }

    #[test]
    fn month_arrows() {
        let mut app = app();
        let screen = render(&mut app);
        let rows: Vec<&str> = screen.lines().collect();
        for (hit, arrow) in [(app::Hit::Prev, "‹"), (app::Hit::Next, "›")] {
            let (r, _) = *app.hits.iter().find(|(_, h)| *h == hit).unwrap();
            let cells: String = rows[r.y as usize]
                .chars()
                .skip(r.x as usize)
                .take(r.width as usize)
                .collect();
            assert!(cells.contains(arrow), "{hit:?} region {cells:?}");
        }
        click_on(&mut app, app::Hit::Prev);
        assert_eq!(app.date, day(10, 16));
        assert!(render(&mut app).contains("‹  October 2026  ›"));
        click_on(&mut app, app::Hit::Next);
        render(&mut app);
        click_on(&mut app, app::Hit::Next);
        assert_eq!(app.date, day(12, 16));
        // Week view: a week at a time.
        press(&mut app, 'v');
        render(&mut app);
        click_on(&mut app, app::Hit::Next);
        assert_eq!(app.date, day(12, 23));
    }

    #[test]
    fn drag_divider_resizes_and_saves() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("layout.toml");
        let mut app = app_with(FIXTURES, "", &state);
        render(&mut app);
        let hit = app::Hit::Divider(layout::PanelKind::Calendar, layout::Dir::Rows);
        let (r, _) = *app.hits.iter().rev().find(|(_, h)| *h == hit).unwrap();
        let cal_before = app
            .hits
            .iter()
            .find(|(_, h)| *h == app::Hit::Panel(layout::PanelKind::Calendar))
            .unwrap()
            .0
            .height;
        // Drag the calendar/todo divider 5 rows down and let go.
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            r.x + 5,
            r.y,
        );
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            r.x + 5,
            r.y + 3,
        );
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            r.x + 5,
            r.y + 5,
        );
        mouse(
            &mut app,
            MouseEventKind::Up(MouseButton::Left),
            r.x + 5,
            r.y + 5,
        );
        let screen = render(&mut app);
        let cal_after = app
            .hits
            .iter()
            .find(|(_, h)| *h == app::Hit::Panel(layout::PanelKind::Calendar))
            .unwrap()
            .0
            .height;
        assert_eq!(cal_after, cal_before + 5, "{screen}");
        assert!(screen.contains("Saved layout 'custom'"), "{screen}");
        // Kept next time.
        let mut again = app_with(FIXTURES, "", &state);
        render(&mut again);
        assert_eq!(again.layout, app.layout);

        // The vertical divider next to appointments works too.
        let hit = app::Hit::Divider(layout::PanelKind::Appointments, layout::Dir::Columns);
        let (r, _) = *app.hits.iter().rev().find(|(_, h)| *h == hit).unwrap();
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            r.x,
            r.y + 3,
        );
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            r.x - 10,
            r.y + 3,
        );
        mouse(
            &mut app,
            MouseEventKind::Up(MouseButton::Left),
            r.x - 10,
            r.y + 3,
        );
        render(&mut app);
        let appts = app
            .hits
            .iter()
            .find(|(_, h)| *h == app::Hit::Panel(layout::PanelKind::Appointments))
            .unwrap()
            .0;
        assert_eq!(appts.right(), r.x - 10, "divider follows the mouse");
    }

    #[test]
    fn weather_next_to_the_date() {
        let mut app = app();
        // Without [weather] nothing is shown.
        assert!(!render(&mut app).contains("now ("));
        app.weather = Some(crate::weather::Weather::fixed(
            crate::weather::tests::sample(),
        ));
        let screen = render(&mut app);
        assert!(
            screen.contains("Monday, November 16, 2026   ☁ cloudy, 4° now (2°–6°)"),
            "{screen}"
        );
        snapshot::assert_snapshot("tui_weather", &screen);
        press(&mut app, 'l');
        assert!(render(&mut app).contains("Tuesday, November 17, 2026   ☂ rain 85%, 2°–5°"));
        // Beyond the forecast: just the date.
        press(&mut app, ']');
        assert!(!render(&mut app).contains("°–"));
    }

    /// `cargo test -p cal-tui print_screen -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn print_screen() {
        let mut app = app();
        println!("{}", render(&mut app));
        app.focus = layout::PanelKind::Calendar;
        app.cal_view = app::CalView::Week;
        app.refresh();
        println!("{}", render(&mut app));
    }

    #[test]
    fn narrow_terminal_uses_stacked_layout() {
        let mut app = app();
        snapshot::assert_snapshot("tui_narrow", &render_size(&mut app, 50, 40));
    }

    #[test]
    fn week_view_and_help() {
        let mut app = app();
        press(&mut app, 'v');
        snapshot::assert_snapshot("tui_week", &render(&mut app));
        press(&mut app, '?');
        snapshot::assert_snapshot("tui_help", &render(&mut app));
    }
}
