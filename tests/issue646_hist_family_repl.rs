//! niubash#646 — the HIST* family must be wired on the REPL face:
//! HISTCONTROL/HISTIGNORE filter at record time, HISTSIZE stifles the
//! list, HISTFILESIZE truncates the file at the session-end write.
//! Before the fix (RC audit D-04, PTY probes) the REPL record path
//! consulted none of them: duplicates and space-prefixed lines were
//! recorded under ignoreboth, HISTIGNORE matches were recorded and
//! saved, the list ignored HISTSIZE (a hard-coded capacity stood in),
//! and the file ignored HISTFILESIZE.
//!
//! HISTTIMEFORMAT is deliberately NOT asserted here: the host/engine
//! `HistoryProvider` contract carries plain entry strings only and
//! reedline's file format has no stamp side-lines, so the REPL
//! timestamp face has no channel to flow through yet — it is tracked
//! with the provider-contract work (niubash#622), not faked here.
//!
//! Assertions use `history > file` dumps and the history file itself,
//! not the pty stream (see tests/issue614_cleared_list_stays_cleared.rs
//! for why).

#[path = "interactive/driver.rs"]
mod driver;

use driver::{default_rc, require_pty_or_skip, NiuSession, DEFAULT_TIMEOUT};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn unique_root(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "niu-hist646-{prefix}-{}-{nanos}",
        std::process::id()
    ))
}

fn spawn_with_rc(prefix: &str, extra_rc: &str, seed: Option<&str>) -> (NiuSession, PathBuf) {
    let root = unique_root(prefix);
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    if let Some(seed) = seed {
        std::fs::write(home.join(".niubash_history"), seed).unwrap();
    }
    let rc = format!("{}\n{extra_rc}", default_rc());
    let mut session =
        NiuSession::spawn_shared_home(&root, &rc, &[], &[], (120, 30), DEFAULT_TIMEOUT);
    session.wait_ready();
    (session, home)
}

fn run(session: &mut NiuSession, line: &str) {
    session.send_line(line);
    session.expect_prompt();
    std::thread::sleep(std::time::Duration::from_millis(400));
    session.drain_screen();
}

#[test]
fn histcontrol_and_histignore_filter_repl_recording() {
    if !require_pty_or_skip("histcontrol_and_histignore_filter_repl_recording") {
        return;
    }
    let (mut session, home) = spawn_with_rc(
        "control",
        "export HISTCONTROL=ignoreboth\nexport HISTIGNORE='secret*'\n",
        None,
    );

    run(&mut session, "echo one");
    run(&mut session, "echo one");
    run(&mut session, " secretcmd");
    run(&mut session, "secretstuff");
    run(&mut session, "echo two");
    run(
        &mut session,
        &format!("history > {}", home.join("dump.txt").display()),
    );
    session.send_line("exit");
    session.wait_exit();

    let dump = std::fs::read_to_string(home.join("dump.txt")).unwrap();
    assert_eq!(
        dump.matches("echo one").count(),
        1,
        "ignoredups: the duplicate is recorded once:\n{dump}"
    );
    assert!(
        dump.contains("echo two"),
        "ordinary lines are recorded:\n{dump}"
    );
    assert!(
        !dump.contains("secretstuff"),
        "HISTIGNORE=secret* must filter at record time:\n{dump}"
    );
    assert!(
        !dump.contains("secretcmd"),
        "ignorespace (via ignoreboth) must filter the space-prefixed line:\n{dump}"
    );

    let contents = std::fs::read_to_string(home.join(".niubash_history")).unwrap();
    assert_eq!(
        contents.matches("echo one").count(),
        1,
        "the file agrees with the list:\n{contents}"
    );
    assert!(
        !contents.contains("secretstuff") && !contents.contains("secretcmd"),
        "filtered lines never reach the file:\n{contents}"
    );
    assert!(
        contents.contains("echo two"),
        "recorded lines reach the file:\n{contents}"
    );
}

#[test]
fn histsize_stifles_list_and_histfilesize_truncates_file() {
    if !require_pty_or_skip("histsize_stifles_list_and_histfilesize_truncates_file") {
        return;
    }
    let seed = "seed0\nseed1\nseed2\nseed3\nseed4\nseed5\nseed6\nseed7\n";
    let (mut session, home) = spawn_with_rc(
        "sizes",
        "export HISTSIZE=3\nexport HISTFILESIZE=4\n",
        Some(seed),
    );

    for n in 1..=6 {
        run(&mut session, &format!("echo n{n}"));
    }
    run(
        &mut session,
        &format!("history > {}", home.join("dump.txt").display()),
    );
    session.send_line("exit");
    session.wait_exit();

    let dump = std::fs::read_to_string(home.join("dump.txt")).unwrap();
    assert_eq!(
        dump.lines().count(),
        3,
        "HISTSIZE=3: the list shows exactly its last three entries:\n{dump}"
    );
    assert!(dump.contains("echo n6"), "the newest lines show:\n{dump}");
    assert!(
        !dump.contains("seed0"),
        "stifled entries do not show:\n{dump}"
    );

    let contents = std::fs::read_to_string(home.join(".niubash_history")).unwrap();
    assert_eq!(
        contents.lines().count(),
        4,
        "HISTFILESIZE=4: the session-end write leaves the file's last four lines:\n{contents}"
    );
    assert!(
        contents.contains("echo n6") && contents.trim_end().ends_with("exit"),
        "the file's tail is the session's newest lines:\n{contents}"
    );
    assert!(
        !contents.contains("seed"),
        "the truncated seeds are gone from the file:\n{contents}"
    );
}
