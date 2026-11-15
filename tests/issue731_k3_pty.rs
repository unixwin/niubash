//! niubash#731 K3 remainder: the PTY-level delivery checks the issue
//! asks for — `bind` entries for the three K3 surfaces must reach a
//! running prompt, not only the unit-level translation table.
//!
//! * `quoted-insert` — `bind '"\C-v": quoted-insert'` must no longer be
//!   judged "not applied" (the 2026-10-29 real-machine miss); the next
//!   key after the arming chord inserts verbatim, so `C-v ENTER`
//!   puts a literal newline in the edit buffer instead of submitting.
//! * `redraw-current-line` — the bound key maps to `Repaint`: the edit
//!   line is repainted byte-identically and the buffer survives.
//! * `vi-editing-mode` — the bound key maps to
//!   `ReedlineEvent::SwitchMode`: the standby vi machine takes over and
//!   `ESC x` deletes under vi-normal semantics.
//! * `history-search-backward` — the bound key maps to
//!   `ReedlineEvent::HistorySearch { anchored: true }`: a typed prefix
//!   filters the recall instead of stepping back one entry.
//!
//! Each check also asserts the boot screen carries no "key bindings …
//! not applied" notice for its bind (the registry's actionable-warning
//! face) — and the unconfigured regression requirement rides along via
//! the markers: nothing about an unbound prompt's rendering changes.
//!
//! Harness: [`mod driver`]. Sessions spawn over a caller-staged root via
//! `spawn_shared_home`.
//!
//! Run: `NIU_INTERACTIVE_TESTS=1 cargo test --test issue731_k3_pty --locked -- --nocapture`
//! CI home: the unix cross-check interactive gate (ci.yml) runs this
//! stem under `NIU_INTERACTIVE_TESTS=1` next to `issue827_pty_gate`;
//! on Windows it stays a manual gate — the ConPTY-rendered prompt
//! carries no literal `P1> ` space, which is why the driver needle
//! is the space-free `P1>` (niubash#1011 audit).

#[path = "interactive/driver.rs"]
mod driver;

use std::path::PathBuf;
use std::time::Duration;

use driver::{require_pty_or_skip, NiuSession, DEFAULT_TIMEOUT};

fn staged_root(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("niu-issue731-{}-{}", std::process::id(), tag));
    std::fs::create_dir_all(&root).expect("stage root");
    root
}

fn spawn_k3(tag: &str, rc_extra: &str) -> NiuSession {
    let rc = format!("{}{}", driver::default_rc(), rc_extra);
    NiuSession::spawn_shared_home(&staged_root(tag), &rc, &[], &[], (100, 30), DEFAULT_TIMEOUT)
}

/// Strip CSI escapes so printed glyphs concatenate the way a user reads
/// them (the transcript marks ESC as `<ESC>` inside rendered words).
fn normalize_screen(raw: &str) -> String {
    // Both ESC spellings appear in driver output: `drain_screen` yields
    // raw `\x1b` bytes, `transcript()` renders the literal text `<ESC>`.
    // In either case the escape body follows — CSI bodies end at a byte
    // in 0x40..=0x7e, non-CSI escapes after one char.
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    let skip_body = |chars: &mut std::iter::Peekable<std::str::Chars>| match chars.peek() {
        Some('[') => {
            chars.next();
            while let Some(&n) = chars.peek() {
                chars.next();
                if ('@'..='~').contains(&n) {
                    break;
                }
            }
        }
        Some(_) => {
            chars.next();
        }
        None => {}
    };
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            skip_body(&mut chars);
            continue;
        }
        if c == '<' {
            let marker: String = chars.clone().take(4).collect();
            if marker == "ESC>" {
                for _ in 0..4 {
                    chars.next();
                }
                skip_body(&mut chars);
                continue;
            }
            out.push(c);
        } else {
            out.push(c);
        }
    }
    out
}

/// `C-v` arms quoted-insert; an `ENTER` pressed inside the quote arm is
/// inserted literally, so the `echo` never runs — nothing prints until
/// the line is actually submitted. Without the delivery arm the chord
/// would be dropped and `ENTER` would submit immediately.
#[test]
fn pty_k3_quoted_insert_delivery_arm() {
    if !require_pty_or_skip("pty_k3_quoted_insert_delivery_arm") {
        return;
    }
    let mut s = spawn_k3(
        "qinsert",
        "Q731=should-not-print\nbind '\"\\C-v\": quoted-insert'\n",
    );
    s.wait_ready();
    // The bind is judged applied: no actionable notice on the boot face.
    s.expect_absent("not applied", Duration::from_secs(2));
    s.expect_absent("key binding", Duration::from_secs(1));

    // Echo-proof (the #367 rule): the typed line shows `$Q731`, only
    // executed output can print the value.
    s.send("echo $Q731");
    s.send("\u{16}"); // C-v arms the quote state
    s.send("\r"); // quoted: literal newline into the buffer, no submit
    s.expect_absent("should-not-print", Duration::from_secs(3));

    // Abort the armed buffer so the session ends clean.
    s.send(driver::CTRL_C);
    s.expect_prompt();
    std::thread::sleep(Duration::from_millis(300));
    s.send_line("exit");
    s.wait_exit();
}

/// `redraw-current-line` repaints the buffer byte-identically: the text
/// typed before the bound key is still the text shown after it.
#[test]
fn pty_k3_redraw_current_line_repaints_identically() {
    if !require_pty_or_skip("pty_k3_redraw_current_line_repaints_identically") {
        return;
    }
    let mut s = spawn_k3("redraw", "bind '\"\\C-q\": redraw-current-line'\n");
    s.wait_ready();
    s.expect_absent("not applied", Duration::from_secs(2));

    s.send("REDRAW-MARKER-731");
    s.send("\u{11}"); // C-q → Repaint
    std::thread::sleep(Duration::from_millis(400));
    let screen = normalize_screen(&s.drain_screen());
    assert!(
        screen.contains("REDRAW-MARKER-731"),
        "redraw-current-line must repaint the buffer byte-identically:\n{screen}"
    );

    s.send(driver::CTRL_C);
    s.expect_prompt();
    s.send_line("exit");
    s.wait_exit();
}

/// Chord observation ① (the #704 review's idle-flush note, carried
/// here as K3 scope item 4): GNU's `keyseq-timeout` fires on its own —
/// a parked chord prefix replays through the inner machine after the
/// window with no partner key (readline.c `_rl_dispatch_subseq`, whose
/// `_rl_input_queued` wait returns the shadowed function). A letter
/// chord proves it end to end: `bind '"zx": kill-whole-line'` parks
/// `z` for the window, then the plain `z` self-inserts by itself —
/// and `z`+`x` inside the window still completes the bound command.
#[test]
fn pty_k3_chord_keyseq_timeout_flushes_on_idle() {
    if !require_pty_or_skip("pty_k3_chord_keyseq_timeout_flushes_on_idle") {
        return;
    }
    let mut s = spawn_k3("chordidle", "bind '\"zx\": kill-whole-line'\n");
    s.wait_ready();
    s.expect_absent("not applied", Duration::from_secs(2));

    // Lone `z`: after ~`keyseq-timeout` of idle the prefix expires and
    // the letter self-inserts — under the old next-key flush the screen
    // stayed bare until a second key was pressed.
    s.send("z");
    std::thread::sleep(Duration::from_millis(1200));
    let screen = normalize_screen(&s.drain_screen());
    let tail = screen.rsplit("P1>").next().unwrap_or("");
    assert!(
        tail.contains('z'),
        "idle keyseq-timeout must flush the parked 'z' into the buffer:\n{screen}"
    );

    // `z`+`x` inside the window still completes the chord — the typed
    // text dies with the line.
    s.send(driver::CTRL_C);
    s.expect_prompt();
    s.send("victim");
    s.send("zx");
    std::thread::sleep(Duration::from_millis(800));
    let screen = normalize_screen(&s.drain_screen());
    let tail = screen.rsplit("P1>").next().unwrap_or("");
    assert!(
        !tail.contains("victim"),
        "z+x inside the window must complete the chord (kill-whole-line):\n{screen}"
    );

    s.send(driver::CTRL_C);
    s.expect_prompt();
    s.send_line("exit");
    s.wait_exit();
}

/// `vi-editing-mode` switches into the standby vi machine: after the
/// bound key, `ESC` lands in vi-normal and `x` deletes the character
/// under the cursor — a mechanism the emacs machine does not have.
#[test]
fn pty_k3_vi_editing_mode_switches_machines() {
    if !require_pty_or_skip("pty_k3_vi_editing_mode_switches_machines") {
        return;
    }
    let mut s = spawn_k3("vimode", "bind '\"\\C-q\": vi-editing-mode'\n");
    s.wait_ready();
    s.expect_absent("not applied", Duration::from_secs(2));

    s.send("zzabc");
    s.send("\u{11}"); // C-q → SwitchMode(Vi(Insert))
    std::thread::sleep(Duration::from_millis(600));
    s.send("\u{1b}"); // ESC: vi-insert → vi-normal (cursor on 'c')
    std::thread::sleep(Duration::from_millis(800));
    s.send("x"); // vi-normal x deletes 'c'
    std::thread::sleep(Duration::from_millis(600));
    let screen = normalize_screen(&s.drain_screen());
    // Assert on the LAST repaint: scrollback still shows the `zzabc`
    // first paint, so judge the buffer row after the final `P1>`.
    let tail = screen.rsplit("P1>").next().unwrap_or("");
    assert!(
        tail.contains("zzab") && !tail.contains("zzabc"),
        "vi-editing-mode must activate the vi machine (x deleted 'c'):\n{screen}"
    );

    s.send(driver::CTRL_C);
    s.expect_prompt();
    s.send_line("exit");
    s.wait_exit();
}

/// `history-search-backward` is GNU's anchored prefix search (search.c
/// `rl_history_search_backward`), not a plain Up: with `uni` typed,
/// the bound key must land on the newest history entry STARTING with
/// `uni` and skip the more recent entry that does not match.
#[test]
fn pty_k3_history_search_backward_is_anchored() {
    if !require_pty_or_skip("pty_k3_history_search_backward_is_anchored") {
        return;
    }
    let mut s = spawn_k3("hsearch", "bind '\"\\C-q\": history-search-backward'\n");
    s.wait_ready();
    s.expect_absent("not applied", Duration::from_secs(2));

    // Seed history: a matching entry, then a newer non-matching one a
    // plain `Up` would have recalled.
    s.send_line("unique731alpha");
    s.expect_prompt();
    s.send_line("zzdifferent731");
    s.expect_prompt();

    s.send("uni");
    s.send("\u{11}"); // C-q → HistorySearch{Backward, anchored}
    std::thread::sleep(Duration::from_millis(800));
    let screen = normalize_screen(&s.drain_screen());
    let tail = screen.rsplit("P1>").next().unwrap_or("");
    assert!(
        tail.contains("unique731alpha"),
        "anchored history-search must land on the prefix match:\n{screen}"
    );
    assert!(
        !tail.contains("zzdifferent731"),
        "a plain Up would have recalled the non-matching latest entry:\n{screen}"
    );

    s.send(driver::CTRL_C);
    s.expect_prompt();
    s.send_line("exit");
    s.wait_exit();
}
