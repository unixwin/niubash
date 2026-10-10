//! niubash#182 — bash-it themes (codeword/gitline) hang the interactive
//! session after `source`.
//!
//! Both themes run `_save-and-reload-history 1` from PROMPT_COMMAND on every
//! prompt: `history -a && history -c && history -r`. Before the fix the
//! host history provider's `append_history` re-appended the WHOLE in-memory
//! list to HISTFILE on every prompt while `read_history` reloaded the grown
//! file back into memory, so the file and the memory list doubled every
//! prompt cycle (self-amplifying loop) and the session wedged after a
//! handful of prompts.
//!
//! GNU semantics (lib/readline/histfile.c, builtins/history.def):
//! - `history -a` appends only the entries added since the last history
//!   write and does not disturb the in-memory list;
//! - `history -c` clears the in-memory list only;
//! - `history -r` re-reads the file into memory.
//! The interlock test below drives N prompt cycles under ConPTY with a
//! deadline and asserts the history file stays bounded and correct.

#[path = "interactive/driver.rs"]
mod driver;

use driver::{require_pty_or_skip, NiuSession, DEFAULT_TIMEOUT};

/// The bash-it codeword/gitline pattern, self-contained: every prompt runs
/// `history -a && history -c && history -r` (HISTCONTROL is set by the
/// themes as well).
const THEME_RC: &str = "\
PS1='P1> '
PS2='P2> '
HISTFILE=$HOME/.niubash_history
NIU_DISABLE_DEFAULT_PLUGINS=1
HISTCONTROL=ignoredups
_save-and-reload-history() {
  history -a && history -c && history -r
}
safe_append_prompt_command() {
  case \"$PROMPT_COMMAND\" in
    *\"$1\"*) ;;
    '') PROMPT_COMMAND=\"$1\" ;;
    *) PROMPT_COMMAND=\"$PROMPT_COMMAND;$1\" ;;
  esac
}
safe_append_prompt_command '_save-and-reload-history 1'
";

/// Prompt cycles the session must survive. The pre-fix loop doubles the
/// history file every cycle and wedges well before 12.
const PROMPT_CYCLES: usize = 12;

/// Per-cycle deadline. The driver's DEFAULT_TIMEOUT (10 s) bounds every
/// expect: a healthy session answers a trivial echo in well under a second
/// even with a full history file, while the pre-fix interlock wedges the
/// session and fails the expect.

#[test]
fn bashit_theme_history_interlock_survives_prompt_cycles() {
    if !require_pty_or_skip("bashit_theme_history_interlock_survives_prompt_cycles") {
        return;
    }
    let mut s = NiuSession::spawn_custom(
        "issue182-interlock",
        THEME_RC,
        &[],
        (120, 30),
        DEFAULT_TIMEOUT,
    );
    s.wait_ready();

    for cycle in 0..PROMPT_CYCLES {
        let marker = format!("issue182-marker-{cycle}");
        s.send_line(&format!("echo {marker}"));
        s.expect(&marker);
        // Re-arm on the prompt before the next send: a command issued while
        // the previous line is still being consumed rides the typeahead
        // reinjection and can re-execute a stale line, which would masquerade
        // as history duplication.
        s.expect_prompt();
    }

    // The session is still alive: one more full prompt round-trip works.
    s.send_line("echo issue182-still-alive");
    s.expect("issue182-still-alive");

    // The history file must hold each command exactly once — the pre-fix
    // loop duplicated entries every prompt cycle.
    let history_path = s.home().to_path_buf().join(".niubash_history");
    let contents = std::fs::read_to_string(&history_path).unwrap_or_default();
    let lines: Vec<&str> = contents.lines().filter(|l| !l.trim().is_empty()).collect();
    for cycle in 0..PROMPT_CYCLES {
        let marker = format!("echo issue182-marker-{cycle}");
        let count = lines.iter().filter(|l| **l == marker).count();
        assert_eq!(
            count, 1,
            "history entry {marker:?} appears {count} times (interlock duplication); \
             file:\n{contents}"
        );
    }
    // Bounded: the file holds the markers plus at most a small tail of
    // session bookkeeping lines, never per-cycle doublings.
    assert!(
        lines.len() <= PROMPT_CYCLES + 8,
        "history file ballooned to {} lines (interlock amplification):\n{contents}",
        lines.len()
    );
}
