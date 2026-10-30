//! niubash#631 regression: the completion *interaction* axis on a real
//! terminal — `completion.interaction = cycle` (the default since
//! 1.5.0) cycles candidates into the line with TAB, Enter accepts the
//! candidate in the line and runs it in the same press, Esc rolls the
//! line back to its pre-completion snapshot, and a leftover pre-1.5.0
//! config.toml prints the one-time migration notice (it is never read).
//!
//! The candidate vehicle is `cd` into one of two same-prefix
//! directories, asserted through `pwd`: builtins only (the driver
//! sandbox PATH carries no externals), and the executed directory
//! names exactly which candidate was in the line when Enter landed.
//!
//! Driver: [`mod driver`] (portable-pty; see tests/interactive.rs for
//! the harness contract). Run:
//! `NIU_INTERACTIVE_TESTS=1 cargo test --test issue631_completion_interaction --locked -- --nocapture`

#[path = "interactive/driver.rs"]
mod driver;

use std::time::Duration;

use driver::{require_pty_or_skip, NiuSession, DEFAULT_TIMEOUT, TAB};

/// Settle window after a TAB whose splice has no screen needle of its
/// own (the grid is already up; only the line changes).
const SETTLE: Duration = Duration::from_millis(400);

const ESC: &str = "\u{1b}";

/// Two same-prefix directories in the session's start dir. Candidate
/// order is the completer's sorted order: cyc-one < cyc-two.
fn write_candidates(s: &NiuSession) {
    std::fs::create_dir_all(s.start().join("cyc-one")).unwrap();
    std::fs::create_dir_all(s.start().join("cyc-two")).unwrap();
}

/// One cycle round in a fresh session: type `cd cyc-`, press TAB
/// `tabs` times, press Enter, and return the session so the caller
/// can assert where the shell landed.
fn cycle_round(prefix: &str, tabs: usize) -> NiuSession {
    let mut s = NiuSession::spawn(prefix);
    write_candidates(&s);
    s.wait_ready();
    s.send("cd cyc-");
    for i in 0..tabs {
        s.send(TAB);
        if i == 0 {
            // The grid being up (a candidate name visible) is the
            // settle signal that the cycle session is live before
            // further keys land.
            s.expect("cyc-one");
        } else {
            std::thread::sleep(SETTLE);
        }
    }
    s.send("\r");
    s.expect_prompt();
    s
}

/// The cycle default end to end: TAB #1 puts the first candidate in
/// the line and Enter runs it in the same press (no commit step — the
/// insert interaction's two-Enter shape is the other contract, pinned
/// by interactive.rs's #486 test under `insert`); TAB #2 reaches the
/// second candidate; TAB #3 wraps the full loop back to the first.
#[test]
fn issue631_pty_cycle_tab_loops_candidates_and_enter_runs_the_pick() {
    if !require_pty_or_skip("issue631_pty_cycle_tab_loops_candidates_and_enter_runs_the_pick") {
        return;
    }
    // No NIU_COMPLETION_INTERACTION anywhere: the default IS cycle.
    let mut s = cycle_round("cycle-loop-631-a", 1);
    s.send_line("pwd");
    s.expect("/cyc-one");
    s.expect_prompt();

    let mut s = cycle_round("cycle-loop-631-b", 2);
    s.send_line("pwd");
    s.expect("/cyc-two");
    s.expect_prompt();

    let mut s = cycle_round("cycle-loop-631-c", 3);
    s.send_line("pwd");
    s.expect("/cyc-one");
    s.expect_prompt();
}

/// Esc inside a cycle session restores the pre-completion line: after
/// cycling onto the second candidate, Esc + Enter submits the line as
/// originally typed (`cd cyc-`), which fails — the shell stays in the
/// start directory. Had Esc merely closed the menu (the insert shape),
/// the spliced candidate would have executed and `pwd` would show it.
#[test]
fn issue631_pty_esc_rolls_back_to_the_pre_completion_line() {
    if !require_pty_or_skip("issue631_pty_esc_rolls_back_to_the_pre_completion_line") {
        return;
    }
    let mut s = NiuSession::spawn("cycle-esc-631");
    write_candidates(&s);
    s.wait_ready();
    let start = s.start().to_string_lossy().into_owned();
    s.send("cd cyc-");
    s.send(TAB);
    s.expect("cyc-one");
    s.send(TAB);
    std::thread::sleep(SETTLE);
    s.send(ESC);
    std::thread::sleep(SETTLE);
    s.send("\r");
    s.expect_prompt();
    s.send_line("pwd");
    // The pwd line is the start directory itself — not a candidate.
    // (No trailing newline in the needle: the raw stream terminates
    // the line with CRLF.)
    s.expect(&start);
    s.expect_prompt();
}

/// A leftover pre-1.5.0 config.toml is never read, but its presence
/// prints one migration notice per interactive session, naming the
/// one-time `niu config migrate` command. The file itself changes no
/// behavior.
#[test]
fn issue631_pty_legacy_config_prints_migration_notice() {
    if !require_pty_or_skip("issue631_pty_legacy_config_prints_migration_notice") {
        return;
    }
    let dir = std::env::temp_dir().join(format!("niu-issue631-migrate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let config_path = dir.join("config.toml");
    std::fs::write(&config_path, "[completion]\nstyle = \"column\"\n").unwrap();
    let env = [(
        "NIU_CONFIG".to_string(),
        config_path.to_string_lossy().into_owned(),
    )];

    let mut s = NiuSession::spawn_custom(
        "migrate-notice-631",
        &driver::default_rc(),
        &env,
        (120, 30),
        DEFAULT_TIMEOUT,
    );
    s.wait_ready();
    std::thread::sleep(SETTLE);
    let transcript = s.transcript();
    assert!(
        transcript.contains("niu config migrate"),
        "the startup notice must name the migration command:\n{transcript}"
    );
    assert!(
        config_path.exists(),
        "the notice never renames or edits the old file"
    );
    s.send_line("exit");
    let _ = std::fs::remove_dir_all(&dir);
}
