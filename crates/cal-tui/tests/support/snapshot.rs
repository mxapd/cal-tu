//! Minimal snapshot testing: compare output with `tests/snapshots/<name>.txt`.
//!
//! Run with `UPDATE_SNAPSHOTS=1` to (re)write the files after an intended
//! change, then review them with `git diff`. A missing snapshot is a failure,
//! so a fresh checkout (or the Nix build) never silently accepts new output.

use std::path::PathBuf;

pub fn assert_snapshot(name: &str, actual: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(format!("{name}.txt"));
    let update = std::env::var_os("UPDATE_SNAPSHOTS").is_some_and(|v| v != "0");
    if update {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "snapshot {} is missing; run with UPDATE_SNAPSHOTS=1 to create it.\nactual:\n{actual}",
            path.display()
        )
    });
    if expected != actual {
        let mut diff = String::new();
        let (e, a): (Vec<_>, Vec<_>) = (expected.lines().collect(), actual.lines().collect());
        for i in 0..e.len().max(a.len()) {
            let (el, al) = (e.get(i).copied(), a.get(i).copied());
            if el != al {
                diff.push_str(&format!(
                    "line {}:\n  - {}\n  + {}\n",
                    i + 1,
                    el.unwrap_or("<none>"),
                    al.unwrap_or("<none>")
                ));
            }
        }
        panic!(
            "snapshot {name} differs (UPDATE_SNAPSHOTS=1 to accept):\n{diff}\nfull output:\n{actual}"
        );
    }
}
