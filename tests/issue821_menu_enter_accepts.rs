//! niubash#821 regression: while a completion menu with a highlighted
//! candidate is up (`select` — the default — and `cycle`; `insert`
//! already had the shape since niubash#486), Enter must ACCEPT the
//! highlighted candidate — insert/keep it in the line, close the
//! menu, stay in the editing state — and only a SECOND Enter submits.
//! The pre-fix behavior (the #631 one-press accept-and-run wrap)
//! executed the line on the first Enter, skipping the accept step a
//! zsh complist / fish user expects.
//!
//! The judgment line ("menu active WITH a selectable candidate") is
//! the engine's `MenuAccept` guard: an active menu whose candidate
//! set is non-empty and not provisional TAKES the Enter; an empty or
//! still-settling set, the GNU printed list, or no menu at all
//! DECLINES it, and the Enter submits as typed — never a dead key.
//!
//! Vehicles are builtins only (the driver sandbox carries no external
//! commands): variable candidates whose values are the execution
//! needles (`echo $v_` + TAB), and same-prefix directories for the
//! arrow-walk identity case (`cd d_` + arrows + `pwd`, the #631/#742
//! precedent — filesystem candidates sort deterministically).
//!
//! Driver: [`mod driver`] (portable-pty; see tests/interactive.rs for
//! the harness contract). Run:
//! `NIU_INTERACTIVE_TESTS=1 cargo test --test issue821_menu_enter_accepts --locked -- --nocapture`

#[path = "interactive/driver.rs"]
mod driver;

use std::time::Duration;

use driver::{require_pty_or_skip, NiuSession, DEFAULT_TIMEOUT, TAB};

const SETTLE: Duration = Duration::from_millis(400);
const ESC: &str = "\u{1b}";
const DOWN: &str = "\u{1b}[B";

/// Window in which an executed line's output must NOT appear after
/// the accepting Enter.
const ABSENT_WINDOW: Duration = Duration::from_millis(800);

/// The variable prefix shared by the variable-vehicle sessions (each
/// spawn gets its own home, so one name serves every case).
const VARS: &str = "v821";

/// The rc for the variable vehicle: three same-prefix variables with
/// one shared value, so the pin does not depend on candidate order
/// (the variable source iterates a hash map). The names' distinct
/// tails survive the stock menus' styled split of cells at the common
/// prefix (the #486 lesson: `$v821_` and `alpha` paint as separate
/// runs), so the grid-up needle stays usable under every style.
fn var_rc() -> String {
    format!(
        "PS1='P1> '\nNIU_DISABLE_DEFAULT_PLUGINS=1\n\
         {p}_alpha=M821-PICK\n{p}_beta=M821-PICK\n{p}_gamma=M821-PICK\n",
        p = VARS
    )
}

/// The acceptance-matrix core on the variable vehicle: `echo $v821_` +
/// TAB leaves the grid up with a candidate highlighted and spliced;
/// Enter #1 accepts (no value prints, the line stays editable); Enter
/// #2 submits it and the value prints.
fn enter_accepts_then_submits(
    prefix: &str,
    interaction: Option<&str>,
    navigation: &str,
) -> NiuSession {
    let env = match interaction {
        Some(value) => vec![("NIU_COMPLETION_INTERACTION".to_string(), value.to_string())],
        None => vec![],
    };
    let mut s = NiuSession::spawn_custom(prefix, &var_rc(), &env, (120, 30), DEFAULT_TIMEOUT);
    s.wait_ready();
    s.send(&format!("echo ${VARS}_"));
    s.send(TAB);
    // The grid is up: the candidate tails (longer than the typed
    // prefix) are on screen — the select/cycle TAB-#1 contract
    // (niubash#778).
    s.expect("alpha");
    if !navigation.is_empty() {
        s.send(navigation);
        std::thread::sleep(SETTLE);
    }
    // Enter #1: the accept. Nothing executes — no value may print.
    s.send("\r");
    s.expect_absent("M821-PICK", ABSENT_WINDOW);
    // Enter #2: the submit — the accepted candidate's value runs.
    s.send("\r");
    s.expect("M821-PICK");
    s.expect_prompt();
    s
}

/// The issue's repro shape, default interaction (select), first
/// candidate highlighted: Enter accepts it without running; the
/// second Enter runs it.
#[test]
fn issue821_pty_select_enter_accepts_highlighted_candidate_not_runs() {
    if !require_pty_or_skip("issue821_pty_select_enter_accepts_highlighted_candidate_not_runs") {
        return;
    }
    let mut s = enter_accepts_then_submits("enter-821-select", None, "");
    // The submitted line was the accepted candidate, still editable
    // afterwards: one more plain command round-trips.
    s.send_line("echo after-821");
    s.expect("after-821");
    s.expect_prompt();
}

/// The same two-Enter shape under `cycle`, stepped to the next
/// candidate with TAB before the accepting Enter: the cycled pick is
/// what the second Enter runs.
#[test]
fn issue821_pty_cycle_enter_accepts_the_cycled_pick() {
    if !require_pty_or_skip("issue821_pty_cycle_enter_accepts_the_cycled_pick") {
        return;
    }
    enter_accepts_then_submits("enter-821-cycle", Some("cycle"), TAB);
}

/// The acceptance matrix's arrow case (select's grid walk,
/// niubash#778), with directory candidates so the pick is
/// identifiable: three same-prefix dirs lay out as one column (row
/// per candidate), Down Down moves the highlight to the third
/// (`d821-two`: d821-one < d821-three < d821-two), Enter accepts IT,
/// and the second Enter runs it. The `&&` marker typed after the
/// accepting Enter proves the first Enter did NOT run the line: had
/// it executed, the marker line would start with `&&` (a syntax
/// error) instead of completing the accepted `cd` — and the marker
/// would never print.
#[test]
fn issue821_pty_select_arrow_to_third_then_enter_accepts_third() {
    if !require_pty_or_skip("issue821_pty_select_arrow_to_third_then_enter_accepts_third") {
        return;
    }
    let mut s = NiuSession::spawn("enter-821-arrow");
    std::fs::create_dir_all(s.start().join("d821-one")).unwrap();
    std::fs::create_dir_all(s.start().join("d821-three")).unwrap();
    std::fs::create_dir_all(s.start().join("d821-two")).unwrap();
    s.wait_ready();
    s.send("cd d821-");
    s.send(TAB);
    s.expect("d821-one");
    s.send(DOWN);
    std::thread::sleep(SETTLE);
    s.send(DOWN);
    std::thread::sleep(SETTLE);
    // Enter #1 accepts the third candidate (`d821-two`); still in
    // the editor, append the marker and submit with Enter #2.
    s.send("\r");
    s.send("&& echo ARROW-MARK");
    s.send("\r");
    s.expect("ARROW-MARK");
    s.expect_prompt();
    s.send_line("pwd");
    s.expect("/start/d821-two");
    s.expect_prompt();
}

/// The issue's insert-mode row: TAB #1 leaves the disambiguation menu
/// up when the typed text is already the common prefix; Enter commits
/// the highlighted candidate (the #486 contract), the second Enter
/// runs it.
#[test]
fn issue821_pty_insert_enter_commits_then_submits() {
    if !require_pty_or_skip("issue821_pty_insert_enter_commits_then_submits") {
        return;
    }
    enter_accepts_then_submits("enter-821-insert", Some("insert"), "");
}

/// Esc rolls the line back to the pre-completion snapshot (the T7
/// pin, unchanged); with the menu closed, ONE Enter submits the
/// restored line — the failed `cd d821-` error proves the submit and
/// that no candidate text survived the rollback.
#[test]
fn issue821_pty_esc_rolls_back_and_enter_submits_the_restored_line() {
    if !require_pty_or_skip("issue821_pty_esc_rolls_back_and_enter_submits_the_restored_line") {
        return;
    }
    let mut s = NiuSession::spawn("enter-821-esc");
    std::fs::create_dir_all(s.start().join("d821-one")).unwrap();
    std::fs::create_dir_all(s.start().join("d821-two")).unwrap();
    s.wait_ready();
    s.send("cd d821-");
    s.send(TAB);
    s.expect("d821-one");
    s.send(ESC);
    std::thread::sleep(SETTLE);
    // One Enter: menu gone, line as typed — a plain submit.
    s.send("\r");
    s.expect("No such file");
    s.expect_prompt();
    s.send_line("pwd");
    s.expect("/start");
    s.expect_prompt();
}

/// No menu open: Enter is a plain submit, byte-identical to before
/// (the "menu inactive" admission row).
#[test]
fn issue821_pty_enter_without_a_menu_submits() {
    if !require_pty_or_skip("issue821_pty_enter_without_a_menu_submits") {
        return;
    }
    let mut s = NiuSession::spawn("enter-821-plain");
    s.wait_ready();
    s.send_line("echo plain-821");
    s.expect("plain-821");
    s.expect_prompt();
}
