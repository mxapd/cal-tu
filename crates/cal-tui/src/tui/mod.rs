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
            "Monday 16 November · today",
            "Linear Algebra  08:15–10:00",
            "Hall A · lecture ↻  University",
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
        assert!(screen.contains("Dentist  14:00–14:45"), "{screen}");
        assert!(screen.contains("personal"), "{screen}");
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
        assert!(render(&mut app).contains("Start time: cannot understand '25:99'"));
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
        assert!(wed.contains("Climbing  18:30–20:00"), "{wed}");
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
                assert_eq!(
                    row.trim().trim_end_matches('•'),
                    d.format("%-d").to_string(),
                    "{d}"
                );
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
        assert!(render(&mut app).contains("Friday 20 November 2026"));
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
    }

    #[test]
    fn hour_grid_scrolls_over_the_whole_day() {
        let mut app = app();
        let screen = render(&mut app);
        assert!(!screen.contains(" 00:00"), "starts at the usual hours");
        assert!(screen.contains(" 08:00"));
        let (appts, _) = app.hits[0];
        // Wheel up over the appointments: an hour per notch, until 00:00.
        for _ in 0..12 {
            mouse(
                &mut app,
                MouseEventKind::ScrollUp,
                appts.x + 10,
                appts.y + 5,
            );
            render(&mut app);
        }
        let screen = render(&mut app);
        assert!(screen.contains(" 00:00"), "{screen}");
        assert_eq!(app.appt_sel, 0, "scrolling does not change the selection");
        // To the end of the day with the keyboard.
        for _ in 0..30 {
            press(&mut app, 'J');
            render(&mut app);
        }
        let screen = render(&mut app);
        assert!(screen.contains(" 23:00"), "{screen}");
        assert!(screen.contains('▲') && !screen.contains('▼'), "{screen}");
        // Moving the selection brings the view back to it.
        press(&mut app, 'j');
        assert!(render(&mut app).contains("Lunch with Anna"));
        // Another day starts at the usual position again.
        press(&mut app, 'J');
        press(&mut app, 'l');
        assert!(render(&mut app).contains(" 08:00"));
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
            screen.contains("Monday 16 November · today               ☁ cloudy, 4° now (2°–6°)"),
            "{screen}"
        );
        snapshot::assert_snapshot("tui_weather", &screen);
        press(&mut app, 'l');
        assert!(render(&mut app).contains("☂ rain 85%, 2°–5°"));
        // Beyond the forecast: just the date.
        press(&mut app, ']');
        assert!(!render(&mut app).contains("°–"));
    }

    #[test]
    fn appointment_titles_have_contrast() {
        use ratatui::style::{Color, Modifier};
        let mut app = app();
        let mut term = Terminal::new(TestBackend::new(110, 32)).unwrap();
        term.draw(|f| ui::draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer();
        // Find "Lunch with Anna" and the calendar tag after it.
        let find = |needle: &str| {
            for y in 0..buf.area.height {
                let row: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
                if let Some(i) = row.find(needle) {
                    let x = row[..i].chars().count() as u16;
                    return buf[(x, y)].clone();
                }
            }
            panic!("{needle} not on screen");
        };
        let title = find("Lunch with Anna");
        assert!(title.modifier.contains(Modifier::BOLD));
        assert_eq!(
            title.fg,
            Color::Reset,
            "normal text colour, not the calendar's"
        );
        let tag = find("personal");
        assert_ne!(tag.fg, Color::Reset, "tag carries the calendar colour");
    }

    #[test]
    fn overlapping_events_side_by_side() {
        let (_dir, mut app) = writable_app();
        // Overlaps lunch (12:00–13:00).
        press(&mut app, 'a');
        typ(&mut app, "Call");
        key(&mut app, KeyCode::Tab);
        key(&mut app, KeyCode::Tab);
        typ(&mut app, "12:30");
        key(&mut app, KeyCode::Enter);
        let screen = render(&mut app);
        let row = screen
            .lines()
            .find(|l| l.contains("Call  12:30"))
            .expect("call shown");
        assert!(
            row.contains("Café Lisa"),
            "same row, separate lanes:\n{screen}"
        );
        // Events that overlap nothing keep the full width.
        assert!(
            screen.contains("Hall A · lecture ↻  University"),
            "{screen}"
        );
        // Both are clickable.
        assert!(
            app.hits
                .iter()
                .filter(|(_, h)| matches!(h, app::Hit::Appointment(_)))
                .count()
                >= 4
        );
    }

    #[test]
    fn popup_is_a_card_over_a_dimmed_screen() {
        use ratatui::style::Color;
        let mut app = app();
        press(&mut app, '?');
        let mut term = Terminal::new(TestBackend::new(110, 32)).unwrap();
        term.draw(|f| ui::draw(f, &mut app)).unwrap();
        let card = *app
            .hits
            .iter()
            .find(|(_, h)| *h == app::Hit::Popup)
            .map(|(r, _)| r)
            .unwrap();
        let buf = term.backend().buffer();
        // No line characters around the card.
        for x in card.left()..card.right() {
            for y in [card.top(), card.bottom() - 1] {
                assert!(
                    !"─│┌┐└┘▔▁▏▕".contains(buf[(x, y)].symbol()),
                    "line at {x},{y}"
                );
            }
        }
        // Accent title bar, card background inside, dimmed screen outside.
        assert_ne!(buf[(card.x, card.y)].bg, Color::Reset);
        assert_eq!(buf[(card.x + 2, card.y + 2)].bg, Color::Indexed(235));
        assert_eq!(
            buf[(1, 1)].fg,
            Color::Indexed(240),
            "screen behind is dimmed"
        );
        // Key hints stay readable.
        assert_ne!(buf[(1, 31)].fg, Color::Indexed(240));
    }

    #[test]
    fn popup_text_is_readable() {
        use ratatui::style::Color;
        // New event, help, go to date, calendars, details, delete question.
        for keys in ["a", "?", "g", "c", "jj\n", "jd"] {
            let mut app = app();
            for k in keys.chars() {
                if k == '\n' {
                    key(&mut app, KeyCode::Enter);
                } else {
                    press(&mut app, k);
                }
            }
            assert!(app.popup.is_some(), "{keys:?} opens a pop-up");
            let mut term = Terminal::new(TestBackend::new(110, 32)).unwrap();
            term.draw(|f| ui::draw(f, &mut app)).unwrap();
            let card = *app
                .hits
                .iter()
                .find(|(_, h)| *h == app::Hit::Popup)
                .map(|(r, _)| r)
                .unwrap();
            let buf = term.backend().buffer();
            // Every visible character on the card uses a light enough colour:
            // never the terminal's dark grey or near-background greys.
            for y in card.top() + 1..card.bottom() {
                for x in card.left()..card.right() {
                    let c = &buf[(x, y)];
                    if c.symbol().trim().is_empty() {
                        continue;
                    }
                    let dark = match c.fg {
                        Color::DarkGray | Color::Black => true,
                        Color::Indexed(n) => (232..=243).contains(&n),
                        _ => false,
                    };
                    assert!(
                        !dark,
                        "{keys:?}: {:?} at {x},{y} has fg {:?}",
                        c.symbol(),
                        c.fg
                    );
                }
            }
        }
    }

    #[test]
    fn form_hints_and_placeholders_fit_the_card() {
        use form::FieldId::*;
        // Card is 72 wide: 1 cell padding each side, 1 leading space.
        for id in [
            Title, Date, Start, End, Location, Repeat, Reminder, Due, Priority, Notes, Calendar,
        ] {
            assert!(id.hint().chars().count() <= 68, "hint for {id:?} too long");
            assert!(
                11 + id.placeholder().chars().count() <= 68,
                "placeholder for {id:?} too long"
            );
        }
    }

    #[test]
    fn weather_is_coloured() {
        use ratatui::style::Color;
        let mut app = app();
        app.weather = Some(crate::weather::Weather::fixed(
            crate::weather::tests::sample(),
        ));
        let mut term = Terminal::new(TestBackend::new(110, 32)).unwrap();
        term.draw(|f| ui::draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer();
        let row: Vec<&ratatui::buffer::Cell> = (0..110).map(|x| &buf[(x, 0)]).collect();
        let at = |s: &str| {
            let text: String = row.iter().map(|c| c.symbol()).collect();
            let i = text.find(s).unwrap_or_else(|| panic!("{s} not in {text}"));
            row[text[..i].chars().count()].fg
        };
        let cloud = at("☁");
        let now = at("4° now");
        let sep = at(" now (");
        assert_ne!(cloud, now, "condition and temperature differ");
        assert_ne!(now, Color::Reset);
        assert_ne!(cloud, sep);
    }

    #[test]
    fn today_stands_out_in_the_month() {
        use ratatui::style::Color;
        let mut app = app();
        press(&mut app, 'l'); // select tomorrow, so today and the selection differ
        let mut term = Terminal::new(TestBackend::new(110, 32)).unwrap();
        term.draw(|f| ui::draw(f, &mut app)).unwrap();
        let cell_bg = |d: NaiveDate| {
            let (r, _) = *app
                .hits
                .iter()
                .find(|(_, h)| *h == app::Hit::Day(d))
                .unwrap();
            let buf = term.backend().buffer();
            (buf[(r.x + 1, r.y)].bg, buf[(r.x + 1, r.y)].fg)
        };
        let (today_bg, today_fg) = cell_bg(day(11, 16));
        let (sel_bg, _) = cell_bg(day(11, 17));
        let (plain_bg, _) = cell_bg(day(11, 18));
        assert_ne!(today_bg, Color::Reset, "today is filled");
        assert_eq!(today_fg, Color::Black);
        assert_ne!(sel_bg, today_bg, "selection looks different from today");
        assert_eq!(plain_bg, Color::Reset);
        // Week 47 (today's week) number in the same colour as today.
        let buf = term.backend().buffer();
        let (r, _) = *app
            .hits
            .iter()
            .find(|(_, h)| *h == app::Hit::Day(day(11, 16)))
            .unwrap();
        let wk: String = (r.x - 4..r.x - 1).map(|x| buf[(x, r.y)].symbol()).collect();
        assert_eq!(wk.trim(), "47");
        assert_eq!(buf[(r.x - 3, r.y)].fg, today_bg);
    }

    #[test]
    fn agenda_says_how_much_is_hidden() {
        let mut app = app();
        let screen = render(&mut app);
        let more = screen
            .lines()
            .find_map(|l| l.split("⋯ ").nth(1).map(str::to_string))
            .expect("a ⋯ N more line");
        assert!(more.contains("more from"), "{more}");
        let n: usize = more.split(' ').next().unwrap().parse().unwrap();
        assert!(
            n > 0 && n < app.upcoming.len(),
            "{n} of {}",
            app.upcoming.len()
        );
        // Clicking it (the last agenda line) goes to that day.
        let (_, last) = *app
            .hits
            .iter()
            .rev()
            .find(|(_, h)| matches!(h, app::Hit::AgendaDay(_)))
            .unwrap();
        click_on(&mut app, last);
        assert!(app.date > day(11, 16));
        // With room for everything, there is no such line.
        let tall_calendar = r#"
            [layout]
            active = "tall"
            [layout.presets.tall]
            split = "rows"
            children = [ { panel = "calendar", size = 60 }, { panel = "appointments" } ]
        "#;
        let mut big = app_with(FIXTURES, tall_calendar, Path::new("/nonexistent/x.toml"));
        let tall = render_size(&mut big, 110, 80);
        assert!(!tall.contains('⋯'), "{tall}");
    }

    /// Render the screen as HTML (screen.html) with the colours of a base16
    /// terminal theme, for previewing styling choices:
    /// `CAL_TUI_PREVIEW_OUT=/tmp/x cargo test -p cal-tui export_previews -- --ignored`
    /// (CAL_TUI_SCREEN_DIR picks a real calendars folder).
    #[test]
    #[ignore]
    fn export_previews() {
        use ratatui::style::{Color, Modifier};
        let Ok(out) = std::env::var("CAL_TUI_PREVIEW_OUT") else {
            return;
        };
        // The user's Stylix scheme (base00..base0F) and kitty's base16 mapping.
        let base = [
            "#121820", "#1b222c", "#242d38", "#2f3a47", "#9aa5b4", "#c5d0de", "#e0e6f0", "#f2f5fa",
            "#e26d6d", "#f2a76b", "#f6d37a", "#7fcf7d", "#6fc6c9", "#7aa7ff", "#c792ea", "#f28ba8",
        ];
        let ansi = [
            base[0], base[8], base[11], base[10], base[13], base[14], base[12], base[5], base[3],
            base[8], base[11], base[10], base[13], base[14], base[12], base[7],
        ];
        let indexed = |n: u8| -> String {
            match n {
                0..=15 => ansi[n as usize].to_string(),
                16..=231 => {
                    let n = n - 16;
                    let c = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                    format!("#{:02x}{:02x}{:02x}", c(n / 36), c((n / 6) % 6), c(n % 6))
                }
                _ => {
                    let g = 8 + (n - 232) * 10;
                    format!("#{g:02x}{g:02x}{g:02x}")
                }
            }
        };
        let css = |c: Color, default: &str| -> String {
            match c {
                Color::Reset => default.to_string(),
                Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
                Color::Indexed(n) => indexed(n),
                Color::Black => indexed(0),
                Color::Red => indexed(1),
                Color::Green => indexed(2),
                Color::Yellow => indexed(3),
                Color::Blue => indexed(4),
                Color::Magenta => indexed(5),
                Color::Cyan => indexed(6),
                Color::Gray => indexed(7),
                Color::DarkGray => indexed(8),
                Color::LightRed => indexed(9),
                Color::LightGreen => indexed(10),
                Color::LightYellow => indexed(11),
                Color::LightBlue => indexed(12),
                Color::LightMagenta => indexed(13),
                Color::LightCyan => indexed(14),
                Color::White => indexed(15),
            }
        };
        {
            let name = "screen";
            let mut app = match std::env::var("CAL_TUI_SCREEN_DIR") {
                Ok(dir) => app_with(
                    &dir,
                    "[calendars.lnu-timeedit]\nread_only = true\nname = \"LNU\"\ncolor = \"orange\"",
                    Path::new("/nonexistent/x.toml"),
                ),
                Err(_) => app(),
            };
            app.weather = Some(crate::weather::Weather::fixed(
                crate::weather::tests::sample(),
            ));
            let mut term = Terminal::new(TestBackend::new(110, 32)).unwrap();
            term.draw(|f| ui::draw(f, &mut app)).unwrap();
            let buf = term.backend().buffer();
            let mut html = String::new();
            for y in 0..buf.area.height {
                for x in 0..buf.area.width {
                    let c = &buf[(x, y)];
                    let (mut fg, mut bg) = (css(c.fg, base[5]), css(c.bg, "transparent"));
                    if c.modifier.contains(Modifier::REVERSED) {
                        std::mem::swap(&mut fg, &mut bg);
                    }
                    let mut style = format!("color:{fg};background:{bg};");
                    if c.modifier.contains(Modifier::BOLD) {
                        style.push_str("font-weight:700;");
                    }
                    if c.modifier.contains(Modifier::ITALIC) {
                        style.push_str("font-style:italic;");
                    }
                    if c.modifier.contains(Modifier::UNDERLINED) {
                        style.push_str("text-decoration:underline;");
                    }
                    let sym = match c.symbol() {
                        "<" => "&lt;",
                        ">" => "&gt;",
                        "&" => "&amp;",
                        s => s,
                    };
                    html.push_str(&format!("<span style=\"{style}\">{sym}</span>"));
                }
                html.push('\n');
            }
            std::fs::write(format!("{out}/{name}.html"), html).unwrap();
        }
    }

    #[test]
    fn hour_lines_do_not_show_through_events() {
        let mut app = app();
        let screen = render(&mut app);
        for line in screen.lines() {
            if line.contains("Linear Algebra") || line.contains("Lunch with Anna") {
                assert!(!line.contains('┈'), "{line}");
            }
        }
        assert!(screen.contains("┈┈┈"), "lines are drawn in free hours");
    }

    /// `cargo test -p cal-tui print_screen -- --ignored --nocapture`
    /// (set CAL_TUI_SCREEN_DIR to render a real calendars folder instead).
    #[test]
    #[ignore]
    fn print_screen() {
        let mut app = match std::env::var("CAL_TUI_SCREEN_DIR") {
            Ok(dir) => app_with(
                &dir,
                "[calendars.lnu-timeedit]\nread_only = true\nname = \"LNU\"",
                Path::new("/nonexistent/x.toml"),
            ),
            Err(_) => app(),
        };
        println!("{}", render(&mut app));
        app.focus = layout::PanelKind::Appointments;
        app.appt_view = app::ApptView::Week;
        app.refresh();
        println!("{}", render(&mut app));
    }

    #[test]
    fn narrow_terminal_uses_stacked_layout() {
        let mut app = app();
        snapshot::assert_snapshot("tui_narrow", &render_size(&mut app, 50, 40));
    }

    #[test]
    fn appointments_week_view() {
        let mut app = app();
        press(&mut app, 'V');
        assert_eq!(app.appt_view, app::ApptView::Week);
        let screen = render(&mut app);
        snapshot::assert_snapshot("tui_appt_week", &screen);
        assert!(
            screen.contains("Week 47 · 16–22 November · this week"),
            "{screen}"
        );
        assert!(
            screen.contains("Mo 16") && screen.contains("Su 22"),
            "{screen}"
        );
        // Hour lines never show through events.
        for line in screen.lines() {
            if line.contains("Linear") {
                let col: String = line.chars().skip(7).take(8).collect();
                assert!(!col.contains('┈'), "{line}");
            }
        }

        // l moves the selected column; selection and edits follow that day.
        press(&mut app, 'l');
        assert_eq!(app.date, NaiveDate::from_ymd_opt(2026, 11, 17).unwrap());
        assert_eq!(
            app.selected_instance().map(|i| i.summary.as_str()),
            Some("Floating standup")
        );
        assert!(render(&mut app).contains("Week 47"), "still the same week");

        // V again: back to the single day.
        press(&mut app, 'V');
        assert!(render(&mut app).contains("Tuesday 17 November 2026"));
    }

    #[test]
    fn week_items_cover_monday_to_sunday() {
        let mut app = app();
        press(&mut app, 'V');
        let sunday = NaiveDate::from_ymd_opt(2026, 11, 22).unwrap();
        app.date = sunday;
        app.refresh();
        let days: Vec<NaiveDate> = app.week_items.iter().map(|(d, _)| *d).collect();
        let monday = NaiveDate::from_ymd_opt(2026, 11, 16).unwrap();
        assert_eq!(
            days,
            (0..7)
                .map(|n| monday + chrono::Duration::days(n))
                .collect::<Vec<_>>()
        );
        let (_, items) = app.week_items.last().unwrap();
        assert_eq!(
            items.len(),
            app.day_items.len(),
            "selected day matches day_items"
        );
        press(&mut app, 'V');
        assert!(app.week_items.is_empty());
    }

    #[test]
    fn week_view_click_selects_day_and_item() {
        let mut app = app();
        press(&mut app, 'V');
        render(&mut app);
        let tue = NaiveDate::from_ymd_opt(2026, 11, 17).unwrap();
        let (rect, hit) = *app
            .hits
            .iter()
            .find(|(_, h)| matches!(h, app::Hit::WeekItem(d, _) if *d == tue))
            .expect("an item on Tuesday");
        app.handle_mouse(ratatui::crossterm::event::MouseEvent {
            kind: ratatui::crossterm::event::MouseEventKind::Down(
                ratatui::crossterm::event::MouseButton::Left,
            ),
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        });
        let app::Hit::WeekItem(_, i) = hit else {
            unreachable!()
        };
        assert_eq!(app.date, tue);
        assert_eq!(app.appt_sel, i);
        assert_eq!(app.focus, layout::PanelKind::Appointments);
    }

    #[test]
    fn week_view_falls_back_to_day_when_narrow() {
        let mut app = app();
        press(&mut app, 'V');
        let screen = render_size(&mut app, 50, 40);
        assert!(screen.contains("today · no room for week"), "{screen}");
    }

    #[test]
    fn help_popup() {
        let mut app = app();
        // `v` used to switch the calendar panel to a week list; the panel is
        // always the month now.
        press(&mut app, 'v');
        assert!(render(&mut app).contains("‹  November 2026  ›"));
        press(&mut app, '?');
        snapshot::assert_snapshot("tui_help", &render(&mut app));
    }

    fn keyed_app(keys: &str) -> App {
        app_with(
            FIXTURES,
            keys,
            Path::new("/nonexistent/cal-tui/layout.toml"),
        )
    }

    #[test]
    fn rebound_keys_drive_the_app_and_show_in_help_and_hints() {
        let mut app = keyed_app(
            r#"
            [keys]
            next_day = "n"
            sync = []
            [keys.popup]
            close = "Q"
            "#,
        );
        let day = app.date;
        press(&mut app, 'l');
        assert_eq!(app.date, day, "l is no longer bound");
        press(&mut app, 'n');
        assert_eq!(app.date, day + chrono::Duration::days(1));
        press(&mut app, 'h');

        let screen = render(&mut app);
        assert!(screen.contains("h/n Day"), "{screen}");
        assert!(
            !screen.contains("Sync"),
            "unbound sync has no hint: {screen}"
        );

        press(&mut app, '?');
        let screen = render(&mut app);
        assert!(screen.contains("h, ← / n"), "{screen}");
        assert!(!screen.contains("sync (runs"), "{screen}");
        assert!(screen.contains("Q Close"), "{screen}");
        press(&mut app, 'q');
        assert!(app.popup.is_some(), "q no longer closes pop-ups");
        press(&mut app, 'Q');
        assert!(app.popup.is_none());
    }

    #[test]
    fn shifted_letters_match_with_or_without_the_shift_flag() {
        let mut app = app();
        let day = app.date;
        // Terminals report 'H' with SHIFT set; bindings say just "H".
        app.handle_key(KeyEvent::new(KeyCode::Char('H'), KeyModifiers::SHIFT));
        assert_eq!(app.date, day - chrono::Duration::days(7));
        app.handle_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert_eq!(app.focus, layout::PanelKind::Todo);
    }

    #[test]
    fn rebound_form_keys() {
        let mut app = keyed_app("[keys.form]\nsave = \"ctrl+s\"\nclear = \"ctrl+w\"");
        press(&mut app, 'a');
        typ(&mut app, "Dentist");
        app.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        let Some(app::Popup::Form(form)) = &app.popup else {
            panic!("form closed")
        };
        assert_eq!(form.fields[0].input.text, "");
        key(&mut app, KeyCode::Enter);
        assert!(
            matches!(app.popup, Some(app::Popup::Form(_))),
            "Enter no longer saves"
        );
        assert!(render(&mut app).contains("Ctrl-S Save"));
    }

    #[test]
    fn bad_keys_fall_back_to_defaults_with_a_message() {
        let mut app = keyed_app("[keys]\nnext_day = \"hyper+x\"");
        assert!(
            app.message
                .as_deref()
                .unwrap()
                .starts_with("Ignoring [keys] in config: keys.next_day"),
            "{:?}",
            app.message
        );
        let day = app.date;
        press(&mut app, 'l');
        assert_eq!(app.date, day + chrono::Duration::days(1));
    }
}
