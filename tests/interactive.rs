#![cfg(windows)]

//! Interactive-mode regression tests.
//!
//! These exercise behavior that only exists when niu runs as an interactive
//! shell on a pseudo terminal: continuation prompts, Ctrl-C/Ctrl-D
//! interruption and EOF, live history expansion, aliases, PS1 re-rendering,
//! TAB completion, theme rendering, and input robustness. The non-interactive
//! suites (`tests/compat.rs`, `tests/host_contract.rs`, ...) cannot see any
//! of this.
//!
//! Driver: [`mod driver`] (portable-pty ConPTY, expect-style with per-call
//! timeouts). Determinism comes from the sentinel prompts `P1> `/`P2> ` pinned
//! in the sandbox rc — assertions match structural markers (echoed command,
//! output line) rather than full prompt bytes, because themed prompts carry
//! ANSI escapes.
//!
//! Coverage matrix (owner pain points first):
//! 1. Continuation — PS2 shown and completion on close for `"`, `'`, `{`,
//!    `(`, backslash-newline, heredoc; Ctrl-C abandons back to PS1.
//! 2. Termination — Ctrl-C on empty/mid-typed/mid-continuation/mid-command
//!    input; Ctrl-D on empty line/mid-line/with background jobs; Ctrl-Z gap.
//! 3. Interactive-only expansion — `!!`, `!$`, alias define/use, `set -H`
//!    flip, PS1 parameter re-render.
//! 4. Completion UX — TAB command and path completion; history persistence.
//! 5. Wizard/theme smoke — default themed prompt renders ANSI escapes.
//! 6. Robustness — input bursts, multiline paste, resize.
//!
//! Run: `NIU_INTERACTIVE_TESTS=1 cargo test --test interactive --locked -- --nocapture`
//! (without the env var the target skips on hosts with no pseudo terminal).

// Helper module lives beside the target: tests/interactive/driver.rs.
#[path = "interactive/driver.rs"]
mod driver;

use std::time::Duration;

use driver::{
    require_pty_or_skip, NiuSession, CTRL_C, CTRL_D, CTRL_D as EOF_KEY, DEFAULT_TIMEOUT,
    HEAVY_TIMEOUT, TAB,
};

/// Short window for negative ("must not run") assertions.
const ABSENT_WINDOW: Duration = Duration::from_millis(800);

// ---------------------------------------------------------------------------
// Matrix 1: continuation (PS2, completion on close, abandon on Ctrl-C)
// ---------------------------------------------------------------------------

/// Unclosed `"` keeps the session in PS2; closing it completes the command
/// (the newline inside the quotes is preserved in the output, like bash).
#[test]
fn continuation_double_quote_ps2_and_completes() {
    if !require_pty_or_skip("continuation_double_quote_ps2_and_completes") {
        return;
    }
    let rc = format!("{}\nCONT_DQUOTE_OUT=double-line\n", driver::default_rc());
    let mut s = NiuSession::spawn_custom("cont-dquote", &rc, &[], (120, 30), DEFAULT_TIMEOUT);
    s.wait_ready();
    s.send_line("printf '%s\\n' \"$CONT_DQUOTE_OUT");
    s.expect_continuation();
    s.send_line("closed\"");
    s.expect("double-line");
    s.expect("closed");
    s.expect_prompt();
}

/// Unclosed `'` keeps the session in PS2; closing it completes the command
/// (the newline inside the quotes is preserved in the output, like bash).
///
/// The closing `'` lands INSIDE the single-quoted word, so the whole word
/// `'$CONT_SQUOTE_OUT<newline>closed'` is LITERAL — single quotes quote
/// everything (GNU parse.y/make_word: no expansion inside `'…'`), so `$V`
/// prints verbatim. (The old "GNU bash 5.3, msys, oracle run 2026-10-13"
/// attribution here was the winuxcmd bash.exe shim — niu itself — so it
/// proved only that niu is self-consistent; GNU's single-quote rule is the
/// real authority.) The pre-fix expectation wanted expansion inside single
/// quotes and failed byte-stable on every host (part of niubash#420's
/// baseline five); the assertion pins the bash semantics niu implements.
#[test]
fn continuation_single_quote_ps2_and_completes() {
    if !require_pty_or_skip("continuation_single_quote_ps2_and_completes") {
        return;
    }
    let mut s = NiuSession::spawn("cont-squote");
    s.wait_ready();
    s.send_line("printf '%s\\n' '$CONT_SQUOTE_OUT");
    s.expect_continuation();
    s.send_line("closed'");
    s.expect("$CONT_SQUOTE_OUT");
    s.expect("closed");
    s.expect_prompt();
}

/// An open `{` block spans lines until `}` closes it.
#[test]
fn continuation_brace_group_completes() {
    if !require_pty_or_skip("continuation_brace_group_completes") {
        return;
    }
    let rc = format!(
        "{}\nCONT_BRACE_ONE=brace-one\nCONT_BRACE_TWO=brace-two\n",
        driver::default_rc()
    );
    let mut s = NiuSession::spawn_custom("cont-brace", &rc, &[], (120, 30), DEFAULT_TIMEOUT);
    s.wait_ready();
    s.send_line("{ printf '%s\\n' \"$CONT_BRACE_ONE\";");
    s.expect_continuation();
    s.send_line("printf '%s\\n' \"$CONT_BRACE_TWO\"; }");
    s.expect("brace-one");
    s.expect("brace-two");
    s.expect_prompt();
}

/// An open `(` subshell spans lines until `)` closes it.
///
/// The closing line once ended `");)"` — a stray `)` after the list
/// separator. The note that "GNU bash 5.3 (msys, oracle run 2026-10-13)
/// tolerates that tail" was a MIS-ORACLE: the `winuxcmd/usr/bin/bash.exe`
/// binary used as the "GNU" baseline is a launcher shim that spawns niu
/// itself, so the tolerant verdict was niu's own behavior echoed back
/// (rubash#852). The authority — WSL GNU bash 5.3.0, re-run 2026-10-19 —
/// rejects the stray tail in every mode (`-c`, script, piped `-i`, real PTY
/// `-i`: `syntax error near unexpected token ')'`, rc 2, subshell not run),
/// and the engine matches byte-for-byte. This test keeps pinning the clean
/// closing `)` both accept; the stray shape is pinned to the GNU verdict by
/// `continuation_subshell_stray_close_is_syntax_error` below and by
/// `tests/issue431_stray_close_paren.rs`.
#[test]
fn continuation_subshell_paren_completes() {
    if !require_pty_or_skip("continuation_subshell_paren_completes") {
        return;
    }
    let rc = format!(
        "{}\nCONT_PAREN_ONE=sub-one\nCONT_PAREN_TWO=sub-two\n",
        driver::default_rc()
    );
    let mut s = NiuSession::spawn_custom("cont-paren", &rc, &[], (120, 30), DEFAULT_TIMEOUT);
    s.wait_ready();
    s.send_line("(printf '%s\\n' \"$CONT_PAREN_ONE\";");
    s.expect_continuation();
    s.send_line("printf '%s\\n' \"$CONT_PAREN_TWO\")");
    s.expect("sub-one");
    s.expect("sub-two");
    s.expect_prompt();
}

/// `(printf …;` + PS2 `printf …);)` — the stray `)` after the list
/// separator — is a syntax error at an interactive prompt too (GNU eval.c
/// keeps the session alive after a parse error; only EOF ends it):
/// the diagnostic prints, the subshell body never produces output, and PS1
/// returns for the next command.
#[test]
fn continuation_subshell_stray_close_is_syntax_error() {
    if !require_pty_or_skip("continuation_subshell_stray_close_is_syntax_error") {
        return;
    }
    let rc = format!(
        "{}\nCONT_STRAY_OUT=stray-out-zznever\n",
        driver::default_rc()
    );
    let mut s = NiuSession::spawn_custom("cont-stray", &rc, &[], (120, 30), DEFAULT_TIMEOUT);
    s.wait_ready();
    s.send_line("(printf '%s\\n' \"$CONT_STRAY_OUT\";");
    s.expect_continuation();
    s.send_line("printf '%s\\n' \"$CONT_STRAY\");)");
    s.expect("syntax error");
    // The marker only exists in *output* if the subshell executed; the typed
    // text carries `$CONT_STRAY_OUT` unexpanded, so the echo can't match it.
    s.expect_absent("stray-out-zznever", ABSENT_WINDOW);
    s.expect_prompt();
    s.send_line("echo session-alive-after-stray");
    s.expect("session-alive-after-stray");
    s.expect_prompt();
}

/// A trailing backslash joins the physical lines into one logical command.
#[test]
fn continuation_backslash_newline_joins() {
    if !require_pty_or_skip("continuation_backslash_newline_joins") {
        return;
    }
    let mut s = NiuSession::spawn("cont-backslash");
    s.wait_ready();
    s.send_line("echo backslash-first\\");
    s.expect_continuation();
    s.send_line("second");
    // bash joins `first\` + `second` into the single word `firstsecond`.
    s.expect("backslash-firstsecond");
    s.expect_prompt();
}

/// A heredoc in progress keeps the PS2 until the delimiter line arrives.
#[test]
fn continuation_heredoc_gathers_until_delimiter() {
    if !require_pty_or_skip("continuation_heredoc_gathers_until_delimiter") {
        return;
    }
    let rc = format!(
        "{}\nCONT_HEREDOC_OUT=heredoc-body-line\n",
        driver::default_rc()
    );
    let mut s = NiuSession::spawn_custom("cont-heredoc", &rc, &[], (120, 30), DEFAULT_TIMEOUT);
    s.wait_ready();
    s.send_line("cat <<HD");
    s.expect_continuation();
    s.send_line("$CONT_HEREDOC_OUT");
    s.send_line("HD");
    s.expect("heredoc-body-line");
    s.expect_prompt();
}

/// Ctrl-C during a continuation abandons the whole construct back to PS1:
/// nothing executes, and the next command works.
#[test]
fn ctrl_c_during_continuation_abandons_construct() {
    if !require_pty_or_skip("ctrl_c_during_continuation_abandons_construct") {
        return;
    }
    let rc = format!("{}\nCONT_C_OUT=never-zznever\n", driver::default_rc());
    let mut s = NiuSession::spawn_custom("cont-ctrlc", &rc, &[], (120, 30), DEFAULT_TIMEOUT);
    s.wait_ready();
    eprintln!("[T] ready");
    // The marker only exists in *output* if the construct executed: the
    // typed text contains ${AB} which the executed echo would resolve.
    s.send_line("printf '%s\\n' \"$CONT_C_OUT");
    eprintln!("[T] sent line 1");
    s.expect_continuation();
    eprintln!("[T] got PS2");
    s.send(CTRL_C);
    eprintln!("[T] sent ctrl-c");
    s.expect_prompt();
    eprintln!("[T] got PS1 after ctrl-c");
    s.expect_absent("never-zznever", ABSENT_WINDOW);
    eprintln!("[T] absence checked");
    s.send_line("echo after-abandon-ok");
    eprintln!("[T] sent line 2");
    s.expect("after-abandon-ok");
    eprintln!("[T] got marker");
    s.expect_prompt();
    eprintln!("[T] done");
}

// ---------------------------------------------------------------------------
// Matrix 2: termination (Ctrl-C, Ctrl-D, jobs)
// ---------------------------------------------------------------------------

/// Ctrl-C on an empty prompt line just yields a fresh prompt.
#[test]
fn ctrl_c_on_empty_line_returns_new_prompt() {
    if !require_pty_or_skip("ctrl_c_on_empty_line_returns_new_prompt") {
        return;
    }
    let rc = format!(
        "{}\nCTRL_C_EMPTY_OUT=after-empty-ctrlc\n",
        driver::default_rc()
    );
    let mut s = NiuSession::spawn_custom("ctrlc-empty", &rc, &[], (120, 30), DEFAULT_TIMEOUT);
    s.wait_ready();
    s.send(CTRL_C);
    s.expect_prompt();
    s.send_line("echo $CTRL_C_EMPTY_OUT");
    s.expect("after-empty-ctrlc");
    s.expect_prompt();
}

/// niubash#611: Ctrl-C at the prompt must leave `$?` = 130 (GNU:
/// 128+SIGINT), not whatever status the previous command left behind.
/// Reedline swallows the ^C byte in raw mode, so no engine signal path
/// ever records it.
#[test]
fn issue611_ctrl_c_at_prompt_sets_status_130() {
    if !require_pty_or_skip("issue611_ctrl_c_at_prompt_sets_status_130") {
        return;
    }
    let mut s = NiuSession::spawn_custom(
        "ctrlc-prompt-611",
        &driver::default_rc(),
        &[],
        (120, 30),
        DEFAULT_TIMEOUT,
    );
    s.wait_ready();
    s.send_line("true");
    s.expect_prompt();
    s.send_line("echo st:$?");
    s.expect("st:0");
    s.expect_prompt();
    s.send(CTRL_C);
    s.expect_prompt();
    s.send_line("echo st:$?");
    s.expect("st:130");
    s.expect_prompt();
}

/// niubash#616: Ctrl-C at the prompt runs a trapped INT immediately
/// (GNU: the trap fires before the next prompt, observes `$?` = 130 on
/// entry, and `$?` is 130 again after it; an `exit` inside the trap
/// ends the session with the trap's status). The marker rides through
/// a variable so the typed trap-definition line cannot satisfy the
/// expectation by itself.
#[test]
fn issue616_ctrl_c_at_prompt_runs_int_trap() {
    if !require_pty_or_skip("issue616_ctrl_c_at_prompt_runs_int_trap") {
        return;
    }
    let mut s = NiuSession::spawn_custom(
        "ctrlc-prompt-trap-616",
        &driver::default_rc(),
        &[],
        (120, 30),
        DEFAULT_TIMEOUT,
    );
    s.wait_ready();
    s.send_line("M616=zap");
    s.expect_prompt();
    s.send_line("trap 'echo $M616-fired; echo entry:$?' INT");
    s.expect_prompt();
    s.send(CTRL_C);
    s.expect("zap-fired");
    s.expect("entry:130");
    s.expect_prompt();
    s.send_line("echo after:$?");
    s.expect("after:130");
    s.expect_prompt();
    // An exit inside the trap ends the session with the trap's status.
    s.send_line("trap 'exit 3' INT");
    s.expect_prompt();
    s.send(CTRL_C);
    assert_eq!(s.wait_exit(), 3);
}

/// niubash#600: Ctrl-C at the prompt echoes a literal `^C` (GNU shows
/// it via ECHOCTL/readline). Reedline's raw mode prints nothing for
/// the byte, so the host must echo it itself.
#[test]
fn issue600_ctrl_c_at_prompt_echoes_caret_c() {
    if !require_pty_or_skip("issue600_ctrl_c_at_prompt_echoes_caret_c") {
        return;
    }
    let mut s = NiuSession::spawn_custom(
        "ctrlc-caret-600",
        &driver::default_rc(),
        &[],
        (120, 30),
        DEFAULT_TIMEOUT,
    );
    s.wait_ready();
    s.send(CTRL_C);
    s.expect("^C");
    s.expect_prompt();
    s.send_line("echo still-alive-600");
    s.expect("still-alive-600");
    s.expect_prompt();
}

/// Ctrl-C with text typed discards the line: it must not execute.
#[test]
fn ctrl_c_midtyped_discards_line() {
    if !require_pty_or_skip("ctrl_c_midtyped_discards_line") {
        return;
    }
    let rc = format!(
        "{}\nCTRL_C_MID_OUT=discarded-marker\n",
        driver::default_rc()
    );
    let mut s = NiuSession::spawn_custom("ctrlc-midtyped", &rc, &[], (120, 30), DEFAULT_TIMEOUT);
    s.wait_ready();
    s.send("echo $CTRL_C_MID_OUT");
    s.send(CTRL_C);
    s.expect_prompt();
    // The typed text contains `discarded-$Q`; execution would print
    // `discarded-<value>`.
    s.expect_absent("discarded-marker", ABSENT_WINDOW);
    s.send_line("echo still-alive-ok");
    s.expect("still-alive-ok");
    s.expect_prompt();
}

/// Ctrl-C while an external command runs interrupts it and returns the
/// prompt. The observed `$?` is captured for the engine ledger (GNU bash
/// reports 130 for a SIGINT-killed command).
#[test]
fn ctrl_c_interrupts_external_command() {
    if !require_pty_or_skip("ctrl_c_interrupts_external_command") {
        return;
    }
    let mut s = NiuSession::spawn_custom(
        "ctrlc-external",
        &driver::default_rc(),
        &[],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    s.wait_ready();
    // Absolute path on purpose: rubash's bare-name exec lookup is
    // case-sensitive and this host's System32 ships PING.EXE uppercase —
    // bare `ping` (and `ping.exe`) reports "command not found" while
    // `type -a ping` resolves and the full path runs (verified 2026-10-13;
    // rubash follow-up). The absolute form keeps the test's intent: a long
    // EXTERNAL command interrupted by Ctrl-C.
    s.send_line("/c/Windows/System32/ping.exe -n 20 127.0.0.1");
    // `TTL=` appears in the first reply in every ping locale.
    s.expect("TTL=");
    s.send(CTRL_C);
    s.expect_prompt();
    s.send_line("echo st:$?");
    let window = s.expect("st:");
    eprintln!("ctrl_c_external status window: {window:?}");
    s.expect_prompt();
}

/// niubash#610: Ctrl-C interrupting a PIPELINE must end the pipeline,
/// not the session. GNU returns to the prompt with `$?` = 130, runs no
/// EXIT trap, and prints no diagnostic. The host previously classified
/// the engine's signal-discard `ExitCode(130)` unwind as a top-level
/// exit jump, so the REPL ended the session (running the EXIT trap)
/// with status 130.
#[test]
fn issue610_ctrl_c_pipeline_returns_to_prompt_with_130() {
    if !require_pty_or_skip("issue610_ctrl_c_pipeline_returns_to_prompt_with_130") {
        return;
    }
    let mut s = NiuSession::spawn_custom(
        "ctrlc-pipeline-610",
        &driver::default_rc(),
        &[],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    s.wait_ready();
    // Arm an EXIT trap whose marker only the trap's own expansion can
    // produce; if the session wrongly exits, the marker appears.
    s.send_line("X610=TRAP610");
    s.expect_prompt();
    s.send_line("trap 'echo ${X610}FIRED' EXIT");
    s.expect_prompt();
    // Two long-running externals in a pipeline (absolute System32
    // paths, same rationale as ctrl_c_interrupts_external_command).
    s.send_line(
        "/c/Windows/System32/ping.exe -n 20 127.0.0.1 | /c/Windows/System32/findstr.exe TTL",
    );
    // `TTL=` appears in the first reply in every ping locale.
    s.expect("TTL=");
    s.send(CTRL_C);
    s.expect_prompt();
    s.send_line("echo st:$?");
    s.expect("st:130");
    s.expect_prompt();
    // The discard unwind is silent: no engine diagnostic, no EXIT trap.
    s.expect_absent("discarding current command line", ABSENT_WINDOW);
    s.expect_absent("TRAP610FIRED", ABSENT_WINDOW);
    s.send_line("echo still-alive-610");
    s.expect("still-alive-610");
    s.expect_prompt();
}

/// Ctrl-C while a builtin `sleep` runs must also return the prompt.
#[test]
fn ctrl_c_interrupts_builtin_sleep() {
    if !require_pty_or_skip("ctrl_c_interrupts_builtin_sleep") {
        return;
    }
    let mut s = NiuSession::spawn_custom(
        "ctrlc-sleep",
        &driver::default_rc(),
        &[],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    s.wait_ready();
    s.send_line("sleep 20");
    std::thread::sleep(Duration::from_millis(500));
    s.send(CTRL_C);
    s.expect_prompt();
    s.send_line("echo after-sleep-ctrlc-ok");
    s.expect("after-sleep-ctrlc-ok");
    s.expect_prompt();
}

/// niubash#669: Ctrl+C during a foreground `sleep` must return the
/// prompt with `$?` = 130. On the reporter's real-machine PTY harness
/// the press never reached the shell at all (no reaction, the sleep ran
/// to completion) while GNU in the same harness settled 130 at once:
/// a Ctrl+C that reaches the console as a queued input record is never
/// translated into the CTRL_C_EVENT the host's console handler waits
/// for. The host now watches the input queue during foreground
/// execution and synthesizes the delivery (`ForegroundWatch`). Like
/// the rest of this suite it skips on hosted CI
/// (`require_pty_or_skip`); it guards desktop / real-machine runs.
#[test]
fn issue669_ctrl_c_during_sleep_returns_prompt_with_130() {
    if !require_pty_or_skip("issue669_ctrl_c_during_sleep_returns_prompt_with_130") {
        return;
    }
    let mut s = NiuSession::spawn_custom(
        "ctrlc-sleep-669",
        &driver::default_rc(),
        &[],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    s.wait_ready();
    s.send_line("sleep 30");
    std::thread::sleep(Duration::from_millis(700));
    s.send(CTRL_C);
    s.expect_prompt();
    s.send_line("echo st:$?");
    s.expect("st:130");
    s.expect_prompt();
    s.send_line("echo still-alive-669");
    s.expect("still-alive-669");
    s.expect_prompt();
}

/// Ctrl-D on an empty line exits the shell with status 0.
#[test]
fn ctrl_d_on_empty_line_exits() {
    if !require_pty_or_skip("ctrl_d_on_empty_line_exits") {
        return;
    }
    let mut s = NiuSession::spawn("ctrld-exit");
    s.wait_ready();
    s.send(CTRL_D);
    let code = s.wait_exit();
    assert_eq!(code, 0, "expected exit code 0 after Ctrl-D on empty line");
}

/// Ctrl-D with text typed must not exit; the line stays editable.
///
/// Echo-proof assertion discipline (niubash#367/#368): the expected marker
/// must not appear verbatim in the submitted input, or the terminal echo of
/// the typed text satisfies `expect` even when the completed line never
/// executes. The probe therefore types `echo half-typed`, survives the
/// Ctrl-D, extends the word in place (`-ok`), and only `half-typed-ok` —
/// a string the input never contains — proves the line both survived the
/// EOF key and executed.
#[test]
fn ctrl_d_midline_does_not_exit() {
    if !require_pty_or_skip("ctrl_d_midline_does_not_exit") {
        return;
    }
    let mut s = NiuSession::spawn("ctrld-midline");
    s.wait_ready();
    s.send("echo half-typed");
    s.send(EOF_KEY);
    std::thread::sleep(ABSENT_WINDOW);
    // The shell is still alive and the line stayed editable: extend it and
    // the completed command must run.
    s.send("-ok\r");
    s.expect("half-typed-ok");
    s.expect_prompt();
}

/// With a background job running, `jobs` lists it and Ctrl-D still exits
/// (bash only warns for *stopped* jobs — see the Ctrl-Z gap test).
#[test]
fn ctrl_d_after_background_job_exits() {
    if !require_pty_or_skip("ctrl_d_after_background_job_exits") {
        return;
    }
    let mut s = NiuSession::spawn_custom(
        "ctrld-jobs",
        &driver::default_rc(),
        &[],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    s.wait_ready();
    // Absolute path on purpose (see ctrl_c_interrupts_external_command:
    // rubash bare-name exec lookup vs PING.EXE casing on this host).
    s.send_line("/c/Windows/System32/ping.exe -n 20 127.0.0.1 &");
    s.expect_prompt();
    s.send_line("jobs");
    let window = s.expect("ping");
    eprintln!("jobs window: {window:?}");
    s.expect_prompt();
    s.send(CTRL_D);
    let code = s.wait_exit();
    assert_eq!(code, 0, "expected exit code 0 after Ctrl-D with bg job");
}

/// niubash#684 (ConPTY face, REPL route): Ctrl-D while a plain `read`
/// builtin waits on the console must be the EOF for that read — rc 1,
/// empty assignment — and must NOT exit the shell: the EOF ends only
/// the current input (GNU: kernel VEOF → zread 0 → read.def:767
/// eof=1, :949 EXECUTION_FAILURE, shell survives). The engine fix is
/// rubash-side (canonical-console VEOF translation + the
/// EOF-verdict cascade gate); this pin covers the host route the
/// engine pins cannot see: reedline hands the console back cooked for
/// the command, so the byte reaches the engine's fd-0 transports.
///
/// Echo-proofing (niubash#367/#368): the asserted marker `D0#RC=1 X=[]`
/// never appears in the submitted text (which carries `$?`/`$x`
/// literals), so terminal echo cannot satisfy it. The trailing Enter
/// completes the cooked console line the Ctrl-D sits in — the harness
/// shape pkg9 A6 used (psmux hex injection + line completion).
#[test]
fn issue684_ctrld_during_read_ends_read_not_shell() {
    if !require_pty_or_skip("issue684_ctrld_during_read_ends_read_not_shell") {
        return;
    }
    let mut s = NiuSession::spawn("ctrld-read684");
    s.wait_ready();
    s.send_line("read x; echo \"D0#RC=$? X=[$x]\"");
    std::thread::sleep(Duration::from_millis(700));
    s.send(CTRL_D);
    s.send("\r");
    s.expect("D0#RC=1 X=[]");
    s.expect_prompt();
    // The EOF ended only the read: the shell takes the next command.
    s.send_line("echo alive-684");
    s.expect("alive-684");
    s.expect_prompt();
}

/// niubash#684: Ctrl-D in the middle of a `read` line is the VEOF
/// flush, not an EOF and not data — the buffered prefix and the rest
/// of the line form one record (GNU: `abc` ^D `def` reads "abcdef",
/// rc 0).
#[test]
fn issue684_ctrld_midline_flushes_read_line() {
    if !require_pty_or_skip("issue684_ctrld_midline_flushes_read_line") {
        return;
    }
    let mut s = NiuSession::spawn("ctrld-flush684");
    s.wait_ready();
    s.send_line("read y; echo \"DM#RC=$? Y=[$y]\"");
    std::thread::sleep(Duration::from_millis(700));
    s.send("abc\u{4}def\r");
    s.expect("DM#RC=0 Y=[abcdef]");
    s.expect_prompt();
}

/// Ctrl-Z at the prompt is a documented gap: reedline's `Signal` has no
/// suspend variant and the REPL ignores unknown signals, so there is no
/// job-control suspend (and therefore no "there are stopped jobs" warning
/// path). The shell must at least stay alive and usable.
#[test]
#[ignore = "rubash#287 residual (post driver-send fix 2026-09-28): line editor swallows the line after 0x1a - the typed text plus Enter never submits; reedline keymap gap in niu, not a wedge"]
fn ctrl_z_at_prompt_is_ignored_but_shell_survives() {
    if !require_pty_or_skip("ctrl_z_at_prompt_is_ignored_but_shell_survives") {
        return;
    }
    let mut s = NiuSession::spawn("ctrlz-gap");
    s.wait_ready();
    s.send("echo suspend-tried");
    s.send("\u{1a}");
    std::thread::sleep(ABSENT_WINDOW);
    s.send("\r");
    s.expect("suspend-tried");
    s.expect_prompt();
}

// ---------------------------------------------------------------------------
// Matrix 3: interactive-only expansion
// ---------------------------------------------------------------------------

/// `!!` re-executes the previous command live.
#[test]
fn history_bang_bang_reexecutes_last_command() {
    if !require_pty_or_skip("history_bang_bang_reexecutes_last_command") {
        return;
    }
    let mut s = NiuSession::spawn("hist-bangbang");
    s.wait_ready();
    s.send_line("echo bang-target");
    s.expect("bang-target");
    s.expect_prompt();
    s.send_line("!!");
    // The expansion may echo the expanded command first; the output must
    // appear again.
    s.expect("bang-target");
    s.expect_prompt();
}

/// `!$` reuses the last argument of the previous command.
#[test]
fn history_bang_dollar_reuses_last_argument() {
    if !require_pty_or_skip("history_bang_dollar_reuses_last_argument") {
        return;
    }
    let mut s = NiuSession::spawn("hist-bangdollar");
    s.wait_ready();
    s.send_line("echo first-arg second-arg");
    s.expect("first-arg second-arg");
    s.expect_prompt();
    s.send_line("echo got:!$");
    s.expect("got:second-arg");
    s.expect_prompt();
}

/// Echo-proof assertion discipline for every Matrix-3 case (niubash#367):
/// the terminal echoes the submitted line, and `expect` matches the FIRST
/// occurrence in the stream, so an expected marker that appears verbatim in
/// the submitted input can be satisfied by that echo even when the command
/// never executed. Expect only strings that an executed command must produce
/// and that the input cannot contain — here the alias VALUE holds the marker
/// behind a `printf` format / an rc variable, and the listing is checked
/// through `type` (`is aliased to`) rather than the raw `alias ll=` text.
///
/// An alias defined in the session is used immediately.
#[test]
fn alias_define_then_use_live() {
    if !require_pty_or_skip("alias_define_then_use_live") {
        return;
    }
    let mut s = NiuSession::spawn("alias-live");
    s.wait_ready();
    s.send_line("alias ll='printf \"alias-%s\\n\" done'");
    s.expect_prompt();
    // `alias ll` would echo `alias ll=`, so the listing rides on `type ll`.
    s.send_line("type ll");
    s.expect("is aliased to");
    s.expect_prompt();
    s.send_line("ll");
    s.expect("alias-done");
    s.expect_absent("command not found", ABSENT_WINDOW);
    s.expect_prompt();
    s.send_line("alias ll='printf \"redefined-%s\\n\" done'");
    s.expect_prompt();
    s.send_line("type ll");
    s.expect("is aliased to");
    s.expect_prompt();
    s.send_line("ll");
    s.expect("redefined-done");
    s.expect_absent("command not found", ABSENT_WINDOW);
    s.expect_prompt();
}

#[test]
fn alias_quoted_and_unalias_use_function_live() {
    if !require_pty_or_skip("alias_quoted_and_unalias_use_function_live") {
        return;
    }
    let mut session = NiuSession::spawn("alias-suppression-live");
    session.wait_ready();
    session.send_line("ll() { printf \"function-%s\\n\" fallback; }");
    session.expect_prompt();
    session.send_line("alias ll='printf \"expanded-%s\\n\" alias'");
    session.expect_prompt();
    session.send_line("type ll");
    session.expect("is aliased to");
    session.expect_prompt();
    session.send_line("ll");
    session.expect("expanded-alias");
    session.expect_absent("command not found", ABSENT_WINDOW);
    session.expect_prompt();
    session.send_line("'ll'");
    session.expect("function-fallback");
    session.expect_absent("command not found", ABSENT_WINDOW);
    session.expect_prompt();
    session.send_line("unalias ll");
    session.expect_prompt();
    session.send_line("ll");
    session.expect("function-fallback");
    session.expect_absent("command not found", ABSENT_WINDOW);
    session.expect_prompt();
}

/// The issue-367 headline shape, echo-proof: `alias` at one prompt, a bare
/// use at the next. The interactive lines carry `$NIU_ALIAS_LIVE_OUT`, never
/// the value, so the echoed definition cannot satisfy the assertion — only
/// expanding and executing the alias prints `alias-live-out`.
#[test]
fn alias_use_at_next_prompt_executes_not_echo_matched() {
    if !require_pty_or_skip("alias_use_at_next_prompt_executes_not_echo_matched") {
        return;
    }
    let rc = format!(
        "{}\nNIU_ALIAS_LIVE_OUT=alias-live-out\n",
        driver::default_rc()
    );
    let mut s = NiuSession::spawn_custom("alias-live-strict", &rc, &[], (120, 30), DEFAULT_TIMEOUT);
    s.wait_ready();
    s.send_line("alias lx='echo $NIU_ALIAS_LIVE_OUT'");
    s.expect_prompt();
    s.send_line("lx");
    s.expect("alias-live-out");
    s.expect_absent("command not found", ABSENT_WINDOW);
    s.expect_prompt();
}

/// The collision is line-specific: an alias defined on physical line 1 of the
/// rc records `ALIAS_LINE=1`, and a fast-path submission also runs at line 1,
/// so it was suppressed even though no interactive definition was involved.
/// It must expand at the very first prompt (independent issue-367
/// manifestation; no session-time alias definition).
#[test]
fn rc_first_line_alias_expands_at_first_prompt() {
    if !require_pty_or_skip("rc_first_line_alias_expands_at_first_prompt") {
        return;
    }
    // The alias sits on line 1 on purpose: that is the exact numeric collision.
    let rc = concat!(
        "alias rc1='echo rc-line-one-out'\n",
        "PS1='P1> '\n",
        "PS2='P2> '\n",
        "NIU_DISABLE_DEFAULT_PLUGINS=1\n",
    );
    let mut s = NiuSession::spawn_custom("alias-rc-line-one", rc, &[], (120, 30), DEFAULT_TIMEOUT);
    s.wait_ready();
    s.send_line("rc1");
    s.expect("rc-line-one-out");
    s.expect_absent("command not found", ABSENT_WINDOW);
    s.expect_prompt();
}

/// GNU read-time rule, preserved on both sides: an alias defined on the SAME
/// submitted physical line is not visible to later tokens on that line
/// (matches `bash -i -c 'alias q=...; q'`), but the NEXT prompt is a fresh
/// read boundary and expands it. Both halves are echo-proof (the marker
/// comes from an rc variable the input never contains literally).
#[test]
fn alias_same_line_definition_is_not_visible_until_next_prompt() {
    if !require_pty_or_skip("alias_same_line_definition_is_not_visible_until_next_prompt") {
        return;
    }
    let rc = format!(
        "{}\nNIU_SAME_LINE_OUT=same-line-out\n",
        driver::default_rc()
    );
    let mut s = NiuSession::spawn_custom("alias-same-line", &rc, &[], (120, 30), DEFAULT_TIMEOUT);
    s.wait_ready();
    s.send_line("alias sl='echo $NIU_SAME_LINE_OUT'; sl");
    s.expect("command not found");
    s.expect_absent("same-line-out", ABSENT_WINDOW);
    s.expect_prompt();
    s.send_line("sl");
    s.expect("same-line-out");
    s.expect_absent("command not found", ABSENT_WINDOW);
    s.expect_prompt();
}

/// Matrix control for the value-dependence (niubash#367): an alias whose
/// value needs parser-level expansion (`;`/redirect) was exempt from the
/// same-line suppression and worked even before the fix. It must keep
/// working, so neither the fix nor a future suppression change regresses the
/// other half of the acceptance matrix.
#[test]
fn parser_level_alias_value_expands_across_prompts() {
    if !require_pty_or_skip("parser_level_alias_value_expands_across_prompts") {
        return;
    }
    let rc = format!(
        "{}\nNIU_PARSE_ALIAS_A=parser-alias-a\nNIU_PARSE_ALIAS_B=parser-alias-b\n",
        driver::default_rc()
    );
    let mut s =
        NiuSession::spawn_custom("alias-parser-value", &rc, &[], (120, 30), DEFAULT_TIMEOUT);
    s.wait_ready();
    s.send_line("alias pa='echo $NIU_PARSE_ALIAS_A; echo $NIU_PARSE_ALIAS_B'");
    s.expect_prompt();
    s.send_line("pa");
    s.expect("parser-alias-a");
    s.expect("parser-alias-b");
    s.expect_absent("command not found", ABSENT_WINDOW);
    s.expect_prompt();
}

/// `set +H` disables history expansion (`!!` becomes a literal command);
/// `set -H` re-enables it.
#[test]
#[allow(non_snake_case)]
fn set_H_flips_history_expansion() {
    if !require_pty_or_skip("set_H_flips_history_expansion") {
        return;
    }
    let mut s = NiuSession::spawn("set-H");
    s.wait_ready();
    s.send_line("echo flip-target");
    s.expect("flip-target");
    s.expect_prompt();

    s.send_line("set +H");
    s.expect_prompt();
    s.send_line("echo no-expand-!!-mark");
    s.expect("no-expand-!!-mark");
    s.expect_prompt();

    s.send_line("set -H");
    s.expect_prompt();
    s.send_line("echo re-expand");
    s.expect("re-expand");
    s.expect_prompt();
    s.send_line("!!");
    s.expect("re-expand");
    s.expect_prompt();
}

/// PS1 with a parameter re-renders on every prompt after the variable
/// changes (bash PS1 expansion is live).
#[test]
fn ps1_parameter_expansion_rerenders_per_prompt() {
    if !require_pty_or_skip("ps1_parameter_expansion_rerenders_per_prompt") {
        return;
    }
    let rc = "PS1='V[$V]> '\nPS2='P2> '\nNIU_DISABLE_DEFAULT_PLUGINS=1\n";
    let mut s = NiuSession::spawn_custom("ps1-render", rc, &[], (120, 30), driver::DEFAULT_TIMEOUT);
    s.expect("Niubash");
    s.expect("V[]> ");
    s.send_line("V=rendered");
    s.expect("V[rendered]> ");
    s.send_line("V=changed");
    s.expect("V[changed]> ");
}

// ---------------------------------------------------------------------------
// Matrix 3b: defaults-as-floor (oh-my-niu design §14.5) — the product's own
// prompt is the lowest-priority floor; an enabled external framework claims
// the slot through PS1, the floor yields without fighting, and releasing
// the claim restores the floor.
// ---------------------------------------------------------------------------

/// Nobody claims PS1 → the product floor (default template, `user@host`)
/// renders in the reedline REPL.
#[test]
fn floor_prompt_renders_when_ps1_unclaimed() {
    if !require_pty_or_skip("floor_prompt_renders_when_ps1_unclaimed") {
        return;
    }
    let rc = "export NIU_FLOOR_PROBE=1\n";
    let mut s = NiuSession::spawn_custom(
        "floor-unclaimed",
        rc,
        &[],
        (120, 30),
        driver::DEFAULT_TIMEOUT,
    );
    s.expect("Niubash");
    // Floor shape: `{user}@{host} {cwd} %#` — the `@` join is the
    // structural marker (ANSI-styled pieces make full-prompt matching
    // brittle, same discipline as the other theme tests).
    s.expect("@");
    s.send_line("echo floor-alive");
    s.expect("floor-alive");
}

/// An enabled oh-my-bash theme claims the prompt end-to-end through its
/// guarded loader: reedline renders the theme face (not the floor), the
/// claim survives prompt cycles, `unset PS1` releases the slot so the floor
/// returns, and re-claiming wins again — floor never fights back.
#[test]
fn omb_theme_claim_renders_and_floor_returns_on_release() {
    if !require_pty_or_skip("omb_theme_claim_renders_and_floor_returns_on_release") {
        return;
    }
    // Fixture oh-my-bash tree (same shape `niu plugin add --path` installs)
    // copied where the guarded loader's default lookup finds it.
    let sources_root = std::env::temp_dir().join(format!(
        "niu-floor-omb-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let omb = sources_root.join("oh-my-bash");
    copy_tree(
        &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/sources/oh-my-bash"),
        &omb,
    );
    // The rc carries the managed-block shape `niu plugin enable` writes:
    // theme line + guarded loader for the fixture tree.
    let base = "${NIU_PLUGIN_SOURCES_ROOT:-$HOME/.niubash/sources}/oh-my-bash";
    let rc = format!(
        "OSH_THEME='agnoster'\nif [ -f \"{base}/oh-my-bash.sh\" ]; then\n  OSH=\"{base}\"\n  OSH=\"${{OSH//\\\\//}}\"\n  export OSH\n  . \"$OSH/oh-my-bash.sh\"\nfi\n"
    );
    let mut s = NiuSession::spawn_custom(
        "omb-claim",
        &rc,
        &[(
            "NIU_PLUGIN_SOURCES_ROOT".to_string(),
            sources_root.to_string_lossy().into_owned(),
        )],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    s.expect("Niubash");
    // The theme's PS1 face renders through the bash-compatible channel.
    s.expect("agnoster-fixture-face");
    s.send_line("echo claim-alive");
    s.expect("claim-alive");
    s.expect("agnoster-fixture-face");

    // Release: `unset PS1` → next prompt is the product floor again.
    s.send_line("unset PS1");
    s.expect("@");

    // Re-claim in the same session still wins.
    s.send_line("PS1='reclaimed> '");
    s.expect("reclaimed> ");

    let _ = std::fs::remove_dir_all(&sources_root);
}

/// Minimal recursive copy for the fixture tree (test-local; the fixture is
/// a handful of small files).
fn copy_tree(src: &std::path::Path, dst: &std::path::Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let target = dst.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

// ---------------------------------------------------------------------------
// Matrix 4: completion UX and history persistence
// ---------------------------------------------------------------------------

/// TAB completes a command prefix from the builtin/PATH set.
///
/// niubash#373: un-ignored. The pre-fix wiring spliced nothing into the
/// line (the engine's partial completions were off) and the menu consumed
/// follow-up input; with the partial completion the typed word's candidates
/// commit their common prefix into the line and the Enter after the
/// completion submits it.
///
/// niubash#486: the open menu gets reedline's first refusal at Enter, so
/// the finishing text and the Enter are sent on opposite sides of a
/// repaint — a ` text\r` burst could reach the menu while its candidate
/// list is still stale (refreshed only at repaint) and commit instead of
/// submitting. The echo of `tab-cmd-ok` proves that repaint ran, leaving
/// the Enter a plain submit — the interactive shape a typist produces.
#[test]
fn tab_completes_command_name() {
    if !require_pty_or_skip("tab_completes_command_name") {
        return;
    }
    let mut s = NiuSession::spawn("tab-cmd");
    s.wait_ready();
    s.send("printf");
    s.send(TAB);
    // Whether the completion is inline or a menu, the buffer must end up
    // usable: finish the line and require the printf round trip.
    s.send(" tab-cmd-ok");
    s.expect("tab-cmd-ok");
    s.send("\r");
    s.expect("tab-cmd-ok");
    s.expect_prompt();
}

/// TAB completes a unique file path prefix in the working directory.
///
/// niubash#373 sub-item 2 (niubash#355 finding 4): un-ignored, and the
/// assertion is the execution-level one — the Enter after the TAB must run
/// the COMPLETED path (unique match commits outright, like bash readline);
/// if the suffix stayed a menu preview, `cat uniqfix` would fail and the
/// body could never print.
///
/// Column stop rule (owner ruling 2026-10-26, zsh/bash parity): the TAB
/// splices `uniqfix.dat` and the menu deactivates instead of lingering, so
/// the first Enter submits the completed line straight away — no commit
/// step in between (that two-Enter dance was the pre-fix shape, where the
/// menu stayed live on the re-queried set). The second Enter below is then
/// just an empty submit; the assertions (body printed, prompt back, no
/// `No such file`) hold either way.
#[test]
fn tab_completes_unique_file_path() {
    if !require_pty_or_skip("tab_completes_unique_file_path") {
        return;
    }
    let mut s = NiuSession::spawn("tab-path");
    std::fs::write(s.start().join("uniqfix.dat"), "uniq-file-body\n").unwrap();
    s.wait_ready();
    s.send("cat uniqfix");
    s.send(TAB);
    s.send("\r");
    s.send("\r");
    s.expect("uniq-file-body");
    s.expect_prompt();
    s.expect_absent("No such file", ABSENT_WINDOW);
}

/// niubash#373 F5 (the pre-fix rubash#287 shape): a TAB whose candidate set
/// is EMPTY must not wedge the editor — whatever the completion machinery
/// did (or didn't) splice, the Enter after it still submits the line.
#[test]
fn tab_completion_empty_set_enter_still_submits() {
    if !require_pty_or_skip("tab_completion_empty_set_enter_still_submits") {
        return;
    }
    let mut s = NiuSession::spawn("tab-empty");
    s.wait_ready();
    // The fresh sandbox start dir has no matching file and the word is not
    // in command position: the candidate set is empty.
    s.send("echo tab373-nomatch");
    s.send(TAB);
    s.send("\r");
    s.expect("tab373-nomatch");
    s.expect_prompt();
}

/// niubash#486 (regression vs 1.1.0): with the completion menu OPEN, Enter
/// must commit the highlighted candidate into the editable line — not
/// execute the partial line — and a second Enter submits. The #373
/// dismissal prefix made the first Enter close the menu and run `cat
/// m486-` (a "No such file" error); pinned for all four interactive menu
/// styles (`column` — the default, `list`, `ide`, `inline`).
///
/// niubash#631: this is the *insert* interaction's contract, so the
/// sessions pin `NIU_COMPLETION_INTERACTION=insert` — since 1.5.0 the
/// default interaction is cycle, whose Enter accepts AND runs in one
/// press (pinned in tests/issue631_completion_interaction.rs).
#[test]
fn completion_menu_enter_commits_highlighted_candidate() {
    if !require_pty_or_skip("completion_menu_enter_commits_highlighted_candidate") {
        return;
    }
    for style in ["column", "list", "ide", "inline"] {
        let mut s = NiuSession::spawn_custom(
            "menu-enter-486",
            &driver::default_rc(),
            &[
                ("NIU_COMPLETION_STYLE".to_string(), style.to_string()),
                (
                    "NIU_COMPLETION_INTERACTION".to_string(),
                    "insert".to_string(),
                ),
            ],
            (120, 30),
            DEFAULT_TIMEOUT,
        );
        // Two candidates sharing the typed prefix, same body in both so the
        // executed output does not depend on which candidate the menu
        // highlights first.
        std::fs::write(s.start().join("m486-one.txt"), "m486-body\n").unwrap();
        std::fs::write(s.start().join("m486-two.txt"), "m486-body\n").unwrap();
        s.wait_ready();
        s.send("cat m486-");
        s.send(TAB);
        // The menu is visibly up — its grid carries the candidates (styled
        // cells split the file names at the common prefix, so the needle is
        // the suffix). Without this the Enter below could race a
        // still-settling completion and read as flake rather than the
        // pinned commit-or-submit contract.
        s.expect("one.txt");
        // The commit Enter: the candidate lands in the line and NOTHING
        // executes — no file body, no partial-line `cat m486-` error.
        s.send("\r");
        s.expect_absent("m486-body", ABSENT_WINDOW);
        s.expect_absent("No such file", ABSENT_WINDOW);
        // The second Enter submits the committed line.
        s.send("\r");
        s.expect("m486-body");
        s.expect_prompt();
    }
}

/// The built-in default completion style is `column` (owner ruling
/// 2026-10-14, shipped since 1.4.0): with NO `NIU_COMPLETION_STYLE`
/// override anywhere, a description-less candidate set renders as a
/// multi-column grid — two candidate names share one rendered row —
/// not as a one-per-line list. The #486 pin above sets every style
/// explicitly, so the no-override default path gets its render-shape
/// pin here.
#[test]
fn default_column_style_renders_candidates_as_grid() {
    if !require_pty_or_skip("default_column_style_renders_candidates_as_grid") {
        return;
    }
    let mut s = NiuSession::spawn_custom(
        "column-grid-default",
        &driver::default_rc(),
        &[],
        (120, 30),
        DEFAULT_TIMEOUT,
    );
    // File candidates carry no descriptions, so the column menu grids
    // them; three same-prefix files in the start dir are the candidate
    // set for `cat gridpin-<TAB>`.
    for name in ["gridpin-alpha.txt", "gridpin-beta.txt", "gridpin-gamma.txt"] {
        std::fs::write(s.start().join(name), "gridpin-body\n").unwrap();
    }
    s.wait_ready();
    s.send("cat gridpin-");
    s.send(TAB);
    // The menu is up: a candidate suffix is visible (the same settle
    // signal as the #486 pin — styled cells split the names at the
    // common prefix, so the contiguous needle is the suffix).
    let mut seen = s.expect("alpha.txt");
    seen.push_str(&s.drain_screen());
    let plain = strip_ansi_escapes(&seen);
    // Menu rows are \r\n-joined inside one paint; a paint boundary can
    // only merge one paint's last row with the next paint's first, so
    // in a one-per-line layout no chunk can hold both the first and
    // the second candidate — co-occurrence on one chunk means the two
    // names were rendered side by side, i.e. a grid row.
    let grid_row = plain
        .split(['\r', '\n'])
        .find(|row| row.contains("alpha.txt") && row.contains("beta.txt"));
    assert!(
        grid_row.is_some(),
        "default (column) menu must render two candidates on one row; \
         stripped output:\n{plain}"
    );
}

/// Minimal ANSI CSI stripper for screen-shape assertions: drops
/// `ESC [ params final-byte` sequences so rendered menu rows can be
/// compared as plain text.
fn strip_ansi_escapes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if let Some('[') = chars.next() {
                for inner in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&inner) {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The session history survives into `~/.niubash_history` and `history`
/// shows it live.
#[test]
fn history_file_persists_across_sessions() {
    if !require_pty_or_skip("history_file_persists_across_sessions") {
        return;
    }
    let mut s = NiuSession::spawn("hist-persist");
    s.wait_ready();
    s.send_line("echo session-marker-one");
    s.expect("session-marker-one");
    s.expect_prompt();
    s.send_line("history");
    s.expect("session-marker-one");
    s.expect_prompt();
    s.send_line("exit");
    let code = s.wait_exit();
    assert_eq!(code, 0);
    let persisted = std::fs::read_to_string(s.home().join(".niubash_history")).unwrap_or_default();
    assert!(
        persisted.contains("session-marker-one"),
        "history file missing session marker; content: {persisted:?}"
    );
}

/// `set +o history` stops recording new entries into the history file.
/// (The recording veto was wired by niubash#660: the editor history
/// instance consults the `history` option snapshot the host refreshes
/// at every prompt boundary.)
#[test]
fn set_plus_o_history_stops_recording() {
    if !require_pty_or_skip("set_plus_o_history_stops_recording") {
        return;
    }
    let mut s = NiuSession::spawn("hist-off");
    s.wait_ready();
    s.send_line("set +o history");
    s.expect_prompt();
    s.send_line("echo secret-not-recorded");
    s.expect("secret-not-recorded");
    s.expect_prompt();
    s.send_line("exit");
    let code = s.wait_exit();
    assert_eq!(code, 0);
    let persisted = std::fs::read_to_string(s.home().join(".niubash_history")).unwrap_or_default();
    assert!(
        !persisted.contains("secret-not-recorded"),
        "command recorded despite `set +o history`; content: {persisted:?}"
    );
}

// ---------------------------------------------------------------------------
// Matrix 5: wizard/theme smoke
// ---------------------------------------------------------------------------

/// Without a PS1 override the default themed prompt renders, carries ANSI
/// escape sequences, and the shell round-trips a command.
#[test]
fn default_theme_prompt_renders_with_ansi_escapes() {
    if !require_pty_or_skip("default_theme_prompt_renders_with_ansi_escapes") {
        return;
    }
    // No PS1 in the rc: the built-in themed prompt is used.
    let rc = "NIU_DISABLE_DEFAULT_PLUGINS=1\n";
    let mut s = NiuSession::spawn_custom("theme-ansi", rc, &[], (120, 30), driver::DEFAULT_TIMEOUT);
    s.expect("Niubash");
    std::thread::sleep(Duration::from_millis(500));
    s.send_line("echo theme-roundtrip");
    s.expect("theme-roundtrip");
    s.expect_absent("command not found", ABSENT_WINDOW);
    let screen = s.drain_screen();
    assert!(
        screen.contains("\x1b["),
        "no ANSI escapes in the rendered prompt; screen: {screen:?}"
    );
}

// ---------------------------------------------------------------------------
// Matrix 6: robustness
// ---------------------------------------------------------------------------

/// niubash#167: keystrokes typed while the prompt rebuilds must survive
/// into the edit line.
///
/// Between two prompts the console sits in the cooked baseline while
/// `PROMPT_COMMAND` machinery runs children that inherit the console;
/// some runtimes (observed: Git-for-Windows MSYS2 `git`/`awk`/`grep`/
/// `date` under a themed prompt) probe the shared console input buffer
/// and consume one pending key record — the PRESS of the first key typed
/// during the window, so `echo` executed as `cho` (wt67 drv-run1,
/// 11/11 resends). The typeahead guard sweeps the queue for the shell
/// during the rebuild and reinjects it before the editor reads.
///
/// The eater models the probe deterministically: PowerShell waits, then
/// reads ONE console key only when one is pending, without blocking.
/// Where PowerShell is unavailable the eater silently no-ops and this
/// test degrades to a plain pass (it can never fail spuriously).
#[test]
fn typeahead_survives_prompt_rebuild_window() {
    if !require_pty_or_skip("typeahead_survives_prompt_rebuild_window") {
        return;
    }
    let rc = concat!(
        "PS1='P1> '\n",
        "NIU_DISABLE_DEFAULT_PLUGINS=1\n",
        "__eat(){ C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe",
        " -NoProfile -Command 'Start-Sleep -m 500;",
        " if([Console]::KeyAvailable){[void][Console]::ReadKey($true)}'",
        " >/dev/null 2>&1; }\n",
        "__p(){ __eat; PS1='P1> '; }\n",
        "PROMPT_COMMAND=__p\n",
    );
    let mut s = NiuSession::spawn_custom("wt167-typeahead", rc, &[], (120, 30), HEAVY_TIMEOUT);
    s.wait_ready();
    // One sacrificial cycle so the first PROMPT_COMMAND (eater) runs.
    s.send_line("true");
    s.expect_prompt();
    for marker in ["W167A", "W167B"] {
        // Submit a command, then type the next line while the rebuild
        // window (the eater child) is still running: without the guard
        // the first byte of `echo` is the eaten key and the line runs as
        // `cho <marker>`.
        s.send("true\r");
        std::thread::sleep(Duration::from_millis(450));
        s.send_line(&format!("echo {marker}"));
        s.expect(marker);
        s.expect_prompt();
    }
}

/// A rapid burst of input lines lands as ONE quiet paste chunk and executes
/// completely, in order, on the user's single submit Enter.
///
/// The burst arrives as one ConPTY write with CRLF line breaks, so every
/// break carries its LF partner (`Enter + CONTROL`) — the structural paste
/// signature (niubash#252/#284). The chunk must not repaint or execute per
/// line (the pre-#252 Windows behavior executed 186 of 200 pasted lines
/// mid-paste, niubash#202); nothing runs until the submitting Enter, then
/// every line runs in order.
///
/// The markers are rc variables on purpose: the echoed edit buffer shows
/// literal `echo $RB01` text after the chunk flush, so only the EXECUTED
/// output (`burst-01-OUT`) can satisfy the assertions — mid-paste
/// execution and lost submissions both fail the test.
#[test]
fn rapid_input_burst_all_lines_execute() {
    if !require_pty_or_skip("rapid_input_burst_all_lines_execute") {
        return;
    }
    let vars: String = (1..=12)
        .map(|i| format!("RB{i:02}=burst-{i:02}-OUT\n"))
        .collect();
    let rc = format!("{}\n{}", driver::default_rc(), vars);
    let mut s = NiuSession::spawn_custom("burst", &rc, &[], (120, 30), HEAVY_TIMEOUT);
    s.wait_ready();
    let burst: String = (1..=12).map(|i| format!("echo $RB{i:02}\r\n")).collect();
    s.send(&burst);
    // The whole burst buffers as one paste chunk: no line executes while
    // it is absorbed.
    s.expect_absent("burst-01-OUT", ABSENT_WINDOW);
    // The user's own Enter submits the buffered script: every line runs,
    // in order.
    s.send("\r");
    for i in 1..=12 {
        s.expect(&format!("burst-{i:02}-OUT"));
    }
    s.expect_prompt();
}

/// niubash#202: a bracketed multiline paste must land in the edit buffer as
/// ONE unit (GNU bash paste semantics) — nothing executes until the user
/// submits the buffer, no matter how many newlines the chunk carries.
///
/// The output markers only appear when a line EXECUTES (`echo $V1` renders
/// as literal `echo $V1` inside the edit buffer, so the echoed buffer text
/// cannot satisfy the assertion — only the executed output can).
///
/// Non-Windows only: crossterm has no ANSI input parser on Windows, so
/// ConPTY consumes the ESC[200~/ESC[201~ markers before they reach the
/// shell (see the builder comment in `build_line_editor`); there a
/// terminal paste is carried by its CRLF signature instead, as pinned by
/// [`multiline_raw_paste_windows_crlf_buffers_and_cr_executes_per_line`].
#[test]
fn multiline_bracketed_paste_buffers_whole_chunk() {
    if cfg!(windows) {
        // Platform limitation, not a regression: markers never reach the
        // parser (crossterm-rs/crossterm#737).
        return;
    }
    if !require_pty_or_skip("multiline_bracketed_paste_buffers_whole_chunk") {
        return;
    }
    let rc = format!("{}\nV1=PX1_DONE\nV2=PX2_DONE\n", driver::default_rc());
    let mut s = NiuSession::spawn_custom("paste-bracketed", &rc, &[], (120, 30), HEAVY_TIMEOUT);
    s.wait_ready();
    // One bracketed paste chunk, two commands, internal newlines, no
    // trailing Enter inside the markers.
    s.send("\u{1b}[200~echo $V1\r\necho $V2\u{1b}[201~");
    // While the chunk sits in the edit buffer nothing may have executed.
    s.expect_absent("PX1_DONE", ABSENT_WINDOW);
    s.expect_absent("PX2_DONE", ABSENT_WINDOW);
    // Submitting the buffered chunk runs it as one multiline script.
    s.send("\r");
    s.expect("PX1_DONE");
    s.expect("PX2_DONE");
    s.expect_prompt();
}

/// Windows raw-input semantics after niubash#252/#284: ConPTY strips the
/// bracketed-paste markers, so a feed's intent is carried by its line
/// breaks. A CRLF-separated feed (what a terminal paste from clipboard
/// text looks like) carries the LF partner of every break — the structural
/// paste signature — and buffers as ONE quiet chunk; nothing executes
/// until the user's Enter, then the whole buffer runs. A bare-CR-separated
/// feed (typeahead: expect scripts, send-keys, a driver's per-line
/// send_line) has no LF partners behind its Enters, so every line submits
/// as it arrives — GNU typeahead semantics, the same as typing each line.
///
/// The CRLF markers are rc variables on purpose: only EXECUTED output can
/// produce them, never the echoed edit buffer. The bare-CR half pins the
/// degraded per-line behavior so a future crossterm input parser cannot
/// silently change it without notice.
#[test]
fn multiline_raw_paste_windows_crlf_buffers_and_cr_executes_per_line() {
    if !cfg!(windows) {
        return;
    }
    if !require_pty_or_skip("multiline_raw_paste_windows_crlf_buffers_and_cr_executes_per_line") {
        return;
    }
    let rc = format!(
        "{}\nPF1=RAWPASTE-ONE-DONE\nPF2=RAWPASTE-TWO-DONE\n",
        driver::default_rc()
    );
    let mut s = NiuSession::spawn_custom("paste-raw-win", &rc, &[], (120, 30), HEAVY_TIMEOUT);
    s.wait_ready();
    // CRLF-separated: the paste shape. Nothing executes while the chunk
    // buffers; one Enter runs the whole buffered script.
    s.send("echo $PF1\r\necho $PF2\r\n");
    s.expect_absent("RAWPASTE-ONE-DONE", ABSENT_WINDOW);
    s.expect_absent("RAWPASTE-TWO-DONE", ABSENT_WINDOW);
    s.send("\r");
    s.expect("RAWPASTE-ONE-DONE");
    s.expect("RAWPASTE-TWO-DONE");
    s.expect_prompt();
    // Bare-CR-separated: the typeahead shape. Each line submits as it
    // arrives, like the pre-#252 per-line fallback.
    s.send("if true; then\recho rawpaste-branch\rfi\r");
    s.expect("rawpaste-branch");
    s.expect_prompt();
}

/// niubash#284, the regression itself: the first Enter of every
/// `send_line` — the "text + Enter within one burst" shape every ConPTY
/// driver, expect script, `send-keys` automation and fast human typist
/// feeds — must submit with no second Enter. The back-to-back half is the
/// typeahead matrix: three lines written with no prompt waits in between;
/// each is one bare-CR write, so none carries a paste signature and each
/// submits and executes completely, in order.
#[test]
fn send_line_first_enter_submits_and_typeahead_executes() {
    if !require_pty_or_skip("send_line_first_enter_submits_and_typeahead_executes") {
        return;
    }
    let mut s = NiuSession::spawn("paste-enter-284");
    s.wait_ready();
    s.send_line("echo probe284-first");
    s.expect("probe284-first");
    s.expect_prompt();
    // Back-to-back lines: classic typeahead, each a separate pty write of
    // text + bare CR. The Enter of line N is followed in the queue by
    // line N+1's TEXT (not an LF partner), so it must submit.
    s.send_line("echo typeahead-one");
    s.send_line("echo typeahead-two");
    s.send_line("echo typeahead-three");
    s.expect("typeahead-one");
    s.expect("typeahead-two");
    s.expect("typeahead-three");
    s.expect_prompt();
}

/// niubash#202/#252/#284 Windows paste contract at issue scale: a long
/// CRLF paste (the issue author pasted 257 lines) must not execute a
/// single line mid-paste and must run the whole buffered script on the
/// user's one Enter. The in-suite load is 60 lines of rc-variable echoes;
/// the executed-output markers can only come from execution, never from
/// the echoed edit buffer.
#[test]
fn multiline_crlf_paste_buffers_whole_chunk_and_executes_once() {
    if !cfg!(windows) {
        // On Unix the bracketed-paste test above covers the buffering
        // semantics through the real Event::Paste path.
        return;
    }
    if !require_pty_or_skip("multiline_crlf_paste_buffers_whole_chunk_and_executes_once") {
        return;
    }
    let vars: String = (1..=60)
        .map(|i| format!("PB{i:02}=ISSUESCALE-{i:02}-DONE\n"))
        .collect();
    let rc = format!("{}\n{}", driver::default_rc(), vars);
    let mut s = NiuSession::spawn_custom("paste-scale", &rc, &[], (120, 30), HEAVY_TIMEOUT);
    s.wait_ready();
    let paste: String = (1..=60).map(|i| format!("echo $PB{i:02}\r\n")).collect();
    s.send(&paste);
    // Not one line may execute while the paste is absorbed.
    s.expect_absent("ISSUESCALE-01-DONE", ABSENT_WINDOW);
    s.expect_absent("ISSUESCALE-30-DONE", ABSENT_WINDOW);
    s.expect_absent("ISSUESCALE-60-DONE", ABSENT_WINDOW);
    // The user's single submit runs everything, in paste order.
    s.send("\r");
    for i in 1..=60 {
        s.expect(&format!("ISSUESCALE-{i:02}-DONE"));
    }
    s.expect_prompt();
}

/// niubash#308: the paste and its submit Enter land in ONE burst —
/// `send(paste_crlf + "\r")`, a single write, the Enter <50 ms after the
/// last paste key. The trailing bare CR carries no LF partner, so it is the
/// submit, not paste data: the whole buffered chunk must execute on that
/// FIRST Enter. The pre-#308 behavior pushed the Enter into the pending
/// chunk as a newline — nothing executed and a second Enter was needed.
///
/// Windows-only: the burst window is the Windows paste-chunk heuristic; on
/// Unix the same write executes per line through the tty's CR translation,
/// which is the ordinary non-paste path (the bracketed-paste test above
/// covers Unix paste buffering).
#[test]
fn paste_with_same_burst_enter_submits_on_first_enter() {
    if !cfg!(windows) {
        return;
    }
    if !require_pty_or_skip("paste_with_same_burst_enter_submits_on_first_enter") {
        return;
    }
    // Output markers are rc variables: only EXECUTED output can produce
    // them, never the echoed edit buffer.
    let rc = format!(
        "{}\nTS1=BURSTENTER-ONE-DONE\nTS2=BURSTENTER-TWO-DONE\n",
        driver::default_rc()
    );
    let mut s =
        NiuSession::spawn_custom("paste-burst-enter-308", &rc, &[], (120, 30), HEAVY_TIMEOUT);
    s.wait_ready();
    // One write — paste body plus the submit Enter, no gap, no second send.
    s.send("echo $TS1\r\necho $TS2\r\n\r");
    // The first Enter executes the whole chunk, in paste order, and the
    // session lands on a fresh prompt (a swallowed Enter would leave no
    // output and no new prompt, failing these expects on timeout).
    s.expect("BURSTENTER-ONE-DONE");
    s.expect("BURSTENTER-TWO-DONE");
    s.expect_prompt();
}

/// The owner's niubash#341 pasted script (2026-10-14), sent with the
/// Windows Terminal KEY_EVENT line-break shape: a bare CR per line break,
/// `\r\r` for empty lines, no LF anywhere — the record shape a real WT
/// paste synthesizes behind ConPTY (record-level evidence and method:
/// `tests/keyevent_record_probe.rs`; the CRLF feeds earlier paste
/// regressions use carry an additional VK_CONTROL sandwich plus the Ctrl+J
/// partner, the shape #252 modeled, which is why four rounds of
/// CRLF-shaped fixes never saw this).
///
/// HONEST SCOPE: this test is a REGRESSION GUARD, not the reproduction —
/// it also PASSES on unpatched master. The live channel's delivery (one
/// big ConPTY write, plus the TypeaheadGuard's sweep/reinjection cycles
/// around every prompt rebuild splicing the queue) does not reproduce the
/// probe-pushback/batch-boundary interleaving the corruption needs. The
/// deterministic RED/GREEN reproduction lives at the protocol layer:
/// `wt_bare_cr_paste_survives_reedline_batch_protocol_341` and the slice
/// sweeps in `crates/niubash-runtime/src/repl.rs` go red on 6332f99 and
/// pin the mechanism byte for byte.
///
/// What this guard pins: the full script runs end to end over the
/// KEY_EVENT shape, and the two `if is_even` branch outputs
/// (`10 是偶数，函数返回成功` / `7 不是偶数，函数返回失败`) — the exact
/// lines the owner saw shredded — keep printing. Markers are the script's
/// printed OUTPUTS, never echoed-input substrings. Expected outputs pinned
/// from GNU bash running the same script: identical except the `$RANDOM`
/// section (nondeterministic) and the `原串` line (a pre-existing
/// non-interactive engine quirk, unrelated to paste).
#[test]
fn wt_paste_bare_cr_key_event_shape_runs_owner_script_341() {
    if !cfg!(windows) {
        return;
    }
    if !require_pty_or_skip("wt_paste_bare_cr_key_event_shape_runs_owner_script_341") {
        return;
    }
    let mut s = NiuSession::spawn_custom(
        "paste-wt-keyevent-341",
        &driver::default_rc(),
        &[],
        (120, 40),
        HEAVY_TIMEOUT,
    );
    s.wait_ready();
    // One write, as the earlier paste regressions feed. (Chunked writes
    // were tried and are WORSE here: this dev box routinely carries a
    // dozen-plus live interactive niu sessions, and the extra batch
    // boundaries multiply exposure to that host noise — the same family
    // as the continuation/ctrl_d cases this host fails on master too.
    // CI's clean runner is the gate for this guard.)
    let paste = OWNER_SCRIPT_341.replace('\n', "\r");
    s.send(&paste);
    // Execution-level markers in paste order: printed output lines of a
    // correct run, never echoed-input substrings.
    s.expect("===== Bash 非交互测试脚本 =====");
    s.expect("a=17 b=5");
    s.expect("a/b=3 a%b=2");
    s.expect("水果数量: 5");
    s.expect("中国 -> 北京");
    s.expect("7! = 5040");
    s.expect("fib(7)=13");
    s.expect("4 是偶数");
    s.expect("替换: Hello Shell Script World");
    s.expect("mapfile 读取到 4 行:");
    s.expect("<第二行>");
    // The corruption witnesses.
    s.expect("10 是偶数，函数返回成功");
    s.expect("7 不是偶数，函数返回失败");
    s.expect("===== 测试结束 =====");
    s.expect_prompt();
}

/// The WT KEY_EVENT shape at #284's canonical typing shape: a bare-CR line
/// with NOTHING queued behind it is still a typed submit — the CR-fold must
/// not delay a driver's `send_line` or a fast typist.
#[test]
fn wt_paste_bare_cr_single_line_still_submits_341() {
    if !cfg!(windows) {
        return;
    }
    if !require_pty_or_skip("wt_paste_bare_cr_single_line_still_submits_341") {
        return;
    }
    let rc = format!("{}\nWT341=KEYEVENT-OK\n", driver::default_rc());
    let mut s =
        NiuSession::spawn_custom("paste-wt-single-341", &rc, &[], (120, 30), DEFAULT_TIMEOUT);
    s.wait_ready();
    s.send("echo $WT341\r");
    s.expect("KEYEVENT-OK");
    s.expect_prompt();
}

/// niubash#420, the minimal wedge: a typeahead burst ending in a bare
/// `\r\r` — "submit the line, submit the empty line" in GNU typeahead
/// semantics (two send_line Enters written in one pty write; an expect
/// script's back-to-back Enters). The bytes are identical to the WT
/// KEY_EVENT paste's empty line, and the pre-fix Enter probe folded the
/// pair as paste data: the line echoed but parked in the PS2 continuation
/// buffer forever — the shared-driver wedge of PR #418's Ctrl+R E2E. The
/// probe-consumed partner CR now goes back to the head of the input stream
/// and the line submits on the FIRST CR. Echo-proof (#367): the marker is
/// an rc variable's value — only execution can produce it.
#[test]
fn typeahead_bare_cr_pair_submits_per_line_420() {
    if !require_pty_or_skip("typeahead_bare_cr_pair_submits_per_line_420") {
        return;
    }
    let rc = format!("{}\nW420A=CR-PAIR-OUT\n", driver::default_rc());
    let mut s = NiuSession::spawn_custom("w420-cr-pair", &rc, &[], (120, 30), DEFAULT_TIMEOUT);
    s.wait_ready();
    s.send("echo $W420A\r\r");
    s.expect("CR-PAIR-OUT");
    s.expect_prompt();
}

/// niubash#420, the PR #418 shape that found the wedge: the Ctrl+R
/// reverse-search accept-Enter and the submit-Enter land as ONE burst
/// (`\r\r` after the query). The accepting Enter must LOAD the match and
/// the requeued second Enter must SUBMIT it — no second send, no pacing.
/// Echo-proof: the seed line echoes an rc variable, so the search UI's
/// history preview (command text) can never satisfy the assertion; only
/// the executed output can, and the first occurrence is consumed before
/// the search begins.
#[test]
fn ctrl_r_accept_submit_in_one_burst_420() {
    if !require_pty_or_skip("ctrl_r_accept_submit_in_one_burst_420") {
        return;
    }
    let rc = format!("{}\nW420SEED=CR-SEED-OUT\n", driver::default_rc());
    let mut s = NiuSession::spawn_custom("w420-ctrlr", &rc, &[], (120, 30), DEFAULT_TIMEOUT);
    s.wait_ready();
    s.send_line("echo $W420SEED");
    s.expect("CR-SEED-OUT");
    s.expect_prompt();
    s.send("\u{12}");
    std::thread::sleep(Duration::from_millis(120));
    s.send("SEED");
    std::thread::sleep(Duration::from_millis(120));
    s.send("\r\r");
    s.expect("CR-SEED-OUT");
    s.expect_prompt();
}

/// niubash#417, the wedge this fix closes: the Enter signature probe reads
/// the queue's next record to decide a bare Enter, and with typeahead
/// behind it (the next line of a fast burst) that record used to PARK
/// in-process while the accepted command ran — the command's engine `read`
/// then saw an EMPTY kernel queue and blocked forever. The submit now
/// writes the parked record back to the console queue's head before the
/// command runs. Echo-proof (#367): the marker is the executed echo's
/// output — the echoed input line carries the unexpanded `$R417`, so only
/// a read that actually consumed the parked 'Z' can print it.
#[test]
fn probe_parked_typeahead_reaches_engine_read_417() {
    if !require_pty_or_skip("probe_parked_typeahead_reaches_engine_read_417") {
        return;
    }
    let mut s = NiuSession::spawn("w417-read");
    s.wait_ready();
    // ONE write: the submit Enter with its typeahead 'Z' immediately
    // behind it — the probe must consume 'Z' to see it is not an LF
    // partner, parking it; `read -n1` runs next against a kernel queue the
    // fix has just refilled.
    s.send("read -n1 R417; echo \"R417=[$R417]\"\rZ");
    s.expect("R417=[Z]");
    s.expect_prompt();
}

/// niubash#417, the typeahead-burst form (the #420 pair test's shape, one
/// line further): three back-to-back lines in one write. Line 2's submit
/// probe parks the 'W' of line 3; the accepted `read -n1` must get exactly
/// that key — and the head writeback must NOT let line 2's own text run
/// before it (a tail append would shred the burst into `ead -n1 ...`).
#[test]
fn typeahead_burst_read_line_gets_parked_key_417() {
    if !require_pty_or_skip("typeahead_burst_read_line_gets_parked_key_417") {
        return;
    }
    let mut s = NiuSession::spawn("w417-burst");
    s.wait_ready();
    s.send("echo ONE417\rread -n1 R417B; echo \"B=[$R417B]\"\rW");
    s.expect("ONE417");
    s.expect("B=[W]");
    s.expect_prompt();
}

/// niubash#434, the discriminating probe: a parked CONTROL chord must ride
/// the #417 submit writeback as the conhost control-code record — uChar
/// 0x12 for Ctrl+R — because the accepted command's engine `read` is a
/// raw-byte reader: it gets the re-injected record's uChar as-is. The
/// pre-fix synthesis keyed its control arm on the control-code char
/// (Char('\u{12}')), but crossterm 0.29 delivers the parked chord as
/// Char(letter) + CONTROL, so on Windows the arm was dead code and the
/// writeback carried uChar 'r' — `read -n1` saw 'r' where the user's
/// keystroke was 0x12. Echo-proof (#367, the #417 shape): the marker is
/// the read variable's value through an rc variable — the echoed input
/// line carries neither the marker text nor the comparison byte, so only
/// a read that consumed exactly 0x12 can print it; a read of 'r' prints
/// the diagnostic leg instead and the marker never appears.
#[test]
fn parked_ctrl_r_writeback_reaches_engine_read_as_control_byte_434() {
    if !require_pty_or_skip("parked_ctrl_r_writeback_reaches_engine_read_as_control_byte_434") {
        return;
    }
    let rc = format!("{}\nW434OK=CTRL12-BYTE\n", driver::default_rc());
    let mut s = NiuSession::spawn_custom("w434-read", &rc, &[], (120, 30), DEFAULT_TIMEOUT);
    s.wait_ready();
    // ONE write: the submit Enter with the Ctrl+R byte (0x12) immediately
    // behind it. ConPTY synthesizes the control-code record (the channel
    // that carries CTRL_C/CTRL_D), crossterm hands the probe
    // Char('r') + CONTROL, the probe parks it, and the accepted `read -n1`
    // reads whatever the writeback re-injected.
    s.send(
        "read -n1 R434; if [ \"$R434\" = $'\\x12' ]; then echo \"R434=$W434OK\"; \
         else echo \"R434-WRONG[$R434]\"; fi\r\u{12}",
    );
    s.expect("R434=CTRL12-BYTE");
    s.expect_prompt();
}

/// niubash#434, the event-level round trip through the REAL crossterm
/// parser: the chord parked behind a submit must come back out of the
/// writeback as an event reedline still reads as Ctrl+R — reverse history
/// search opens at the next prompt, exactly like a freshly typed chord.
/// (The byte-level face of the same writeback is
/// `parked_ctrl_r_writeback_reaches_engine_read_as_control_byte_434`.)
/// Echo-proof (#367, the #420 shape): the marker is an rc variable's
/// value — the search UI's preview paints the history line's unexpanded
/// `$W434SEED` text, so only execution can produce `REPLAY-OUT`.
#[test]
fn parked_ctrl_r_writeback_replays_as_history_search_434() {
    if !require_pty_or_skip("parked_ctrl_r_writeback_replays_as_history_search_434") {
        return;
    }
    let rc = format!("{}\nW434SEED=REPLAY-OUT\n", driver::default_rc());
    let mut s = NiuSession::spawn_custom("w434-replay", &rc, &[], (120, 30), DEFAULT_TIMEOUT);
    s.wait_ready();
    s.send_line("echo $W434SEED");
    s.expect("REPLAY-OUT");
    s.expect_prompt();
    // ONE write: the submit Enter with the Ctrl+R byte behind it — the
    // #420 one-burst shape one line further. The probe parks the chord;
    // the accepted echo's writeback must requeue it at the stream's head.
    s.send("echo TAIL434\r\u{12}");
    s.expect("TAIL434");
    // The next prompt consumes the re-injected chord (kernel-queue head)
    // as Ctrl+R; the query then narrows the search to the seed line.
    s.send("SEED");
    s.send("\r\r");
    s.expect("REPLAY-OUT");
    s.expect_prompt();
}

/// The niubash#341 owner script (issue #341, Windows Terminal,
/// 2026-10-14): 76 non-empty lines of bash with 16 empty-line boundaries,
/// including the two `if is_even` blocks whose line starts were shredded.
/// One deviation, documented: section 3's multiline
/// `declare -A capital=(...)` literal is rewritten to the output-identical
/// declare-then-assign form, because ANY `declare -A name=(...)` through
/// the line-editor path panics a debug-build child at rubash
/// `locale.rs`'s COMPOUND_ASSIGNMENT_MARKER debug assertion — a
/// pre-existing engine bug independent of this fix (a 3-line TYPED repro,
/// no paste involved, panics an unpatched master build the same way; the
/// owner's release build never trips `debug_assert!`).
const OWNER_SCRIPT_341: &str = r###"#!/usr/bin/env bash
set -euo pipefail

echo "===== Bash 非交互测试脚本 ====="

# 1. 变量和算术
a=17
b=5
echo "a=$a b=$b"
echo "a+b=$((a + b))"
echo "a-b=$((a - b))"
echo "a*b=$((a * b))"
echo "a/b=$((a / b)) a%b=$((a % b))"

# 2. 普通数组
fruits=(苹果 香蕉 橙子 葡萄 西瓜)
echo "水果数量: ${#fruits[@]}"
for i in "${!fruits[@]}"; do
  printf "  %d: %s\n" "$((i + 1))" "${fruits[$i]}"
done

# 3. 关联数组
declare -A capital
capital[中国]="北京"
capital[日本]="东京"
capital[法国]="巴黎"
capital[德国]="柏林"
for country in "${!capital[@]}"; do
  echo "  $country -> ${capital[$country]}"
done

# 4. 函数：阶乘
fact() {
  local n=$1
  local r=1
  local i
  for ((i = 2; i <= n; i++)); do
    r=$((r * i))
  done
  echo "$r"
}
for n in 3 5 7; do
  echo "$n! = $(fact "$n")"
done

# 5. 函数：斐波那契
fib() {
  local n=$1
  if ((n <= 1)); then
    echo "$n"
    return
  fi
  echo $(( $(fib $((n - 1))) + $(fib $((n - 2))) ))
}
for n in {0..7}; do
  printf "fib(%d)=%s " "$n" "$(fib "$n")"
done
echo

# 6. 条件和 case
check_number() {
  local n=$1
  if ((n % 2 == 0)); then
    echo "$n 是偶数"
  else
    echo "$n 是奇数"
  fi

  case $n in
    0) echo "  -> 零" ;;
    1|2|3) echo "  -> 小数字" ;;
    *) echo "  -> 其他数字" ;;
  esac
}
for n in 0 1 4 7; do
  check_number "$n"
done

# 7. 字符串处理
text="Hello Bash Script World"
echo "原串: $text"
echo "长度: ${#text}"
echo "大写: ${text^^}"
echo "小写: ${text,,}"
echo "替换: ${text/Bash/Shell}"
echo "切片: ${text:6:4}"

# 8. here-string 和 mapfile
mapfile -t lines <<< $'alpha\nbeta\ngamma\ndelta'
echo "mapfile 读取到 ${#lines[@]} 行:"
for line in "${lines[@]}"; do
  echo "  [$line]"
done

# 9. while 读取 here-doc
echo "while 读取 here-doc:"
while IFS= read -r line; do
  echo "  <$line>"
done <<'SCRIPT_EOF'
第一行
第二行
第三行
SCRIPT_EOF

# 10. 随机数统计
sum=0
count=5
echo "随机数:"
for ((i = 1; i <= count; i++)); do
  r=$((RANDOM % 100 + 1))
  sum=$((sum + r))
  printf "  #%d = %d\n" "$i" "$r"
done
echo "总和=$sum 平均=$((sum / count))"

# 11. 乘法表
echo "5x5 乘法表:"
for ((i = 1; i <= 5; i++)); do
  for ((j = 1; j <= 5; j++)); do
    printf "%4d" "$((i * j))"
  done
  echo
done

# 12. 函数退出码
is_even() {
  (( $1 % 2 == 0 ))
}

if is_even 10; then
  echo "10 是偶数，函数返回成功"
fi

if ! is_even 7; then
  echo "7 不是偶数，函数返回失败"
fi

echo "===== 测试结束 ====="
"###;

/// niubash#277/#280 adjacency: ESC-sequenced arrow keys (ConPTY synthesizes
/// them into arrow KEY_EVENTs from the pty's ESC[A..D stream) ride the
/// input path untouched by paste-chunk detection — no chunk opens, no
/// keystroke or the final Enter is absorbed, and the edits land.
#[test]
fn esc_sequenced_arrow_keys_edit_without_absorption() {
    if !require_pty_or_skip("esc_sequenced_arrow_keys_edit_without_absorption") {
        return;
    }
    let mut s = NiuSession::spawn("arrow-277");
    s.wait_ready();
    // A hot burst into the buffer, then cursor motion, then an insert.
    s.send("echo abc");
    s.send("\u{1b}[D\u{1b}[D"); // Left Left: park before the 'b'
    s.send("X");
    s.send("\r");
    // The executed output `aXbc` proves the buffer was `echo aXbc` at
    // submit — the arrows moved the cursor and no keystroke was absorbed.
    // (The buffer echo also renders `aXbc`, so the fresh prompt below is
    // the real discriminator: a swallowed Enter would leave the line
    // unexecuted with no new prompt, and this expect would time out.)
    s.expect("aXbc");
    s.expect_prompt();
}

/// niubash#202 (Windows path): Ctrl+V (EditCommand::PasteSystem) inserts the
/// whole clipboard into the edit buffer as one unit — no line executes
/// until Enter — restoring GNU bash paste semantics on Windows where
/// bracketed paste cannot reach the shell.
///
/// Per-line execution would print each line's output (and a fresh prompt)
/// while the paste is still being typed; the expect_absent assertions below
/// pin the buffered, single-submission behavior.
#[test]
fn ctrl_v_clipboard_paste_single_buffer_execution() {
    if !cfg!(windows) {
        // CI clipboards on headless unix runners are not addressable; the
        // bracketed-paste test above covers the unix semantics.
        return;
    }
    if !require_pty_or_skip("ctrl_v_clipboard_paste_single_buffer_execution") {
        return;
    }
    const PS: &str = "C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe";
    // Output markers come from rc variables so the echoed edit buffer
    // (`echo $V1`) can never satisfy the "not executed" assertions — only
    // the executed output text can.
    let rc = format!("{}\nV1=CBX1_DONE\nV2=CBX2_DONE\n", driver::default_rc());
    let mut s = NiuSession::spawn_custom("paste-ctrlv", &rc, &[], (120, 30), HEAVY_TIMEOUT);
    s.wait_ready();
    let mut set = std::process::Command::new(PS);
    set.args([
        "-NoProfile",
        "-Command",
        "Set-Clipboard -Value 'echo $V1\r\necho $V2'",
    ]);
    let st = set.output().expect("spawn powershell");
    if !st.status.success() {
        // No clipboard on this host: the test cannot be exercised.
        return;
    }
    // Let the clipboard settle.
    std::thread::sleep(Duration::from_millis(300));
    s.send("\u{16}"); // Ctrl+V
    std::thread::sleep(Duration::from_millis(500));
    // Both lines buffered as one unit; nothing executed yet.
    s.expect_absent("CBX1_DONE", ABSENT_WINDOW);
    s.expect_absent("CBX2_DONE", ABSENT_WINDOW);
    s.send("\r");
    s.expect("CBX1_DONE");
    s.expect("CBX2_DONE");
    s.expect_prompt();
}

/// Resizing the terminal mid-session does not crash the shell or wedge the
/// prompt.
#[test]
fn resize_does_not_crash_shell() {
    if !require_pty_or_skip("resize_does_not_crash_shell") {
        return;
    }
    let mut s = NiuSession::spawn("resize");
    s.wait_ready();
    s.resize(60, 20);
    std::thread::sleep(Duration::from_millis(200));
    s.send_line("echo after-resize-ok");
    s.expect("after-resize-ok");
    s.expect_prompt();
    s.resize(200, 50);
    std::thread::sleep(Duration::from_millis(200));
    s.send_line("echo after-resize-two-ok");
    s.expect("after-resize-two-ok");
    s.expect_prompt();
}

// ---------------------------------------------------------------------------
// niubash#145: interactive startup must reach the prompt when an rc carries
// the retired built-in plugin/theme stack lines. The original bug: the host
// spawned the 'niubash-gitstatus' worker thread for the async git prompt and
// the main thread waited on it before the first prompt render, so a shell
// whose rc loaded the framework (non-empty NIU_PLUGINS) hung forever after
// the banner. The whole machinery is deleted; these tests pin the fix by
// driving the real interactive (PTY) path with timeout-bounded asserts.
// ---------------------------------------------------------------------------

/// The legacy rc exactly as the old setup wizard wrote it (stack variables,
/// export block, oh-my-niu discovery loop), plus the sentinels.
fn legacy_stack_rc() -> String {
    r#"NIU_THEME='spaceship'
NIU_THEME_PLUGIN='theme-spaceship'
NIU_PROMPT_SYMBOL='❯'
NIU_DISABLE_DEFAULT_PLUGINS=1
export NIU_THEME NIU_THEME_PLUGIN NIU_PROMPT_SYMBOL
export NIU_DISABLE_DEFAULT_PLUGINS
NIU_PLUGINS=(prompt-core git starship common-aliases path-tools extract zoxide fzf thefuck command-not-found direnv dotenv kubectl npm keybindings env-sync)
if [ -z "${NIUBASH:-}" ]; then
  for __niubash_bundle in "$HOME/.oh-my-niu" "$HOME/.niubash/oh-my-niu" "$HOME/.niubash/bundles/oh-my-niu"/* "$NIU_APP_BUNDLE_PATH"; do
    if [ -f "$__niubash_bundle/oh-my-niu.niu" ]; then
      NIUBASH="$__niubash_bundle"
      export NIUBASH
      break
    fi
  done
fi
if [ -f "$NIUBASH/oh-my-niu.niu" ]; then
  . "$NIUBASH/oh-my-niu.niu"
fi
unset __niubash_bundle
PS1='P1> '
PS2='P2> '
"#
    .to_string()
}

/// A minimal oh-my-niu-shaped framework entry the legacy discovery loop can
/// find: it defines the hook-runner functions the old host dispatched. If the
/// async git-status machinery ever comes back, sourcing this plus the
/// non-empty NIU_PLUGINS list is what used to hang startup.
fn write_legacy_framework_fixture(root: &std::path::Path) -> std::path::PathBuf {
    let bundle = root.join("oh-my-niu");
    std::fs::create_dir_all(&bundle).expect("create fixture bundle dir");
    std::fs::write(
        bundle.join("oh-my-niu.niu"),
        r#"# legacy framework fixture (niubash#145 regression)
niubash_run_precmd_hooks() {
  NIU_PROMPT_GIT="SNAPSHOT:precmd:${NIU_LAST_EXIT_CODE:-0} "
  export NIU_PROMPT_GIT
  return 0
}
niubash_run_startup_hooks() { :; }
niubash_run_preexec_hooks() { :; }
"#,
    )
    .expect("write framework fixture");
    bundle
}

/// The headline regression: banner -> prompt must be reached (bounded by the
/// driver timeout) with the legacy stack rc, and interactive commands must
/// keep working afterwards.
#[test]
fn legacy_niu_plugins_rc_reaches_prompt() {
    if !require_pty_or_skip("legacy_niu_plugins_rc_reaches_prompt") {
        return;
    }
    let mut s = NiuSession::spawn_custom(
        "legacy-plugins",
        &legacy_stack_rc(),
        &[],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    s.wait_ready();
    s.send_line("echo legacy-ok");
    s.expect("legacy-ok");
    s.expect_prompt();
}

/// Same rc, but the discovery loop actually finds a framework entry through
/// $NIU_APP_BUNDLE_PATH — the exact shape of the owner-machine hang (rc
/// sourced the framework, NIU_PLUGINS non-empty, prompt never rendered).
#[test]
fn legacy_framework_rc_reaches_prompt() {
    if !require_pty_or_skip("legacy_framework_rc_reaches_prompt") {
        return;
    }
    let fixture_root = std::env::temp_dir().join(format!(
        "niu-legacy-framework-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let bundle = write_legacy_framework_fixture(&fixture_root);
    let mut s = NiuSession::spawn_custom(
        "legacy-framework",
        &legacy_stack_rc(),
        &[(
            "NIU_APP_BUNDLE_PATH".to_string(),
            bundle.to_string_lossy().into_owned(),
        )],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    s.wait_ready();
    // The framework entry was really sourced (rc-side effect survives).
    s.send_line("command -v niubash_run_precmd_hooks >/dev/null && echo fw-loaded");
    s.expect("fw-loaded");
    s.expect_prompt();
    s.send_line("echo still-alive");
    s.expect("still-alive");
    s.expect_prompt();

    let _ = std::fs::remove_dir_all(&fixture_root);
}

// ---------------------------------------------------------------------------
// niubash#147: after `cd` under a paren-bearing theme prompt (robbyrussell's
// `git:(branch)`), the shell must not report
// `unexpected EOF while looking for matching ')'` and the next prompt must
// not carry a stray literal `)` as echoed input. The owner report (build
// @688f224) also showed `cd` printing the working directory; with CDPATH
// unset GNU `cd` prints nothing, and the engine's cd already follows GNU
// builtins/cd.def (print only on a CDPATH hit, `cd -`, or cdable_vars), so
// these tests pin both sides: no print without CDPATH, print with
// CDPATH="." (what oh-my-bash's lib/shopt.sh sets).
// ---------------------------------------------------------------------------

/// robbyrussell-shaped PS1 with raw color escapes and the `git:(branch)`
/// parens, matching the theme byte shape the report ran.
fn paren_theme_rc() -> String {
    let e = "\u{1b}";
    format!(
        "PS1='{e}[1;92m\u{279c}{e}[97m  {e}[96m\\W{e}[97m {e}[94mgit:({e}[91mmaster{e}[94m){e}[97m '\n\
         PS2='P2> '\n\
         NIU_DISABLE_DEFAULT_PLUGINS=1\n"
    )
}

/// cd under the paren prompt: no EOF diagnostic, no stray `)` in the echoed
/// input, and no working-directory print while CDPATH is unset.
#[test]
fn cd_with_paren_theme_prompt_leaks_nothing_into_input() {
    if !require_pty_or_skip("cd_with_paren_theme_prompt_leaks_nothing_into_input") {
        return;
    }
    let mut s = NiuSession::spawn_custom(
        "paren-cd",
        &paren_theme_rc(),
        &[],
        (120, 30),
        DEFAULT_TIMEOUT,
    );
    s.expect("Niubash");
    s.expect("git:(");

    let repo = s.start().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    // Fidelity to the report (cd into a git repo): init one when git is
    // available; the assertions hold either way.
    let _ = std::process::Command::new("git")
        .arg("init")
        .arg(&repo)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    s.send_line("cd repo");
    s.send_line("echo after-paren-cd-ok");
    // The window covers the cd input echo, any cd output, the next prompt,
    // and the follow-up command's echo + output. A paren fragment leaking
    // into the input stream would either parse-fail (EOF diagnostic) or sit
    // in the edit buffer and break the follow-up command (its marker would
    // never print), so this one expect is the stray-paren detector.
    let follow = s.expect("after-paren-cd-ok");
    assert!(
        !follow.replace('\u{1b}', "").contains("unexpected EOF"),
        "unexpected EOF diagnostic after cd; window: {follow:?}"
    );
    // No working-directory print without CDPATH (GNU cd.def prints only on
    // a CDPATH hit / `cd -` / cdable_vars).
    let start_path = s
        .start()
        .to_string_lossy()
        .replace('\\', "/")
        .to_lowercase();
    assert!(
        !follow.to_lowercase().contains(&start_path),
        "plain cd printed the working directory without CDPATH; window: {follow:?}"
    );
    s.expect("git:(");
    assert!(
        !s.transcript()
            .replace('\u{1b}', "")
            .contains("unexpected EOF"),
        "unexpected EOF diagnostic in the session transcript"
    );
}

/// With CDPATH set (oh-my-bash's lib/shopt.sh sets CDPATH="."), a relative
/// cd that hits CDPATH prints the new directory — the GNU behavior the
/// owner's window actually exercised, pinned here so the engine contract
/// survives host changes.
#[test]
fn cd_with_cdpath_prints_directory_like_gnu() {
    if !require_pty_or_skip("cd_with_cdpath_prints_directory_like_gnu") {
        return;
    }
    let mut s = NiuSession::spawn_custom(
        "cdpath-print",
        &paren_theme_rc(),
        &[],
        (120, 30),
        DEFAULT_TIMEOUT,
    );
    s.expect("Niubash");
    s.expect("git:(");
    std::fs::create_dir_all(s.start().join("repo")).unwrap();

    s.send_line("export CDPATH=.");
    s.expect("git:(");
    s.send_line("cd repo");
    // GNU builtins/cd.def:353-378 — a non-empty CDPATH element that finds
    // the directory prints it.
    let window = s.expect("/start/repo");
    assert!(
        !window.replace('\u{1b}', "").contains("unexpected EOF"),
        "EOF diagnostic after CDPATH cd; window: {window:?}"
    );
    s.expect("git:(");
    s.send_line("echo cdpath-ok");
    s.expect("cdpath-ok");
}

// ---------------------------------------------------------------------------
// Matrix 7: multi-line theme cursor placement (niubash#169)
// ---------------------------------------------------------------------------

/// PS1 of the oh-my-bash/bash-it powerline-multiline family: the right-aligned
/// segment is produced by RAW cursor surgery inside PS1 — a jump to the right
/// margin (`\e[500C`, clamping at the terminal edge) plus a back-move
/// (`\e[6D`), then the segment text, all unmarked by `\[ \]`. The line editor
/// walks the prompt as a linear text run with ANSI stripped, so unmarked
/// surgery detaches its last-line visible-width accounting from the real
/// render and the editing cursor lands away from the input line. The prompt
/// channel now splits the idiom (prompt_right_align::split_right_align): the
/// tail renders through the editor's right-prompt placement (escape-excluded
/// width math), the jump/back bytes never reach the terminal, and typing
/// lands at the input line's end.
#[test]
fn multiline_right_align_theme_renders_tail_without_cursor_surgery() {
    if !require_pty_or_skip("multiline_right_align_theme_renders_tail_without_cursor_surgery") {
        return;
    }
    let rc = concat!(
        r"PS1='\[\e[34;1m\]left-seg \[\e[0m\]\e[500C\e[6D\[\e[33;1m\] RIGHT \[\e[0m\]\ni7> '",
        "\nPS2='P2> '\nNIU_DISABLE_DEFAULT_PLUGINS=1\n",
    );
    // 80 columns: ` RIGHT ` is 7 visible columns, so the editor's right
    // prompt is placed at column 73 (0-based).
    let mut s = NiuSession::spawn_custom("i169-right-align", rc, &[], (80, 24), DEFAULT_TIMEOUT);
    s.expect("Niubash");
    // Both halves render: the left segment inline, the aligned tail through
    // the editor's right prompt.
    s.expect("left-seg");
    s.expect("RIGHT");
    let transcript = s.transcript();
    assert!(
        !transcript.contains("<ESC>[500C"),
        "the raw right-align jump must be split out of the painted prompt"
    );
    // ConPTY re-renders the editor's absolute placement as relative ops:
    // from the left segment's column 9, a 64-column forward reaches column
    // 73 = 80 - 7 (` RIGHT ` escape-excluded), then the tail. The final
    // `ESC[3;5H` parks the cursor after `i7> ` on the input line — the #169
    // contract.
    assert!(
        transcript.contains("<ESC>[64C RIGHT"),
        "the tail must be placed at the escape-excluded margin column \
         (73 of 80); transcript: {transcript:?}"
    );
    assert!(
        transcript.contains("<ESC>[3;5H"),
        "the cursor must land after `i7> ` on the input line; transcript: {transcript:?}"
    );
    // The editor's cursor model survives typing at the input line.
    s.send_line("echo i169-ok");
    s.expect("i169-ok");
    s.expect("i7>");
}

/// Same shape with a WIDE-CJK tail: the editor's placement must count the
/// CJK glyphs as 2 columns each (` 项目 ` = 6 columns), so the MoveTo lands
/// one column further left than the ASCII tail — the visible-width rule the
/// fix exists for.
#[test]
fn multiline_right_align_theme_cjk_tail_width_matches_editor_columns() {
    if !require_pty_or_skip("multiline_right_align_theme_cjk_tail_width_matches_editor_columns") {
        return;
    }
    let rc = concat!(
        r"PS1='\[\e[34;1m\]left-seg \[\e[0m\]\e[500C\e[6D\[\e[33;1m\] 项目 \[\e[0m\]\ni7> '",
        "\nPS2='P2> '\nNIU_DISABLE_DEFAULT_PLUGINS=1\n",
    );
    let mut s =
        NiuSession::spawn_custom("i169-right-align-cjk", rc, &[], (80, 24), DEFAULT_TIMEOUT);
    s.expect("Niubash");
    s.expect("left-seg");
    s.expect("项目");
    let transcript = s.transcript();
    assert!(
        !transcript.contains("<ESC>[500C"),
        "the raw right-align jump must be split out of the painted prompt"
    );
    // 6 visible columns (space + two wide glyphs + space): from column 9 the
    // forward lands on column 74 — one further than the ASCII tail, which is
    // the 2-columns-per-glyph width rule end to end. Cursor after `i7> `.
    assert!(
        transcript.contains("<ESC>[65C 项目"),
        "the CJK tail must be placed by visible width (2 columns per glyph); \
         transcript: {transcript:?}"
    );
    assert!(
        transcript.contains("<ESC>[3;5H"),
        "the cursor must land after `i7> ` on the input line; transcript: {transcript:?}"
    );
    s.send_line("echo i169-cjk-ok");
    s.expect("i169-cjk-ok");
    s.expect("i7>");
}

// ---------------------------------------------------------------------------
// Matrix 8: vi editing mode (niubash#184)
// ---------------------------------------------------------------------------
//
// `set -o vi` / `set -o emacs` must switch the LIVE line editor, like GNU
// bash: both options route to one readline editing-mode state
// (builtins/set.def:200/235 -> set_edit_mode, set.def:424), whose
// `rl_variable_bind("editing-mode")` rebinds the active keymap immediately
// (lib/readline/bind.c:2001 sv_editmode, bind.c:2092/2104). Every fresh
// line starts in insert mode even in vi mode (lib/readline/readline.c:
// 1243-1249). The journeys drive the real editor: ESC enters normal mode,
// `k` recalls history, `dd` kills the line, and the vi-mode indicator
// tracks insert/normal on the floor prompt.
//
// Assertion style: the syntax highlighter wraps command words in ANSI,
// output lines carry a trailing erase, and ConPTY re-renders wrapped lines
// and elides redundant SGR runs — so multi-token substrings and
// escape-anchored matches never survive. Every marker below is a single
// token counted in the transcript, and [`settle_and_drain`] runs after
// every submitted line so keystrokes always land on a settled prompt.

/// ESC as sent through the pty input pipe.
const ESC_KEY: &str = "\u{1b}";

/// Send ESC as its own pty write, then wait a beat before the vi motion.
/// A lone ESC byte that shares one read chunk with the following key is
/// parsed by crossterm as Alt+<key>, not as the vi ESC — the separate
/// write (and the gap) is what makes it a standalone Esc key event.
fn send_esc(s: &mut NiuSession) {
    s.send(ESC_KEY);
    std::thread::sleep(Duration::from_millis(200));
}

/// Let the shell finish the submitted line (execute + per-prompt editor
/// rebuild) and consume every pending paint, so the next keystrokes land
/// on a settled prompt and no later assertion depends on which repaint a
/// consuming match lands in.
fn settle_and_drain(s: &mut NiuSession) {
    std::thread::sleep(Duration::from_millis(400));
    s.drain_screen();
}

/// Count occurrences of `needle` in everything seen so far.
fn transcript_count(s: &NiuSession, needle: &str) -> usize {
    s.transcript().matches(needle).count()
}

/// Wait until `needle` has been seen at least `at_least` times and return
/// the count; panics with the transcript on timeout.
fn wait_for_count(s: &NiuSession, needle: &str, at_least: usize, timeout: Duration) -> usize {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let count = transcript_count(s, needle);
        if count >= at_least {
            return count;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "expected {needle:?} seen {at_least} times (now {}); transcript: {}",
                count,
                s.transcript()
            );
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}

/// rc-set `set -o vi` (the standard bashrc line) must put the editor in vi
/// mode from the first prompt: ESC + `k` recalls the previous history line,
/// and Enter re-runs it. In emacs mode `k` would type the letter instead.
#[test]
fn vi_rc_set_o_vi_recalls_history_with_k_at_startup() {
    if !require_pty_or_skip("vi_rc_set_o_vi_recalls_history_with_k_at_startup") {
        return;
    }
    let mut rc = driver::default_rc();
    rc.push_str("set -o vi\n");
    let mut s = NiuSession::spawn_custom("vi-rc-startup", &rc, &[], (120, 30), DEFAULT_TIMEOUT);
    s.wait_ready();
    s.send_line("echo vi_rc_m1");
    // Typed-buffer paint + echo output.
    let baseline = wait_for_count(&s, "vi_rc_m1", 2, DEFAULT_TIMEOUT);
    settle_and_drain(&mut s);

    // ESC -> normal mode, then k -> previous history entry repaints it.
    send_esc(&mut s);
    s.send("k");
    let recalled = wait_for_count(&s, "vi_rc_m1", baseline + 1, DEFAULT_TIMEOUT);
    s.send("\r");
    // Enter re-runs the recalled echo.
    wait_for_count(&s, "vi_rc_m1", recalled + 1, DEFAULT_TIMEOUT);
}

/// The floor prompt's vi-mode indicator follows the live reedline mode:
/// `i ` in insert, `- ` in normal, right after the prompt's `% `. A
/// claimed PS1 renders no product indicator by design (whoever sets PS1
/// owns the prompt slot, like GNU bash where vi-mode plugins add their own
/// marker), so this journey runs on the unclaimed floor prompt.
#[test]
fn vi_floor_indicator_follows_esc_live() {
    if !require_pty_or_skip("vi_floor_indicator_follows_esc_live") {
        return;
    }
    let rc = "set -o vi\nNIU_DISABLE_DEFAULT_PLUGINS=1\n".to_string();
    let mut s =
        NiuSession::spawn_custom("vi-floor-indicator", &rc, &[], (120, 30), DEFAULT_TIMEOUT);
    s.expect("Niubash");
    // Shrink the cwd so the prompt cannot wrap: ConPTY re-renders wrapped
    // lines with injected breaks, which would split `% ` from the
    // indicator.
    s.send_line("cd /");
    // Insert-mode indicator after the prompt's `% `. (No trailing space in
    // the pattern: with an empty buffer ConPTY elides the space into the
    // erase-to-EOL.)
    s.expect("$ i");
    settle_and_drain(&mut s);

    s.send("abc");
    // ESC flips the live mode; the repaint carries the normal indicator.
    send_esc(&mut s);
    s.expect("$ - ");
    settle_and_drain(&mut s);
    // Normal-mode dd kills the line; back to insert for a clean one.
    s.send("dd");
    s.send("i");
    s.send_line("echo floor_ok_out");
    wait_for_count(&s, "floor_ok_out", 2, DEFAULT_TIMEOUT);
    assert!(
        !s.transcript().contains("floor_ok_outN"),
        "leftover tail after dd: {}",
        s.transcript()
    );
}

/// `set -o vi` typed mid-session must switch the live editor (normal-mode
/// motions active at the very next prompt), `set -o emacs` must return to
/// emacs (ESC stays a no-op and k types the letter), and `set -o vi` must
/// come back (normal-mode dd works again).
#[test]
fn vi_set_o_live_switch_vi_emacs_vi_journey() {
    if !require_pty_or_skip("vi_set_o_live_switch_vi_emacs_vi_journey") {
        return;
    }
    let mut s = NiuSession::spawn("vi-live-switch");
    s.wait_ready();

    // Enter vi mode mid-session.
    s.send_line("set -o vi");
    settle_and_drain(&mut s);
    s.send_line("echo live_vi_one");
    // Typed-buffer paint + echo output.
    let baseline = wait_for_count(&s, "live_vi_one", 2, DEFAULT_TIMEOUT);
    settle_and_drain(&mut s);
    // In vi normal mode, k recalls the history line...
    send_esc(&mut s);
    s.send("k");
    let recalled = wait_for_count(&s, "live_vi_one", baseline + 1, DEFAULT_TIMEOUT);
    // ...and Enter re-runs it.
    s.send("\r");
    wait_for_count(&s, "live_vi_one", recalled + 1, DEFAULT_TIMEOUT);
    settle_and_drain(&mut s);

    // Back to emacs: ESC is a no-op and k must TYPE the letter, not recall
    // history and not re-run the echo — the marker count must not move.
    s.send_line("set -o emacs");
    settle_and_drain(&mut s);
    let frozen = transcript_count(&s, "live_vi_one");
    send_esc(&mut s);
    s.send("k");
    s.send("\r");
    std::thread::sleep(ABSENT_WINDOW);
    assert_eq!(
        transcript_count(&s, "live_vi_one"),
        frozen,
        "in emacs mode k must type the letter, not recall history; transcript: {}",
        s.transcript()
    );
    settle_and_drain(&mut s);

    // And `set -o vi` returns: normal-mode dd works again. (k cannot prove
    // this leg — the most recent history entries are the `set -o` commands
    // themselves, so recall would repaint those, not the marker.)
    s.send_line("set -o vi");
    settle_and_drain(&mut s);
    s.send("zz_final_q");
    send_esc(&mut s);
    s.send("dd");
    s.send("i");
    s.send_line("echo final_ok_out");
    // Typed-buffer paint + echo output: only reachable with the vi normal
    // keymap active (in emacs, ESC/dd/i would type literally and the line
    // would never execute cleanly).
    wait_for_count(&s, "final_ok_out", 2, DEFAULT_TIMEOUT);
    assert!(
        !s.transcript().contains("final_ok_outq"),
        "leftover tail after dd in the returned vi mode: {}",
        s.transcript()
    );
}

/// In vi normal mode `dd` kills the whole line (reedline maps it to a
/// line-wise Cut), so only the re-typed command runs.
#[test]
fn vi_normal_dd_kills_line_before_insert() {
    if !require_pty_or_skip("vi_normal_dd_kills_line_before_insert") {
        return;
    }
    let mut s = NiuSession::spawn("vi-dd");
    s.wait_ready();
    s.send_line("set -o vi");
    settle_and_drain(&mut s);
    // Type a broken line, ESC, dd the whole line, then insert a clean one.
    s.send("echo zz_dd_broken");
    send_esc(&mut s);
    s.send("dd");
    s.send("i");
    s.send_line("echo dd_ok_out");
    // Typed-buffer paint + echo output.
    wait_for_count(&s, "dd_ok_out", 2, DEFAULT_TIMEOUT);
    let transcript = s.transcript();
    // If dd had failed, the clean text would have been inserted before the
    // ESC cursor (one char before the line end), leaving the broken tail
    // glued right after it in the executed line and its output.
    assert!(
        !transcript.contains("dd_ok_outN"),
        "leftover broken-line tail after dd: {transcript:?}"
    );
    assert!(
        !transcript.contains("zz_dd_broken:"),
        "the broken line must never be executed as a command: {transcript:?}"
    );
}

/// A session that never enabled vi stays emacs: with no vi flag, ESC + k
/// must type the letter (the resolver stays quiet on the all-off default
/// and the emacs keymap never switches).
#[test]
fn vi_default_emacs_keeps_k_typing() {
    if !require_pty_or_skip("vi_default_emacs_keeps_k_typing") {
        return;
    }
    let mut s = NiuSession::spawn("vi-default-emacs");
    s.wait_ready();
    s.send_line("echo emacs_m5");
    let baseline = wait_for_count(&s, "emacs_m5", 2, DEFAULT_TIMEOUT);
    settle_and_drain(&mut s);
    send_esc(&mut s);
    s.send("k");
    s.send("\r");
    std::thread::sleep(ABSENT_WINDOW);
    assert_eq!(
        transcript_count(&s, "emacs_m5"),
        baseline,
        "with no vi flag, k must type the letter (emacs default); transcript: {}",
        s.transcript()
    );
}

// ---------------------------------------------------------------------------
// Matrix 7: PROMPT_COMMAND exit-jump consumption (niubash#191)
// ---------------------------------------------------------------------------
//
// `PROMPT_COMMAND='exit'` must end the session: the engine re-arms the
// top-level jump (rubash#433), and the REPL consumes it instead of
// re-arming the prompt. GNU: jump_to_top_level unwinds reader_loop
// (evalstring.c:618-619) — no further prompt, no further read, the jump's
// status becomes the session status.

/// A PC `exit 5` submitted mid-session kills the shell with rc 5 before the
/// next prompt renders and before the next input line is read.
#[test]
fn prompt_command_exit_ends_repl_session_with_status() {
    if !require_pty_or_skip("prompt_command_exit_ends_repl_session_with_status") {
        return;
    }
    let mut s = NiuSession::spawn("pc-exit-jump");
    s.wait_ready();
    s.send_line("PROMPT_COMMAND='exit 5'");
    s.expect_prompt();
    // The NEXT pre-prompt pass runs the PC; the jump unwinds the REPL, so
    // the queued line is never read or run. (Reedline repaints the prompt
    // per keystroke and ConPTY echoes those paints asynchronously, so the
    // transcript cannot pin "exactly N prompts" — the load-bearing pins are
    // the jump's status and the follow-up line never executing.)
    s.send_line("echo AFTER-PC-JUMP");
    let code = s.wait_exit();
    assert_eq!(code, 5, "the jump's status is the session status");
    s.expect_absent("AFTER-PC-JUMP", ABSENT_WINDOW);
}

/// Control: a PC that merely fails (no `exit`, no errexit) leaves the shell
/// alive — the follow-up prompt renders and input still runs.
#[test]
fn prompt_command_plain_failure_keeps_session_alive() {
    if !require_pty_or_skip("prompt_command_plain_failure_keeps_session_alive") {
        return;
    }
    let mut s = NiuSession::spawn("pc-fail-survive");
    s.wait_ready();
    s.send_line("PROMPT_COMMAND='false'");
    s.expect_prompt();
    s.send_line("echo AFTER-FAIL");
    s.expect("AFTER-FAIL");
    s.expect_prompt();
}

/// The same consumption in the `--noediting` REPL loop: a PC `exit` from the
/// rc kills the session at the FIRST pre-prompt pass — no prompt at all is
/// printed and the status is carried out.
#[test]
fn prompt_command_exit_ends_noediting_repl_session() {
    if !require_pty_or_skip("prompt_command_exit_ends_noediting_repl_session") {
        return;
    }
    let rc = concat!(
        "PS1='P1> '\n",
        "PS2='P2> '\n",
        "NIU_DISABLE_DEFAULT_PLUGINS=1\n",
        "PROMPT_COMMAND='exit 7'\n",
    );
    let mut s = NiuSession::spawn_custom_with_args(
        "pc-exit-noedit",
        rc,
        &["--noediting"],
        &[],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    s.expect("Niubash");
    let code = s.wait_exit();
    assert_eq!(code, 7, "the jump's status is the session status");
    let transcript = s.transcript();
    assert_eq!(
        transcript.matches(driver::PROMPT1).count(),
        0,
        "no prompt may render after a PC exit jump: {transcript}"
    );
}

/// niubash: the Windows factory-default "Git Bash compatible" prompt.
///
/// Real-ConPTY proof for the owner adjudication (2026-10-12): with no rc
/// theme and no PS1 claim, the interactive prompt on Windows renders the
/// Git Bash skeleton — green `user@host`, yellow POSIX-persona cwd, `$ ` —
/// and a `cd` into a repository adds the cyan branch segment without any
/// rc or PROMPT_COMMAND involvement. The rc here is empty on purpose: the
/// sandbox rc's `P1> ` sentinel would claim the prompt and hide the floor.
#[test]
fn gitbash_factory_prompt_renders_and_tracks_the_branch() {
    if !driver::require_pty_or_skip("gitbash factory prompt") {
        return;
    }
    let repo_dir = std::env::temp_dir().join(format!(
        "gitbash-floor-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&repo_dir).unwrap();
    let repo_path = repo_dir.clone();
    let run_path = repo_dir.clone();
    let run = move |cmd: &str| {
        std::process::Command::new("git")
            .args(["-C", run_path.to_string_lossy().as_ref()])
            .args(cmd.split_whitespace())
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .expect("git")
    };
    run("init");
    std::fs::write(repo_path.join("f.txt"), b"x").unwrap();
    run("add f.txt");
    run("commit -m init");

    // The branch segment is rendered by git_prompt spawning `git` through
    // the session PATH; the sandbox PATH (System32 + Windows) has no git,
    // so the branch could never render and the tail of this test failed
    // byte-stable on every host (part of niubash#420's baseline five — a
    // test-design gap, not a wedge: the `cd` submit itself always ran).
    // Discover the host git the same way the `run` helper above does and
    // prepend its dir to the session PATH.
    let git_where = std::process::Command::new("where.exe")
        .arg("git")
        .output()
        .expect("locate git for the session PATH");
    let git_exe = String::from_utf8_lossy(&git_where.stdout)
        .lines()
        .next()
        .expect("git.exe path from where.exe")
        .trim()
        .to_string();
    let git_dir = std::path::Path::new(&git_exe)
        .parent()
        .expect("git.exe parent dir")
        .to_string_lossy()
        .to_string();
    let system_root = std::env::var_os("SystemRoot")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"));
    let session_path = format!(
        "{git_dir};{};{}",
        system_root.join("System32").display(),
        system_root.display()
    );
    let extra_env = vec![("PATH".to_string(), session_path)];
    let mut session = driver::NiuSession::spawn_custom(
        "gitbash-floor",
        "", // empty rc: no theme, no PS1 claim — the factory floor shows
        &extra_env,
        (120, 30),
        driver::DEFAULT_TIMEOUT,
    );
    // No rc sentinel here: the floor prompt has no `P1> `, so readiness is
    // polled on the skeleton's own SGR signature.
    let mut first = String::new();
    for _ in 0..40 {
        first.push_str(&session.drain_screen());
        if first.contains("\x1b[33m") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    assert!(
        first.contains("\x1b[32m") && first.contains("\x1b[33m"),
        "expected the green user@host + yellow cwd skeleton on the floor prompt, got:\n{first}"
    );
    assert!(
        !first.contains("\x1b[36m"),
        "no branch outside a repo:\n{first}"
    );

    let home = std::env::var("USERPROFILE").unwrap();
    let drive = home.chars().next().unwrap().to_lowercase();
    let tail = repo_path.to_string_lossy().replace('\\', "/");
    let repo_posix = format!("/{drive}{}", &tail[2..]);
    let _ = &repo_dir;
    session.send_line(&format!("cd {repo_posix}"));
    let mut after = String::new();
    for _ in 0..40 {
        after.push_str(&session.drain_screen());
        if after.contains("master") || after.contains("main") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    assert!(
        after.contains("master") || after.contains("main"),
        "expected the cyan branch segment after cd into a repo, pane:\n{after}"
    );
}

// ---------------------------------------------------------------------------
// Matrix 6b: multiline paste across EMPTY lines (niubash#291, #309 surface)
// ---------------------------------------------------------------------------

/// Execution-level markers for paste fidelity assertions (niubash#291
/// discipline): the pasted body carries `echo RES_$((40+2))`-shaped lines,
/// so the OUTPUT (`RES_42`) is a string that appears NOWHERE in the echoed
/// input (the echo renders the unexpanded `$((40+2))`). `RES_42` on screen
/// therefore proves the line executed with every leading byte intact — a
/// swallowed head (`cho RES_...`) is a not-found and leaves no marker.
/// Substring checks against the screen are banned here: the scratch probe
/// this suite replaces asserted `!screen.contains("cho two")`, which fails
/// on a CORRECT run too ("cho two" is a substring of the intact rendering
/// "echo two") — a false positive that masqueraded as niubash#291.
const PASTE_MARKER_A: &str = "echo RES_$((40+2))";
const PASTE_MARKER_A_OUT: &str = "RES_42";
const PASTE_MARKER_B: &str = "echo RES_$((50+8))";
const PASTE_MARKER_B_OUT: &str = "RES_58";

fn marker_rc() -> String {
    driver::default_rc()
}

/// niubash#291, the reported shape: a pasted block containing an EMPTY line
/// must keep the following line's leading bytes. The exact reported burst —
/// `echo one\r\n\r\necho two\r\n\n` in one write, then the user's Enter
/// after the burst settles — with execution-marker lines: both markers on
/// screen prove both lines ran byte-intact, including the line after the
/// empty line.
#[test]
fn empty_line_paste_keeps_the_next_line_intact() {
    if !cfg!(windows) {
        // Unix runs the real bracketed paste path (Event::Paste); the
        // arrival heuristic under test does not exist there.
        return;
    }
    if !require_pty_or_skip("empty_line_paste_keeps_the_next_line_intact") {
        return;
    }
    let mut s = NiuSession::spawn_custom(
        "paste-empty-line",
        &marker_rc(),
        &[],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    s.wait_ready();
    s.send(&format!("{PASTE_MARKER_A}\r\n\r\n{PASTE_MARKER_B}\r\n\n"));
    // Leave the burst window, then the user's own Enter (owner rhythm).
    std::thread::sleep(Duration::from_millis(200));
    s.send("\r");
    s.expect(PASTE_MARKER_A_OUT);
    s.expect(PASTE_MARKER_B_OUT);
    s.expect_prompt();
}

/// niubash#309 coverage on the #291 shape: the submit Enter lands INSIDE
/// the burst window (one write — `send(paste + "\r")`, the owner's
/// paste-then-Enter-at-once rhythm). The trailing bare CR has no LF
/// partner behind it, so it is the submit: the whole block (empty line
/// included) executes on the FIRST Enter — this test sends nothing after
/// the single write, so a swallowed Enter cannot produce the markers.
#[test]
fn empty_line_paste_same_burst_enter_submits_on_first_enter() {
    if !cfg!(windows) {
        return;
    }
    if !require_pty_or_skip("empty_line_paste_same_burst_enter_submits_on_first_enter") {
        return;
    }
    let mut s = NiuSession::spawn_custom(
        "paste-empty-line-same-burst",
        &marker_rc(),
        &[],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    s.wait_ready();
    s.send(&format!("{PASTE_MARKER_A}\r\n\r\n{PASTE_MARKER_B}\r\n\r"));
    s.expect(PASTE_MARKER_A_OUT);
    s.expect(PASTE_MARKER_B_OUT);
    s.expect_prompt();
}

/// Two consecutive empty lines: the line after them still executes with
/// every byte intact.
#[test]
fn empty_line_paste_two_consecutive_empty_lines_execute_both_lines() {
    if !cfg!(windows) {
        return;
    }
    if !require_pty_or_skip("empty_line_paste_two_consecutive_empty_lines_execute_both_lines") {
        return;
    }
    let mut s = NiuSession::spawn_custom(
        "paste-two-empty-lines",
        &marker_rc(),
        &[],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    s.wait_ready();
    s.send(&format!(
        "{PASTE_MARKER_A}\r\n\r\n\r\n{PASTE_MARKER_B}\r\n\n"
    ));
    std::thread::sleep(Duration::from_millis(200));
    s.send("\r");
    s.expect(PASTE_MARKER_A_OUT);
    s.expect(PASTE_MARKER_B_OUT);
    s.expect_prompt();
}

/// An indented line right after an empty line: the leading spaces are data
/// and the line executes as-is.
#[test]
fn empty_line_paste_indented_line_after_empty_line_executes() {
    if !cfg!(windows) {
        return;
    }
    if !require_pty_or_skip("empty_line_paste_indented_line_after_empty_line_executes") {
        return;
    }
    let mut s = NiuSession::spawn_custom(
        "paste-indented-after-empty",
        &marker_rc(),
        &[],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    s.wait_ready();
    s.send(&format!("{PASTE_MARKER_A}\r\n\r\n  {PASTE_MARKER_B}\r\n\n"));
    std::thread::sleep(Duration::from_millis(200));
    s.send("\r");
    s.expect(PASTE_MARKER_A_OUT);
    s.expect(PASTE_MARKER_B_OUT);
    s.expect_prompt();
}

/// Control: the same CRLF block WITHOUT the empty line — pins that the
/// plain multi-line paste (the #288 surface) is untouched by whatever the
/// empty-line handling does.
#[test]
fn crlf_paste_without_empty_lines_control_executes_every_line() {
    if !cfg!(windows) {
        return;
    }
    if !require_pty_or_skip("crlf_paste_without_empty_lines_control_executes_every_line") {
        return;
    }
    let mut s = NiuSession::spawn_custom(
        "paste-no-empty-control",
        &marker_rc(),
        &[],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    s.wait_ready();
    s.send(&format!("{PASTE_MARKER_A}\r\n{PASTE_MARKER_B}\r\n"));
    std::thread::sleep(Duration::from_millis(200));
    s.send("\r");
    s.expect(PASTE_MARKER_A_OUT);
    s.expect(PASTE_MARKER_B_OUT);
    s.expect_prompt();
}

/// The owner machine's prompt was a claimed/custom one (`user@host ~ %`,
/// issue comment 3) — the one reproduction variable the original 30-run
/// sweep never varied. Same #291 shape under an ANSI-claimed PS1.
#[test]
fn empty_line_paste_under_claimed_prompt_executes_both_lines() {
    if !cfg!(windows) {
        return;
    }
    if !require_pty_or_skip("empty_line_paste_under_claimed_prompt_executes_both_lines") {
        return;
    }
    let rc = concat!(
        "PS1='\\[\\e[1;36m\\]user@host ~ %\\[\\e[0m\\] '\n",
        "PS2='> '\n",
        "NIU_DISABLE_DEFAULT_PLUGINS=1\n",
    );
    let mut s = NiuSession::spawn_custom(
        "paste-empty-line-claimed",
        rc,
        &[],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    // A claimed PS1 renders itself, not the P1> sentinel — wait for the
    // claimed prompt text instead of wait_ready().
    s.expect("Niubash");
    s.expect("user@host");
    s.send(&format!("{PASTE_MARKER_A}\r\n\r\n{PASTE_MARKER_B}\r\n\n"));
    std::thread::sleep(Duration::from_millis(200));
    s.send("\r");
    s.expect(PASTE_MARKER_A_OUT);
    s.expect(PASTE_MARKER_B_OUT);
}

// ---------------------------------------------------------------------------
// niubash#399: the login profile chain (and every remaining startup-file
// face) honors a top-level `exit` by ending the invocation with its status.
// Oracle: WSL GNU Bash 5.3.0 over stdin (`bash -l -c ...` against a scratch
// HOME): a profile `exit 9` prints the profile's marker and exits 9, the
// command line never runs; /etc/profile precedes the FIRST EXISTING of
// ~/.bash_profile > ~/.bash_login > ~/.profile; --noprofile skips the
// chain; a login shell never sources the rc or BASH_ENV; a BASH_ENV or
// posix-$ENV `exit` ends the shell the same way.
// ---------------------------------------------------------------------------

/// Unique sandbox root the test stages login profiles into before the
/// session exists (mirrors the driver's own temp-dir discipline).
fn login_sandbox(label: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("niu-login-{label}-{}-{nanos}", std::process::id()))
}

/// Stage `content` at `root/home/<name>`, creating the home first.
fn stage_login_file(root: &std::path::Path, name: &str, content: &str) -> std::path::PathBuf {
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let path = home.join(name);
    std::fs::write(&path, content).unwrap();
    path
}

/// A profile `exit` ends a `-l -c` invocation with the profile's status
/// before the command string runs (GNU: `bash -l -c` over a scratch HOME
/// with `.bash_profile` = "echo marker; exit 9" prints the marker, rc 9).
#[test]
fn issue399_login_profile_exit_terminates_command_line() {
    if !require_pty_or_skip("issue399_login_profile_exit_terminates_command_line") {
        return;
    }
    let root = login_sandbox("bp-exit");
    stage_login_file(&root, ".bash_profile", "echo BP-RAN-$((40/4))\nexit 9\n");
    let mut s = NiuSession::spawn_shared_home_with_args(
        &root,
        &driver::default_rc(),
        &["-l", "-c", "echo AFTER-$((3*3))"],
        &[],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    s.expect("BP-RAN-10");
    let code = s.wait_exit();
    assert_eq!(
        code, 9,
        "the profile's exit status is the invocation status"
    );
    s.expect_absent("AFTER-9", ABSENT_WINDOW);
}

/// The chain sources only the FIRST EXISTING candidate: `.bash_profile`
/// beats `.bash_login` beats `.profile` (GNU shell.c:1863-1880).
#[test]
fn issue399_login_profile_chain_first_existing_wins() {
    if !require_pty_or_skip("issue399_login_profile_chain_first_existing_wins") {
        return;
    }
    let root = login_sandbox("chain-bp");
    stage_login_file(&root, ".bash_profile", "echo FROM-BP-$((5*2))\n");
    stage_login_file(&root, ".bash_login", "echo FROM-BL\n");
    stage_login_file(&root, ".profile", "echo FROM-PROF\n");
    let mut s = NiuSession::spawn_shared_home_with_args(
        &root,
        &driver::default_rc(),
        &["-l", "-c", "echo AFTER-$((2*5))"],
        &[],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    s.expect("FROM-BP-10");
    s.expect("AFTER-10");
    s.expect_absent("FROM-BL", ABSENT_WINDOW);
    s.expect_absent("FROM-PROF", ABSENT_WINDOW);
    assert_eq!(s.wait_exit(), 0);
}

/// Fallback: with no `.bash_profile`/`.bash_login`, `.profile` is sourced.
#[test]
fn issue399_login_profile_chain_falls_back_to_profile() {
    if !require_pty_or_skip("issue399_login_profile_chain_falls_back_to_profile") {
        return;
    }
    let root = login_sandbox("chain-prof");
    stage_login_file(&root, ".profile", "echo FROM-PROF-$((9-3))\n");
    let mut s = NiuSession::spawn_shared_home_with_args(
        &root,
        &driver::default_rc(),
        &["-l", "-c", "echo AFTER-OK"],
        &[],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    s.expect("FROM-PROF-6");
    s.expect("AFTER-OK");
    assert_eq!(s.wait_exit(), 0);
}

/// `--noprofile` skips the chain entirely (GNU: nothing sourced, the
/// command still runs), while the plain non-login face never sources it.
#[test]
fn issue399_login_noprofile_skips_chain_and_nonlogin_ignores_it() {
    if !require_pty_or_skip("issue399_login_noprofile_skips_chain_and_nonlogin_ignores_it") {
        return;
    }
    let root = login_sandbox("noprofile");
    stage_login_file(&root, ".bash_profile", "echo FROM-BP-$((8-8))\n");
    let mut s = NiuSession::spawn_shared_home_with_args(
        &root,
        &driver::default_rc(),
        // GNU 5.3 parses `-l --noprofile` as `--` (option-word quirk); the
        // working order is the long flag first.
        &["--noprofile", "-l", "-c", "echo AFTER-$((4+4))"],
        &[],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    s.expect("AFTER-8");
    s.expect_absent("FROM-BP-0", ABSENT_WINDOW);
    assert_eq!(s.wait_exit(), 0);

    let root2 = login_sandbox("nonlogin-control");
    stage_login_file(&root2, ".bash_profile", "echo FROM-BP-$((8-8))\n");
    let mut s2 = NiuSession::spawn_shared_home_with_args(
        &root2,
        &driver::default_rc(),
        &["-c", "echo AFTER-$((4+4))"],
        &[],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    s2.expect("AFTER-8");
    s2.expect_absent("FROM-BP-0", ABSENT_WINDOW);
    assert_eq!(s2.wait_exit(), 0);
}

/// A login shell sources the profile chain INSTEAD of the rc file and
/// BASH_ENV, interactive or not (GNU shell.c:1833 — L1/L11 oracles: `-l -i`
/// sources /etc/profile + the profile, never ~/.bashrc; BASH_ENV stays
/// unread).
#[test]
fn issue399_login_sources_profile_not_rc_or_bash_env() {
    if !require_pty_or_skip("issue399_login_sources_profile_not_rc_or_bash_env") {
        return;
    }
    let root = login_sandbox("login-exclusive");
    let profile = stage_login_file(&root, ".bash_profile", "echo FROM-BP-$((6+6))\n");
    let env_file = stage_login_file(&root, "env.src", "echo FROM-BASH-ENV\n");
    let bash_env = (
        "BASH_ENV".to_string(),
        env_file.to_string_lossy().into_owned(),
    );
    let mut s = NiuSession::spawn_shared_home_with_args(
        &root,
        // The rc content must not run on the login face.
        "echo FROM-RC\nPS1='P1> '\nPS2='P2> '\nNIU_DISABLE_DEFAULT_PLUGINS=1\n",
        &["-l", "-i", "-c", "echo AFTER-$((2*6))"],
        &[bash_env],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    let _ = profile;
    s.expect("FROM-BP-12");
    s.expect("AFTER-12");
    s.expect_absent("FROM-RC", ABSENT_WINDOW);
    s.expect_absent("FROM-BASH-ENV", ABSENT_WINDOW);
    assert_eq!(s.wait_exit(), 0);
}

/// A profile `exit` also ends a `-l script` invocation before the script
/// body runs (GNU L4: marker, rc 9, script never executed).
#[test]
fn issue399_login_profile_exit_terminates_before_script() {
    if !require_pty_or_skip("issue399_login_profile_exit_terminates_before_script") {
        return;
    }
    let root = login_sandbox("bp-script");
    stage_login_file(&root, ".bash_profile", "echo BP-X-$((9+8))\nexit 9\n");
    let script = stage_login_file(&root, "late.sh", "echo SCRIPT-RAN\n");
    let mut s = NiuSession::spawn_shared_home_with_args(
        &root,
        &driver::default_rc(),
        &["-l", script.to_string_lossy().as_ref()],
        &[],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    s.expect("BP-X-17");
    let code = s.wait_exit();
    assert_eq!(
        code, 9,
        "the profile's exit status is the invocation status"
    );
    s.expect_absent("SCRIPT-RAN", ABSENT_WINDOW);
}

/// The argv[0] login marker (a leading `-`, the same face the shims
/// forward) sources the profile chain too — GNU 5.2 honors the marker
/// (`-bash`); the niu contract carries it through `__RUBASH_LOGIN_SHELL`.
#[test]
fn issue399_login_argv0_marker_sources_profile() {
    if !require_pty_or_skip("issue399_login_argv0_marker_sources_profile") {
        return;
    }
    let root = login_sandbox("argv0-marker");
    stage_login_file(&root, ".bash_profile", "echo FROM-BP-$((7+7))\n");
    let marker = ("NIU_INVOKED_AS".to_string(), "-niu".to_string());
    let mut s = NiuSession::spawn_shared_home_with_args(
        &root,
        &driver::default_rc(),
        &["-c", "echo AFTER-$((3*4))"],
        &[marker],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    s.expect("FROM-BP-14");
    s.expect("AFTER-12");
    assert_eq!(s.wait_exit(), 0);
}

/// A BASH_ENV `exit` ends a non-interactive `-c` invocation with its status
/// (GNU L5: marker, rc 6, command never runs).
#[test]
fn issue399_bash_env_exit_terminates_command_line() {
    if !require_pty_or_skip("issue399_bash_env_exit_terminates_command_line") {
        return;
    }
    let root = login_sandbox("bashenv-exit");
    let env_file = stage_login_file(&root, "env.src", "echo ENV-RAN-$((4+4))\nexit 6\n");
    let bash_env = (
        "BASH_ENV".to_string(),
        env_file.to_string_lossy().into_owned(),
    );
    let mut s = NiuSession::spawn_shared_home_with_args(
        &root,
        &driver::default_rc(),
        &["-c", "echo AFTER-$((11-11))"],
        &[bash_env],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    s.expect("ENV-RAN-8");
    let code = s.wait_exit();
    assert_eq!(
        code, 6,
        "the env file's exit status is the invocation status"
    );
    s.expect_absent("AFTER-0", ABSENT_WINDOW);
}

/// An `exit` in a posix `$ENV` file under `-i -c` ends the invocation with
/// its status (GNU L6: marker, rc 5, command never runs).
#[test]
fn issue399_posix_env_exit_terminates_interactive_command() {
    if !require_pty_or_skip("issue399_posix_env_exit_terminates_interactive_command") {
        return;
    }
    let root = login_sandbox("posix-env-exit");
    let env_file = stage_login_file(&root, "env2.src", "echo ENV2-RAN-$((2+3))\nexit 5\n");
    let env = ("ENV".to_string(), env_file.to_string_lossy().into_owned());
    let mut s = NiuSession::spawn_shared_home_with_args(
        &root,
        &driver::default_rc(),
        &["--posix", "-i", "-c", "echo AFTER-$((5*5))"],
        &[env],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    s.expect("ENV2-RAN-5");
    let code = s.wait_exit();
    assert_eq!(
        code, 5,
        "the ENV file's exit status is the invocation status"
    );
    s.expect_absent("AFTER-25", ABSENT_WINDOW);
}

/// The REPL login face: the profile chain replaces the rc, runs before the
/// first prompt, and a profile `exit` kills the session with its status.
#[test]
fn issue399_login_repl_profile_exit_ends_session_before_prompt() {
    if !require_pty_or_skip("issue399_login_repl_profile_exit_ends_session_before_prompt") {
        return;
    }
    let root = login_sandbox("repl-login");
    stage_login_file(&root, ".bash_profile", "echo BP-REPL-$((20-20))\nexit 4\n");
    let marker = ("NIU_INVOKED_AS".to_string(), "-niu".to_string());
    let mut s = NiuSession::spawn_shared_home(
        &root,
        "echo FROM-RC\nPS1='P1> '\nPS2='P2> '\nNIU_DISABLE_DEFAULT_PLUGINS=1\n",
        &[marker],
        &[],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    s.expect("Niubash");
    s.expect("BP-REPL-0");
    let code = s.wait_exit();
    let transcript = s.transcript();
    assert_eq!(
        code, 4,
        "the profile's exit status is the session status: {transcript}"
    );
    assert_eq!(
        transcript.matches(driver::PROMPT1).count(),
        0,
        "no prompt may render after a profile exit jump: {transcript}"
    );
    assert!(
        !transcript.contains("FROM-RC"),
        "the login face replaces the rc: {transcript}"
    );
}

/// niubash#422: on the `sh -l -i` REPL face (the argv[0] `sh` face flips
/// posix mode, `-l` flips login), an `exit` in the `$ENV` file ends the
/// session BEFORE the login startup tail — no prompt, no greeting output,
/// the ENV's status is the session status. Oracle: WSL GNU Bash 5.3.0
/// (`sh -l -i` over a scratch HOME, $ENV = "echo ENV-RAN; exit 7") prints
/// the ENV marker and exits 7 with the first prompt never rendered; the
/// control run without the exit renders it. The profile and rc exit jumps
/// already took this early return; the ENV face joined them (the lib-side
/// greeting-marker pin lives in shell.rs).
#[test]
fn issue422_sh_login_repl_env_exit_ends_session_before_startup_tail() {
    if !require_pty_or_skip("issue422_sh_login_repl_env_exit_ends_session_before_startup_tail") {
        return;
    }
    let root = login_sandbox("sh-env-exit");
    stage_login_file(&root, ".bash_profile", "echo BP-RAN-$((3+4))\n");
    // PS1 goes into the ENV file so a wrongly-continued session would
    // render the sentinel: the no-prompt assertion below stays meaningful
    // even though the login face never sources the rc.
    let env_file = stage_login_file(
        &root,
        "env422.src",
        "PS1='P1> '\necho ENV-RAN-$((2+5))\nexit 7\n",
    );
    let env = ("ENV".to_string(), env_file.to_string_lossy().into_owned());
    let mut s = NiuSession::spawn_face_in_root(
        &root,
        "sh",
        &driver::default_rc(),
        // The REPL dispatch: no -c, no script — the face where the login
        // tail (completion sync + finish_startup + greeting) used to run.
        &["-l"],
        &[env],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    s.expect("Niubash");
    s.expect("BP-RAN-7");
    s.expect("ENV-RAN-7");
    let code = s.wait_exit();
    let transcript = s.transcript();
    assert_eq!(
        code, 7,
        "the ENV file's exit status is the session status: {transcript}"
    );
    assert_eq!(
        transcript.matches(driver::PROMPT1).count(),
        0,
        "no prompt may render after the ENV exit jump: {transcript}"
    );
}

/// Control for the jump above: the same `sh -l -i` face with an ENV file
/// that does NOT exit reaches the first prompt and accepts the command
/// line (the guard is the exit jump, not the face).
#[test]
fn issue422_sh_login_repl_env_without_exit_reaches_the_prompt() {
    if !require_pty_or_skip("issue422_sh_login_repl_env_without_exit_reaches_the_prompt") {
        return;
    }
    let root = login_sandbox("sh-env-live");
    // The login face ignores the rc, so the prompt sentinel comes from the
    // profile chain.
    stage_login_file(&root, ".bash_profile", "PS1='P1> '\necho BP-RAN-$((6+1))\n");
    let env_file = stage_login_file(&root, "env422ctl.src", "echo ENV-RAN-$((4+4))\n");
    let env = ("ENV".to_string(), env_file.to_string_lossy().into_owned());
    let mut s = NiuSession::spawn_face_in_root(
        &root,
        "sh",
        &driver::default_rc(),
        &["-l"],
        &[env],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    s.expect("Niubash");
    s.expect("BP-RAN-7");
    s.expect("ENV-RAN-8");
    s.expect_prompt();
    s.send_line("echo AFTER-$((5*5))");
    s.expect("AFTER-25");
    s.expect_prompt();
    // The REPL fast-path exit matches a bare `exit`/`logout` only (repl.rs);
    // the command-line work above already proved the face is interactive.
    s.send_line("exit");
    assert_eq!(s.wait_exit(), 0);
}

// ---------------------------------------------------------------------------
// niubash#400: a shell invoked under the name `rbash` starts restricted.
// Oracle: WSL GNU Bash 5.3.0 via an `rbash` symlink over stdin — `$-` reads
// `hrBc`, `cd` is refused ("cd: restricted"), PATH/SHELL turn read-only,
// command names with `/` are refused, `set +r` cannot lift the mode, and a
// plain `bash`/`niu` name stays unrestricted. The engine's own binary keys
// the same face off argv[0] (rubash shell.c:1258-1299 derivation, #598);
// the host face must assemble the identical executor state.
// ---------------------------------------------------------------------------

/// The restricted-face probe script: every marker is produced by executing a
/// command, never by screen shape. GNU expectations: flags read `hrBc`, the
/// cd is refused with the working directory untouched, PATH/SHELL
/// assignments fail readonly (rc 1), and `set +r` is itself refused.
/// A failed readonly assignment is fatal to the running command list (GNU
/// aborts a `-c` script on `PATH=/x` with rc 1), so the assignment probes
/// run inside subshells: the subshell carries the failure status, the
/// script keeps going.
const RBASH_PROBE: &str = concat!(
    "echo FLAGS=$-; ",
    "cd / && echo CD-OK || echo CD-BLOCKED; ",
    "echo CWD=$PWD; ",
    "( PATH=/x ) 2>/dev/null; echo PATHRC=$?; ",
    "( SHELL=/x ) 2>/dev/null; echo SHELLRC=$?; ",
    "set +r 2>/dev/null; cd / && echo ESCAPED || echo STILL-RESTRICTED; ",
    "echo DONE-$((6*7))",
);

/// The argv[0] face: a niu copy invoked as `rbash.exe` starts restricted.
#[test]
fn issue400_rbash_argv0_face_is_restricted() {
    if !require_pty_or_skip("issue400_rbash_argv0_face_is_restricted") {
        return;
    }
    let mut s = NiuSession::spawn_face_with_args(
        "issue400-rbash-argv0",
        "rbash",
        &driver::default_rc(),
        &["-c", RBASH_PROBE],
        &[],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    s.expect("FLAGS=hrBc");
    s.expect("CD-BLOCKED");
    s.expect_absent("CD-OK", ABSENT_WINDOW);
    s.expect("PATHRC=1");
    s.expect("SHELLRC=1");
    s.expect("STILL-RESTRICTED");
    s.expect_absent("ESCAPED", ABSENT_WINDOW);
    s.expect("DONE-42");
    assert_eq!(s.wait_exit(), 0);
}

/// The flag form and the argv0 face assemble the same state: plain `niu -r`
/// refuses the same probes the `rbash` name does (the flag surface was the
/// earlier half of this contract; the name must not diverge from it).
#[test]
fn issue400_flag_form_matches_the_argv0_face() {
    if !require_pty_or_skip("issue400_flag_form_matches_the_argv0_face") {
        return;
    }
    let mut s = NiuSession::spawn_custom_with_args(
        "issue400-rbash-flag",
        &driver::default_rc(),
        &["-r", "-c", RBASH_PROBE],
        &[],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    s.expect("FLAGS=hrBc");
    s.expect("CD-BLOCKED");
    s.expect("PATHRC=1");
    s.expect("SHELLRC=1");
    s.expect("STILL-RESTRICTED");
    s.expect("DONE-42");
    assert_eq!(s.wait_exit(), 0);
}

/// The shim-forwarded face (`NIU_INVOKED_AS=rbash`, how the rubash bash
/// shim hands its argv[0] stem to the host) is restricted too, and `$0`
/// keeps reading the invocation name like GNU set_shell_name.
#[test]
fn issue400_shim_forwarded_face_is_restricted() {
    if !require_pty_or_skip("issue400_shim_forwarded_face_is_restricted") {
        return;
    }
    let invoked = ("NIU_INVOKED_AS".to_string(), "rbash".to_string());
    let mut s = NiuSession::spawn_custom_with_args(
        "issue400-rbash-shim",
        &driver::default_rc(),
        &[
            "-c",
            "echo FLAGS=$-; echo P0=$0; cd / 2>/dev/null && echo CD-OK || echo CD-BLOCKED",
        ],
        &[invoked],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    s.expect("FLAGS=hrBc");
    s.expect("P0=rbash");
    s.expect("CD-BLOCKED");
    s.expect_absent("CD-OK", ABSENT_WINDOW);
    assert_eq!(s.wait_exit(), 0);
}

/// Control: the same binary under its own name is unrestricted — `cd`
/// succeeds, no `r` in `$-`, PATH assignable, and the `.EXE` spelling is
/// normalized the way the shims spell it.
#[test]
fn issue400_plain_name_and_exe_spelling_control() {
    if !require_pty_or_skip("issue400_plain_name_and_exe_spelling_control") {
        return;
    }
    let mut s = NiuSession::spawn_custom_with_args(
        "issue400-plain-control",
        &driver::default_rc(),
        &[
            "-c",
            "echo FLAGS=$-; cd / && echo CD-OK; PATH=/x; echo PATHRC=$?",
        ],
        &[],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    s.expect("FLAGS=hBc");
    s.expect("CD-OK");
    s.expect("PATHRC=0");
    assert_eq!(s.wait_exit(), 0);

    let mut s2 = NiuSession::spawn_face_with_args(
        "issue400-rbash-upper",
        "RBASH",
        &driver::default_rc(),
        &[
            "-c",
            "echo FLAGS=$-; cd / 2>/dev/null && echo CD-OK || echo CD-BLOCKED",
        ],
        &[],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    s2.expect("FLAGS=hrBc");
    s2.expect("CD-BLOCKED");
    assert_eq!(s2.wait_exit(), 0);
}

// ---------------------------------------------------------------------------
// niubash#413 review round: the rbash face must activate at the same point
// as the `-r` flag (after the startup files), and the #399 login face must
// leave the env-file gates independent of login, exactly like GNU.
// ---------------------------------------------------------------------------

/// Stage `content` at `root/home/<name>`, creating the home first.
fn stage_face_file(root: &std::path::Path, name: &str, content: &str) -> String {
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let path = home.join(name);
    std::fs::write(&path, content).unwrap();
    path.to_string_lossy().into_owned()
}

/// The startup-file line is face-equivalent (niubash#413 review): the rbash
/// argv0 face and the `-r` flag both source a BASH_ENV file and the login
/// profile chain UNRESTRICTED (GNU `rbash -c` loads BASH_ENV; `rbash -l -c`
/// loads the chain), and the command line after the files runs restricted.
#[test]
fn issue400_startup_files_load_unrestricted_like_the_flag_form() {
    if !require_pty_or_skip("issue400_startup_files_load_unrestricted_like_the_flag_form") {
        return;
    }
    let probe = "echo BODY-$((3*3)); cd / 2>/dev/null && echo CD-OK || echo CD-BLOCKED";

    // argv0 face + BASH_ENV: the file loads, the command runs restricted.
    let root = login_sandbox("face-bashenv");
    let env_file = stage_face_file(&root, "env.src", "echo ENV-M-$((2*4))\n");
    let bash_env = ("BASH_ENV".to_string(), env_file);
    let mut s = NiuSession::spawn_face_in_root(
        &root,
        "rbash",
        &driver::default_rc(),
        &["-c", probe],
        &[bash_env],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    s.expect("ENV-M-8");
    s.expect("BODY-9");
    s.expect("CD-BLOCKED");
    s.expect_absent("CD-OK", ABSENT_WINDOW);
    assert_eq!(s.wait_exit(), 0);

    // flag form over the identical staging: byte-for-byte the same markers.
    let root2 = login_sandbox("flag-bashenv");
    let env_file2 = stage_face_file(&root2, "env.src", "echo ENV-M-$((2*4))\n");
    let bash_env2 = ("BASH_ENV".to_string(), env_file2);
    let mut s2 = NiuSession::spawn_shared_home_with_args(
        &root2,
        &driver::default_rc(),
        &["-r", "-c", probe],
        &[bash_env2],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    s2.expect("ENV-M-8");
    s2.expect("BODY-9");
    s2.expect("CD-BLOCKED");
    assert_eq!(s2.wait_exit(), 0);

    // argv0 face + login profile chain: the chain loads unrestricted.
    let root3 = login_sandbox("face-profile");
    stage_face_file(&root3, ".bash_profile", "echo BP-M-$((5*2))\n");
    let mut s3 = NiuSession::spawn_face_in_root(
        &root3,
        "rbash",
        &driver::default_rc(),
        &["-l", "-c", "echo BODY-$((7+7))"],
        &[],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    s3.expect("BP-M-10");
    s3.expect("BODY-14");
    assert_eq!(s3.wait_exit(), 0);

    // flag form + login profile chain: identical.
    let root4 = login_sandbox("flag-profile");
    stage_face_file(&root4, ".bash_profile", "echo BP-M-$((5*2))\n");
    let mut s4 = NiuSession::spawn_shared_home_with_args(
        &root4,
        &driver::default_rc(),
        &["-r", "-l", "-c", "echo BODY-$((7+7))"],
        &[],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    s4.expect("BP-M-10");
    s4.expect("BODY-14");
    assert_eq!(s4.wait_exit(), 0);
}

/// The BASH_ENV gate is independent of login (GNU shell.c:1214-1220 keys on
/// interactivity, not login): `bash -l -c` sources the profile chain AND the
/// env file, profile first.
#[test]
fn issue399_login_noninteractive_sources_bash_env_after_the_chain() {
    if !require_pty_or_skip("issue399_login_noninteractive_sources_bash_env_after_the_chain") {
        return;
    }
    let root = login_sandbox("login-bashenv");
    stage_login_file(&root, ".bash_profile", "echo BP-M-$((5+5))\n");
    let env_file = stage_login_file(&root, "env.src", "echo ENVF-M-$((4+4))\n");
    let bash_env = (
        "BASH_ENV".to_string(),
        env_file.to_string_lossy().into_owned(),
    );
    let mut s = NiuSession::spawn_shared_home_with_args(
        &root,
        &driver::default_rc(),
        &["-l", "-c", "echo BODY"],
        &[bash_env],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    s.expect("BP-M-10");
    s.expect("ENVF-M-8");
    s.expect("BODY");
    assert_eq!(s.wait_exit(), 0);
}

/// The posix `$ENV` gate is independent of login too (GNU shell.c:1238-1246
/// keys on posix+interactive): `sh -l -i -c` sources the profile chain AND
/// $ENV, chain first. An `exit` in the chain still ends the invocation
/// before the env file or the command runs.
#[test]
fn issue399_posix_login_interactive_sources_env_after_the_chain() {
    if !require_pty_or_skip("issue399_posix_login_interactive_sources_env_after_the_chain") {
        return;
    }
    let root = login_sandbox("posix-login-env");
    stage_login_file(&root, ".bash_profile", "echo BP-M-$((5+5))\n");
    let env_file = stage_login_file(&root, "env2.src", "echo ENV2-M-$((4+4))\n");
    let env = ("ENV".to_string(), env_file.to_string_lossy().into_owned());
    let sh_face = ("NIU_INVOKED_AS".to_string(), "sh".to_string());
    let mut s = NiuSession::spawn_shared_home_with_args(
        &root,
        &driver::default_rc(),
        &["-l", "-i", "-c", "echo BODY"],
        &[env, sh_face],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    s.expect("BP-M-10");
    s.expect("ENV2-M-8");
    s.expect("BODY");
    assert_eq!(s.wait_exit(), 0);

    let root2 = login_sandbox("posix-login-exit");
    stage_login_file(&root2, ".bash_profile", "echo BP-X\nexit 9\n");
    let env_file2 = stage_login_file(&root2, "env2.src", "echo ENV2-M\n");
    let env2 = ("ENV".to_string(), env_file2.to_string_lossy().into_owned());
    let sh_face2 = ("NIU_INVOKED_AS".to_string(), "sh".to_string());
    let mut s2 = NiuSession::spawn_shared_home_with_args(
        &root2,
        &driver::default_rc(),
        &["-l", "-i", "-c", "echo AFTER"],
        &[env2, sh_face2],
        (120, 30),
        HEAVY_TIMEOUT,
    );
    s2.expect("BP-X");
    let code = s2.wait_exit();
    assert_eq!(
        code, 9,
        "the profile's exit status is the invocation status"
    );
    s2.expect_absent("ENV2-M", ABSENT_WINDOW);
    s2.expect_absent("AFTER", ABSENT_WINDOW);
}

/// Regression for the `RefCell already borrowed` panic the #399 PR fixed in
/// `run_repl`: the rc-exit Option must be bound outside the `if let`
/// scrutinee, so a plain REPL with `exit 5` in the rc ends with status 5
/// instead of panicking (the old shape exited 1 via the panic path).
#[test]
fn repl_rc_exit_carries_status_without_panic() {
    if !require_pty_or_skip("repl_rc_exit_carries_status_without_panic") {
        return;
    }
    let rc = format!("{}\nexit 5\n", driver::default_rc());
    let mut s = NiuSession::spawn_custom("repl-rc-exit-status", &rc, &[], (120, 30), HEAVY_TIMEOUT);
    s.expect("Niubash");
    let code = s.wait_exit();
    let transcript = s.transcript();
    assert_eq!(
        code, 5,
        "the rc's exit status is the session status: {transcript}"
    );
    assert!(
        !transcript.contains("panicked"),
        "no RefCell panic: {transcript}"
    );
    assert!(
        !transcript.contains(driver::PROMPT1),
        "no prompt may render after the rc exit: {transcript}"
    );
}
