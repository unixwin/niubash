//! niubash#741/#742/#743 zsh-parity completion pins on a real terminal:
//!
//! * select interaction (the default since niubash#742): TAB #1 opens
//!   the candidate grid WITHOUT splicing a candidate; TAB #2 selects
//!   the first candidate; further TABs cycle; Esc rolls back.
//! * bare TAB (blank line before the cursor): inserts a four-space
//!   indent instead of opening the command-position grid (#742).
//! * large candidate sets keep a multi-column grid even with a long
//!   outlier name (#741, packed zsh-style layout).
//! * filesystem candidates paint through LS_COLORS (#743).
//!
//! Driver: [`mod driver`] (portable-pty; see tests/interactive.rs for
//! the harness contract). Run:
//! `NIU_INTERACTIVE_TESTS=1 cargo test --test issue742_completion_zsh --locked -- --nocapture`

#[path = "interactive/driver.rs"]
mod driver;

use std::time::Duration;

use driver::{require_pty_or_skip, NiuSession, DEFAULT_TIMEOUT, TAB};

const SETTLE: Duration = Duration::from_millis(400);
const ESC: &str = "\u{1b}";

/// Two same-prefix directories, the select-mode candidate vehicle.
fn write_dirs(s: &NiuSession) {
    std::fs::create_dir_all(s.start().join("sel-one")).unwrap();
    std::fs::create_dir_all(s.start().join("sel-two")).unwrap();
}

/// select is the built-in default: no NIU_COMPLETION_INTERACTION env.
/// The execution-level proof that TAB #1 spliced nothing: Enter right
/// after it submits `cd sel-` — which fails — where a spliced
/// candidate would have executed `cd sel-one/` instead.
#[test]
fn issue742_pty_select_first_tab_shows_grid_without_splicing() {
    if !require_pty_or_skip("issue742_pty_select_first_tab_shows_grid_without_splicing") {
        return;
    }
    let mut s = NiuSession::spawn("sel-open-742");
    write_dirs(&s);
    s.wait_ready();
    s.send("cd sel-");
    s.send(TAB);
    // The grid is up (a candidate is visible) but nothing was spliced.
    s.expect("sel-one");
    s.send("\r");
    s.expect("No such file");
    s.expect_prompt();
    s.send_line("pwd");
    s.expect("/start");
    s.expect_prompt();
}

/// TAB #2 selects the FIRST candidate; TAB #3 moves to the second —
/// verified end to end through `pwd`.
#[test]
fn issue742_pty_select_second_tab_picks_first_and_cycles() {
    if !require_pty_or_skip("issue742_pty_select_second_tab_picks_first_and_cycles") {
        return;
    }
    for (tabs, expected) in [(2usize, "/sel-one"), (3, "/sel-two"), (4, "/sel-one")] {
        let mut s = NiuSession::spawn("sel-pick-742");
        write_dirs(&s);
        s.wait_ready();
        s.send("cd sel-");
        for i in 0..tabs {
            s.send(TAB);
            if i == 0 {
                s.expect("sel-one");
            } else {
                std::thread::sleep(SETTLE);
            }
        }
        s.send("\r");
        s.expect_prompt();
        s.send_line("pwd");
        s.expect(expected);
        s.expect_prompt();
    }
}

/// Esc on an unselected open grid just closes it; Esc after a pick
/// rolls the line back to the pre-completion snapshot.
#[test]
fn issue742_pty_select_esc_rolls_back_the_pick() {
    if !require_pty_or_skip("issue742_pty_select_esc_rolls_back_the_pick") {
        return;
    }
    let mut s = NiuSession::spawn("sel-esc-742");
    write_dirs(&s);
    s.wait_ready();
    s.send("cd sel-");
    s.send(TAB);
    s.expect("sel-one");
    s.send(TAB); // select sel-one
    std::thread::sleep(SETTLE);
    s.send(ESC);
    std::thread::sleep(SETTLE);
    s.send("\r");
    s.expect_prompt();
    s.send_line("pwd");
    // POSIX persona form: `C:\foo\bar` renders as `/c/foo/bar`.
    s.expect("/start");
    s.expect_prompt();
}

/// Bare TAB on an empty line inserts a four-space indent and never
/// opens the command-position grid.
#[test]
fn issue742_pty_bare_tab_inserts_indent() {
    if !require_pty_or_skip("issue742_pty_bare_tab_inserts_indent") {
        return;
    }
    let mut s = NiuSession::spawn("bare-tab-742");
    s.wait_ready();
    s.send(TAB);
    std::thread::sleep(SETTLE);
    s.send("echo bare-ok");
    s.send("\r");
    s.expect("bare-ok");
    s.expect_prompt();
    let transcript = s.transcript();
    // The painted input line carries the indent straight after the
    // prompt's reset code (`<ESC>[m` + four spaces + `echo` — the
    // driver's transcript escapes ESC to `<ESC>`).
    assert!(
        transcript.contains("<ESC>[m    "),
        "the TAB must have left a four-space indent in the line:\n{transcript}"
    );
}

/// niubash#741: a candidate set with one very long name keeps a
/// multi-column grid — the outlier no longer collapses the uniform
/// field into a one-per-line list.
#[test]
fn issue741_pty_long_outlier_keeps_multi_column_grid() {
    if !require_pty_or_skip("issue741_pty_long_outlier_keeps_multi_column_grid") {
        return;
    }
    let mut s = NiuSession::spawn("pack-grid-741");
    for i in 0..24 {
        std::fs::write(s.start().join(format!("pk{i:02}")), "x\n").unwrap();
    }
    std::fs::write(
        s.start()
            .join("pk-veryverylongcandidatenamethatkeepsgoing1234"),
        "x\n",
    )
    .unwrap();
    s.wait_ready();
    s.send("cat pk");
    s.send(TAB);
    // Grid up: with the packed layout the 24 short names grid over
    // several columns — the transcript must show two candidate names
    // on one row (the pre-fix shape was one per line).
    s.expect("pk00");
    std::thread::sleep(SETTLE);
    let transcript = s.transcript();
    let multi = transcript
        .lines()
        .filter(|line| line.matches("pk").count() >= 2)
        .count();
    assert!(
        multi >= 1,
        "at least one grid row must carry 2+ candidates (multi-column):\n{transcript}"
    );
    s.send(ESC);
    s.send("\r");
    s.expect_prompt();
}

/// niubash#741, the issue's own geometry: ~90 candidates with name
/// widths spread over 4..=25 in the home-sized start dir, `ls ` + TAB
/// at 120x30 — the grid stays multi-column. (Regression guard: this
/// plain shape already laid out correctly; the collapse lives in the
/// two shapes pinned below.)
#[test]
fn issue741_pty_home_sized_set_keeps_multi_column_grid() {
    if !require_pty_or_skip("issue741_pty_home_sized_set_keeps_multi_column_grid") {
        return;
    }
    let mut s = NiuSession::spawn("home-grid-741");
    let mut heads = Vec::new();
    for i in 0..88 {
        let width = 4 + (i % 22);
        let mut name = format!("h{i:02}");
        while name.len() < width {
            name.push('x');
        }
        heads.push(format!("h{i:02}"));
        if i % 3 == 0 {
            std::fs::create_dir_all(s.start().join(&name)).unwrap();
        } else {
            std::fs::write(s.start().join(&name), "x\n").unwrap();
        }
    }
    std::fs::create_dir_all(s.start().join("HoleFeature_x64_20261030")).unwrap();
    s.wait_ready();
    s.send("ls ");
    s.send(TAB);
    s.expect("HoleFeature");
    std::thread::sleep(SETTLE);
    let transcript = s.transcript();
    let multi = transcript
        .lines()
        .filter(|line| heads.iter().filter(|h| line.contains(h.as_str())).count() >= 2)
        .count();
    assert!(
        multi >= 1,
        "at least one grid row must carry 2+ candidates (multi-column):\n{transcript}"
    );
    s.send(ESC);
    s.send("\r");
    s.expect_prompt();
}

/// niubash#741: one 100-wide file name among 25-wide companions —
/// uncapped, no second column fits beside the outlier's at 120
/// (100 + 25 + gutter > 120) and the grid renders one-per-line.
/// With the truncation rescue the outlier is cut and the grid
/// stays multi-column.
#[test]
fn issue741_pty_wide_outlier_keeps_multi_column_grid() {
    if !require_pty_or_skip("issue741_pty_wide_outlier_keeps_multi_column_grid") {
        return;
    }
    let mut s = NiuSession::spawn("outlier-grid-741");
    for i in 0..89 {
        std::fs::write(s.start().join(format!("pk{i:023}")), "x\n").unwrap();
    }
    std::fs::write(s.start().join(format!("z{}", "9".repeat(99))), "x\n").unwrap();
    s.wait_ready();
    s.send("cat ");
    s.send(TAB);
    s.expect("pk000");
    std::thread::sleep(SETTLE);
    let transcript = s.transcript();
    let multi = transcript
        .lines()
        .filter(|line| line.matches("pk").count() >= 2)
        .count();
    assert!(
        multi >= 1,
        "at least one grid row must carry 2+ candidates (multi-column):\n{transcript}"
    );
    s.send(ESC);
    s.send("\r");
    s.expect_prompt();
}

/// niubash#741: the shared-prefix shape — completing inside a typed
/// deep path puts the whole prefix in every candidate's display
/// value (here 62 cells of prefix on 3-cell names). Uncapped, every
/// column's max is ~65 and two columns bust 120: one-per-line.
/// Truncation keeps the grid at two columns, and each cell's tail
/// (the file name) stays visible.
#[test]
fn issue741_pty_prefix_inflated_set_keeps_multi_column_grid() {
    if !require_pty_or_skip("issue741_pty_prefix_inflated_set_keeps_multi_column_grid") {
        return;
    }
    let mut s = NiuSession::spawn("prefix-grid-741");
    let a_dir = "a".repeat(30);
    let b_dir = "b".repeat(30);
    let deep = s.start().join(&a_dir).join(&b_dir);
    std::fs::create_dir_all(&deep).unwrap();
    for i in 0..30 {
        std::fs::write(deep.join(format!("f{i:02}")), "x\n").unwrap();
    }
    s.wait_ready();
    s.send(&format!("cat {a_dir}/{b_dir}/"));
    s.send(TAB);
    s.expect("f00");
    std::thread::sleep(SETTLE);
    let transcript = s.transcript();
    // Each painted cell ends in its file name ("/fNN" survives the
    // middle truncation), so a two-cell row shows "/f" twice; the
    // one-per-line collapse never does.
    let multi = transcript
        .lines()
        .filter(|line| line.matches("/f").count() >= 2)
        .count();
    assert!(
        multi >= 1,
        "at least one grid row must carry 2+ candidates (multi-column):\n{transcript}"
    );
    s.send(ESC);
    s.send("\r");
    s.expect_prompt();
}

/// niubash#743: the grid paints candidates through the session's
/// LS_COLORS — an explicit tiny table pins the ANSI sequences.
#[test]
fn issue743_pty_candidates_paint_ls_colors() {
    if !require_pty_or_skip("issue743_pty_candidates_paint_ls_colors") {
        return;
    }
    let mut s = NiuSession::spawn_custom(
        "ls-colors-743",
        &driver::default_rc(),
        &[("LS_COLORS".to_string(), "di=31:*.zzt=35".to_string())],
        (120, 30),
        DEFAULT_TIMEOUT,
    );
    std::fs::create_dir_all(s.start().join("cadir")).unwrap();
    std::fs::write(s.start().join("cafile.zzt"), "x\n").unwrap();
    s.wait_ready();
    s.send("cat ca");
    s.send(TAB);
    s.expect("cafile");
    std::thread::sleep(SETTLE);
    let transcript = s.transcript();
    // nu_ansi_term emits combined attributes as `\x1b[N;31m`-style
    // sequences — assert on the color-code tails, not one exact form.
    assert!(
        transcript.contains("31m"),
        "the directory candidate must paint di=31 (red):\n{transcript}"
    );
    assert!(
        transcript.contains("35m"),
        "the .zzt candidate must paint the suffix rule 35 (magenta):\n{transcript}"
    );
    s.send(ESC);
    s.send("\r");
    s.expect_prompt();
}
