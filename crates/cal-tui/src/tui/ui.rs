//! Drawing the TUI.

use cal_core::{Kind, Moment};
use chrono::{Datelike, Duration, NaiveDate};
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};
use ratatui::Frame;

use super::app::{App, CalView, Popup};
use super::layout::{Node, PanelKind};
use crate::fmt::{humanize, parse_color, time_range, Rgb, TimeRange};

/// Width below which the stacked layout is used.
const NARROW: u16 = 70;

pub fn draw(f: &mut Frame, app: &mut App) {
    let [main, status, keys] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(f.area());

    let layout = if main.width < NARROW {
        Node::stacked()
    } else {
        app.layout.clone()
    };
    let mut panels = Vec::new();
    layout.resolve(main, &mut panels);
    for (kind, area) in panels {
        match kind {
            PanelKind::Appointments => draw_appointments(f, app, area),
            PanelKind::Calendar => draw_calendar(f, app, area),
            PanelKind::Todo => draw_todo(f, app, area),
        }
    }
    draw_status(f, app, status);
    draw_keys(f, app, keys);

    match app.popup.clone() {
        Some(Popup::Help) => draw_help(f),
        Some(Popup::Details) => draw_details(f, app),
        Some(Popup::Calendars { sel }) => draw_calendars(f, app, sel),
        Some(Popup::Goto { input, error }) => draw_goto(f, &input, error.as_deref()),
        Some(Popup::Warnings { scroll }) => draw_warnings(f, app, scroll),
        None => {}
    }
}

pub fn color(c: Option<&str>) -> Option<Color> {
    parse_color(c).map(|c| match c {
        Rgb::True(r, g, b) => Color::Rgb(r, g, b),
        Rgb::Named(n) => Color::Indexed(n),
    })
}

fn cal_style(app: &App, cal: usize) -> Style {
    match color(app.store.calendars[cal].color.as_deref()) {
        Some(c) => Style::new().fg(c),
        None => Style::new(),
    }
}

fn panel_block(app: &App, kind: PanelKind, title: String) -> Block<'static> {
    let focused = app.focus == kind && app.popup.is_none();
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(Line::from(format!(" {title} ")).alignment(Alignment::Center));
    if focused {
        block
            .border_style(Style::new().fg(Color::Cyan))
            .title_style(Style::new().bold())
    } else {
        block.border_style(Style::new().dark_gray())
    }
}

/// Render `lines` into the block, scrolled so that rows `sel` are visible.
fn scrolled(
    lines: Vec<Line<'static>>,
    sel: Option<(usize, usize)>,
    height: usize,
) -> Vec<Line<'static>> {
    let offset = match sel {
        Some((first, last)) if last >= height => (last + 1).saturating_sub(height).min(first),
        _ => 0,
    };
    lines.into_iter().skip(offset).collect()
}

fn draw_appointments(f: &mut Frame, app: &mut App, area: Rect) {
    let title = if app.date == app.today() {
        "Appointments · today".to_string()
    } else {
        "Appointments".to_string()
    };
    let block = panel_block(app, PanelKind::Appointments, title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let focused = app.focus == PanelKind::Appointments;
    let home = app.store.home;
    let mut lines: Vec<Line> = vec![
        Line::from(app.date.format(" %A, %B %-d, %Y").to_string()).bold(),
        Line::from(""),
    ];
    let mut sel_rows = None;

    if app.day_items.is_empty() {
        lines.push(Line::from("  nothing scheduled").dark_gray());
    }

    let mut timed_started = false;
    for (idx, inst) in app.day_items.iter().enumerate() {
        let cal = &app.store.calendars[inst.calendar];
        let selected = idx == app.appt_sel;
        let hl = |s: Style| {
            if selected && focused {
                s.add_modifier(Modifier::REVERSED)
            } else if selected {
                s.add_modifier(Modifier::UNDERLINED)
            } else {
                s
            }
        };
        let bullet = if cal.read_only { "○" } else { "●" };
        let mut flags = String::new();
        if inst.recurrence_id.is_some() {
            flags.push('*');
        }
        if inst.description.is_some() {
            flags.push('>');
        }
        let tag = Span::styled(format!("  [{}]", cal.name), Style::new().dark_gray());
        let summary = Span::styled(inst.summary.clone(), hl(cal_style(app, inst.calendar)));
        let range = time_range(inst, app.date, home);
        let start_row = lines.len();

        match range {
            TimeRange::AllDay | TimeRange::Due(_) => {
                let lead = if inst.kind == Kind::Todo {
                    "[ ]"
                } else {
                    bullet
                };
                let mut spans = vec![
                    Span::styled(format!(" {lead} "), cal_style(app, inst.calendar)),
                    summary,
                ];
                if let TimeRange::Due(Some(t)) = &range {
                    spans.push(Span::raw(format!(" (due {t})")).dark_gray());
                }
                spans.push(tag);
                lines.push(Line::from(spans));
            }
            _ => {
                if !timed_started && idx > 0 {
                    lines.push(Line::from(" ─────").dark_gray());
                }
                timed_started = true;
                lines.push(Line::from(vec![
                    Span::styled(format!(" {bullet} "), cal_style(app, inst.calendar)),
                    Span::styled(range.arrow(), hl(Style::new())),
                    Span::raw(format!(" {flags}")).dark_gray(),
                ]));
                let mut spans = vec![Span::raw("   "), summary];
                if let Some(loc) = &inst.location {
                    spans.push(Span::raw(format!(" · {loc}")).dark_gray());
                }
                spans.push(tag);
                lines.push(Line::from(spans));
            }
        }
        if selected {
            sel_rows = Some((start_row, lines.len() - 1));
        }
    }

    let lines = scrolled(lines, sel_rows, inner.height as usize);
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_calendar(f: &mut Frame, app: &mut App, area: Rect) {
    let title = match app.cal_view {
        CalView::Month => app.date.format("%B %Y").to_string(),
        CalView::Week => format!(
            "Week {} · {}",
            app.date.iso_week().week(),
            app.date.format("%b %Y")
        ),
    };
    let block = panel_block(app, PanelKind::Calendar, title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let today = app.today();
    let focused = app.focus == PanelKind::Calendar;
    let day_style = |d: NaiveDate| {
        let mut s = match app.marks.get(&d).and_then(|c| c.first()) {
            Some(cal) => cal_style(app, *cal).bold(),
            None => Style::new(),
        };
        if d == today {
            s = s.add_modifier(Modifier::UNDERLINED | Modifier::BOLD);
        }
        if d == app.date {
            s = if focused {
                s.add_modifier(Modifier::REVERSED)
            } else {
                s.bg(Color::DarkGray)
            };
        }
        s
    };

    let lines: Vec<Line> = match app.cal_view {
        CalView::Month => {
            let (start, _) = app.visible_range();
            let mut lines = vec![Line::from("Mo Tu We Th Fr Sa Su").dark_gray()];
            for week in 0..6 {
                let mut spans = Vec::new();
                for wd in 0..7 {
                    let d = start + Duration::days(week * 7 + wd);
                    if d.month() != app.date.month() {
                        spans.push(Span::raw("   "));
                        continue;
                    }
                    spans.push(Span::styled(format!("{:>2}", d.day()), day_style(d)));
                    spans.push(Span::raw(" "));
                }
                lines.push(Line::from(spans));
            }
            lines
        }
        CalView::Week => {
            let (start, _) = app.visible_range();
            (0..7)
                .map(|n| {
                    let d = start + Duration::days(n);
                    let mut spans = vec![
                        Span::styled(d.format("%a %d").to_string(), day_style(d)),
                        Span::raw("  "),
                    ];
                    for cal in app.marks.get(&d).into_iter().flatten().take(6) {
                        spans.push(Span::styled("●", cal_style(app, *cal)));
                    }
                    Line::from(spans)
                })
                .collect()
        }
    };
    // Centre the 20-column grid.
    let width = 20u16.min(inner.width);
    let x = inner.x + (inner.width - width) / 2;
    let grid = Rect::new(x, inner.y, inner.width - (x - inner.x), inner.height);
    f.render_widget(Paragraph::new(lines), grid);
}

fn draw_todo(f: &mut Frame, app: &mut App, area: Rect) {
    let title = if app.show_completed {
        "TODO (all)"
    } else {
        "TODO"
    };
    let block = panel_block(app, PanelKind::Todo, title.to_string());
    let inner = block.inner(area);
    f.render_widget(block, area);

    let focused = app.focus == PanelKind::Todo;
    let mut lines = Vec::new();
    if app.todos.is_empty() {
        lines.push(Line::from(" no todos").dark_gray());
    }
    for (idx, t) in app.todos.iter().enumerate() {
        let mut style = cal_style(app, t.calendar);
        if t.completed {
            style = style.add_modifier(Modifier::CROSSED_OUT | Modifier::DIM);
        }
        if idx == app.todo_sel && focused {
            style = style.add_modifier(Modifier::REVERSED);
        }
        let prio = if t.completed {
            "x".to_string()
        } else if t.priority == 0 {
            "-".to_string()
        } else {
            t.priority.to_string()
        };
        let mut spans = vec![
            Span::raw(format!(" {prio}. ")).dark_gray(),
            Span::styled(t.summary.clone(), style),
        ];
        if let Some(due) = t.due {
            let d = due.date_in(app.store.home);
            let overdue = !t.completed && d < app.today();
            let label = Span::raw(format!(" {}", d.format("%d %b")));
            spans.push(if overdue {
                label.red()
            } else {
                label.dark_gray()
            });
        }
        lines.push(Line::from(spans));
    }
    let lines = scrolled(
        lines,
        Some((app.todo_sel, app.todo_sel)),
        inner.height as usize,
    );
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let now = app.now();
    let bar = Style::new().bg(Color::Indexed(236)).fg(Color::Gray);
    let mut spans = vec![
        Span::raw(format!(" {} ", now.format("%a %d %b %Y"))),
        Span::raw("│ "),
        Span::raw(format!("{} ", now.format("%H:%M"))).bold(),
        Span::raw("│ "),
    ];
    for cal in app.store.calendars.iter() {
        let style = match color(cal.color.as_deref()) {
            Some(c) => Style::new().fg(c),
            None => Style::new(),
        };
        if cal.hidden {
            spans.push(
                Span::raw(format!("○ {} ", cal.name))
                    .dark_gray()
                    .crossed_out(),
            );
        } else {
            spans.push(Span::styled("● ", style));
            spans.push(Span::raw(format!("{} ", cal.name)));
        }
    }
    if let Some((summary, start)) = &app.next_up {
        spans.push(Span::raw("│ next: "));
        spans.push(Span::raw(summary.clone()).bold());
        spans.push(Span::raw(format!(" in {} ", humanize(*start - now))));
    }
    if !app.warnings.is_empty() {
        spans.push(Span::raw(format!("│ ⚠ {} (W) ", app.warnings.len())).yellow());
    }
    f.render_widget(Paragraph::new(Line::from(spans)).style(bar), area);
}

fn draw_keys(f: &mut Frame, app: &App, area: Rect) {
    if let Some(msg) = &app.message {
        f.render_widget(Paragraph::new(format!(" {msg}")).yellow(), area);
        return;
    }
    let hints: &[(&str, &str)] = match &app.popup {
        Some(Popup::Calendars { .. }) => &[("j/k", "Move"), ("Space", "Toggle"), ("Esc", "Close")],
        Some(Popup::Goto { .. }) => &[("Enter", "Go"), ("Esc", "Cancel")],
        Some(_) => &[("Esc", "Close")],
        None => &[
            ("?", "Help"),
            ("Tab", "Panel"),
            ("h/l", "Day"),
            ("j/k", "Move"),
            ("[/]", "Month"),
            ("g", "Goto"),
            ("t", "Today"),
            ("Enter", "View"),
            ("c", "Calendars"),
            ("S", "Sync"),
            ("q", "Quit"),
        ],
    };
    let mut spans = vec![Span::raw(" ")];
    for (k, label) in hints {
        spans.push(Span::raw(*k).bold().cyan());
        spans.push(Span::raw(format!(" {label}  ")));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn popup_area(f: &Frame, width: u16, height: u16) -> Rect {
    let a = f.area();
    let w = width.min(a.width.saturating_sub(2));
    let h = height.min(a.height.saturating_sub(2));
    Rect::new(a.x + (a.width - w) / 2, a.y + (a.height - h) / 2, w, h)
}

fn popup_block(title: &str) -> Block<'_> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::Cyan))
        .title(Line::from(format!(" {title} ")).bold().centered())
}

fn draw_help(f: &mut Frame) {
    let keys = [
        ("h / l, ← / →", "previous / next day"),
        ("H / L", "previous / next week"),
        ("[ / ]", "previous / next month"),
        ("j / k, ↓ / ↑", "move selection (calendar panel: week)"),
        ("Tab / Shift-Tab", "next / previous panel"),
        ("t", "go to today"),
        ("g", "go to date (2026-12-24, 12-24, 24, +7, -7)"),
        ("Enter", "show details of the selected item"),
        ("v", "switch month / week view"),
        ("c", "show / hide calendars"),
        ("C", "show / hide completed todos"),
        ("S", "sync (runs sync_command), then reload"),
        ("R", "reload from disk"),
        ("W", "show loading warnings"),
        ("q", "quit"),
        ("", ""),
        (
            "Markers",
            "● calendar item  ○ read-only  * repeats  > has notes",
        ),
    ];
    let lines: Vec<Line> = keys
        .iter()
        .map(|(k, d)| {
            Line::from(vec![
                Span::raw(format!(" {k:<18}")).bold().cyan(),
                Span::raw(*d),
            ])
        })
        .collect();
    let area = popup_area(f, 74, lines.len() as u16 + 2);
    f.render_widget(Clear, area);
    f.render_widget(Paragraph::new(lines).block(popup_block("Help")), area);
}

fn field(label: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::raw(format!(" {label:<10}")).dark_gray(),
        Span::raw(value),
    ])
}

fn moment_text(m: &Moment, app: &App) -> String {
    match m {
        Moment::Date(d) => d.format("%a %d %b %Y").to_string(),
        Moment::DateTime(dt) => dt
            .with_timezone(&app.store.home)
            .format("%a %d %b %Y %H:%M")
            .to_string(),
    }
}

fn draw_details(f: &mut Frame, app: &App) {
    let mut lines = Vec::new();
    let title;

    let description = if app.focus == PanelKind::Todo {
        let Some(t) = app.selected_todo() else { return };
        let cal = &app.store.calendars[t.calendar];
        title = t.summary.clone();
        lines.push(field(
            "Calendar",
            format!(
                "{}{}",
                cal.name,
                if cal.read_only { " (read-only)" } else { "" }
            ),
        ));
        lines.push(field(
            "Priority",
            if t.priority == 0 {
                "none".into()
            } else {
                t.priority.to_string()
            },
        ));
        if let Some(due) = &t.due {
            lines.push(field("Due", moment_text(due, app)));
        }
        lines.push(field(
            "Status",
            if t.completed {
                "completed".into()
            } else {
                "open".into()
            },
        ));
        t.description.clone()
    } else {
        let Some(i) = app.selected_instance() else {
            return;
        };
        let cal = &app.store.calendars[i.calendar];
        title = i.summary.clone();
        lines.push(field(
            "Calendar",
            format!(
                "{}{}",
                cal.name,
                if cal.read_only { " (read-only)" } else { "" }
            ),
        ));
        if i.all_day {
            let last = match i.end {
                Moment::Date(e) => e - Duration::days(1),
                other => other.date_in(app.store.home),
            };
            let start = i.start.date_in(app.store.home);
            let when = if last > start {
                format!(
                    "{} – {}",
                    start.format("%a %d %b"),
                    last.format("%a %d %b %Y")
                )
            } else {
                format!("{} (all day)", start.format("%a %d %b %Y"))
            };
            lines.push(field("When", when));
        } else {
            lines.push(field("Starts", moment_text(&i.start, app)));
            lines.push(field("Ends", moment_text(&i.end, app)));
        }
        if let Some(loc) = &i.location {
            lines.push(field("Location", loc.clone()));
        }
        if i.recurrence_id.is_some() {
            lines.push(field("Repeats", "yes (part of a series)".into()));
        }
        if i.tentative {
            lines.push(field("Status", "tentative".into()));
        }
        i.description.clone()
    };
    if let Some(desc) = description {
        lines.push(Line::from(""));
        lines.extend(desc.lines().map(|l| Line::from(format!(" {l}"))));
    }
    let area = popup_area(f, 70, lines.len() as u16 + 4);
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(popup_block(&title)),
        area,
    );
}

fn draw_calendars(f: &mut Frame, app: &App, sel: usize) {
    let lines: Vec<Line> = app
        .store
        .calendars
        .iter()
        .enumerate()
        .map(|(i, cal)| {
            let check = if cal.hidden { "[ ]" } else { "[x]" };
            let mut style = Style::new();
            if i == sel {
                style = style.add_modifier(Modifier::REVERSED);
            }
            let dot_style = match color(cal.color.as_deref()) {
                Some(c) => Style::new().fg(c),
                None => Style::new(),
            };
            Line::from(vec![
                Span::styled(format!(" {check} "), style),
                Span::styled("● ", dot_style),
                Span::styled(cal.name.clone(), style),
                Span::raw(if cal.read_only { "  read-only" } else { "" }).dark_gray(),
            ])
        })
        .collect();
    let area = popup_area(f, 44, lines.len() as u16 + 2);
    f.render_widget(Clear, area);
    f.render_widget(Paragraph::new(lines).block(popup_block("Calendars")), area);
}

fn draw_goto(f: &mut Frame, input: &str, error: Option<&str>) {
    let mut lines = vec![
        Line::from(vec![
            Span::raw(" Date: "),
            Span::raw(input.to_string()).bold(),
            Span::raw("█"),
        ]),
        Line::from(" 2026-12-24 · 12-24 · 24 · +7 · -7 · today").dark_gray(),
    ];
    if let Some(e) = error {
        lines.push(Line::from(format!(" {e}")).red());
    }
    let area = popup_area(f, 48, lines.len() as u16 + 2);
    f.render_widget(Clear, area);
    f.render_widget(Paragraph::new(lines).block(popup_block("Go to date")), area);
}

fn draw_warnings(f: &mut Frame, app: &App, scroll: usize) {
    let lines: Vec<Line> = if app.warnings.is_empty() {
        vec![Line::from(" No warnings.")]
    } else {
        app.warnings
            .iter()
            .skip(scroll)
            .map(|w| Line::from(format!(" {w}")))
            .collect()
    };
    let area = popup_area(f, 100, 20);
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(popup_block("Warnings")),
        area,
    );
}
