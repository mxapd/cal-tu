//! Full-screen terminal UI.

mod app;
mod layout;
mod ui;

use std::time::{Duration, Instant};

use anyhow::Result;
use cal_core::{Config, Store};
use chrono::Timelike;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::DefaultTerminal;

use app::{Action, App};

/// How often to check the calendar folders for changes made by a sync.
const CHANGE_CHECK: Duration = Duration::from_secs(3);

pub fn run(config: Config) -> Result<()> {
    let store = Store::load(&config)?;
    let mut app = App::new(config, store);
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app);
    ratatui::restore();
    result
}

fn event_loop(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    let mut last_check = Instant::now();
    let mut last_minute = app.now().minute();
    loop {
        terminal.draw(|f| ui::draw(f, app))?;

        if event::poll(Duration::from_millis(500))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match app.handle_key(key) {
                        Action::Quit => return Ok(()),
                        Action::Sync => run_sync(terminal, app)?,
                        Action::None => {}
                    }
                }
            }
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
    ratatui::restore();
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
    *terminal = ratatui::init();
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
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;

    fn app() -> App {
        let dir = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../cal-core/tests/fixtures/calendars"
        );
        let config: Config = toml::from_str(&format!(
            r#"
            calendars_dir = "{dir}"
            [calendars.uni]
            read_only = true
            hide_matching = ["^Tutorial group B"]
            "#
        ))
        .unwrap();
        let store = Store::load_dir(dir.as_ref(), &config, Stockholm);
        let mut app = App::new(config, store);
        app.now_override = Some(Stockholm.with_ymd_and_hms(2026, 11, 16, 11, 32, 0).unwrap());
        app.date = NaiveDate::from_ymd_opt(2026, 11, 16).unwrap();
        app.refresh();
        app
    }

    fn render(app: &mut App) -> String {
        let mut term = Terminal::new(TestBackend::new(110, 32)).unwrap();
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
            "Mo Tu We Th Fr Sa Su",
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
        let mut term = Terminal::new(TestBackend::new(50, 40)).unwrap();
        term.draw(|f| ui::draw(f, &mut app)).unwrap();
    }
}
