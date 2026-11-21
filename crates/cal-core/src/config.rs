//! User configuration (`~/.config/cal-tui/config.toml`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono_tz::Tz;
use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Folder containing one sub-folder per calendar (vdir).
    pub calendars_dir: Option<String>,
    /// Calendar (folder name) that new items go into.
    pub default_calendar: Option<String>,
    /// Command run by the sync key, e.g. `vdirsyncer sync`.
    pub sync_command: Option<String>,
    /// IANA zone for display and floating times; defaults to the system zone.
    pub timezone: Option<String>,
    /// Mouse support in the TUI (default on). While on, select text in the
    /// terminal with Shift+drag.
    pub mouse: Option<bool>,
    /// Per-calendar settings, keyed by folder name.
    #[serde(default)]
    pub calendars: BTreeMap<String, CalendarConfig>,
    /// Weather in the TUI (off unless set). Fetched from Open-Meteo.
    pub weather: Option<WeatherConfig>,
    /// TUI layouts; see `layout.rs`. Parsed by [`Config::layout`] so errors
    /// can name the exact key.
    pub layout: Option<toml::Value>,
    /// Top-level sections this version does not know (e.g. `[jarvis]` from a
    /// newer build or another branch). They are skipped, not errors, so one
    /// config can serve several builds; `check-config` and the TUI say so.
    #[serde(skip)]
    pub ignored_sections: Vec<String>,
}

/// Top-level keys `Config` understands (keep in sync with its fields).
const KNOWN_KEYS: [&str; 8] = [
    "calendars_dir",
    "default_calendar",
    "sync_command",
    "timezone",
    "mouse",
    "calendars",
    "weather",
    "layout",
];

/// `[weather]`: where to show the forecast for. Only this location is sent
/// to the weather service (Open-Meteo, no account needed).
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WeatherConfig {
    /// Place name, looked up once (e.g. "Uppsala").
    pub location: Option<String>,
    /// Or exact coordinates.
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    /// `celsius` (default) or `fahrenheit`.
    pub units: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalendarConfig {
    /// Display name; defaults to the vdir `displayname` file or folder name.
    pub name: Option<String>,
    /// Colour name (`red`, `cyan`, …) or `#rrggbb`; defaults to the vdir
    /// `color` file.
    pub color: Option<String>,
    #[serde(default)]
    pub read_only: bool,
    /// Do not show this calendar at all.
    #[serde(default)]
    pub hidden: bool,
    /// Reminder applied to events without their own alarm, e.g. `15m`.
    pub default_reminder: Option<String>,
    /// Regular expressions; events whose title matches any are hidden.
    #[serde(default)]
    pub hide_matching: Vec<String>,
}

impl Config {
    /// Default config file location.
    pub fn default_path() -> PathBuf {
        xdg_dir("XDG_CONFIG_HOME", ".config").join("cal-tui/config.toml")
    }

    /// Load from `path`; a missing file yields the defaults.
    pub fn load(path: &Path) -> Result<Config> {
        match std::fs::read_to_string(path) {
            Ok(s) => Self::parse(&s).with_context(|| format!("invalid config {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
        }
    }

    /// Folder for state the app keeps between runs (reminders already sent,
    /// layouts saved from the TUI): `$XDG_STATE_HOME/cal-tui`.
    pub fn state_dir() -> PathBuf {
        xdg_dir("XDG_STATE_HOME", ".local/state").join("cal-tui")
    }

    pub fn calendars_dir(&self) -> PathBuf {
        match &self.calendars_dir {
            Some(d) => expand_tilde(d),
            None => xdg_dir("XDG_DATA_HOME", ".local/share").join("calendars"),
        }
    }

    /// The `[layout]` section (empty if absent).
    pub fn layout(&self) -> std::result::Result<crate::layout::LayoutConfig, String> {
        match &self.layout {
            Some(v) => crate::layout::LayoutConfig::from_toml(v, "layout"),
            None => Ok(Default::default()),
        }
    }

    /// Parse a config from TOML text. Unknown keys are errors, except whole
    /// unknown top-level sections, which are skipped (see `ignored_sections`).
    pub fn from_toml(text: &str) -> Result<Config> {
        Self::parse(text)
    }

    fn parse(text: &str) -> Result<Config> {
        let mut table: toml::Table = text.parse()?;
        let ignored: Vec<String> = table
            .iter()
            .filter(|(k, v)| v.is_table() && !KNOWN_KEYS.contains(&k.as_str()))
            .map(|(k, _)| k.clone())
            .collect();
        for k in &ignored {
            table.remove(k);
        }
        let mut config: Config = table.try_into()?;
        config.ignored_sections = ignored;
        Ok(config)
    }

    /// One line per skipped section, for `check-config` and the TUI.
    pub fn ignored_notices(&self) -> Vec<String> {
        self.ignored_sections
            .iter()
            .map(|k| format!("ignoring unknown section [{k}] (not used by this version)"))
            .collect()
    }

    /// Problems that parsing alone does not catch: unknown time zone, bad
    /// reminder offsets, invalid title filters, a read-only default calendar.
    /// Colours are checked by the UI layer, which owns their meaning.
    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if let Err(e) = self.layout() {
            problems.push(e);
        }
        if let Some(z) = &self.timezone {
            if crate::time::resolve_tzid(z).is_none() {
                problems.push(format!("timezone: unknown time zone '{z}'"));
            }
        }
        for (id, cal) in &self.calendars {
            if let Some(r) = &cal.default_reminder {
                if parse_offset(r).is_none() {
                    problems.push(format!(
                        "calendars.{id}.default_reminder: cannot parse '{r}' (use e.g. 15m, 1h, 1h30m, 1d)"
                    ));
                }
            }
            for pat in &cal.hide_matching {
                if let Err(e) = regex::Regex::new(pat) {
                    problems.push(format!("calendars.{id}.hide_matching: '{pat}': {e}"));
                }
            }
        }
        if let Some(w) = &self.weather {
            match (&w.location, w.latitude, w.longitude) {
                (_, Some(lat), Some(lon)) => {
                    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
                        problems.push(format!(
                            "weather: coordinates {lat}, {lon} are out of range"
                        ));
                    }
                }
                (Some(l), None, None) if !l.trim().is_empty() => {}
                _ => problems.push(
                    "weather: set either location = \"Place\" or both latitude and longitude"
                        .into(),
                ),
            }
            if let Some(u) = &w.units {
                if !matches!(u.as_str(), "celsius" | "fahrenheit") {
                    problems.push(format!(
                        "weather.units: '{u}' must be \"celsius\" or \"fahrenheit\""
                    ));
                }
            }
        }
        if let Some(d) = &self.default_calendar {
            if self.calendars.get(d).is_some_and(|c| c.read_only) {
                problems.push(format!(
                    "default_calendar: '{d}' is marked read_only, new items could not be saved"
                ));
            }
        }
        problems
    }

    pub fn home_zone(&self) -> Result<Tz> {
        match &self.timezone {
            Some(z) => crate::time::resolve_tzid(z)
                .with_context(|| format!("unknown timezone '{z}' in config")),
            None => Ok(crate::time::system_zone()),
        }
    }
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

fn xdg_dir(var: &str, fallback: &str) -> PathBuf {
    match std::env::var_os(var) {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => home_dir().join(fallback),
    }
}

pub fn expand_tilde(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => home_dir().join(rest),
        None if p == "~" => home_dir(),
        None => PathBuf::from(p),
    }
}

/// Parse a short reminder offset such as `15m`, `1h`, `1h30m`, `2d`.
pub fn parse_offset(s: &str) -> Option<chrono::Duration> {
    let mut total = chrono::Duration::zero();
    let mut num = String::new();
    for c in s.trim().chars() {
        if c.is_ascii_digit() {
            num.push(c);
            continue;
        }
        let n: i64 = num.parse().ok()?;
        num.clear();
        total += match c {
            'd' => chrono::Duration::days(n),
            'h' => chrono::Duration::hours(n),
            'm' => chrono::Duration::minutes(n),
            _ => return None,
        };
    }
    if !num.is_empty() {
        // Bare number = minutes.
        total += chrono::Duration::minutes(num.parse().ok()?);
    }
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_config() {
        let c: Config = toml::from_str(
            r#"
            calendars_dir = "~/cals"
            default_calendar = "personal"
            [calendars.uni]
            color = "cyan"
            read_only = true
            hide_matching = ["^Group B"]
            "#,
        )
        .unwrap();
        assert!(c.calendars["uni"].read_only);
        assert!(c.calendars_dir().ends_with("cals"));
        assert!(toml::from_str::<Config>("typo_key = 1").is_err());
        assert!(c.validate().is_empty());
    }

    #[test]
    fn unknown_sections_are_skipped_but_typos_are_not() {
        let c = Config::from_toml(
            r#"
            timezone = "Europe/Stockholm"
            [jarvis]
            report_url = "http://127.0.0.1:8485/report/calendar"
            [weather]
            location = "Uppsala"
            "#,
        )
        .unwrap();
        assert_eq!(c.ignored_sections, ["jarvis"]);
        assert_eq!(c.weather.unwrap().location.as_deref(), Some("Uppsala"));
        // A stray top-level key, or a typo inside a known section, still fails.
        assert!(Config::from_toml("calendar_dir = \"x\"").is_err());
        assert!(Config::from_toml("[weather]\nlocaton = \"x\"").is_err());
        assert!(Config::from_toml("[calendars.uni]\ncolour = \"red\"").is_err());
        assert!(Config::from_toml("").unwrap().ignored_sections.is_empty());
    }

    #[test]
    fn validate_reports_semantic_problems() {
        let c = Config::from_toml(
            r#"
            timezone = "Mars/Olympus"
            default_calendar = "uni"
            [calendars.uni]
            read_only = true
            default_reminder = "soon"
            hide_matching = ["(unclosed"]
            "#,
        )
        .unwrap();
        let p = c.validate();
        assert_eq!(p.len(), 4, "{p:#?}");
        assert!(p[0].starts_with("timezone"));
    }

    #[test]
    fn offsets() {
        assert_eq!(parse_offset("15m"), Some(chrono::Duration::minutes(15)));
        assert_eq!(parse_offset("1h30m"), Some(chrono::Duration::minutes(90)));
        assert_eq!(parse_offset("10"), Some(chrono::Duration::minutes(10)));
        assert_eq!(parse_offset("x"), None);
    }
}
