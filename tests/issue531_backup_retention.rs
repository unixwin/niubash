//! Regression pin for niubash#531: `~/.niubash/backups/` grew without
//! bound — every `niu setup` run wrote a fresh full rc backup and
//! nothing ever pruned the set (the reporter's directory held 16 full
//! copies spanning ten days). Issue repro: run `niu setup --preset
//! minimal` repeatedly in one HOME; the .bak count grew linearly.
//!
//! Where the behavior stands now (black-box probed against this
//! build, 2026-10-25): the growth vector is closed — the setup wizard
//! is create-only (niubash#359) and writes NO backups, and no other
//! verb creates them either. The retention cap that exists in the
//! runtime (`prune_rc_backups`, keep-newest-10, fired from
//! `backup_rc`) is reachable only from the rc-rewrite writers that
//! #359 retired, so a PRE-EXISTING over-cap directory is not pruned by
//! any public verb (setup / plugin list / plugin migrate-rc / doctor
//! all leave a seeded 15-file set untouched). That half is therefore
//! not pinnable from the host surface; this pin covers the contract
//! that IS observable and was the issue's repro: setup never adds
//! backups, so the set cannot grow.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

fn niu_binary() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_niu"));
    if p.exists() {
        return p;
    }
    let mut fallback = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fallback.push("target");
    fallback.push("debug");
    fallback.push(if cfg!(windows) { "niu.exe" } else { "niubash" });
    fallback
}

fn unique_temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir =
        std::env::temp_dir().join(format!("niubash-bfd-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn backup_names(home: &Path) -> Vec<String> {
    let dir = home.join(".niubash").join("backups");
    let mut names: Vec<String> = match std::fs::read_dir(&dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    names
}

fn run_setup(home: &PathBuf) {
    let output = Command::new(niu_binary())
        .args(["setup", "--preset", "minimal"])
        .env("HOME", home)
        .env("USERPROFILE", home)
        .stdin(Stdio::null())
        .output()
        .expect("spawn niu setup");
    assert!(
        output.status.success(),
        "niu setup --preset minimal must run headless in a sandbox HOME, status: {:?}",
        output.status
    );
}

#[test]
fn issue531_repeated_setup_never_grows_the_backup_set() {
    let root = unique_temp_dir("531");
    let home = root.join("home");
    let backups = home.join(".niubash").join("backups");
    std::fs::create_dir_all(&backups).expect("backups dir");
    std::fs::write(home.join(".niubashrc"), "# user rc\necho hello\n").expect("seed rc");
    // Seed an already-over-retention set, as the issue found in the
    // wild; setup must not add to it (and this build prunes nothing —
    // see header — so the set must come back byte-identical).
    for i in 1..=12 {
        std::fs::write(
            backups.join(format!(".niubashrc.17000000000000000{i:02}.bak")),
            format!("old backup {i}\n"),
        )
        .expect("seed backup");
    }
    let before = backup_names(&home);
    assert_eq!(before.len(), 12, "seed sanity");
    for _ in 0..3 {
        run_setup(&home);
    }
    let after = backup_names(&home);
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(
        after, before,
        "repeated setup runs must not create or rename rc backups (unbounded-growth vector of #531)"
    );
}

#[test]
fn issue531_setup_in_fresh_home_creates_no_backups() {
    let root = unique_temp_dir("531fresh");
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("home dir");
    run_setup(&home);
    run_setup(&home);
    let names = backup_names(&home);
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        names.is_empty(),
        "a fresh HOME must not accumulate backups from setup, got: {names:?}"
    );
}
