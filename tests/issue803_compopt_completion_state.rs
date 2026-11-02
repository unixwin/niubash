//! niubash#803 regression pins (engine-dependent): bash-completion under
//! niu hit three engine gaps at once —
//!
//! 1. `compopt` was rejected inside a completion function
//!    ("niu: compopt: not currently executing completion function"),
//!    breaking every `_comp_compgen`-family helper that tunes the live
//!    attempt (`compopt -o nospace`, `_comp_ltrim_colon_completions`, ...).
//!    GNU complete.def:862-886 admits the bare form while the completion
//!    function runs; the engine fix (rubash branch
//!    fix/803-compopt-completion-state) ports `pcomp_curcs` /
//!    RL_STATE_COMPLETING.
//! 2. `compgen -c` listed every data file in every PATH directory
//!    (~4700 DLL/PNG/MOF "commands" from C:\Windows\System32 — the
//!    Tab menu on a polluted line showed the whole System32 listing).
//!    The engine now applies the spawn-rule executability filter
//!    (PATHEXT or extensionless).
//! 3. `ssh al<Tab><Tab>` "session restart" shape: the t6 transcript's
//!    second banner followed a completion attempt; this pin holds the
//!    contract the issue names — a double-Tab completion leaves the
//!    session alive with exactly one startup banner.
//!
//! Also pins niubash#804: an interactive session reports `emacs` on
//! (GNU get_edit_mode view, set.def:448-452 — ble.sh gates on
//! `! -o emacs && ! -o vi`).

#[path = "interactive/driver.rs"]
mod driver;

use std::time::Duration;

use driver::{require_pty_or_skip, NiuSession, DEFAULT_TIMEOUT, TAB};

fn frame(session: &mut NiuSession) -> String {
    std::thread::sleep(Duration::from_millis(700));
    session.drain_screen()
}

/// The chunk with every escape sequence removed — the text a user would
/// read off the frame.
fn plain(chunk: &str) -> String {
    let mut out = String::new();
    let mut chars = chunk.chars();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            match chars.next() {
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    for c in chars.by_ref() {
                        if c == '\x07' {
                            break;
                        }
                    }
                }
                _ => {}
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// The completion-function state: `compopt` runs twice inside the -F
/// function — the option form (`compopt -o nospace`, complete.def:876-884)
/// and the bare report form (complete.def:870-874) — and both statuses land
/// in COMPREPLY. Before the fix both calls failed with rc 1 and the
/// "not currently executing completion function" diagnostic.
#[test]
fn issue803_compopt_accepted_inside_completion_function() {
    if !require_pty_or_skip("issue803_compopt_accepted_inside_completion_function") {
        return;
    }
    let rc = "PS1='P1> '\nNIU_DISABLE_DEFAULT_PLUGINS=1\n\
              _c803() { compopt -o nospace; r1=$?; compopt >/dev/null 2>&1; r2=$?; \
              COMPREPLY=(\"ok_${r1}_${r2}\"); }\n\
              complete -F _c803 cmd803\n"
        .to_string();
    let mut s = NiuSession::spawn_custom("issue803-compopt", &rc, &[], (80, 24), DEFAULT_TIMEOUT);
    s.wait_ready();
    s.send("cmd803 ");
    let _ = frame(&mut s);
    s.send(TAB);
    let f = frame(&mut s);
    assert!(
        plain(&f).contains("ok_0_0"),
        "both compopt forms must succeed inside the completion function:\n{f:?}"
    );
    assert!(
        !f.contains("not currently executing completion function"),
        "no compopt rejection diagnostic:\n{f:?}"
    );
}

/// The t6 crash shape: a prefix completion driven through TWO Tab presses
/// leaves the session alive (still answering) with exactly one startup
/// banner — no restart, no lost line discipline.
#[test]
fn issue803_double_tab_completion_keeps_session_alive_with_one_banner() {
    if !require_pty_or_skip("issue803_double_tab_completion_keeps_session_alive_with_one_banner") {
        return;
    }
    let rc = "PS1='P1> '\nNIU_DISABLE_DEFAULT_PLUGINS=1\n\
              _h803() { compopt -o filenames; compgen -c -- \"${COMP_WORDS[COMP_CWORD]}\" \
              >/dev/null; COMPREPLY=(alpha-test beta-test); }\n\
              complete -F _h803 ssh803\n"
        .to_string();
    let mut s = NiuSession::spawn_custom("issue803-dtab", &rc, &[], (100, 30), DEFAULT_TIMEOUT);
    s.wait_ready();
    let banners_before = s.transcript().matches("Niubash").count();
    s.send("ssh803 al");
    let _ = frame(&mut s);
    s.send(TAB);
    let _ = frame(&mut s);
    s.send(TAB);
    let _ = frame(&mut s);
    // Close the candidate menu (Esc rolls the line back to the typed
    // prefix, niubash#778 contract) so the marker line is a COMMAND, not
    // menu filter text.
    s.send("\x1b");
    let _ = frame(&mut s);
    s.send("\u{15}");
    let _ = frame(&mut s);
    s.send_line("echo STILL_$((1+1))_ALIVE");
    s.expect("STILL_2_ALIVE");
    let banners_after = s.transcript().matches("Niubash").count();
    assert_eq!(
        banners_before,
        banners_after,
        "a double-Tab completion must not restart the session \
         (banners before {banners_before}, after {banners_after}):\n{}",
        s.transcript()
    );
    assert!(
        !s.transcript()
            .contains("not currently executing completion function"),
        "no compopt rejection diagnostic in the whole session"
    );
}

/// `compgen -c` (the PATH command listing `_comp_compgen_commands` drives)
/// must not offer Windows data files as commands — before the fix the
/// whole C:\Windows\System32 directory listing flowed into the Tab menu.
/// All-builtin filtering (the driver PATH is System32-only, so a leak
/// would be visible).
#[cfg(windows)]
#[test]
fn issue803_compgen_c_lists_no_windows_data_files() {
    if !require_pty_or_skip("issue803_compgen_c_lists_no_windows_data_files") {
        return;
    }
    let mut s = NiuSession::spawn("issue803-compgen-c");
    s.wait_ready();
    s.send_line(
        "n=0; for w in $(compgen -c); do case $w in *.dll|*.png|*.mof|*.json) \
         n=$((n+1));; esac; done; echo DLLISH=$n",
    );
    s.expect("DLLISH=0");
    s.send_line(
        "c=0; for w in $(compgen -c cmd); do case $w in cmd.exe) c=1;; esac; done; \
                echo CMDEXE=$c",
    );
    s.expect("CMDEXE=1");
}

/// niubash#804: an interactive session defaults to emacs editing mode in
/// the `set -o` view — the gate ble.sh applies (`! -o emacs && ! -o vi`).
#[test]
fn issue804_interactive_emacs_option_defaults_on() {
    if !require_pty_or_skip("issue804_interactive_emacs_option_defaults_on") {
        return;
    }
    let mut s = NiuSession::spawn("issue804-emacs");
    s.wait_ready();
    s.send_line("[[ -o emacs ]] && echo EMACS_ON || echo EMACS_OFF");
    s.expect("EMACS_ON");
    s.send_line("[[ -o vi ]] && echo VI_ON || echo VI_OFF");
    s.expect("VI_OFF");
}
