//! Weather forecast for the TUI, from Open-Meteo (free, no account).
//!
//! Opt-in via `[weather]` in the config; only the configured location is sent.
//! Fetching happens on a background thread so the UI never waits for the
//! network. The last forecast is cached in `$XDG_CACHE_HOME/cal-tui/`, so it
//! shows immediately at startup and when offline.

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use cal_core::WeatherConfig;
use chrono::{NaiveDate, Utc};
use serde::{Deserialize, Serialize};

/// Refresh a forecast older than this.
const MAX_AGE: i64 = 60 * 60;
/// After a failed fetch, wait this long before trying again.
const RETRY: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Day {
    pub date: NaiveDate,
    /// WMO weather code.
    pub code: u8,
    pub min: f64,
    pub max: f64,
    /// Highest chance of precipitation that day, in percent.
    pub precip: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Forecast {
    /// Unix time of the fetch.
    pub fetched: i64,
    /// What it was fetched for (to notice config changes).
    pub query: WeatherQuery,
    /// Current temperature and code.
    pub current: Option<(f64, u8)>,
    pub days: Vec<Day>,
}

/// The config as it affects the request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WeatherQuery {
    pub location: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub fahrenheit: bool,
}

impl From<&WeatherConfig> for WeatherQuery {
    fn from(c: &WeatherConfig) -> Self {
        WeatherQuery {
            location: c.location.clone(),
            latitude: c.latitude,
            longitude: c.longitude,
            fahrenheit: c.units.as_deref() == Some("fahrenheit"),
        }
    }
}

/// Symbol and words for a WMO weather code. Symbols are single-width so
/// they line up in every terminal.
pub fn describe(code: u8) -> (&'static str, &'static str) {
    match code {
        0 => ("☀", "clear"),
        1 => ("☀", "mostly clear"),
        2 => ("☁", "partly cloudy"),
        3 => ("☁", "cloudy"),
        45 | 48 => ("≋", "fog"),
        51..=55 => ("☂", "drizzle"),
        56 | 57 | 66 | 67 => ("☂", "freezing rain"),
        61..=65 => ("☂", "rain"),
        71..=77 => ("❄", "snow"),
        80..=82 => ("☂", "showers"),
        85 | 86 => ("❄", "snow showers"),
        95..=99 => ("↯", "thunderstorm"),
        _ => ("?", "unknown"),
    }
}

impl Forecast {
    /// One line for `date`, e.g. `☂ rain 80%, 2°–6°` (today also `4° now`).
    pub fn line(&self, date: NaiveDate, today: NaiveDate) -> Option<String> {
        let d = self.days.iter().find(|d| d.date == date)?;
        let (icon, text) = describe(d.code);
        let mut s = format!("{icon} {text}");
        if let Some(p) = d.precip.filter(|p| *p >= 30) {
            if d.code >= 51 {
                s.push_str(&format!(" {p}%"));
            }
        }
        let range = format!("{:.0}°–{:.0}°", d.min, d.max);
        match self.current.filter(|_| date == today) {
            Some((t, _)) => s.push_str(&format!(", {t:.0}° now ({range})")),
            None => s.push_str(&format!(", {range}")),
        }
        Some(s)
    }
}

// --- parsing --------------------------------------------------------------------

#[derive(Deserialize)]
struct GeoResponse {
    results: Option<Vec<GeoResult>>,
}

#[derive(Deserialize)]
struct GeoResult {
    latitude: f64,
    longitude: f64,
}

#[derive(Deserialize)]
struct ForecastResponse {
    current: Option<Current>,
    daily: Daily,
}

#[derive(Deserialize)]
struct Current {
    temperature_2m: Option<f64>,
    weather_code: Option<u8>,
}

#[derive(Deserialize)]
struct Daily {
    time: Vec<NaiveDate>,
    weather_code: Vec<Option<u8>>,
    temperature_2m_max: Vec<Option<f64>>,
    temperature_2m_min: Vec<Option<f64>>,
    precipitation_probability_max: Option<Vec<Option<f64>>>,
}

pub fn parse_geocode(json: &str, place: &str) -> Result<(f64, f64)> {
    let r: GeoResponse = serde_json::from_str(json).context("unexpected geocoding answer")?;
    r.results
        .and_then(|r| r.into_iter().next())
        .map(|g| (g.latitude, g.longitude))
        .ok_or_else(|| anyhow!("place '{place}' not found; try latitude/longitude"))
}

pub fn parse_forecast(json: &str, query: WeatherQuery, fetched: i64) -> Result<Forecast> {
    let r: ForecastResponse = serde_json::from_str(json).context("unexpected forecast answer")?;
    let d = r.daily;
    let precip = d.precipitation_probability_max.unwrap_or_default();
    let days = d
        .time
        .iter()
        .enumerate()
        .filter_map(|(i, date)| {
            Some(Day {
                date: *date,
                code: (*d.weather_code.get(i)?)?,
                min: (*d.temperature_2m_min.get(i)?)?,
                max: (*d.temperature_2m_max.get(i)?)?,
                precip: precip.get(i).copied().flatten().map(|p| p.round() as u8),
            })
        })
        .collect();
    let current = r
        .current
        .and_then(|c| Some((c.temperature_2m?, c.weather_code?)));
    Ok(Forecast {
        fetched,
        query,
        current,
        days,
    })
}

// --- network ----------------------------------------------------------------------

fn get(url: &str, params: &[(&str, String)]) -> Result<String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .https_only(true)
        .build()
        .into();
    let mut req = agent.get(url);
    for (k, v) in params {
        req = req.query(*k, v);
    }
    let mut resp = req.call().with_context(|| format!("cannot reach {url}"))?;
    Ok(resp.body_mut().read_to_string()?)
}

/// Fetch a forecast (blocking). `known` coordinates skip the place lookup.
pub fn fetch(query: &WeatherQuery, known: Option<(f64, f64)>) -> Result<Forecast> {
    let (lat, lon) = match (query.latitude, query.longitude, known) {
        (Some(lat), Some(lon), _) => (lat, lon),
        (_, _, Some(c)) => c,
        _ => {
            let place = query.location.clone().unwrap_or_default();
            let json = get(
                "https://geocoding-api.open-meteo.com/v1/search",
                &[("name", place.clone()), ("count", "1".into())],
            )?;
            parse_geocode(&json, &place)?
        }
    };
    let mut params = vec![
        ("latitude", lat.to_string()),
        ("longitude", lon.to_string()),
        (
            "daily",
            "weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max"
                .into(),
        ),
        ("current", "temperature_2m,weather_code".into()),
        ("timezone", "auto".into()),
        ("forecast_days", "16".into()),
    ];
    if query.fahrenheit {
        params.push(("temperature_unit", "fahrenheit".into()));
    }
    let json = get("https://api.open-meteo.com/v1/forecast", &params)?;
    let mut f = parse_forecast(&json, query.clone(), Utc::now().timestamp())?;
    // Remember the coordinates so the place is looked up only once.
    f.query.latitude.get_or_insert(lat);
    f.query.longitude.get_or_insert(lon);
    Ok(f)
}

// --- background worker ------------------------------------------------------------

pub fn cache_path() -> PathBuf {
    let base = match std::env::var_os("XDG_CACHE_HOME") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join(".cache"),
    };
    base.join("cal-tui/weather.json")
}

/// Does a cached forecast belong to this config? (A looked-up place stores
/// its coordinates too, so compare the fields the user set.)
fn matches(cached: &WeatherQuery, wanted: &WeatherQuery) -> bool {
    cached.fahrenheit == wanted.fahrenheit
        && match (&wanted.location, wanted.latitude, wanted.longitude) {
            (_, Some(lat), Some(lon)) => {
                cached.latitude == Some(lat) && cached.longitude == Some(lon)
            }
            (Some(l), _, _) => cached.location.as_deref() == Some(l.as_str()),
            _ => false,
        }
}

pub struct Weather {
    query: WeatherQuery,
    cache: Option<PathBuf>,
    pub forecast: Option<Forecast>,
    pub error: Option<String>,
    pending: Option<Receiver<Result<Forecast, String>>>,
    last_attempt: Option<Instant>,
}

impl Weather {
    /// Start for a config; loads the cache. Fetching starts on `tick`.
    pub fn new(config: &WeatherConfig, cache: Option<PathBuf>) -> Weather {
        let query = WeatherQuery::from(config);
        let forecast = cache
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str::<Forecast>(&s).ok())
            .filter(|f| matches(&f.query, &query));
        Weather {
            query,
            cache,
            forecast,
            error: None,
            pending: None,
            last_attempt: None,
        }
    }

    /// A fixed forecast that never fetches (tests).
    #[cfg(test)]
    pub fn fixed(forecast: Forecast) -> Weather {
        Weather {
            query: forecast.query.clone(),
            cache: None,
            forecast: Some(forecast),
            error: None,
            pending: None,
            last_attempt: Some(Instant::now()),
        }
    }

    /// Collect a finished fetch, or start one when due. Returns true if the
    /// forecast changed.
    pub fn tick(&mut self) -> bool {
        if let Some(rx) = &self.pending {
            match rx.try_recv() {
                Ok(Ok(f)) => {
                    if let Some(p) = &self.cache {
                        if let Some(dir) = p.parent() {
                            let _ = std::fs::create_dir_all(dir);
                        }
                        if let Ok(json) = serde_json::to_string(&f) {
                            let _ = cal_core::fsio::write_atomic(p, json.as_bytes());
                        }
                    }
                    self.forecast = Some(f);
                    self.error = None;
                    self.pending = None;
                    return true;
                }
                Ok(Err(e)) => {
                    self.error = Some(e);
                    self.pending = None;
                    return true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => return false,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.pending = None,
            }
        }
        let stale = self
            .forecast
            .as_ref()
            .is_none_or(|f| Utc::now().timestamp() - f.fetched > MAX_AGE);
        let may_try = self.last_attempt.is_none_or(|t| t.elapsed() > RETRY);
        if stale && may_try {
            self.last_attempt = Some(Instant::now());
            let query = self.query.clone();
            let known = self
                .forecast
                .as_ref()
                .and_then(|f| Some((f.query.latitude?, f.query.longitude?)));
            let (tx, rx) = channel();
            std::thread::spawn(move || {
                let _ = tx.send(fetch(&query, known).map_err(|e| format!("{e:#}")));
            });
            self.pending = Some(rx);
        }
        false
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub const SAMPLE: &str = r#"{
      "latitude": 59.33, "longitude": 18.07,
      "current": {"time": "2026-11-16T11:30", "temperature_2m": 4.4, "weather_code": 3},
      "daily": {
        "time": ["2026-11-16", "2026-11-17", "2026-11-18", "2026-11-19"],
        "weather_code": [3, 61, 71, null],
        "temperature_2m_max": [6.2, 5.0, 0.4, 3.0],
        "temperature_2m_min": [1.6, 2.1, -3.2, 1.0],
        "precipitation_probability_max": [10, 85, 60, null]
      }
    }"#;

    pub fn sample() -> Forecast {
        let q = WeatherQuery {
            location: Some("Stockholm".into()),
            latitude: None,
            longitude: None,
            fahrenheit: false,
        };
        parse_forecast(SAMPLE, q, 0).unwrap()
    }

    #[test]
    fn parses_and_describes() {
        let f = sample();
        assert_eq!(f.days.len(), 3, "the day with missing data is skipped");
        let d = |m, day| NaiveDate::from_ymd_opt(2026, m, day).unwrap();
        let today = d(11, 16);
        assert_eq!(f.line(today, today).unwrap(), "☁ cloudy, 4° now (2°–6°)");
        assert_eq!(f.line(d(11, 17), today).unwrap(), "☂ rain 85%, 2°–5°");
        assert_eq!(f.line(d(11, 18), today).unwrap(), "❄ snow 60%, -3°–0°");
        assert_eq!(f.line(d(12, 30), today), None);
    }

    #[test]
    fn geocode() {
        let j = r#"{"results":[{"name":"Uppsala","latitude":59.86,"longitude":17.64}]}"#;
        assert_eq!(parse_geocode(j, "Uppsala").unwrap(), (59.86, 17.64));
        assert!(parse_geocode(r#"{"generationtime_ms":0.2}"#, "Nowhere")
            .unwrap_err()
            .to_string()
            .contains("'Nowhere' not found"));
    }

    /// Talks to the real service: `cargo test -p cal-tui live_fetch -- --ignored`.
    #[test]
    #[ignore]
    fn live_fetch() {
        let q = WeatherQuery {
            location: Some("Stockholm".into()),
            latitude: None,
            longitude: None,
            fahrenheit: false,
        };
        let f = fetch(&q, None).unwrap();
        assert!(f.days.len() >= 7, "{f:?}");
        assert!(f.current.is_some());
        assert!(f.query.latitude.is_some());
        let today = f.days[0].date;
        println!("{}", f.line(today, today).unwrap());
        println!("{}", f.line(f.days[1].date, today).unwrap());
    }

    #[test]
    fn cache_matches_config() {
        let mut cached = sample().query;
        cached.latitude = Some(59.33);
        cached.longitude = Some(18.07);
        let by_name = WeatherQuery {
            location: Some("Stockholm".into()),
            latitude: None,
            longitude: None,
            fahrenheit: false,
        };
        assert!(matches(&cached, &by_name));
        let other = WeatherQuery {
            location: Some("Uppsala".into()),
            ..by_name.clone()
        };
        assert!(!matches(&cached, &other));
        let f = WeatherQuery {
            fahrenheit: true,
            ..by_name
        };
        assert!(!matches(&cached, &f));
    }
}
