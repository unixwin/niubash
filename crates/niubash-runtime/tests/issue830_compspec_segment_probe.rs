//! niubash#830 regression pins: the compspec probe must name the
//! command word of the cursor's segment, not the first whitespace
//! token of the whole line.
//!
//! Before the fix, `NiubashCompleter` probed
//! `input.split_whitespace().next()`, so on 'true; git <TAB>' it
//! asked the engine about "true;", missed git's compspec, and let
//! the static definition tables answer with the wrong candidates.
//! The probe now goes through `CompletionContext::get_command_name`,
//! which is quote/extglob-aware, follows the last command separator,
//! and skips grouping openers and assignment prefixes.
//!
//! These pins cover the context machinery through the public API;
//! the end-to-end compspec ownership shapes ('true; git ' yielding
//! exactly the compspec's candidates, no static-table leak) are
//! pinned by the in-crate unit test
//! `issue830_compspec_probe_uses_cursor_segment_command_word`.

use std::path::PathBuf;

use niubash_runtime::completion::CompletionContext;

fn command_name(line: &str) -> Option<String> {
    CompletionContext::new(PathBuf::from("."), line.to_string(), line.len()).get_command_name()
}

/// The command name of `line` with `extglob` on (niubash#530 +
/// GAP-9, niubash#863): the extglob `|` verdict holds only with the
/// option enabled — with it off GNU parses `ls @(a|b)` as a
/// pipeline and the engine view reports exactly that (see the lib
/// pin `test_extglob_pipe_is_not_a_command_separator`). The context
/// is built from a live session with the option enabled, and the
/// process-global gate is restored before returning.
fn command_name_with_extglob(line: &str) -> Option<String> {
    let mut shell = niubash_runtime::shell::Shell::new().unwrap();
    shell.execute_script("shopt -s extglob").unwrap();
    let mut ctx = CompletionContext::new(PathBuf::from("."), line.to_string(), line.len());
    ctx.token_view = Some(shell.session().token_view(line));
    shell.execute_script("shopt -u extglob").unwrap();
    ctx.get_command_name()
}

#[test]
fn issue830_command_name_follows_the_cursor_segment() {
    // The issue's regression shapes: the command word is git on all
    // of them, never the line's first whitespace token.
    assert_eq!(command_name("true; git ").as_deref(), Some("git"));
    assert_eq!(command_name("VAR=1 git ").as_deref(), Some("git"));
    assert_eq!(command_name("( git ").as_deref(), Some("git"));
    assert_eq!(command_name("sudo x; git ").as_deref(), Some("git"));
    assert_eq!(command_name("nospec; git ").as_deref(), Some("git"));
}

#[test]
fn issue830_command_name_keeps_plain_and_quoted_shapes() {
    assert_eq!(command_name("git ").as_deref(), Some("git"));
    assert_eq!(command_name("echo hi | gre").as_deref(), Some("gre"));
    // niubash#530: an extglob `|` does not split the segment — with
    // `extglob` on (see `command_name_with_extglob`).
    assert_eq!(
        command_name_with_extglob("ls @(a|b)").as_deref(),
        Some("ls")
    );
    // Multiple assignment prefixes still name the command.
    assert_eq!(command_name("A=1 B=2 git ").as_deref(), Some("git"));
    // A lone assignment names no command word beyond itself.
    assert_eq!(command_name("VAR=1 ").as_deref(), Some("VAR=1"));
}
