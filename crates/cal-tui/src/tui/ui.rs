//! Drawing the TUI.

use cal_core::{Instance, Kind, Moment};
use chrono::{Datelike, Duration, NaiveDate};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};
use ratatui::Frame;

use super::app::{App, CalView, Hit, Pending, Popup};
use super::form::{FieldId, Form};
use super::layout::{Dir, PanelKind};
use crate::fmt::{humanize, parse_color, time_range, Rgb, TimeRange};

pub fn draw(f: &mut Frame, app: &mut App) {
    let content = f.area();
    let [main, status, keys] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(content);

    let layout = app.displayed_layout(main.width);
    let mut panels = Vec::new();
    super::layout::resolve(&layout, main, &mut panels);
    app.main_area = main;
    app.shown_panels = panels.iter().map(|(k, _)| *k).collect();
    app.hits.clear();
    let mut dividers = Dividers::new();
    for (kind, area) in panels {
        let inner = take_dividers(area, main, &mut dividers);
        app.hits.push((inner, Hit::Panel(kind)));
        // Dividers can be dragged with the mouse.
        if inner.width < area.width {
            let r = Rect::new(area.right() - 1, area.y, 1, area.height);
            app.hits.push((r, Hit::Divider(kind, Dir::Columns)));
        }
        if inner.height < area.height {
            let r = Rect::new(area.x, area.bottom() - 1, area.width, 1);
            app.hits.push((r, Hit::Divider(kind, Dir::Rows)));
        }
        let area = inner;
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
    if let Some(area) = LAST_POPUP.take().filter(|_| app.popup.is_some()) {
        app.hits.push((area, Hit::Popup));
        if matches!(app.popup, Some(Popup::Calendars { .. })) {
            for i in 0..app.store.calendars.len() as u16 {
                let row = Rect::new(area.x + 1, area.y + 1 + i, area.width.saturating_sub(2), 1);
                app.hits.push((row, Hit::CalendarToggle(i as usize)));
            }
        }
    }
}

pub fn color(c: Option<&str>) -> Option<Color> {
    parse_color(c).map(|c| match c {
        Rgb::True(r, g, b) => Color::Rgb(r, g, b),
        Rgb::Named(n) => Color::Indexed(n),
    })
}

/// Calendars without a colour of their own get one of the soft colours,
/// chosen by a stable hash of the folder name so a calendar keeps its colour.
fn palette(i: usize) -> Color {
    let (_, (r, g, b)) = crate::fmt::SOFT[i % crate::fmt::SOFT.len()];
    Color::Rgb(r, g, b)
}

/// A calendar's colour: configured (or from the vdir `color` file), else
/// one from the palette.
pub fn cal_color(app: &App, cal: usize) -> Color {
    let c = &app.store.calendars[cal];
    color(c.color.as_deref()).unwrap_or_else(|| {
        let h =
            c.id.bytes()
                .fold(0usize, |h, b| h.wrapping_mul(31).wrapping_add(b as usize));
        palette(h)
    })
}

fn cal_style(app: &App, cal: usize) -> Style {
    Style::new().fg(cal_color(app, cal))
}

/// Shorten to `max` characters, ending in … if cut.
fn ellipsize(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
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
/// Scroll so the selected rows are visible; returns the lines and how many
/// were scrolled off the top.
fn scrolled(
    lines: Vec<Line<'static>>,
    sel: Option<(usize, usize)>,
    height: usize,
) -> (Vec<Line<'static>>, usize) {
    let offset = match sel {
        Some((first, last)) if last >= height => (last + 1).saturating_sub(height).min(first),
        _ => 0,
    };
    (lines.into_iter().skip(offset).collect(), offset)
}

/// Screen row of line `row` after scrolling by `offset`, if visible.
fn screen_row(inner: Rect, row: usize, offset: usize) -> Option<u16> {
    let r = row.checked_sub(offset)?;
    (r < inner.height as usize).then(|| inner.y + r as u16)
}

/// First and last hour shown by the day grid unless events fall outside.
const DAY_FIRST_HOUR: u32 = 8;
const DAY_LAST_HOUR: u32 = 20;
/// Faint background of event blocks, so their length shows.
const BLOCK_BG: Color = Color::Indexed(235);
/// Colour of the "now" line.
const NOW: Color = Color::Rgb(215, 95, 95);

/// Title and extra details of an item. Comma-separated titles from read-only
/// feeds ("Seminarium, Ola Flygt, 2DV704") keep only the first part as the
/// title; the rest becomes details.
pub fn split_title(app: &App, inst: &Instance) -> (String, Vec<String>) {
    let cal = &app.store.calendars[inst.calendar];
    if cal.read_only && inst.summary.contains(", ") {
        let mut parts = inst.summary.split(", ").map(str::to_string);
        let first = parts.next().unwrap_or_default();
        return (first, parts.collect());
    }
    (inst.summary.clone(), Vec::new())
}

/// Second line of an item: room, the rest of a split title, markers and the
/// calendar name.
fn details(app: &App, inst: &Instance, rest: &[String]) -> Vec<Span<'static>> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(l) = &inst.location {
        parts.push(l.clone());
    }
    parts.extend(rest.iter().cloned());
    let mut spans = Vec::new();
    if !parts.is_empty() {
        spans.push(Span::raw(parts.join(" · ")).fg(Color::Indexed(250)));
        spans.push(Span::raw(" "));
    }
    let mut marks = String::new();
    if inst.recurrence_id.is_some() {
        marks.push_str("↻ ");
    }
    if inst.description.is_some() {
        marks.push_str("≡ ");
    }
    if !marks.is_empty() {
        spans.push(Span::raw(marks).fg(Color::Indexed(246)));
    }
    spans.push(Span::styled(
        format!(" {}", app.store.calendars[inst.calendar].name),
        cal_style(app, inst.calendar),
    ));
    spans
}

fn weather_text(app: &App) -> Option<String> {
    let w = app.weather.as_ref()?;
    match &w.forecast {
        Some(f) => f.line(app.date, app.today()),
        None if w.error.is_some() && app.date == app.today() => {
            Some("weather unavailable".to_string())
        }
        None => None,
    }
}

/// Write `line` into the buffer at (x, y), at most `width` cells, with `bg`
/// under every cell of that width.
fn put(
    buf: &mut ratatui::buffer::Buffer,
    x: u16,
    y: u16,
    width: u16,
    line: Line<'_>,
    bg: Option<Color>,
) {
    if let Some(bg) = bg {
        buf.set_style(Rect::new(x, y, width, 1), Style::new().bg(bg));
    }
    buf.set_line(x, y, &line, width);
}

/// The appointments panel: a header with the date and weather, all-day items,
/// then an hour grid with events as blocks.
fn draw_appointments(f: &mut Frame, app: &mut App, area: Rect) {
    let today = app.today();
    let is_today = app.date == today;
    let title = if is_today {
        app.date.format("%A %-d %B · today").to_string()
    } else {
        app.date.format("%A %-d %B %Y").to_string()
    };
    let inner = panel_frame(f, app, PanelKind::Appointments, &title, area, false);
    if let Some(w) = weather_text(app) {
        let need = title.chars().count() + w.chars().count() + 6;
        if (area.width as usize) >= need {
            let row = Rect::new(area.x, area.y, area.width.saturating_sub(1), 1);
            f.render_widget(
                Paragraph::new(Line::from(w).fg(Color::Indexed(248)).right_aligned()),
                row,
            );
        }
    }
    if inner.height == 0 || inner.width < 12 {
        return;
    }

    let focused = app.focus == PanelKind::Appointments;
    let home = app.store.home;
    let now = app.now();
    let sel_bg = if focused {
        Color::Indexed(24)
    } else {
        Color::Indexed(238)
    };
    let label = Style::new().fg(Color::Indexed(244));
    let mut hits: Vec<(Rect, Hit)> = Vec::new();
    let buf = f.buffer_mut();

    // All-day items and due todos: one row each at the top.
    let (all_day, timed): (Vec<usize>, Vec<usize>) = (0..app.day_items.len()).partition(|&i| {
        matches!(
            time_range(&app.day_items[i], app.date, home),
            TimeRange::AllDay | TimeRange::Due(_)
        )
    });
    let mut y = inner.y;
    if app.day_items.is_empty() {
        put(
            buf,
            inner.x,
            y,
            inner.width,
            Line::from(" nothing scheduled").style(label),
            None,
        );
        y += 1;
    }
    for &idx in &all_day {
        if y + 2 > inner.bottom() {
            break;
        }
        let inst = &app.day_items[idx];
        let selected = idx == app.appt_sel;
        let (title, rest) = split_title(app, inst);
        let lead = match time_range(inst, app.date, home) {
            TimeRange::Due(Some(t)) => format!(" due {t} "),
            TimeRange::Due(None) => " due   ".to_string(),
            _ => " all day".to_string(),
        };
        let mut spans = vec![
            Span::styled(format!("{lead:<7}"), label),
            Span::styled(" ▌ ", cal_style(app, inst.calendar)),
            Span::raw(title).bold(),
            Span::raw("  "),
        ];
        spans.extend(details(app, inst, &rest));
        let bg = selected.then_some(sel_bg);
        put(buf, inner.x, y, inner.width, Line::from(spans), bg);
        hits.push((Rect::new(inner.x, y, inner.width, 1), Hit::Appointment(idx)));
        y += 1;
    }

    // The hour grid.
    let grid = Rect::new(inner.x, y, inner.width, inner.bottom().saturating_sub(y));
    if grid.height >= 2 {
        let day_start = app.store.day_start(app.date);
        let day_end = app.store.day_start(app.date + Duration::days(1));
        let minutes = |t: chrono::DateTime<chrono_tz::Tz>| {
            (t.clamp(day_start, day_end) - day_start)
                .num_minutes()
                .max(0) as u32
        };
        // (index, start minute, end minute) of each timed item today.
        let spans_min: Vec<(usize, u32, u32)> = timed
            .iter()
            .map(|&i| {
                let inst = &app.day_items[i];
                let s = minutes(inst.start.instant(home));
                let e = minutes(inst.end.instant(home)).max(s + 1);
                (i, s, e)
            })
            .collect();
        let mut first = DAY_FIRST_HOUR;
        let mut last = DAY_LAST_HOUR;
        for &(_, s, e) in &spans_min {
            first = first.min(s / 60);
            last = last.max(e.div_ceil(60));
        }
        let now_min = is_today.then(|| minutes(now));
        if let Some(n) = now_min {
            first = first.min(n / 60);
            last = last.max(n / 60 + 1);
        }
        let mut last = last.min(24);
        let rph = (grid.height as u32 / (last - first)).clamp(1, 4);
        // Use leftover rows for more hours: the evening first, then the morning.
        let fits = grid.height as u32 / rph;
        while last - first < fits && last < 24 {
            last += 1;
        }
        while last - first < fits && first > 0 {
            first -= 1;
        }
        let hours = last - first;
        let slot = 60 / rph;
        let total = hours * rph;
        let row_of = |m: u32| (m.saturating_sub(first * 60)) / slot;

        // Scroll so the selected item (or now) is in view when it does not fit.
        let focus_row = spans_min
            .iter()
            .find(|(i, _, _)| *i == app.appt_sel)
            .map(|(_, s, _)| row_of(*s))
            .or(now_min.map(row_of))
            .unwrap_or(0);
        let offset = if total > grid.height as u32 {
            focus_row
                .saturating_sub(grid.height as u32 / 3)
                .min(total - grid.height as u32)
        } else {
            0
        };

        // Hour labels and the rule.
        const LABEL_W: u16 = 6;
        let x0 = grid.x + LABEL_W + 1;
        let cw = grid.width.saturating_sub(LABEL_W + 1);
        for r in 0..grid.height {
            let abs = offset + r as u32;
            if abs >= total {
                break;
            }
            let text = if abs.is_multiple_of(rph) {
                format!(" {:02}:00", first + abs / rph)
            } else {
                String::new()
            };
            buf.set_string(grid.x, grid.y + r, format!("{text:<6}"), label);
            buf.set_string(
                grid.x + LABEL_W,
                grid.y + r,
                "│",
                Style::new().fg(Color::Indexed(240)),
            );
        }

        // Lanes for overlapping events. Events are grouped into clusters that
        // overlap each other; only a cluster is split into lanes, so an event
        // that overlaps nothing keeps the full width. (Items are sorted by
        // start, so a cluster ends where nothing running reaches the next.)
        let mut lane_of = vec![0usize; spans_min.len()];
        let mut lanes_of = vec![1usize; spans_min.len()];
        let mut cluster: Vec<usize> = Vec::new();
        let mut lane_end: Vec<u32> = Vec::new();
        let mut cluster_end = 0u32;
        let close = |cluster: &mut Vec<usize>, lanes: usize, lanes_of: &mut Vec<usize>| {
            for k in cluster.drain(..) {
                lanes_of[k] = lanes.max(1);
            }
        };
        for (k, &(_, s, e)) in spans_min.iter().enumerate() {
            let (srow, erow) = (row_of(s), row_of(e).max(row_of(s) + 1));
            if !cluster.is_empty() && srow >= cluster_end {
                close(&mut cluster, lane_end.len(), &mut lanes_of);
                lane_end.clear();
            }
            match lane_end.iter().position(|end| *end <= srow) {
                Some(l) => {
                    lane_of[k] = l;
                    lane_end[l] = erow;
                }
                None => {
                    lane_of[k] = lane_end.len();
                    lane_end.push(erow);
                }
            }
            cluster_end = if cluster.is_empty() {
                erow
            } else {
                cluster_end.max(erow)
            };
            cluster.push(k);
        }
        close(&mut cluster, lane_end.len(), &mut lanes_of);
        let mut occupied = vec![vec![false; cw as usize]; grid.height as usize];

        for (k, &(idx, s, e)) in spans_min.iter().enumerate() {
            let inst = &app.day_items[idx];
            let selected = idx == app.appt_sel;
            let (srow, erow) = (row_of(s), row_of(e).max(row_of(s) + 1));
            let lanes = lanes_of[k] as u16;
            let lane_w = (cw / lanes).max(4);
            let x = x0 + lane_of[k] as u16 * lane_w;
            let w = if lane_of[k] as u16 + 1 == lanes {
                x0 + cw - x
            } else {
                lane_w - 1
            };
            let start = inst.start.instant(home);
            let end = inst.end.instant(home);
            let past = is_today && end <= now;
            let (title, rest) = split_title(app, inst);
            let bg = if selected { sel_bg } else { BLOCK_BG };
            let text_fg = if past {
                Color::Indexed(244)
            } else {
                Color::Reset
            };
            let times = format!("{}–{}", start.format("%H:%M"), end.format("%H:%M"));
            for (n, abs) in (srow..erow).enumerate() {
                let Some(r) = abs.checked_sub(offset).filter(|r| *r < grid.height as u32) else {
                    continue;
                };
                let ry = grid.y + r as u16;
                let mut spans = vec![Span::styled("▌", cal_style(app, inst.calendar))];
                match n {
                    0 => {
                        spans.push(Span::styled(
                            format!(" {title}"),
                            Style::new().fg(text_fg).add_modifier(Modifier::BOLD),
                        ));
                        spans.push(Span::raw(format!("  {times}")).fg(Color::Indexed(246)));
                        if erow - srow == 1 {
                            spans.push(Span::raw("  "));
                            spans.extend(details(app, inst, &rest));
                        }
                    }
                    1 => {
                        spans.push(Span::raw(" "));
                        spans.extend(details(app, inst, &rest));
                    }
                    _ => {}
                }
                if past {
                    for sp in spans.iter_mut().skip(1) {
                        sp.style = sp.style.fg(Color::Indexed(244));
                    }
                }
                put(buf, x, ry, w, Line::from(spans), Some(bg));
                for c in (x - x0)..(x - x0 + w) {
                    if let Some(cell) = occupied[r as usize].get_mut(c as usize) {
                        *cell = true;
                    }
                }
                hits.push((Rect::new(x, ry, w, 1), Hit::Appointment(idx)));
            }
        }

        // The "now" line through the free space, and the time in the margin.
        if let Some(n) = now_min {
            if let Some(r) = row_of(n)
                .checked_sub(offset)
                .filter(|r| *r < grid.height as u32)
            {
                let ry = grid.y + r as u16;
                let accent = Style::new().fg(NOW);
                buf.set_string(
                    grid.x,
                    ry,
                    format!(" {:<5}", now.format("%H:%M")),
                    accent.bold(),
                );
                buf.set_string(grid.x + LABEL_W, ry, "┼", accent);
                for c in 0..cw {
                    if !occupied[r as usize][c as usize] {
                        buf.set_string(x0 + c, ry, "─", accent);
                    }
                }
            }
        }
    }
    app.hits.extend(hits);
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
    // ‹ title › with clickable arrows (the title row is centred the same
    // way panel_frame centres it).
    let title = format!("‹  {title}  ›");
    let inner = panel_frame(f, app, PanelKind::Calendar, &title, area, true);
    let shown = title.chars().count() as u16 + 2;
    if area.width >= shown && area.height > 0 {
        let x = area.x + (area.width - shown) / 2;
        // Generous targets: the arrow plus the cells around it.
        app.hits.push((Rect::new(x, area.y, 3, 1), Hit::Prev));
        app.hits
            .push((Rect::new(x + shown - 3, area.y, 3, 1), Hit::Next));
    }

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

    let mut hits: Vec<(Rect, Hit)> = Vec::new();
    let row_rect = |idx: usize| {
        (idx < inner.height as usize)
            .then(|| Rect::new(inner.x, inner.y + idx as u16, inner.width, 1))
    };
    let lines: Vec<Line> = match app.cal_view {
        CalView::Month => {
            let (start, _) = app.visible_range();
            let home = app.store.home;
            // Week-number column, then seven day cells centred in the panel:
            // 5 wide (room for two calendar dots), 4 (one dot), or 3 if tight.
            let cw: usize = (inner.width.saturating_sub(3) as usize / 7).clamp(3, 5);
            let grid_w = 3 + 7 * cw;
            let pad = " ".repeat((inner.width as usize).saturating_sub(grid_w) / 2);
            let label = Style::new().fg(Color::Indexed(244));
            let dim = Style::new().fg(Color::Indexed(240));
            let day_cell = |d: NaiveDate| -> Vec<Span<'static>> {
                let other_month = d.month() != app.date.month();
                // Numbers stay in the normal text colour; the dots carry the
                // calendar colours.
                let mut text = if other_month { dim } else { Style::new() };
                if d == today {
                    text = text.add_modifier(Modifier::UNDERLINED | Modifier::BOLD);
                }
                let mut around = Style::new();
                if d == app.date {
                    let bg = if focused {
                        Color::Indexed(24)
                    } else {
                        Color::Indexed(236)
                    };
                    around = around.bg(bg);
                    text = text.bg(bg).add_modifier(Modifier::BOLD);
                }
                let mut cell = vec![
                    Span::styled(" ", around),
                    Span::styled(format!("{:>2}", d.day()), text),
                ];
                // One dot per calendar with items that day, as many as fit.
                let slots = cw - 3;
                let cals = app.marks.get(&d).map(Vec::as_slice).unwrap_or(&[]);
                for i in 0..slots {
                    cell.push(match cals.get(i) {
                        Some(cal) => {
                            let c = if other_month {
                                Color::Indexed(240)
                            } else {
                                cal_color(app, *cal)
                            };
                            Span::styled("•", around.fg(c))
                        }
                        None => Span::styled(" ", around),
                    });
                }
                cell
            };

            let mut header = vec![Span::raw(pad.clone()), Span::styled(" v ", label)];
            for name in ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"] {
                header.push(Span::styled(
                    format!(" {name}{}", " ".repeat(cw - 3)),
                    label,
                ));
            }
            let mut lines = vec![Line::from(header)];
            for week in 0..6 {
                let monday = start + Duration::days(week * 7);
                let mut spans = vec![
                    Span::raw(pad.clone()),
                    Span::styled(format!("{:>2} ", monday.iso_week().week()), label),
                ];
                let y = 1 + week as usize;
                for wd in 0..7 {
                    if let Some(r) = row_rect(y) {
                        let x = inner.x + (pad.len() + 3 + wd as usize * cw) as u16;
                        let w = (cw as u16).min(inner.right().saturating_sub(x));
                        hits.push((
                            Rect::new(x, r.y, w, 1),
                            Hit::Day(monday + Duration::days(wd)),
                        ));
                    }
                    spans.extend(day_cell(monday + Duration::days(wd)));
                }
                lines.push(Line::from(spans));
            }

            // Below the grid: what comes after the selected day. Day headers
            // on their own line, items as "13:15 Title" with the time in the
            // calendar's colour, so titles get most of the width.
            let room = (inner.height as usize).saturating_sub(lines.len());
            if room >= 3 {
                // Week separators double as labels: "── This week ───".
                let monday =
                    |d: NaiveDate| d - Duration::days(d.weekday().num_days_from_monday() as i64);
                let this_week = monday(today);
                let rule = |week: NaiveDate| {
                    let name = if week == this_week {
                        "This week".to_string()
                    } else if week == this_week + Duration::days(7) {
                        "Next week".to_string()
                    } else if week == this_week - Duration::days(7) {
                        "Last week".to_string()
                    } else {
                        format!("Week {}", week.iso_week().week())
                    };
                    let text = format!(" ── {name} ");
                    let fill = (inner.width as usize).saturating_sub(text.chars().count() + 1);
                    Line::from(format!("{text}{}", "─".repeat(fill))).style(dim)
                };
                let from = app.date + Duration::days(1);
                if app.upcoming.is_empty() {
                    lines.push(rule(monday(from)));
                    lines.push(Line::from(" nothing in the next two weeks").style(dim));
                }
                let title_w = (inner.width as usize).saturating_sub(9);
                let mut last_day = None;
                let mut last_week = None;
                for i in &app.upcoming {
                    let day = i.start.date_in(home).max(from);
                    if last_day != Some(day) {
                        let new_week = last_week != Some(monday(day));
                        // Only start a day if one item fits under its header.
                        let needed = if new_week { 3 } else { 2 };
                        if lines.len() + needed > inner.height as usize {
                            break;
                        }
                        if new_week {
                            last_week = Some(monday(day));
                            lines.push(rule(monday(day)));
                        }
                        last_day = Some(day);
                        if let Some(r) = row_rect(lines.len()) {
                            hits.push((r, Hit::AgendaDay(day)));
                        }
                        let when = if day == today + Duration::days(1) {
                            day.format(" Tomorrow · %a %-d %b").to_string()
                        } else {
                            day.format(" %A %-d %b").to_string()
                        };
                        lines.push(Line::from(when).style(label.add_modifier(Modifier::BOLD)));
                    }
                    if lines.len() >= inner.height as usize {
                        break;
                    }
                    if let Some(r) = row_rect(lines.len()) {
                        hits.push((r, Hit::AgendaDay(day)));
                    }
                    let colour = cal_style(app, i.calendar);
                    let (when, title_style) = if i.kind == Kind::Todo {
                        ("due  ".to_string(), Style::new())
                    } else if i.all_day {
                        // No time: the title itself carries the colour.
                        ("     ".to_string(), colour)
                    } else {
                        (
                            i.start.instant(home).format("%H:%M").to_string(),
                            Style::new(),
                        )
                    };
                    lines.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(when, colour),
                        Span::raw(" "),
                        Span::styled(ellipsize(&split_title(app, i).0, title_w), title_style),
                    ]));
                }
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
                if let Some(r) = row_rect(lines.len()) {
                    hits.push((r, Hit::Day(d)));
                }
                lines.push(Line::from(spans));
            }
            lines
        }
    };
    app.hits.extend(hits);
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_todo(f: &mut Frame, app: &mut App, area: Rect) {
    let title = if app.show_completed {
        "Todo (all)"
    } else {
        "Todo"
    };
    let inner = panel_frame(f, app, PanelKind::Todo, title, area, true);

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
    let n = lines.len();
    let (lines, offset) = scrolled(
        lines,
        Some((app.todo_sel, app.todo_sel)),
        inner.height as usize,
    );
    if !app.todos.is_empty() {
        for i in 0..n {
            if let Some(y) = screen_row(inner, i, offset) {
                app.hits
                    .push((Rect::new(inner.x, y, inner.width, 1), Hit::Todo(i)));
            }
        }
    }
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
    for (i, cal) in app.store.calendars.iter().enumerate() {
        let style = cal_style(app, i);
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

thread_local! {
    /// Area of the pop-up drawn in this frame, for mouse clicks.
    static LAST_POPUP: std::cell::Cell<Option<Rect>> = const { std::cell::Cell::new(None) };
}

fn popup_area(f: &Frame, width: u16, height: u16) -> Rect {
    let a = f.area();
    let w = width.min(a.width.saturating_sub(2));
    let h = height.min(a.height.saturating_sub(2));
    let r = Rect::new(a.x + (a.width - w) / 2, a.y + (a.height - h) / 2, w, h);
    LAST_POPUP.set(Some(r));
    r
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
            let dot_style = cal_style(app, i);
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
