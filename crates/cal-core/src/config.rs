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
    /// Per-calendar settings, keyed by folder name.
    #[serde(default)]
    pub calendars: BTreeMap<String, CalendarConfig>,
    /// TUI layout (milestone 5); accepted but not interpreted yet.
    pub layout: Option<toml::Value>,
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
            Ok(s) => {
                toml::from_str(&s).with_context(|| format!("invalid config {}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
        }
    }

    pub fn calendars_dir(&self) -> PathBuf {
        match &self.calendars_dir {
            Some(d) => expand_tilde(d),
            None => xdg_dir("XDG_DATA_HOME", ".local/share").join("calendars"),
        }
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
    }

    #[test]
    fn offsets() {
        assert_eq!(parse_offset("15m"), Some(chrono::Duration::minutes(15)));
        assert_eq!(parse_offset("1h30m"), Some(chrono::Duration::minutes(90)));
        assert_eq!(parse_offset("10"), Some(chrono::Duration::minutes(10)));
        assert_eq!(parse_offset("x"), None);
    }
}
