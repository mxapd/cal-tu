//! Drawing the TUI.

use cal_core::{Kind, Moment};
use chrono::{Datelike, Duration, NaiveDate};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};
use ratatui::Frame;

use super::app::{App, CalView, Pending, Popup};
use super::form::{FieldId, Form};
use super::layout::PanelKind;
use crate::fmt::{humanize, parse_color, time_range, Rgb, TimeRange};

pub fn draw(f: &mut Frame, app: &mut App) {
    let [main, status, keys] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(f.area());

    let layout = app.displayed_layout(main.width);
    let mut panels = Vec::new();
    super::layout::resolve(&layout, main, &mut panels);
    app.main_area = main;
    app.shown_panels = panels.iter().map(|(k, _)| *k).collect();
    let mut dividers = Dividers::new();
    for (kind, area) in panels {
        let area = take_dividers(area, main, &mut dividers);
        match kind {
            PanelKind::Appointments => draw_appointments(f, app, area),
            PanelKind::Calendar => draw_calendar(f, app, area),
            PanelKind::Todo => draw_todo(f, app, area),
        }
    }
    draw_dividers(f, &dividers);
    draw_status(f, app, status);
    draw_keys(f, app, keys);

    match app.popup.clone() {
        Some(Popup::Help) => draw_help(f),
        Some(Popup::Details) => draw_details(f, app),
        Some(Popup::Calendars { sel }) => draw_calendars(f, app, sel),
        Some(Popup::Goto { input, error }) => draw_goto(f, &input, error.as_deref()),
        Some(Popup::Warnings { scroll }) => draw_warnings(f, app, scroll),
        Some(Popup::Form(form)) => draw_form(f, app, &form),
        Some(Popup::Scope(p)) => draw_scope(f, app, &p),
        Some(Popup::Confirm(p)) => draw_confirm(f, &p),
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

/// Cells used by thin dividers between panels.
type Dividers = std::collections::BTreeSet<(u16, u16)>;

/// A panel gives up its right column and bottom row to dividers, unless it
/// touches the edge of the main area there.
fn take_dividers(area: Rect, main: Rect, dividers: &mut Dividers) -> Rect {
    let mut inner = area;
    if area.right() < main.right() && area.width > 1 {
        let x = area.right() - 1;
        for y in area.top()..area.bottom() {
            dividers.insert((x, y));
        }
        inner.width -= 1;
    }
    if area.bottom() < main.bottom() && area.height > 1 {
        let y = area.bottom() - 1;
        for x in area.left()..area.right() {
            dividers.insert((x, y));
        }
        inner.height -= 1;
    }
    inner
}

/// Draw dividers, joining lines where they meet (├ ┤ ┬ ┴ ┼ and corners).
fn draw_dividers(f: &mut Frame, dividers: &Dividers) {
    let style = Style::new().fg(Color::Indexed(240));
    let buf = f.buffer_mut();
    for &(x, y) in dividers {
        let has = |dx: i32, dy: i32| {
            let (nx, ny) = (x as i32 + dx, y as i32 + dy);
            nx >= 0 && ny >= 0 && dividers.contains(&(nx as u16, ny as u16))
        };
        let symbol = match (has(0, -1), has(0, 1), has(-1, 0), has(1, 0)) {
            (true, true, true, true) => "┼",
            (true, true, false, true) => "├",
            (true, true, true, false) => "┤",
            (false, true, true, true) => "┬",
            (true, false, true, true) => "┴",
            (false, true, false, true) => "┌",
            (false, true, true, false) => "┐",
            (true, false, false, true) => "└",
            (true, false, true, false) => "┘",
            (true, _, false, false) | (_, true, false, false) => "│",
            _ => "─",
        };
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_symbol(symbol).set_style(style);
        }
    }
}

/// Draw a panel's title row and return the area below it. The focused
/// panel's title is bold in the accent colour (a yellow bar in layout mode).
fn panel_frame(
    f: &mut Frame,
    app: &App,
    kind: PanelKind,
    title: &str,
    area: Rect,
    center: bool,
) -> Rect {
    if area.height == 0 {
        return area;
    }
    let focused = app.focus == kind && app.popup.is_none();
    let row = Rect::new(area.x, area.y, area.width, 1);
    let text = format!(" {title} ");
    let (line, bar) = if app.layout_mode && focused {
        (
            Line::from(text).bold().black(),
            Style::new().bg(Color::Yellow),
        )
    } else if focused {
        (Line::from(text).bold().fg(Color::Cyan), Style::new())
    } else {
        (Line::from(text).fg(Color::Gray), Style::new())
    };
    let line = if center { line.centered() } else { line };
    f.render_widget(Paragraph::new(line).style(bar), row);
    Rect::new(area.x, area.y + 1, area.width, area.height - 1)
}

/// Highlight a selected row across the whole panel width.
fn highlight_row(line: Line<'static>, width: u16, focused: bool) -> Line<'static> {
    let bg = if focused {
        Color::Indexed(24)
    } else {
        Color::Indexed(236)
    };
    let used = line.width();
    let mut spans: Vec<Span<'static>> = line
        .spans
        .into_iter()
        .map(|s| {
            let st = s.style.bg(bg);
            let st = if focused {
                st.add_modifier(Modifier::BOLD)
            } else {
                st
            };
            Span::styled(s.content, st)
        })
        .collect();
    if (width as usize) > used {
        spans.push(Span::styled(
            " ".repeat(width as usize - used),
            Style::new().bg(bg),
        ));
    }
    Line::from(spans)
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
    let inner = panel_frame(f, app, PanelKind::Appointments, &title, area, false);

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
        let bullet = if cal.read_only { "○" } else { "●" };
        let mut flags = String::new();
        if inst.recurrence_id.is_some() {
            flags.push('*');
        }
        if inst.description.is_some() {
            flags.push('>');
        }
        let tag = Span::styled(format!("  [{}]", cal.name), Style::new().dark_gray());
        let summary = Span::styled(inst.summary.clone(), cal_style(app, inst.calendar));
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
                    Span::raw(range.arrow()),
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

    if let Some((a, b)) = sel_rows {
        for line in &mut lines[a..=b] {
            *line = highlight_row(std::mem::take(line), inner.width, focused);
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
    let inner = panel_frame(f, app, PanelKind::Calendar, &title, area, true);

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
                s.bg(Color::Indexed(24)).add_modifier(Modifier::BOLD)
            } else {
                s.bg(Color::Indexed(236))
            };
        }
        s
    };

    let lines: Vec<Line> = match app.cal_view {
        CalView::Month => {
            // Day columns as wide as the panel allows (3–6 cells), centred.
            let cw = (inner.width / 7).clamp(3, 6) as usize;
            let pad = " ".repeat((inner.width as usize).saturating_sub(cw * 7) / 2);
            // A blank line between weeks when there is room for it.
            let gap = inner.height > 6 * 2; // header row + 6 weeks × 2
                                            // A cell: the 2-digit text centred, with one space of highlight
                                            // on each side when the cell is wide enough.
            let cell = |text: String, style: Style| -> Vec<Span<'static>> {
                let left = (cw - 2) / 2;
                let right = cw - 2 - left;
                if left >= 1 && right >= 1 {
                    vec![
                        Span::raw(" ".repeat(left - 1)),
                        Span::styled(format!(" {text} "), style),
                        Span::raw(" ".repeat(right - 1)),
                    ]
                } else {
                    vec![
                        Span::raw(" ".repeat(left)),
                        Span::styled(text, style),
                        Span::raw(" ".repeat(right)),
                    ]
                }
            };
            let (start, _) = app.visible_range();
            let mut header = vec![Span::raw(pad.clone())];
            for name in ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"] {
                header.extend(cell(name.into(), Style::new().fg(Color::Indexed(244))));
            }
            let mut lines = vec![Line::from(header)];
            for week in 0..6 {
                if gap {
                    lines.push(Line::from(""));
                }
                let mut spans = vec![Span::raw(pad.clone())];
                for wd in 0..7 {
                    let d = start + Duration::days(week * 7 + wd);
                    if d.month() == app.date.month() {
                        spans.extend(cell(format!("{:>2}", d.day()), day_style(d)));
                    } else {
                        spans.push(Span::raw(" ".repeat(cw)));
                    }
                }
                lines.push(Line::from(spans));
            }
            lines
        }
        CalView::Week => {
            let (start, _) = app.visible_range();
            let gap = inner.height >= 7 * 2 - 1;
            let mut lines = Vec::new();
            for n in 0..7 {
                if gap && n > 0 {
                    lines.push(Line::from(""));
                }
                let d = start + Duration::days(n);
                let mut spans = vec![
                    Span::raw(" "),
                    Span::styled(format!(" {} ", d.format("%a %d")), day_style(d)),
                    Span::raw(" "),
                ];
                for cal in app.marks.get(&d).into_iter().flatten().take(6) {
                    spans.push(Span::styled("● ", cal_style(app, *cal)));
                }
                lines.push(Line::from(spans));
            }
            lines
        }
    };
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_todo(f: &mut Frame, app: &mut App, area: Rect) {
    let title = if app.show_completed {
        "Todo (all)"
    } else {
        "Todo"
    };
    let inner = panel_frame(f, app, PanelKind::Todo, title, area, false);

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
        let line = Line::from(spans);
        lines.push(if idx == app.todo_sel {
            highlight_row(line, inner.width, focused)
        } else {
            line
        });
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
    if app.layout_mode {
        let line = Line::from(vec![
            Span::raw(" LAYOUT ").bold().black().on_yellow(),
            Span::raw(format!(
                " {}{}  ·  arranging: {}",
                app.layout_name,
                if app.layout_modified {
                    " (changed)"
                } else {
                    ""
                },
                app.focus.name()
            )),
        ]);
        f.render_widget(Paragraph::new(line).style(bar), area);
        return;
    }
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
        None if app.layout_mode => &[
            ("←/→", "Preset"),
            ("Tab", "Panel"),
            ("+/-", "Size"),
            ("</>", "Column size"),
            ("s", "Swap"),
            ("r", "Rotate"),
            ("x", "Hide"),
            ("u", "Unhide"),
            ("Enter", "Save"),
            ("Esc", "Done"),
        ],
        Some(Popup::Calendars { .. }) => &[("j/k", "Move"), ("Space", "Toggle"), ("Esc", "Close")],
        Some(Popup::Goto { .. }) => &[("Enter", "Go"), ("Esc", "Cancel")],
        Some(Popup::Form(_)) => &[
            ("Enter", "Save"),
            ("Tab/↓", "Next field"),
            ("Shift-Tab/↑", "Previous"),
            ("Ctrl-U", "Clear"),
            ("Esc", "Cancel"),
        ],
        Some(Popup::Scope(_)) => &[("o", "This occurrence"), ("a", "All"), ("Esc", "Cancel")],
        Some(Popup::Confirm(_)) => &[("y", "Yes"), ("n", "No")],
        Some(_) => &[("Esc", "Close")],
        // Most important first; the bar is cut at the terminal width.
        None if app.focus == PanelKind::Todo => &[
            ("?", "Help"),
            ("Tab", "Panel"),
            ("j/k", "Move"),
            ("a", "Add"),
            ("e", "Edit"),
            ("d", "Delete"),
            ("x", "Done"),
            ("+/-", "Priority"),
            ("C", "Show done"),
            ("S", "Sync"),
            ("q", "Quit"),
        ],
        None => &[
            ("?", "Help"),
            ("Tab", "Panel"),
            ("h/l", "Day"),
            ("j/k", "Move"),
            ("g", "Goto"),
            ("t", "Today"),
            ("a", "Add"),
            ("e", "Edit"),
            ("d", "Delete"),
            ("Enter", "View"),
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
        .border_type(BorderType::Plain)
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
        ("a", "add an event (todo panel: a todo)"),
        ("e", "edit the selected item"),
        ("d", "delete the selected item"),
        ("x, Space", "todo: mark done / not done"),
        ("+ / -", "todo: raise / lower priority"),
        ("v", "switch month / week view"),
        ("c", "show / hide calendars"),
        ("C", "show / hide completed todos"),
        ("S", "sync (runs sync_command), then reload"),
        ("R", "reload from disk"),
        ("W", "show loading warnings"),
        (
            "w",
            "layout mode: presets, resize, swap, hide panels; Enter saves",
        ),
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

fn draw_form(f: &mut Frame, app: &App, form: &Form) {
    const LABEL: usize = 10;
    let mut lines = Vec::new();
    let mut cursor = None;
    for (i, field) in form.fields.iter().enumerate() {
        let focused = i == form.focus;
        let label = Span::raw(format!(" {:<LABEL$} ", field.id.label()));
        let label = if focused {
            label.bold().cyan()
        } else {
            label.dark_gray()
        };
        let value = if field.id == FieldId::Calendar {
            match form.calendars.get(form.calendar) {
                Some(&c) => Span::styled(
                    format!("◀ {} ▶", app.store.calendars[c].name),
                    cal_style(app, c).add_modifier(Modifier::BOLD),
                ),
                None => Span::raw("(no writable calendar)").red(),
            }
        } else {
            if focused {
                let before: String = field.input.text.chars().take(field.input.cursor).collect();
                cursor = Some((i, LABEL + 2 + before.chars().count()));
            }
            let s = Span::raw(field.input.text.clone());
            if focused {
                s.bold()
            } else {
                s
            }
        };
        lines.push(Line::from(vec![label, value]));
    }
    lines.push(Line::from(""));
    let hint = form.fields[form.focus].id.hint();
    lines.push(Line::from(format!(" {hint}")).dark_gray());
    if let Some(e) = &form.error {
        lines.push(Line::from(format!(" {e}")).red());
    }
    let area = popup_area(f, 72, lines.len() as u16 + 2);
    f.render_widget(Clear, area);
    f.render_widget(Paragraph::new(lines).block(popup_block(&form.title)), area);
    if let Some((row, col)) = cursor {
        let x = (area.x + 1 + col as u16).min(area.right().saturating_sub(2));
        f.set_cursor_position((x, area.y + 1 + row as u16));
    }
}

fn draw_scope(f: &mut Frame, app: &App, p: &Pending) {
    let (verb, i) = match p {
        Pending::Edit(i) => ("Edit", i),
        Pending::Delete(i) => ("Delete", i),
        Pending::DeleteTodo(_) => return,
    };
    let when = i
        .start
        .instant(app.store.home)
        .format("%a %d %b")
        .to_string();
    let lines = vec![
        Line::from(format!(" “{}” repeats.", i.summary)),
        Line::from(""),
        Line::from(vec![
            Span::raw("  o ").bold().cyan(),
            Span::raw(format!(" only this occurrence ({when})")),
        ]),
        Line::from(vec![
            Span::raw("  a ").bold().cyan(),
            Span::raw(" all occurrences"),
        ]),
        Line::from(vec![Span::raw("  Esc").bold().cyan(), Span::raw(" cancel")]),
    ];
    let area = popup_area(f, 56, lines.len() as u16 + 2);
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines).block(popup_block(&format!("{verb} repeating event"))),
        area,
    );
}

fn draw_confirm(f: &mut Frame, p: &Pending) {
    let what = match p {
        Pending::Delete(i) | Pending::Edit(i) => &i.summary,
        Pending::DeleteTodo(t) => &t.summary,
    };
    let lines = vec![
        Line::from(format!(" Delete “{what}”?")),
        Line::from(""),
        Line::from(vec![
            Span::raw("  y ").bold().cyan(),
            Span::raw(" yes    "),
            Span::raw("n ").bold().cyan(),
            Span::raw(" no"),
        ]),
    ];
    let area = popup_area(f, 56, lines.len() as u16 + 2);
    f.render_widget(Clear, area);
    f.render_widget(Paragraph::new(lines).block(popup_block("Delete")), area);
}
