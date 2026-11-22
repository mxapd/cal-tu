//! Drawing the TUI.

use cal_core::keys::{Action as K, Keymap};
use cal_core::{Instance, Kind, Moment};
use chrono::{Datelike, Duration, NaiveDate};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};
use ratatui::Frame;

use super::app::{week_start, App, ApptView, Hit, Pending, Popup};
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
    let mut focused_area = None;
    for (kind, area) in panels {
        let inner = take_dividers(area, main, &mut dividers);
        if kind == app.focus && app.popup.is_none() {
            focused_area = Some(inner);
        }
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
    draw_dividers(f, &dividers, focused_area);
    draw_status(f, app, status);
    draw_keys(f, app, keys);

    // With a pop-up open, everything behind it (except the key hints) is
    // dimmed so the card stands out.
    if app.popup.is_some() {
        let behind = Rect::new(content.x, content.y, content.width, keys.y - content.y);
        f.buffer_mut().set_style(
            behind,
            Style::new()
                .fg(Color::Indexed(240))
                .bg(Color::Reset)
                .remove_modifier(Modifier::BOLD | Modifier::UNDERLINED | Modifier::REVERSED),
        );
    }
    match app.popup.clone() {
        Some(Popup::Help) => draw_help(f, &app.keymap),
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
        // The card's title row becomes an accent bar.
        f.buffer_mut().set_style(
            Rect::new(area.x, area.y, area.width, 1),
            Style::new().bg(POPUP_BAR).fg(Color::Black),
        );
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

/// Accent for the focused panel: its title and the dividers around it.
const FOCUS: Color = Color::Cyan;
/// Faint tint behind the focused panel's title row.
const FOCUS_BAR: Color = Color::Rgb(0x1c, 0x2b, 0x38);

/// Does divider cell (x, y) run along an edge of `r` (corners included)?
fn borders(r: Rect, x: u16, y: u16) -> bool {
    let (l, t) = (r.x.saturating_sub(1), r.y.saturating_sub(1));
    let (rt, b) = (r.right(), r.bottom());
    let in_cols = x >= l && x <= rt;
    let in_rows = y >= t && y <= b;
    ((x == l || x == rt) && in_rows) || ((y == t || y == b) && in_cols)
}

/// Draw dividers, joining lines where they meet (├ ┤ ┬ ┴ ┼ and corners).
/// The dividers around the focused panel are drawn in the accent colour.
fn draw_dividers(f: &mut Frame, dividers: &Dividers, focused: Option<Rect>) {
    let normal = Style::new().fg(Color::Indexed(240));
    let accent = Style::new().fg(FOCUS);
    let buf = f.buffer_mut();
    for &(x, y) in dividers {
        let style = match focused {
            Some(r) if borders(r, x, y) => accent,
            _ => normal,
        };
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
/// panel's title is bold in the accent colour on a tinted bar (a yellow bar
/// in layout mode); other titles are dimmed.
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
        (
            Line::from(text).bold().fg(FOCUS),
            Style::new().bg(FOCUS_BAR),
        )
    } else {
        (Line::from(text).fg(Color::Indexed(245)), Style::new())
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

/// Colour for a piece of the weather line (tuned to the Stylix palette:
/// yellow sun, blue rain, purple thunder; temperatures from icy to hot).
fn weather_color(t: crate::weather::Tone) -> Color {
    use crate::weather::Tone::*;
    match t {
        Sun => Color::Rgb(246, 211, 122),
        Cloud => Color::Rgb(154, 165, 180),
        Fog => Color::Rgb(130, 140, 155),
        Rain => Color::Rgb(122, 167, 255),
        Snow => Color::Rgb(168, 216, 234),
        Storm => Color::Rgb(199, 146, 234),
        Temp(c) if c <= 0 => Color::Rgb(126, 200, 227),
        Temp(c) if c < 10 => Color::Rgb(197, 208, 222),
        Temp(c) if c < 20 => Color::Rgb(127, 207, 125),
        Temp(c) if c < 27 => Color::Rgb(242, 167, 107),
        Temp(_) => Color::Rgb(226, 109, 109),
        Plain => Color::Indexed(246),
    }
}

/// The weather for the selected day as coloured spans.
fn weather_line(app: &App) -> Option<Line<'static>> {
    let w = app.weather.as_ref()?;
    match &w.forecast {
        Some(f) => {
            let parts = f.parts(app.date, app.today())?;
            Some(Line::from(
                parts
                    .into_iter()
                    .map(|(s, t)| Span::styled(s, Style::new().fg(weather_color(t))))
                    .collect::<Vec<_>>(),
            ))
        }
        None if w.error.is_some() && app.date == app.today() => {
            Some(Line::from("weather unavailable").fg(Color::Indexed(246)))
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
    // Blank the whole width first, so nothing drawn underneath (the hour
    // lines) shows through after the text.
    buf.set_string(x, y, " ".repeat(width as usize), Style::new());
    if let Some(bg) = bg {
        buf.set_style(Rect::new(x, y, width, 1), Style::new().bg(bg));
    }
    buf.set_line(x, y, &line, width);
}

/// The appointments panel: a header with the date and weather, all-day items,
/// then an hour grid with events as blocks.
fn draw_appointments(f: &mut Frame, app: &mut App, area: Rect) {
    let week = app.appt_view == ApptView::Week;
    if week && week_fits(area.width) {
        return draw_week(f, app, area);
    }
    let today = app.today();
    let is_today = app.date == today;
    let mut title = if is_today {
        app.date.format("%A %-d %B · today").to_string()
    } else {
        app.date.format("%A %-d %B %Y").to_string()
    };
    if week {
        title.push_str(" · no room for week");
    }
    let inner = panel_frame(f, app, PanelKind::Appointments, &title, area, false);
    if let Some(w) = weather_line(app) {
        let need = title.chars().count() + w.width() + 6;
        if (area.width as usize) >= need {
            let row = Rect::new(area.x, area.y, area.width.saturating_sub(1), 1);
            f.render_widget(Paragraph::new(w.right_aligned()), row);
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
    let (all_day, timed) = split_all_day(app, &app.day_items, app.date);
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
    // (offset, rows per hour, largest offset) of the hour grid, for scrolling.
    let mut day_view = (0u32, 1u32, 0u32);
    if grid.height >= 2 {
        let spans_min = timed_spans(app, &app.day_items, &timed, app.date);
        let now_min = is_today.then(|| day_minute(app, app.date, now));
        let selected = spans_min
            .iter()
            .find(|(i, _, _)| *i == app.appt_sel)
            .map(|(_, s, _)| *s);
        let g = HourGrid::new(
            app,
            grid.height,
            spans_min.iter().map(|&(_, s, e)| (s, e)),
            selected,
            now_min,
        );
        day_view = (g.offset, g.rph, g.max_offset);
        let x0 = grid.x + LABEL_W + 1;
        let cw = grid.width.saturating_sub(LABEL_W + 1);
        draw_hour_axis(buf, grid, &g, x0, cw);
        let mut occupied = vec![vec![false; grid.width as usize]; grid.height as usize];
        let col = Column {
            items: &app.day_items,
            spans: &spans_min,
            x: x0,
            width: cw,
            selected: Some(app.appt_sel),
            past_before: is_today.then_some(now),
            compact: false,
        };
        draw_blocks(
            buf,
            app,
            grid,
            &g,
            &col,
            sel_bg,
            &mut occupied,
            &mut hits,
            Hit::Appointment,
        );
        if let Some(n) = now_min {
            draw_now_line(buf, grid, &g, n, now, x0, cw, &occupied);
        }
    }
    app.hits.extend(hits);
    app.day_view = day_view;
}

/// Width of the hour labels left of the grid's rule.
const LABEL_W: u16 = 6;
/// Narrowest day column (without its separator) the week grid will draw.
const WEEK_MIN_COL: u16 = 6;

/// Can the week grid fit in a panel this wide?
fn week_fits(width: u16) -> bool {
    width >= LABEL_W + 1 + 7 * (WEEK_MIN_COL + 1)
}

/// Minutes since the start of `day` (clamped to that day).
fn day_minute(app: &App, day: NaiveDate, t: chrono::DateTime<chrono_tz::Tz>) -> u32 {
    let day_start = app.store.day_start(day);
    let day_end = app.store.day_start(day + Duration::days(1));
    (t.clamp(day_start, day_end) - day_start)
        .num_minutes()
        .max(0) as u32
}

/// (index, start minute, end minute) of each timed item of `day`.
fn timed_spans(
    app: &App,
    items: &[Instance],
    timed: &[usize],
    day: NaiveDate,
) -> Vec<(usize, u32, u32)> {
    let home = app.store.home;
    timed
        .iter()
        .map(|&i| {
            let inst = &items[i];
            let s = day_minute(app, day, inst.start.instant(home));
            let e = day_minute(app, day, inst.end.instant(home)).max(s + 1);
            (i, s, e)
        })
        .collect()
}

/// Indices of the all-day items (and due todos) of `day`, then the timed ones.
fn split_all_day(app: &App, items: &[Instance], day: NaiveDate) -> (Vec<usize>, Vec<usize>) {
    (0..items.len()).partition(|&i| {
        matches!(
            time_range(&items[i], day, app.store.home),
            TimeRange::AllDay | TimeRange::Due(_)
        )
    })
}

/// Vertical geometry of an hour grid: the whole day (00–24) can be
/// scrolled, at a density chosen so the usual hours fit.
struct HourGrid {
    /// First row shown (rows from 00:00).
    offset: u32,
    max_offset: u32,
    /// Rows in the whole day.
    total: u32,
    rph: u32,
}

impl HourGrid {
    /// `spans` are the (start, end) minutes of the items shown; `selected` is
    /// the start minute of the selected item, `now` the current minute (if
    /// shown). The usual hours (08–20) are widened to fit the items.
    fn new(
        app: &App,
        height: u16,
        spans: impl Iterator<Item = (u32, u32)>,
        selected: Option<u32>,
        now: Option<u32>,
    ) -> HourGrid {
        let mut want_first = DAY_FIRST_HOUR;
        let mut want_last = DAY_LAST_HOUR;
        for (s, e) in spans {
            want_first = want_first.min(s / 60);
            want_last = want_last.max(e.div_ceil(60));
        }
        let span_hours = (want_last.min(24) - want_first).max(1);
        let rph = (height as u32 / span_hours).clamp(1, 4);
        let total = 24 * rph;
        let view = height as u32;
        let max_offset = total.saturating_sub(view);
        let mut g = HourGrid {
            offset: 0,
            max_offset,
            total,
            rph,
        };
        // Automatic position: the usual hours, unless the selected item (or
        // now) would be outside them; then that, a third from the top.
        let base = g.row_of(want_first * 60);
        let focus_row = selected.or(now).map(|m| g.row_of(m));
        let auto = match focus_row {
            Some(r) if r < base || r >= base + view => r.saturating_sub(view / 3),
            _ => base,
        };
        g.offset = app.day_scroll.unwrap_or(auto).min(max_offset);
        g
    }

    fn row_of(&self, minute: u32) -> u32 {
        minute / (60 / self.rph)
    }

    /// Screen row (from the grid's top) of absolute row `abs`, if visible.
    fn visible(&self, abs: u32, height: u16) -> Option<u16> {
        abs.checked_sub(self.offset)
            .filter(|r| *r < height as u32)
            .map(|r| r as u16)
    }
}

/// Hour labels, the rule, faint dotted hour lines over `x0..x0 + cw`, and
/// the ▲ ▼ markers when more hours are above / below.
fn draw_hour_axis(buf: &mut Buffer, grid: Rect, g: &HourGrid, x0: u16, cw: u16) {
    let label = Style::new().fg(Color::Indexed(244));
    for r in 0..grid.height {
        let abs = g.offset + r as u32;
        if abs >= g.total {
            break;
        }
        let text = if abs.is_multiple_of(g.rph) {
            format!(" {:02}:00", abs / g.rph)
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

    // A faint dotted line at each full hour (behind the events, which are
    // drawn next). Colour: one step above the background of the Stylix
    // scheme this was tuned on. See DESIGN.md for the band variants.
    for r in 0..grid.height {
        let abs = g.offset + r as u32;
        if abs >= g.total {
            break;
        }
        if abs.is_multiple_of(g.rph) && abs > 0 {
            let dots = "┈".repeat(cw as usize);
            buf.set_string(
                x0,
                grid.y + r,
                dots,
                Style::new().fg(Color::Rgb(36, 45, 56)),
            );
        }
    }

    let more = Style::new().fg(Color::Indexed(250));
    if g.offset > 0 {
        buf.set_string(grid.x + LABEL_W, grid.y, "▲", more);
    }
    if g.offset < g.max_offset {
        buf.set_string(grid.x + LABEL_W, grid.bottom() - 1, "▼", more);
    }
}

/// Lanes for overlapping events: (lane of each span, lanes in its cluster).
/// Events are grouped into clusters that overlap each other; only a cluster
/// is split into lanes, so an event that overlaps nothing keeps the full
/// width. (Items are sorted by start, so a cluster ends where nothing running
/// reaches the next.)
fn lanes(spans: &[(usize, u32, u32)], g: &HourGrid) -> (Vec<usize>, Vec<usize>) {
    let mut lane_of = vec![0usize; spans.len()];
    let mut lanes_of = vec![1usize; spans.len()];
    let mut cluster: Vec<usize> = Vec::new();
    let mut lane_end: Vec<u32> = Vec::new();
    let mut cluster_end = 0u32;
    let close = |cluster: &mut Vec<usize>, lanes: usize, lanes_of: &mut Vec<usize>| {
        for k in cluster.drain(..) {
            lanes_of[k] = lanes.max(1);
        }
    };
    for (k, &(_, s, e)) in spans.iter().enumerate() {
        let (srow, erow) = (g.row_of(s), g.row_of(e).max(g.row_of(s) + 1));
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
    (lane_of, lanes_of)
}

/// One column of event blocks: a day's items drawn into `x..x + width`.
struct Column<'a> {
    items: &'a [Instance],
    /// (index into `items`, start minute, end minute) of the timed items.
    spans: &'a [(usize, u32, u32)],
    x: u16,
    width: u16,
    /// Index of the selected item, if it is in this column.
    selected: Option<usize>,
    /// Items ending at or before this are dimmed as past.
    past_before: Option<chrono::DateTime<chrono_tz::Tz>>,
    /// Narrow week column: title, then times, on their own rows.
    compact: bool,
}

/// Draw a column's timed items as blocks. Marks the cells they cover in
/// `occupied` (indexed from the grid's top-left) and adds their hits.
#[allow(clippy::too_many_arguments)]
fn draw_blocks(
    buf: &mut Buffer,
    app: &App,
    grid: Rect,
    g: &HourGrid,
    col: &Column,
    sel_bg: Color,
    occupied: &mut [Vec<bool>],
    hits: &mut Vec<(Rect, Hit)>,
    hit: impl Fn(usize) -> Hit,
) {
    let home = app.store.home;
    let (lane_of, lanes_of) = lanes(col.spans, g);
    // Lanes are at least 4 cells; in a narrow column, extra lanes share the
    // last one rather than spill out of it.
    let max_lanes = (col.width / 4).max(1) as usize;
    for (k, &(idx, s, e)) in col.spans.iter().enumerate() {
        let inst = &col.items[idx];
        let selected = col.selected == Some(idx);
        let (srow, erow) = (g.row_of(s), g.row_of(e).max(g.row_of(s) + 1));
        let lanes = lanes_of[k].min(max_lanes) as u16;
        let lane = lane_of[k].min(lanes as usize - 1) as u16;
        let lane_w = (col.width / lanes).max(4);
        let x = col.x + lane * lane_w;
        let w = if lane + 1 == lanes {
            col.x + col.width - x
        } else {
            lane_w - 1
        };
        let start = inst.start.instant(home);
        let end = inst.end.instant(home);
        let past = col.past_before.is_some_and(|now| end <= now);
        let (title, rest) = split_title(app, inst);
        let bg = if selected { sel_bg } else { BLOCK_BG };
        let text_fg = if past {
            Color::Indexed(244)
        } else {
            Color::Reset
        };
        let times = format!("{}–{}", start.format("%H:%M"), end.format("%H:%M"));
        let bold = Style::new().fg(text_fg).add_modifier(Modifier::BOLD);
        for (n, abs) in (srow..erow).enumerate() {
            let Some(r) = g.visible(abs, grid.height) else {
                continue;
            };
            let ry = grid.y + r;
            let mut spans = vec![Span::styled("▌", cal_style(app, inst.calendar))];
            let room = (w as usize).saturating_sub(1);
            if col.compact {
                match n {
                    0 => spans.push(Span::styled(ellipsize(&title, room), bold)),
                    1 => {
                        // The whole range, the start, or nothing: never a
                        // clipped time.
                        let t = if times.chars().count() <= room {
                            times.clone()
                        } else if room >= 5 {
                            start.format("%H:%M").to_string()
                        } else {
                            String::new()
                        };
                        spans.push(Span::raw(t).fg(Color::Indexed(246)));
                    }
                    2 if w >= 18 => spans.extend(details(app, inst, &rest)),
                    _ => {}
                }
            } else {
                match n {
                    0 => {
                        spans.push(Span::styled(format!(" {title}"), bold));
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
            }
            if past {
                for sp in spans.iter_mut().skip(1) {
                    sp.style = sp.style.fg(Color::Indexed(244));
                }
            }
            put(buf, x, ry, w, Line::from(spans), Some(bg));
            for c in (x - grid.x)..(x - grid.x + w) {
                if let Some(cell) = occupied[r as usize].get_mut(c as usize) {
                    *cell = true;
                }
            }
            hits.push((Rect::new(x, ry, w, 1), hit(idx)));
        }
    }
}

/// The "now" line through the free cells of `x0..x0 + cw`, and the time in
/// the label margin.
#[allow(clippy::too_many_arguments)]
fn draw_now_line(
    buf: &mut Buffer,
    grid: Rect,
    g: &HourGrid,
    now_min: u32,
    now: chrono::DateTime<chrono_tz::Tz>,
    x0: u16,
    cw: u16,
    occupied: &[Vec<bool>],
) {
    let Some(r) = g.visible(g.row_of(now_min), grid.height) else {
        return;
    };
    let ry = grid.y + r;
    let accent = Style::new().fg(NOW);
    buf.set_string(
        grid.x,
        ry,
        format!(" {:<5}", now.format("%H:%M")),
        accent.bold(),
    );
    buf.set_string(grid.x + LABEL_W, ry, "┼", accent);
    for x in x0..x0 + cw {
        if !occupied[r as usize][(x - grid.x) as usize] {
            buf.set_string(x, ry, "─", accent);
        }
    }
}

/// The appointments panel in week view: Monday to Sunday of the selected
/// day's week as columns on one hour grid. The selected day's column is
/// highlighted; its items are the ones `j`/`k`, edit and delete act on.
fn draw_week(f: &mut Frame, app: &mut App, area: Rect) {
    let today = app.today();
    let monday = week_start(app.date);
    let sunday = monday + Duration::days(6);
    let range = if monday.month() == sunday.month() {
        format!("{}–{}", monday.day(), sunday.format("%-d %B"))
    } else {
        format!("{} – {}", monday.format("%-d %b"), sunday.format("%-d %b"))
    };
    let mut title = format!("Week {} · {range}", monday.iso_week().week());
    if (monday..=sunday).contains(&today) {
        title.push_str(" · this week");
    } else if monday.year() != today.year() {
        title.push_str(&format!(" {}", sunday.year()));
    }
    let inner = panel_frame(f, app, PanelKind::Appointments, &title, area, false);
    if inner.height < 3 {
        return;
    }

    let focused = app.focus == PanelKind::Appointments;
    let now = app.now();
    let sel_bg = if focused {
        Color::Indexed(24)
    } else {
        Color::Indexed(238)
    };
    let label = Style::new().fg(Color::Indexed(244));
    let sep = Style::new().fg(Color::Indexed(238));
    let mut hits: Vec<(Rect, Hit)> = Vec::new();
    let buf = f.buffer_mut();

    // Day columns: x and width (without the separator after it).
    let x0 = inner.x + LABEL_W + 1;
    let avail = inner.width.saturating_sub(LABEL_W + 1);
    let col_x = |n: u16| x0 + n * avail / 7;
    let cols: Vec<(u16, u16)> = (0..7)
        .map(|n| {
            let x = col_x(n);
            let w = if n == 6 {
                x0 + avail - x
            } else {
                col_x(n + 1) - x - 1
            };
            (x, w)
        })
        .collect();
    let days: Vec<(NaiveDate, &[Instance])> = (0..7)
        .map(|n| {
            let d = monday + Duration::days(n);
            let items = app
                .week_items
                .iter()
                .find(|(day, _)| *day == d)
                .map(|(_, v)| v.as_slice())
                .unwrap_or(&[]);
            (d, items)
        })
        .collect();
    let parts: Vec<(Vec<usize>, Vec<usize>)> = days
        .iter()
        .map(|(d, items)| split_all_day(app, items, *d))
        .collect();

    // Header: "Mo 16" per column; today as a pill in the "now" red, the
    // selected day on the selection colour.
    let y = inner.y;
    for (n, &(x, w)) in cols.iter().enumerate() {
        let d = days[n].0;
        let text = format!(" {} {:>2}", &d.format("%a").to_string()[..2], d.day());
        let mut around = Style::new().fg(Color::Indexed(250));
        if d == app.date {
            around = around.bg(sel_bg).add_modifier(Modifier::BOLD);
            buf.set_style(Rect::new(x, y, w, 1), around);
        }
        let style = if d == today {
            Style::new()
                .bg(NOW)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD)
        } else {
            around
        };
        buf.set_string(x, y, ellipsize(&format!("{text} "), w as usize), style);
        hits.push((Rect::new(x, y, w, 1), Hit::WeekDay(d)));
    }

    // All-day items and due todos: up to two rows; a full column says how
    // many more it has.
    let most = parts.iter().map(|(a, _)| a.len()).max().unwrap_or(0);
    let rows = most.min(2) as u16;
    for r in 0..rows {
        let ry = inner.y + 1 + r;
        if r == 0 {
            buf.set_string(inner.x, ry, " all", label);
        }
        for (n, &(x, w)) in cols.iter().enumerate() {
            let (d, items) = days[n];
            let all = &parts[n].0;
            let r = r as usize;
            if all.len() > rows as usize && r + 1 == rows as usize {
                let more = format!(" +{}", all.len() - r);
                buf.set_string(x, ry, ellipsize(&more, w as usize), label);
                hits.push((Rect::new(x, ry, w, 1), Hit::WeekDay(d)));
                continue;
            }
            let Some(&idx) = all.get(r) else { continue };
            let inst = &items[idx];
            let selected = d == app.date && idx == app.appt_sel;
            let (title, _) = split_title(app, inst);
            let line = Line::from(vec![
                Span::styled("▌", cal_style(app, inst.calendar)),
                Span::raw(ellipsize(&title, (w as usize).saturating_sub(1))),
            ]);
            let bg = if selected { sel_bg } else { BLOCK_BG };
            put(buf, x, ry, w, line, Some(bg));
            hits.push((Rect::new(x, ry, w, 1), Hit::WeekItem(d, idx)));
        }
    }

    // The hour grid.
    let top = inner.y + 1 + rows;
    let grid = Rect::new(
        inner.x,
        top,
        inner.width,
        inner.bottom().saturating_sub(top),
    );
    let mut day_view = (0u32, 1u32, 0u32);
    if grid.height >= 2 {
        let spans: Vec<Vec<(usize, u32, u32)>> = days
            .iter()
            .zip(&parts)
            .map(|((d, items), (_, timed))| timed_spans(app, items, timed, *d))
            .collect();
        let sel_n = days.iter().position(|(d, _)| *d == app.date);
        let selected = sel_n.and_then(|n| {
            spans[n]
                .iter()
                .find(|(i, _, _)| *i == app.appt_sel)
                .map(|(_, s, _)| *s)
        });
        let today_n = days.iter().position(|(d, _)| *d == today);
        let now_min = today_n.map(|_| day_minute(app, today, now));
        let g = HourGrid::new(
            app,
            grid.height,
            spans.iter().flatten().map(|&(_, s, e)| (s, e)),
            selected,
            now_min,
        );
        day_view = (g.offset, g.rph, g.max_offset);
        draw_hour_axis(buf, grid, &g, x0, avail);

        // Column separators, from the header down.
        for &(x, w) in &cols[..6] {
            for ry in inner.y..grid.bottom() {
                buf.set_string(x + w, ry, "│", sep);
            }
        }

        let mut occupied = vec![vec![false; grid.width as usize]; grid.height as usize];
        for (n, &(x, w)) in cols.iter().enumerate() {
            let (d, items) = days[n];
            // Free space in a column selects that day.
            hits.push((Rect::new(x, grid.y, w, grid.height), Hit::WeekDay(d)));
            let col = Column {
                items,
                spans: &spans[n],
                x,
                width: w,
                selected: (d == app.date).then_some(app.appt_sel),
                past_before: Some(now),
                compact: true,
            };
            draw_blocks(
                buf,
                app,
                grid,
                &g,
                &col,
                sel_bg,
                &mut occupied,
                &mut hits,
                |i| Hit::WeekItem(d, i),
            );
        }
        if let (Some(n), Some(m)) = (today_n, now_min) {
            let (x, w) = cols[n];
            draw_now_line(buf, grid, &g, m, now, x, w, &occupied);
        }
    }
    app.hits.extend(hits);
    app.day_view = day_view;
}

fn draw_calendar(f: &mut Frame, app: &mut App, area: Rect) {
    let title = app.date.format("%B %Y").to_string();
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

    let mut hits: Vec<(Rect, Hit)> = Vec::new();
    let row_rect = |idx: usize| {
        (idx < inner.height as usize)
            .then(|| Rect::new(inner.x, inner.y + idx as u16, inner.width, 1))
    };
    let lines: Vec<Line> = {
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
            // Today: a filled pill in the "now" red, the same colour as the
            // now line in the day view (wins over the selection colour).
            let pill = if d == today {
                Style::new()
                    .bg(NOW)
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD)
            } else {
                around
            };
            if d == today {
                text = pill;
            }
            let mut cell = vec![
                Span::styled(" ", pill),
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
                Span::styled(
                    format!("{:>2} ", monday.iso_week().week()),
                    // The current week's number in the "now" colour.
                    if (monday..monday + Duration::days(7)).contains(&today) {
                        Style::new().fg(NOW).add_modifier(Modifier::BOLD)
                    } else {
                        label
                    },
                ),
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
            // Lines the whole list needs (week rules, day headers, items); if
            // it does not fit, keep the last line for "N more".
            let needed_total = {
                let (mut n, mut ld, mut lw) = (0, None, None);
                for i in &app.upcoming {
                    let day = i.start.date_in(home).max(from);
                    if ld != Some(day) {
                        if lw != Some(monday(day)) {
                            n += 1;
                            lw = Some(monday(day));
                        }
                        n += 1;
                        ld = Some(day);
                    }
                    n += 1;
                }
                n
            };
            let limit = if lines.len() + needed_total > inner.height as usize {
                (inner.height as usize).saturating_sub(1)
            } else {
                inner.height as usize
            };
            let mut shown = 0;
            let title_w = (inner.width as usize).saturating_sub(9);
            let mut last_day = None;
            let mut last_week = None;
            for i in &app.upcoming {
                let day = i.start.date_in(home).max(from);
                if last_day != Some(day) {
                    let new_week = last_week != Some(monday(day));
                    // Only start a day if one item fits under its header.
                    let needed = if new_week { 3 } else { 2 };
                    if lines.len() + needed > limit {
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
                if lines.len() >= limit {
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
                shown += 1;
            }
            // What did not fit: how many, from which day (click to go there).
            if let Some(next) = app.upcoming.get(shown) {
                let hidden = app.upcoming.len() - shown;
                let day = next.start.date_in(home).max(from);
                if let Some(r) = row_rect(lines.len()) {
                    hits.push((r, Hit::AgendaDay(day)));
                }
                lines.push(
                    Line::from(format!(" ⋯ {hidden} more from {}", day.format("%A %-d %b")))
                        .style(Style::new().fg(MUTED).add_modifier(Modifier::ITALIC)),
                );
            }
        }
        lines
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
        lines.push(Line::from(" no todos").fg(Color::Indexed(245)));
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
            Span::raw(format!(" {prio}. ")).fg(Color::Indexed(245)),
            Span::styled(t.summary.clone(), style),
        ];
        if let Some(due) = t.due {
            let d = due.date_in(app.store.home);
            let overdue = !t.completed && d < app.today();
            let label = Span::raw(format!(" {}", d.format("%d %b")));
            spans.push(if overdue {
                label.red()
            } else {
                label.fg(Color::Indexed(245))
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

/// One hint in the key bar: which keys to show for it.
#[derive(Clone, Copy)]
enum Hint {
    /// The action's first key.
    First(K),
    /// All of the action's keys, `Tab/↓`.
    All(K),
    /// First keys of two opposite actions, `h/l`.
    Pair(K, K),
}

fn hint_keys(km: &Keymap, h: Hint) -> String {
    let first = |a| km.keys(a).first().map(|k| k.to_string());
    let parts: Vec<String> = match h {
        Hint::First(a) => first(a).into_iter().collect(),
        Hint::All(a) => km.keys(a).iter().map(|k| k.to_string()).collect(),
        Hint::Pair(a, b) => first(a).into_iter().chain(first(b)).collect(),
    };
    parts.join("/")
}

fn draw_keys(f: &mut Frame, app: &App, area: Rect) {
    use Hint::{All, First, Pair};
    if let Some(msg) = &app.message {
        f.render_widget(Paragraph::new(format!(" {msg}")).yellow(), area);
        return;
    }
    let hints: &[(Hint, &str)] = match &app.popup {
        None if app.layout_mode => &[
            (Pair(K::LayoutPrevPreset, K::LayoutNextPreset), "Preset"),
            (First(K::LayoutNextPanel), "Panel"),
            (Pair(K::LayoutGrow, K::LayoutShrink), "Size"),
            (
                Pair(K::LayoutShrinkColumn, K::LayoutGrowColumn),
                "Column size",
            ),
            (First(K::LayoutSwap), "Swap"),
            (First(K::LayoutRotate), "Rotate"),
            (First(K::LayoutHide), "Hide"),
            (First(K::LayoutUnhide), "Unhide"),
            (First(K::LayoutSave), "Save"),
            (First(K::LayoutDone), "Done"),
        ],
        Some(Popup::Calendars { .. }) => &[
            (Pair(K::PopupDown, K::PopupUp), "Move"),
            (First(K::PopupToggle), "Toggle"),
            (First(K::PopupClose), "Close"),
        ],
        Some(Popup::Goto { .. }) => &[(First(K::FormSave), "Go"), (First(K::FormCancel), "Cancel")],
        Some(Popup::Form(_)) => &[
            (First(K::FormSave), "Save"),
            (All(K::FormNextField), "Next field"),
            (All(K::FormPrevField), "Previous"),
            (First(K::FormClear), "Clear"),
            (First(K::FormCancel), "Cancel"),
        ],
        Some(Popup::Scope(_)) => &[
            (First(K::PopupThisOccurrence), "This occurrence"),
            (First(K::PopupAllOccurrences), "All"),
            (First(K::PopupClose), "Cancel"),
        ],
        Some(Popup::Confirm(_)) => &[(First(K::PopupYes), "Yes"), (First(K::PopupNo), "No")],
        Some(_) => &[(First(K::PopupClose), "Close")],
        // Most important first; the bar is cut at the terminal width.
        None if app.focus == PanelKind::Todo => &[
            (First(K::Help), "Help"),
            (First(K::NextPanel), "Panel"),
            (Pair(K::Down, K::Up), "Move"),
            (First(K::Add), "Add"),
            (First(K::Edit), "Edit"),
            (First(K::Delete), "Delete"),
            (First(K::ToggleDone), "Done"),
            (Pair(K::PriorityUp, K::PriorityDown), "Priority"),
            (First(K::ToggleCompleted), "Show done"),
            (First(K::Sync), "Sync"),
            (First(K::Quit), "Quit"),
        ],
        None => &[
            (First(K::Help), "Help"),
            (First(K::NextPanel), "Panel"),
            (Pair(K::PrevDay, K::NextDay), "Day"),
            (Pair(K::Down, K::Up), "Move"),
            (First(K::Goto), "Goto"),
            (First(K::Today), "Today"),
            (First(K::Add), "Add"),
            (First(K::Edit), "Edit"),
            (First(K::Delete), "Delete"),
            (First(K::Details), "View"),
            (First(K::ToggleWeekView), "Week"),
            (First(K::Sync), "Sync"),
            (First(K::Quit), "Quit"),
        ],
    };
    let mut spans = vec![Span::raw(" ")];
    for (h, label) in hints {
        let k = hint_keys(&app.keymap, *h);
        if k.is_empty() {
            continue; // unbound
        }
        spans.push(Span::raw(k).bold().cyan());
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

/// Background of pop-ups: a card slightly lighter than the screen.
const POPUP_BG: Color = Color::Indexed(235);

/// Secondary text in pop-ups (labels, hints): light enough to read on the
/// card background.
const MUTED: Color = Color::Indexed(248);

/// Accent bar at the top of a pop-up.
const POPUP_BAR: Color = Color::Rgb(95, 135, 175);

/// A pop-up is a filled card without lines: padding where a border would be
/// (so sizes and positions stay the same), title in the top row, which
/// `draw` turns into an accent bar.
fn popup_block(title: &str) -> Block<'_> {
    Block::new()
        .style(Style::new().bg(POPUP_BG))
        .title(Line::from(format!(" {title} ")).bold().centered())
        .padding(ratatui::widgets::Padding::new(1, 1, 0, 1))
}

/// Keys of one action for the help pop-up: `x, Space`.
fn help_keys(km: &Keymap, a: K) -> String {
    let v: Vec<String> = km.keys(a).iter().map(|k| k.to_string()).collect();
    v.join(", ")
}

/// Keys of two opposite actions: `h / l, ← / →` when they pair up.
fn help_pair(km: &Keymap, a: K, b: K) -> String {
    let (ka, kb) = (km.keys(a), km.keys(b));
    if ka.is_empty() || kb.is_empty() {
        return format!("{}{}", help_keys(km, a), help_keys(km, b));
    }
    if ka.len() == kb.len() {
        let v: Vec<String> = ka
            .iter()
            .zip(kb)
            .map(|(x, y)| format!("{x} / {y}"))
            .collect();
        return v.join(", ");
    }
    format!("{} / {}", help_keys(km, a), help_keys(km, b))
}

fn draw_help(f: &mut Frame, km: &Keymap) {
    let pair = |a, b, d: &'static str| (help_pair(km, a, b), d);
    let one = |a, d: &'static str| (help_keys(km, a), d);
    let keys = [
        pair(K::PrevDay, K::NextDay, "previous / next day"),
        pair(K::PrevWeek, K::NextWeek, "previous / next week"),
        pair(K::PrevMonth, K::NextMonth, "previous / next month"),
        pair(K::Down, K::Up, "move selection (calendar panel: week)"),
        pair(
            K::ScrollDown,
            K::ScrollUp,
            "scroll the day's hours (also the mouse wheel)",
        ),
        pair(K::NextPanel, K::PrevPanel, "next / previous panel"),
        one(K::Today, "go to today"),
        one(K::Goto, "go to date (2026-12-24, 12-24, 24, +7, -7)"),
        one(K::Details, "show details of the selected item"),
        one(K::Add, "add an event (todo panel: a todo)"),
        one(K::Edit, "edit the selected item"),
        one(K::Delete, "delete the selected item"),
        one(K::ToggleDone, "todo: mark done / not done"),
        pair(
            K::PriorityUp,
            K::PriorityDown,
            "todo: raise / lower priority",
        ),
        one(K::ToggleWeekView, "day / week view of the appointments"),
        one(K::Calendars, "show / hide calendars"),
        one(K::ToggleCompleted, "show / hide completed todos"),
        one(K::Sync, "sync (runs sync_command), then reload"),
        one(K::Reload, "reload from disk"),
        one(K::Warnings, "show loading warnings"),
        one(
            K::LayoutMode,
            "layout mode: presets, resize, swap, hide panels; Enter saves",
        ),
        one(K::Quit, "quit"),
    ];
    let row = |k: &str, d: &str| {
        Line::from(vec![
            Span::raw(format!(" {k:<18}")).bold().cyan(),
            Span::raw(d.to_string()),
        ])
    };
    let mut lines: Vec<Line> = keys
        .iter()
        .filter(|(k, _)| !k.is_empty()) // unbound
        .map(|(k, d)| row(k, d))
        .collect();
    lines.push(row("", ""));
    lines.push(row(
        "Markers",
        "● calendar item  ○ read-only  * repeats  > has notes",
    ));
    let area = popup_area(f, 74, lines.len() as u16 + 2);
    f.render_widget(Clear, area);
    f.render_widget(Paragraph::new(lines).block(popup_block("Help")), area);
}

fn field(label: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::raw(format!(" {label:<10}")).fg(MUTED),
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
                Span::raw(if cal.read_only { "  read-only" } else { "" }).fg(MUTED),
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
        Line::from(" 2026-12-24 · 12-24 · 24 · +7 · -7 · today").fg(MUTED),
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
            label.fg(MUTED)
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
            if field.input.text.is_empty() {
                // Example of what to type, until something is typed.
                Span::raw(field.id.placeholder()).fg(MUTED).italic()
            } else if focused {
                Span::raw(field.input.text.clone()).bold()
            } else {
                Span::raw(field.input.text.clone())
            }
        };
        lines.push(Line::from(vec![label, value]));
    }
    lines.push(Line::from(""));
    let hint = form.fields[form.focus].id.hint();
    lines.push(Line::from(format!(" {hint}")).fg(MUTED));
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
