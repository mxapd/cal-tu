//! Crash-safe file operations.
//!
//! Files are never written in place: data goes to a hidden temporary file in
//! the same folder, is flushed to disk, and then renamed over the target.
//! A crash leaves either the old or the new file, never a mix. The temporary
//! name does not end in `.ics`, so neither cal-tui nor vdirsyncer picks it up.

use std::fs::{self, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Fingerprint of file contents, used to detect changes made by others
/// between loading and writing.
pub fn content_hash(bytes: &[u8]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.cal-tui-tmp-{}", std::process::id()))
}

/// Atomically replace (or create) `path` with `data`.
pub fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    let tmp = temp_path(path);
    let _ = fs::remove_file(&tmp); // leftover from a crash
    let result = (|| {
        let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)?;
        sync_dir(path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Write a new file; fails if `path` already exists.
pub fn create_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    if path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} already exists", path.display()),
        ));
    }
    write_atomic(path, data)
}

/// Delete a file and make the deletion durable.
pub fn remove(path: &Path) -> io::Result<()> {
    fs::remove_file(path)?;
    sync_dir(path)
}

/// fsync the folder containing `path`, so a rename or delete survives a crash.
fn sync_dir(path: &Path) -> io::Result<()> {
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => fs::File::open(dir)?.sync_all(),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_replaces_and_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.ics");
        write_atomic(&p, b"one").unwrap();
        write_atomic(&p, b"two").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"two");
        let names: Vec<_> = fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(names.len(), 1, "temp file left behind");
    }

    #[test]
    fn failed_write_keeps_the_original() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.ics");
        write_atomic(&p, b"original").unwrap();
        // Renaming a file over a non-empty directory fails.
        let blocker = dir.path().join("b.ics");
        fs::create_dir(&blocker).unwrap();
        fs::write(blocker.join("x"), "x").unwrap();
        assert!(write_atomic(&blocker, b"new").is_err());
        assert_eq!(fs::read(&p).unwrap(), b"original");
        assert!(!temp_path(&blocker).exists());
    }

    #[test]
    fn create_refuses_to_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.ics");
        create_atomic(&p, b"one").unwrap();
        assert!(create_atomic(&p, b"two").is_err());
        assert_eq!(fs::read(&p).unwrap(), b"one");
    }
}
