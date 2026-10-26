//! niubash#571 — bare `history -a/-w/-r/-n` must work in the interactive
//! REPL, and bash-it-style per-prompt hooks must not print errors.
//!
//! GNU interactive shells always have a HISTFILE default, so the bare
//! forms resolve their filename from it. niu's REPL used to unset
//! HISTFILE (and the provider dispatch had no backing-file fallback), so
//! every bare form failed with `history: filename not specified` and the
//! `history -a; history -c; history -r` per-prompt hook broke at the
//! first leg. The fix defaults HISTFILE to the host history's backing
//! file in `enter_interactive`.
//!
//! These cases drive the real REPL through the shared pty driver (the
//! audit's tmux morphology, minus tmux: same fake HOME + seeded history
//! file + typed lines + final-file byte assertions). The rc never sets
//! HISTFILE itself — that would mask the default under test.

#[path = "interactive/driver.rs"]
mod driver;

use driver::{default_rc, require_pty_or_skip, NiuSession, DEFAULT_TIMEOUT};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Unique sandbox root the caller stages before spawning (so a seeded
/// history file exists before the shell constructs its provider).
fn unique_root(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "niu-hist571-{prefix}-{}-{nanos}",
        std::process::id()
    ))
}

/// Spawn a REPL over a home pre-seeded with `seed` as `.niubash_history`,
/// with the deterministic rc (PS1/PS2 sentinels, no plugins, no HISTFILE).
fn spawn_seeded(prefix: &str, seed: &str, rc_extra: &str) -> NiuSession {
    let root = unique_root(prefix);
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join(".niubash_history"), seed).unwrap();
    let mut rc = default_rc();
    rc.push_str(rc_extra);
    NiuSession::spawn_shared_home(&root, &rc, &[], &[], (120, 30), DEFAULT_TIMEOUT)
}

fn read_history_file(s: &NiuSession) -> String {
    std::fs::read_to_string(s.home().join(".niubash_history")).unwrap_or_default()
}

fn count_lines(contents: &str, line: &str) -> usize {
    contents.lines().filter(|l| *l == line).count()
}

#[test]
fn bare_history_forms_work_and_chain_survives() {
    if !require_pty_or_skip("bare_history_forms_work_and_chain_survives") {
        return;
    }
    let mut s = spawn_seeded("bare", "SEEDONE\nSEEDTWO\n", "");
    s.wait_ready();

    // The issue's regression seed, with rc markers so the pane proves each
    // bare form's exit status (the input echo carries the literal `$?`,
    // only the executed output carries the value).
    s.send_line("echo alpha");
    s.expect_prompt();
    s.send_line("history -a; echo \"A_RC=$?\"");
    s.expect("A_RC=0");
    s.expect_prompt();
    s.send_line("history -w; echo \"W_RC=$?\"");
    s.expect("W_RC=0");
    s.expect_prompt();
    s.send_line("history -n; echo \"N_RC=$?\"");
    s.expect("N_RC=0");
    s.expect_prompt();
    s.send_line("history -r; echo \"R_RC=$?\"");
    s.expect("R_RC=0");
    s.expect_prompt();
    s.send_line("history -a && history -c && history -r; echo \"CHAIN_RC=$?\"");
    s.expect("CHAIN_RC=0");
    s.expect_prompt();
    s.send_line("exit");
    s.wait_exit();

    let transcript = s.transcript();
    assert!(
        !transcript.contains("filename not specified"),
        "bare history forms must not report a missing filename:\n{transcript}"
    );
    // The chain's -a commits everything typed so far, so its -c clears an
    // empty pending set and nothing is lost at exit: the final file holds
    // the seeds plus every typed line, each exactly once.
    let contents = read_history_file(&s);
    assert_eq!(
        contents,
        "SEEDONE\nSEEDTWO\necho alpha\n\
         history -a; echo \"A_RC=$?\"\n\
         history -w; echo \"W_RC=$?\"\n\
         history -n; echo \"N_RC=$?\"\n\
         history -r; echo \"R_RC=$?\"\n\
         history -a && history -c && history -r; echo \"CHAIN_RC=$?\"\n\
         exit\n",
        "final history file must hold every typed line exactly once:\n{contents}"
    );
}

#[test]
fn bashit_hook_cycle_runs_without_filename_errors() {
    if !require_pty_or_skip("bashit_hook_cycle_runs_without_filename_errors") {
        return;
    }
    // The hook itself lives in the rc (startup files are not recorded, so
    // it contributes no history lines); every prompt then runs the exact
    // bash-it shape `history -a; history -c; history -r`.
    let mut s = spawn_seeded(
        "hook",
        "SEEDONE\nSEEDTWO\n",
        "PROMPT_COMMAND='history -a; history -c; history -r'\n",
    );
    s.wait_ready();

    for cycle in 1..=3 {
        s.send_line(&format!("echo HOOK_{cycle}"));
        s.expect(&format!("HOOK_{cycle}"));
        s.expect_prompt();
    }
    s.send_line("history");
    s.expect_prompt();
    s.send_line("exit");
    s.wait_exit();

    let transcript = s.transcript();
    assert!(
        !transcript.contains("filename not specified"),
        "the per-prompt hook must not print filename errors (pre-#571 it \
         printed two per prompt):\n{transcript}"
    );
    // No doubling (niubash#182) and no loss: each typed line exactly once.
    let contents = read_history_file(&s);
    for line in [
        "SEEDONE",
        "SEEDTWO",
        "echo HOOK_1",
        "echo HOOK_2",
        "echo HOOK_3",
        "history",
        "exit",
    ] {
        assert_eq!(
            count_lines(&contents, line),
            1,
            "history line {line:?} must appear exactly once:\n{contents}"
        );
    }
}
