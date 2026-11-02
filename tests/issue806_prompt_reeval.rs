//! niubash#806 facet 2 pins: PROMPT_COMMAND executes and PS1 re-expands
//! before EVERY primary prompt (interactive PTY regression pins).
//!
//! GNU semantics being pinned (oracle: WSL GNU Bash 5.3.20, PTY-verified
//! on 2026-11-02 — transcript shapes in the PR body):
//!
//! - `PROMPT_COMMAND` runs before each primary prompt read — GNU calls
//!   `execute_prompt_command` (eval.c:305) from `parse_command`
//!   (eval.c:336, "This is where PROMPT_COMMAND is executed") in the
//!   interactive reader, i.e. before EVERY prompt, including the first
//!   prompt after the rc loads and every prompt after an empty-line
//!   Enter. A scalar runs once; a set indexed array runs each non-empty
//!   element in order (eval.c:286-307 `execute_array_command`).
//! - PS1 is re-read and re-expanded at EVERY render: `prompt_again`
//!   (parse.y:6150) feeds PS1 through `decode_prompt_string`
//!   (parse.y:6609-6622) and, under `promptvars` (the default), the
//!   promptvars expansion pass (parse.y:6683-6698,
//!   `expand_prompt_string(result, Q_DOUBLE_QUOTES, 0)`) — so a command
//!   substitution, parameter expansion, or `\w` INSIDE a single-quoted
//!   PS1 re-evaluates per prompt, while a double-quoted assignment
//!   expanded the body ONCE at assignment time and the per-render pass
//!   sees only the frozen literal.
//! - Ordering: the PROMPT_COMMAND run precedes the PS1 expansion of the
//!   same prompt (eval.c:336 sits in the reader's pre-prompt parse step),
//!   so a directory a PC `cd`s into is the directory the same prompt's
//!   PS1 renders.
//!
//! niubash's face: the REPL loop runs `run_precmd_hooks` (engine
//! `execute_prompt_command` + host hooks) then `sync_bash_prompt_from_env`
//! (engine `expand_prompt_string_mut`) before each read — these pins guard
//! that channel end-to-end through the real line editor, which is where
//! niubash#806 R3 originally observed frozen theme clocks (pre-1.5.0
//! installed build, 2026-11-01). Master behavior was re-verified green
//! against the WSL GNU 5.3.20 oracle on 2026-11-02 across nine shapes
//! (PS1 command substitution single/double-quoted, `\w`, `\t`, rc-loaded
//! PS1, an rc-defined function inside PS1, PROMPT_COMMAND scalar/array,
//! and the PC-before-PS1 ordering); these tests pin the deterministic
//! subset.
//!
//! Driving note: every setup line rides the STARTUP RC, and the only keys
//! these tests send are empty-line Enters. That keeps the input side of
//! the pins maximally hermetic (no typed text, so nothing about these
//! tests depends on the paste/typeahead input paths that niubash#806
//! facet 1 / PR #819 owns), while the state flips — the directory
//! toggles, the counters — all ride PROMPT_COMMAND, the very hook under
//! test.

#[path = "interactive/driver.rs"]
mod driver;

use driver::{require_pty_or_skip, NiuSession, DEFAULT_TIMEOUT};

/// Base rc lines shared by every test: quiet startup, no default plugin
/// packs, no sentinel PS1 (each test's rc claims PS1 itself, and the
/// first prompt must render the test's own marker).
const RC_BASE: &str = "NIU_DISABLE_DEFAULT_PLUGINS=1\n";

/// Spawn a session whose rc carries `extra` (PS1/PROMPT_COMMAND staging),
/// wait for the banner, and return the session ready for its first
/// prompt assertions. The rc-load prompt is itself the first prompt the
/// pins observe — GNU runs PROMPT_COMMAND before it like any other
/// (eval.c:336), so a PC the rc sets has already run once by then.
fn spawn_rc(prefix: &str, extra: &str) -> NiuSession {
    let rc = format!("{RC_BASE}{extra}");
    let mut session = NiuSession::spawn_custom(prefix, &rc, &[], (120, 30), DEFAULT_TIMEOUT);
    session.expect("Niubash");
    session
}

/// A single-quoted PS1 keeps the command-substitution TEXT, so the
/// per-render promptvars pass (parse.y:6683-6698) re-runs it every
/// prompt: the rendered `$(echo ${PWD##*/})` basename follows the
/// directory. The PROMPT_COMMAND toggles between two sandbox directories
/// (`home/localappdata` and `start`) before every prompt — including the
/// empty-line Enter prompts — so consecutive renders must alternate
/// basenames, which is impossible with a frozen or once-expanded PS1.
/// The toggle also demonstrates the eval.c:336 ordering: the PC's `cd`
/// lands before the PS1 of the SAME prompt renders the new basename.
#[test]
fn ps1_command_substitution_reevaluates_per_prompt() {
    if !require_pty_or_skip("issue806 ps1 comsub") {
        return;
    }
    let mut session = spawn_rc(
        "806-ps1-comsub",
        concat!(
            "PS1='[A$(echo ${PWD##*/})]$ '\n",
            "PROMPT_COMMAND='cd ../home/localappdata 2>/dev/null || cd ../../start'\n",
        ),
    );
    session.expect("[Alocalappdata]$ ");
    session.press_enter();
    session.expect("[Astart]$ ");
    session.press_enter();
    session.expect("[Alocalappdata]$ ");
}

/// The double-quoted control pins the direction of the difference: the
/// body expanded ONCE at assignment (double-quoted assignment RHS in the
/// rc), so every later render shows the frozen assignment-time basename
/// even though the PC has since changed the directory. GNU oracle: the
/// frozen directory keeps rendering after the cd, and no
/// `[Blocalappdata]$ ` render ever appears (WSL GNU 5.3.20 PTY).
#[test]
fn ps1_double_quote_control_expands_once_at_assignment() {
    if !require_pty_or_skip("issue806 ps1 double-quote control") {
        return;
    }
    let mut session = spawn_rc(
        "806-ps1-frozen",
        concat!(
            "PS1=\"[B$(echo ${PWD##*/})]$ \"\n",
            // Every prompt moves to localappdata and stays there; the
            // render must keep the assignment-time basename regardless.
            "PROMPT_COMMAND='cd ../home/localappdata'\n",
        ),
    );
    session.expect("[Bstart]$ ");
    session.press_enter();
    session.expect("[Bstart]$ ");
    session.expect_absent("[Blocalappdata]$ ", std::time::Duration::from_millis(1200));
}

/// `\w` decodes per render (decode_prompt_string, parse.y:6609+): the
/// rendered path follows the PC's directory toggles.
/// `home/localappdata` is deliberately not the HOME directory itself, so
/// both engines render its real path (GNU abbreviates only an exact HOME
/// to `~`).
#[test]
fn ps1_w_escape_tracks_directory() {
    if !require_pty_or_skip("issue806 ps1 backslash-w") {
        return;
    }
    let mut session = spawn_rc(
        "806-ps1-w",
        concat!(
            r"PS1='[W\w]$ '",
            "\n",
            "PROMPT_COMMAND='cd ../home/localappdata 2>/dev/null || cd ../../start'\n",
        ),
    );
    session.expect("localappdata]$ ");
    session.press_enter();
    session.expect("start]$ ");
    session.press_enter();
    session.expect("localappdata]$ ");
}

/// PROMPT_COMMAND executes before EVERY primary prompt — the rc-load
/// prompt (PC-1) and each empty-line Enter prompt after it (GNU
/// eval.c:336 runs it in the reader's pre-prompt step regardless of the
/// previous line's content). Deterministic counter form.
#[test]
fn prompt_command_scalar_runs_before_every_prompt() {
    if !require_pty_or_skip("issue806 prompt_command scalar") {
        return;
    }
    let mut session = spawn_rc(
        "806-pc-scalar",
        "PROMPT_COMMAND='PCN=$((PCN+1)); echo PC-$PCN'\n",
    );
    session.expect("PC-1");
    session.press_enter();
    session.expect("PC-2");
    session.press_enter();
    session.expect("PC-3");
}

/// A set indexed-array PROMPT_COMMAND runs each non-empty element in
/// order per prompt (eval.c:286-307 execute_array_command), including the
/// rc-load prompt and empty-line Enter prompts. GNU oracle: both
/// elements' lines appear before each of the prompts.
#[test]
fn prompt_command_array_runs_each_element_per_prompt() {
    if !require_pty_or_skip("issue806 prompt_command array") {
        return;
    }
    let mut session = spawn_rc(
        "806-pc-array",
        "PROMPT_COMMAND=('echo ARRAY-one' 'echo ARRAY-two')\n",
    );
    session.expect("ARRAY-one");
    session.expect("ARRAY-two");
    session.press_enter();
    session.expect("ARRAY-one");
    session.expect("ARRAY-two");
}

/// Ordering within one prompt: PROMPT_COMMAND runs BEFORE the PS1 of the
/// same prompt expands (eval.c:336 is the reader's pre-prompt step; the
/// prompt render follows), so the counter the PC bumps is the value the
/// same prompt's PS1 renders. GNU oracle: renders climb [F1]$ , [F2]$ ,
/// [F3]$ one prompt apart, never lagging one behind (WSL GNU 5.3.20 PTY).
#[test]
fn prompt_command_runs_before_ps1_expansion() {
    if !require_pty_or_skip("issue806 pc-before-ps1 ordering") {
        return;
    }
    let mut session = spawn_rc(
        "806-pc-order",
        concat!("PS1='[F$FC]$ '\n", "PROMPT_COMMAND='FC=$((FC+1))'\n",),
    );
    // The rc-load prompt: the PC already ran once, so FC is 1 — a PS1
    // expansion that ran BEFORE its prompt's PC would render [F0]$ or
    // lag one behind on every subsequent prompt.
    session.expect("[F1]$ ");
    session.press_enter();
    session.expect("[F2]$ ");
    session.press_enter();
    session.expect("[F3]$ ");
}
