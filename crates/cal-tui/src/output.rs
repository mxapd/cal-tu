//! Plain-text and JSON output for the shell commands.

use anyhow::Result;
use cal_core::{Instance, Kind, Moment, Store, Warning};
use chrono::{DateTime, Duration, NaiveDate};
use chrono_tz::Tz;
use serde::Serialize;

#[derive(Serialize)]
struct JsonItem<'a> {
    calendar: &'a str,
    calendar_name: &'a str,
    kind: &'static str,
    uid: &'a str,
    summary: &'a str,
    location: Option<&'a str>,
    description: Option<&'a str>,
    start: String,
    end: String,
    all_day: bool,
    recurring: bool,
    read_only: bool,
}

fn moment_json(m: &Moment, home: Tz) -> String {
    match m {
        Moment::Date(d) => d.format("%Y-%m-%d").to_string(),
        Moment::DateTime(dt) => dt.with_timezone(&home).to_rfc3339(),
    }
}

fn json_item<'a>(store: &'a Store, i: &'a Instance) -> JsonItem<'a> {
    let cal = &store.calendars[i.calendar];
    JsonItem {
        calendar: &cal.id,
        calendar_name: &cal.name,
        kind: match i.kind {
            Kind::Event => "event",
            Kind::Todo => "todo",
        },
        uid: &i.uid,
        summary: &i.summary,
        location: i.location.as_deref(),
        description: i.description.as_deref(),
        start: moment_json(&i.start, store.home),
        end: moment_json(&i.end, store.home),
        all_day: i.all_day,
        recurring: i.recurrence_id.is_some(),
        read_only: cal.read_only,
    }
}

/// ANSI foreground escape for a colour name or `#rrggbb`.
fn ansi(color: Option<&str>) -> Option<String> {
    let c = color?.trim();
    if let Some(hex) = c.strip_prefix('#') {
        if hex.len() >= 6 {
            let p = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
            return Some(format!("\x1b[38;2;{};{};{}m", p(0)?, p(2)?, p(4)?));
        }
        return None;
    }
    let code = match c.to_ascii_lowercase().as_str() {
        "black" => 30,
        "red" => 31,
        "green" => 32,
        "yellow" => 33,
        "blue" => 34,
        "magenta" => 35,
        "cyan" => 36,
        "white" => 37,
        _ => return None,
    };
    Some(format!("\x1b[{code}m"))
}

fn paint(text: &str, color: Option<&str>, enabled: bool) -> String {
    match ansi(color).filter(|_| enabled) {
        Some(esc) => format!("{esc}{text}\x1b[0m"),
        None => text.to_string(),
    }
}

/// Time column for an instance shown on `day`, e.g. `08:15-10:00`.
fn time_column(i: &Instance, day: NaiveDate, home: Tz) -> String {
    if i.kind == Kind::Todo {
        return match i.start {
            Moment::DateTime(dt) => format!("due {}", dt.with_timezone(&home).format("%H:%M")),
            Moment::Date(_) => "due".into(),
        };
    }
    match (i.start, i.end) {
        (Moment::Date(_), _) | (_, Moment::Date(_)) => "all day".into(),
        (Moment::DateTime(s), Moment::DateTime(e)) => {
            let s = s.with_timezone(&home);
            let e = e.with_timezone(&home);
            let starts_before = s.date_naive() < day;
            // An end at exactly midnight belongs to the previous day.
            let ends_after = e.date_naive() > day
                && !(e.date_naive() == day + Duration::days(1)
                    && e.time() == chrono::NaiveTime::MIN);
            match (starts_before, ends_after) {
                (true, true) => "all day".into(),
                (true, false) => format!("   …-{}", e.format("%H:%M")),
                (false, true) => format!("{}-…   ", s.format("%H:%M")),
                (false, false) if s == e => format!("{}      ", s.format("%H:%M")),
                (false, false) => format!("{}-{}", s.format("%H:%M"), e.format("%H:%M")),
            }
        }
    }
}

fn on_day(store: &Store, i: &Instance, day: NaiveDate) -> bool {
    let start = store.day_start(day);
    let end = store.day_start(day + Duration::days(1));
    let s = i.start.instant(store.home);
    let e = i.end.instant(store.home);
    if s == e {
        s >= start && s < end
    } else {
        s < end && e > start
    }
}

pub fn agenda(
    store: &Store,
    inst: &[Instance],
    from: NaiveDate,
    days: u32,
    json: bool,
    color: bool,
) -> Result<()> {
    if json {
        let items: Vec<_> = inst.iter().map(|i| json_item(store, i)).collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
        return Ok(());
    }
    let mut first = true;
    for n in 0..days {
        let day = from + Duration::days(n as i64);
        let mut items: Vec<&Instance> = inst.iter().filter(|i| on_day(store, i, day)).collect();
        // All-day items first, then by start time.
        items.sort_by_key(|i| {
            (
                time_column(i, day, store.home) != "all day",
                i.start.instant(store.home),
            )
        });
        if items.is_empty() && days > 1 {
            continue;
        }
        if !first {
            println!();
        }
        first = false;
        let header = day.format("%a %d %b %Y").to_string();
        println!(
            "{}",
            if color {
                format!("\x1b[1m{header}\x1b[0m")
            } else {
                header
            }
        );
        if items.is_empty() {
            println!("  nothing scheduled");
        }
        for i in items {
            let cal = &store.calendars[i.calendar];
            let mut line = paint(&i.summary, cal.color.as_deref(), color);
            if let Some(loc) = &i.location {
                line.push_str(&format!(" · {loc}"));
            }
            let tag = paint(&format!("[{}]", cal.name), cal.color.as_deref(), color);
            println!("  {:<11}  {line}  {tag}", time_column(i, day, store.home));
        }
    }
    if first {
        println!("nothing scheduled");
    }
    Ok(())
}

fn humanize(d: Duration) -> String {
    let mins = d.num_minutes().max(0);
    match (mins / 1440, (mins % 1440) / 60, mins % 60) {
        (0, 0, m) => format!("{m}m"),
        (0, h, 0) => format!("{h}h"),
        (0, h, m) => format!("{h}h{m:02}m"),
        (d, h, _) => format!("{d}d{h}h"),
    }
}

pub fn next(
    store: &Store,
    i: &Instance,
    now: DateTime<Tz>,
    format: &str,
    json: bool,
) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(&json_item(store, i))?);
        return Ok(());
    }
    let start = i.start.instant(store.home);
    let time = if start.date_naive() == now.date_naive() {
        start.format("%H:%M").to_string()
    } else {
        start.format("%a %H:%M").to_string()
    };
    let out = format
        .replace("{time}", &time)
        .replace("{summary}", &i.summary)
        .replace("{in}", &humanize(start - now))
        .replace("{calendar}", &store.calendars[i.calendar].name)
        .replace("{location}", i.location.as_deref().unwrap_or(""));
    println!("{out}");
    Ok(())
}

#[derive(Serialize)]
struct JsonTodo<'a> {
    calendar: &'a str,
    uid: &'a str,
    summary: &'a str,
    description: Option<&'a str>,
    priority: u8,
    due: Option<String>,
    completed: bool,
}

pub fn todos(store: &Store, all: bool, json: bool, color: bool) -> Result<()> {
    let todos: Vec<_> = store
        .todos()
        .into_iter()
        .filter(|t| all || !t.completed)
        .collect();
    if json {
        let items: Vec<_> = todos
            .iter()
            .map(|t| JsonTodo {
                calendar: &store.calendars[t.calendar].id,
                uid: &t.uid,
                summary: &t.summary,
                description: t.description.as_deref(),
                priority: t.priority,
                due: t.due.map(|d| moment_json(&d, store.home)),
                completed: t.completed,
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
        return Ok(());
    }
    if todos.is_empty() {
        println!("no todos");
    }
    for t in todos {
        let cal = &store.calendars[t.calendar];
        let prio = if t.priority == 0 {
            "-".to_string()
        } else {
            t.priority.to_string()
        };
        let mark = if t.completed { "x" } else { " " };
        let due = t
            .due
            .map(|d| match d {
                Moment::Date(d) => format!("  (due {})", d.format("%a %d %b")),
                Moment::DateTime(dt) => format!(
                    "  (due {})",
                    dt.with_timezone(&store.home).format("%a %d %b %H:%M")
                ),
            })
            .unwrap_or_default();
        let tag = paint(&format!("[{}]", cal.name), cal.color.as_deref(), color);
        println!(
            "[{mark}] {prio} {}{due}  {tag}",
            paint(&t.summary, cal.color.as_deref(), color)
        );
    }
    Ok(())
}

pub fn report_warnings(warnings: &[Warning], verbose: bool) {
    if warnings.is_empty() {
        return;
    }
    if verbose {
        for w in warnings {
            eprintln!("warning: {w}");
        }
    } else {
        let mut files: Vec<_> = warnings.iter().filter_map(|w| w.path.as_ref()).collect();
        files.sort();
        files.dedup();
        eprintln!(
            "warning: {} problem(s) in {} file(s) while loading; run with -v for details",
            warnings.len(),
            files.len()
        );
    }
}
