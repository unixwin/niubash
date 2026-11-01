//! niubash#778 regression: in the default completion interaction
//! (`select`, the zsh menu-select form, default since niubash#742),
//! pressing TAB showed the candidate grid with NO candidate
//! highlighted, no matter how many TABs; a highlight appeared only
//! after pressing Down once and TABbing again, and the arrow keys
//! did not move the pick through the grid.
//!
//! Root cause (all niubash-side; the editor crate forwards every
//! event involved): the #742 select form opened the grid unselected
//! (`CycleMenu::has_selection` false), the edit-mode wrapper's TAB
//! splices echo back to the menu through `MenuEvent::Edit`, and the
//! echo sync adopted the index but never the selection — so the
//! wrapper path could never paint a highlight. The wrapper also
//! classified every arrow key as session-ending: Down silently
//! killed the shared session (the following TAB then took the
//! menu-local navigate path, which is why a highlight "appeared"
//! after Down+TAB), and Left/Right closed the menu outright.
//!
//! The contract (cycle_menu DESIGN §7): TAB #1 puts the first
//! candidate into the line AND paints it highlighted in frame #1;
//! TAB/Shift-TAB cycle; the arrows move the highlight through the
//! grid geometry with the line following in the same frame; Esc
//! rolls the line back to the pre-completion snapshot.
//!
//! Driver: [`mod driver`] (portable-pty; see tests/interactive.rs
//! for the harness contract — the reader thread answers the
//! painter's `ESC[6n` cursor-position query, without which nothing
//! paints).

#[path = "interactive/driver.rs"]
mod driver;

use std::time::Duration;

use driver::{require_pty_or_skip, NiuSession, DEFAULT_TIMEOUT, TAB};

const DOWN: &str = "\x1b[B";
const UP: &str = "\x1b[A";
const RIGHT: &str = "\x1b[C";
const LEFT: &str = "\x1b[D";
const ESC: &str = "\x1b";

const CANDIDATES: [&str; 9] = [
    "cand-01.txt",
    "cand-02.txt",
    "cand-03.txt",
    "cand-04.txt",
    "cand-05.txt",
    "cand-06.txt",
    "cand-07.txt",
    "cand-08.txt",
    "cand-09.txt",
];

/// Let the frame for the last key settle, then take it.
fn frame(session: &mut NiuSession) -> String {
    std::thread::sleep(Duration::from_millis(700));
    session.drain_screen()
}

/// The chunk with every escape sequence removed — the text a user
/// would read off the frame, for line-content assertions.
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

/// Whether `candidate`'s last painted occurrence in this frame
/// chunk wears the selection (SGR reverse video). The grid is
/// painted after the input line, so a candidate's last occurrence
/// is its grid cell; the SGR state is tracked cumulatively across
/// the chunk exactly as a terminal would apply it.
fn painted_selected(chunk: &str, candidate: &str) -> bool {
    let bytes = chunk.as_bytes();
    let mut reverse = false;
    let mut selected = false;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            let mut j = i + 2;
            while j < bytes.len() && !((0x40..=0x7e).contains(&bytes[j])) {
                j += 1;
            }
            if j >= bytes.len() {
                break;
            }
            if bytes[j] == b'm' {
                let params = &chunk[i + 2..j];
                if params.is_empty() {
                    reverse = false;
                }
                for part in params.split(';') {
                    match part {
                        "" | "0" => reverse = false,
                        "7" => reverse = true,
                        "27" => reverse = false,
                        _ => {}
                    }
                }
            }
            i = j + 1;
            continue;
        }
        if chunk[i..].starts_with(candidate) {
            selected = reverse;
            i += candidate.len();
            continue;
        }
        i += 1;
    }
    selected
}

#[test]
fn issue778_menu_select_highlight_tab1_and_arrows_move_the_pick() {
    if !require_pty_or_skip("issue778_menu_select_highlight_tab1_and_arrows_move_the_pick") {
        return;
    }
    // 40 columns over the width-11 candidates lays the grid out as
    // 3 columns x 3 rows, column-major:
    //   cand-01 cand-04 cand-07
    //   cand-02 cand-05 cand-08
    //   cand-03 cand-06 cand-09
    let mut s = NiuSession::spawn_custom(
        "menu-select-highlight",
        &driver::default_rc(),
        &[],
        (40, 30),
        DEFAULT_TIMEOUT,
    );
    for name in CANDIDATES {
        std::fs::write(s.start().join(name), "x\n").unwrap();
    }
    s.wait_ready();
    s.send("cat cand-");
    let _ = frame(&mut s);

    // TAB #1: first candidate spliced into the line AND highlighted
    // in the same frame (the #778 regression: line untouched, no
    // highlight, for any number of TABs).
    s.send(TAB);
    let f = frame(&mut s);
    assert!(
        plain(&f).contains("cat cand-01.txt"),
        "TAB #1 must splice the first candidate into the line:\n{f:?}"
    );
    assert!(
        painted_selected(&f, "cand-01.txt"),
        "TAB #1 must paint candidate 1 highlighted:\n{f:?}"
    );
    assert!(
        !painted_selected(&f, "cand-02.txt"),
        "candidate 2 must not wear the selection yet:\n{f:?}"
    );

    // TAB #2: the pick cycles to candidate 2, line and highlight in
    // the same frame.
    s.send(TAB);
    let f = frame(&mut s);
    assert!(plain(&f).contains("cat cand-02.txt"), "{f:?}");
    assert!(painted_selected(&f, "cand-02.txt"), "{f:?}");
    assert!(!painted_selected(&f, "cand-01.txt"), "{f:?}");

    // Down steps within the column (candidate 2 -> 3); before #778
    // this key silently ended the session and nothing moved.
    s.send(DOWN);
    let f = frame(&mut s);
    assert!(plain(&f).contains("cat cand-03.txt"), "{f:?}");
    assert!(painted_selected(&f, "cand-03.txt"), "{f:?}");

    // Right steps across the row (candidate 3 -> 6); Left back;
    // Up within the column (candidate 3 -> 2).
    s.send(RIGHT);
    let f = frame(&mut s);
    assert!(plain(&f).contains("cat cand-06.txt"), "{f:?}");
    assert!(painted_selected(&f, "cand-06.txt"), "{f:?}");
    s.send(LEFT);
    let f = frame(&mut s);
    assert!(plain(&f).contains("cat cand-03.txt"), "{f:?}");
    assert!(painted_selected(&f, "cand-03.txt"), "{f:?}");
    s.send(UP);
    let f = frame(&mut s);
    assert!(plain(&f).contains("cat cand-02.txt"), "{f:?}");
    assert!(painted_selected(&f, "cand-02.txt"), "{f:?}");

    // Esc rolls the line back to the pre-completion snapshot and no
    // candidate stays highlighted.
    s.send(ESC);
    let f = frame(&mut s);
    assert!(
        plain(&f).contains("cat cand-"),
        "Esc must restore the typed prefix:\n{f:?}"
    );
    assert!(
        !painted_selected(&f, "cand-02.txt"),
        "the menu is closed; nothing stays highlighted:\n{f:?}"
    );
}
