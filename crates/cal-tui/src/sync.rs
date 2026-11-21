//! Syncing: run `sync_command` (e.g. vdirsyncer), one at a time.
//!
//! The TUI's `S` key, `cal-tui sync` (a timer's) and `serve` (after an edit)
//! all sync through [`run`], under a lock: two vdirsyncer runs at once can
//! corrupt its status. With the `jarvis` feature, what changed then goes to
//! the journal (`journal.rs`); without it, a sync is only the command.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use cal_core::Config;
use chrono::DateTime;
use chrono_tz::Tz;

/// Held while syncing or touching the snapshot; released on drop.
pub struct Lock(#[allow(dead_code)] File);

/// Take the sync lock, saying so when another sync holds it rather than
/// waiting in silence.
pub fn lock(dir: &Path) -> Result<Lock> {
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let path = dir.join("sync.lock");
    let f = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    match f.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            eprintln!("Another sync is running (the timer or serve); waiting for it…");
            f.lock()
                .with_context(|| format!("cannot lock {}", path.display()))?;
        }
        Err(TryLockError::Error(e)) => {
            return Err(e).with_context(|| format!("cannot lock {}", path.display()))
        }
    }
    Ok(Lock(f))
}

pub struct Options {
    /// Run `sync_command` (else only compare).
    pub run_command: bool,
    /// Print journal entries instead of posting them.
    #[cfg(feature = "jarvis")]
    pub print: bool,
    /// Where state lives; default `Config::state_dir()`.
    pub state_dir: PathBuf,
    /// Whether this caller raises the alert (`cal-tui sync`, the timer's).
    /// Others (serve's push, the TUI) count failures but leave the alert to
    /// it, so it is not used up where nobody reports it.
    pub may_alert: bool,
    /// Run on a terminal (the TUI): `sync_command` shows its output and can
    /// ask questions, rather than being captured for the log.
    pub inherit_io: bool,
}

impl Options {
    pub fn new() -> Options {
        Options {
            run_command: true,
            #[cfg(feature = "jarvis")]
            print: false,
            state_dir: Config::state_dir(),
            may_alert: false,
            inherit_io: false,
        }
    }
}

/// How a sync went.
pub enum Outcome {
    /// Synced (or nothing to run).
    Synced,
    /// The sync failed; `alert` when this failure is the one to report.
    Failed {
        message: String,
        /// Failures in a row, this one included.
        failures: u32,
        alert: bool,
    },
}

fn run_command(cmd: &str, inherit_io: bool) -> Result<(), String> {
    if inherit_io {
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .status()
            .map_err(|e| format!("cannot run '{cmd}': {e}"))?;
        return if status.success() {
            Ok(())
        } else {
            Err(format!("'{cmd}' failed ({status})"))
        };
    }
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .output()
        .map_err(|e| format!("cannot run '{cmd}': {e}"))?;
    // Into the unit's log.
    let text = String::from_utf8_lossy(&out.stdout) + String::from_utf8_lossy(&out.stderr);
    eprint!("{text}");
    if out.status.success() {
        return Ok(());
    }
    // The cause, for the alert: vdirsyncer's first error line (its last
    // one is a hint about -vdebug), else the last line.
    let why = text
        .lines()
        .find(|l| l.trim_start().to_ascii_lowercase().starts_with("error:"))
        .or_else(|| text.lines().rev().find(|l| !l.trim().is_empty()))
        .unwrap_or("")
        .trim();
    Err(format!("'{cmd}' failed ({}): {why}", out.status))
}

/// Sync; with the `jarvis` feature, then compare and post what changed.
#[cfg_attr(not(feature = "jarvis"), allow(unused_variables))]
pub fn run(config: &Config, now: DateTime<Tz>, opts: &Options) -> Result<Outcome> {
    let _lock = lock(&opts.state_dir)?;
    let result = match (&config.sync_command, opts.run_command) {
        (Some(cmd), true) => run_command(cmd, opts.inherit_io),
        _ => Ok(()),
    };
    #[cfg(feature = "jarvis")]
    return Ok(crate::journal::after_sync(config, now, opts, result));
    #[cfg(not(feature = "jarvis"))]
    Ok(match result {
        Ok(()) => Outcome::Synced,
        Err(message) => {
            eprintln!("sync failed: {message}");
            Outcome::Failed {
                message,
                failures: 1,
                alert: opts.may_alert,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_lock_is_waited_for() {
        let dir = tempfile::tempdir().unwrap();
        let held = lock(dir.path()).unwrap();
        let path = dir.path().to_path_buf();
        let waiter = std::thread::spawn(move || {
            let t = std::time::Instant::now();
            let _l = lock(&path).unwrap();
            t.elapsed()
        });
        std::thread::sleep(std::time::Duration::from_millis(300));
        drop(held);
        let waited = waiter.join().unwrap();
        assert!(
            waited >= std::time::Duration::from_millis(250),
            "{waited:?}"
        );
    }
}
