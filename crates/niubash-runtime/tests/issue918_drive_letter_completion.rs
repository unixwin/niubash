//! niubash#918 regression pins: a Windows drive-letter word
//! (`C:/...` / `C:\...`) must survive completion whole.
//!
//! Under the default `COMP_WORDBREAKS` (which contains `:`, GNU
//! bashline.c:313), the engine's completion split cut the word at
//! the drive colon, so the current word became the post-drive
//! fragment (`/tmp/.../pl`), file candidates for the real path
//! resolved to nothing, and a governing `-F` function was handed
//! the fragment as `$2`/`COMP_WORDS[COMP_CWORD]` instead of the
//! word the user typed. `compgen` on the same word worked because
//! it parses the original word text (rubash#2846 has the engine
//! side of this face).
//!
//! The host fix is a scoped repair in the completion drivers: for
//! the duration of one attempt whose current word is drive-rooted,
//! the colon leaves `COMP_WORDBREAKS`; the user's value is restored
//! verbatim when the attempt ends. These pins cover the mechanism
//! through the public API — what the completion function observes,
//! what candidates come back, and that the session value is
//! untouched afterwards — plus the control shapes that must NOT
//! change (a colon in a non-drive word still splits).

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard};

use niubash_runtime::completion::{CompletionState, NiubashCompleter};
use niubash_runtime::shell::Shell;

/// The engine's host write path mirrors `COMP_WORDBREAKS` into the
/// process environment, and a `Shell` created afterwards imports
/// the process environment — so these pins must not run
/// concurrently inside one test process (the same hazard the
/// in-crate tests serialize with `PROCESS_STATE_LOCK`).
static ISSUE918_LOCK: Mutex<()> = Mutex::new(());

fn serialized() -> MutexGuard<'static, ()> {
    ISSUE918_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

const WORDLIST_SCRIPT: &str = r#"
complete -W 'C:/tmp/issue918/plain.txt C:/tmp/issue918/other.txt' mycmd
"#;

const PROBE_FN_SCRIPT: &str = r#"
_niubash918_probe() {
    NIU918_CUR=$2
    NIU918_WORDS=${COMP_WORDS[*]}
    NIU918_CWORD=$COMP_CWORD
    NIU918_BREAKS=$COMP_WORDBREAKS
    COMPREPLY=(hit)
}
complete -F _niubash918_probe mycmd
"#;

fn completer_for(shell: Shell) -> (Rc<RefCell<Shell>>, NiubashCompleter) {
    let handle = Rc::new(RefCell::new(shell));
    let state = Arc::new(Mutex::new(CompletionState::new(PathBuf::from("."))));
    let completer = NiubashCompleter::with_shell(state, handle.clone());
    (handle, completer)
}

fn shell_with(script: &str) -> Shell {
    let mut shell = Shell::new().unwrap();
    shell.execute_script(script).unwrap();
    shell
}

fn complete_values(completer: &mut NiubashCompleter, line: &str) -> Vec<String> {
    completer
        .complete(line, line.len())
        .into_iter()
        .map(|s| s.value)
        .collect()
}

fn session_value(shell: &Rc<RefCell<Shell>>, name: &str) -> Option<String> {
    shell.borrow().session().variable_value(name)
}

/// The headline shape: a wordlist compspec whose candidates are
/// full drive paths must serve the one matching the typed prefix.
/// Before the fix the split made the current word `/tmp/.../pl`,
/// which prefixes none of the candidates, so the user got `[]`.
#[test]
fn issue918_drive_word_serves_full_path_candidates() {
    let _lock = serialized();
    let (_handle, mut completer) = completer_for(shell_with(WORDLIST_SCRIPT));
    let values = complete_values(&mut completer, "mycmd C:/tmp/issue918/pl");
    assert_eq!(
        values,
        vec!["C:/tmp/issue918/plain.txt".to_string()],
        "the drive word must complete whole, not as its post-colon fragment"
    );
}

/// The backslash spelling of the same drive word: the completion
/// function must be handed the whole word, exactly as typed.
/// (Wordlist prefix filtering is literal, so this spelling is
/// pinned through the function's own view, not a `/`-spelled
/// wordlist.)
#[test]
fn issue918_drive_word_backslash_spelling() {
    let _lock = serialized();
    let (handle, mut completer) = completer_for(shell_with(PROBE_FN_SCRIPT));
    let values = complete_values(&mut completer, "mycmd C:\\tmp\\issue918\\pl");
    assert_eq!(values, vec!["hit".to_string()]);
    assert_eq!(
        session_value(&handle, "NIU918_CUR").as_deref(),
        Some("C:\\tmp\\issue918\\pl"),
        "the backslash drive spelling must reach the function whole"
    );
    assert_eq!(
        session_value(&handle, "NIU918_WORDS").as_deref(),
        Some("mycmd C:\\tmp\\issue918\\pl"),
        "COMP_WORDS must carry the backslash drive word whole"
    );
}

/// What the governing completion function observes: `$2`,
/// `COMP_WORDS` and `COMP_CWORD` must name the whole drive word,
/// exactly as the host's own token view spells it.
#[test]
fn issue918_completion_function_sees_the_whole_drive_word() {
    let _lock = serialized();
    let (handle, mut completer) = completer_for(shell_with(PROBE_FN_SCRIPT));
    let values = complete_values(&mut completer, "mycmd C:/tmp/issue918/pl");
    assert_eq!(values, vec!["hit".to_string()]);
    assert_eq!(
        session_value(&handle, "NIU918_CUR").as_deref(),
        Some("C:/tmp/issue918/pl"),
        "$2 must be the whole drive word"
    );
    assert_eq!(
        session_value(&handle, "NIU918_WORDS").as_deref(),
        Some("mycmd C:/tmp/issue918/pl"),
        "COMP_WORDS must carry the whole drive word"
    );
    assert_eq!(
        session_value(&handle, "NIU918_CWORD").as_deref(),
        Some("1"),
        "COMP_CWORD must index the drive word's slot"
    );
}

/// During the repaired attempt the function observes the session's
/// wordbreaks with only the colon removed (a user-customised value
/// keeps its other characters); after the attempt the user's value
/// is back verbatim.
#[test]
fn issue918_wordbreaks_scoped_to_the_attempt_and_restored() {
    let _lock = serialized();
    let mut shell = shell_with(PROBE_FN_SCRIPT);
    shell.execute_script("COMP_WORDBREAKS=' :XY'").unwrap();
    let (handle, mut completer) = completer_for(shell);
    let _ = complete_values(&mut completer, "mycmd C:/tmp/issue918/pl");
    assert_eq!(
        session_value(&handle, "NIU918_BREAKS").as_deref(),
        Some(" XY"),
        "inside the attempt only the colon leaves the user's wordbreaks"
    );
    assert_eq!(
        session_value(&handle, "COMP_WORDBREAKS").as_deref(),
        Some(" :XY"),
        "the user's COMP_WORDBREAKS must be restored verbatim after the attempt"
    );
}

/// Control: a colon inside a word that is not drive-rooted keeps
/// the stock split — the repair is scoped to the drive shape, it
/// does not rewrite wordbreak semantics for the session.
#[test]
fn issue918_non_drive_colon_word_still_splits() {
    let _lock = serialized();
    let (handle, mut completer) = completer_for(shell_with(PROBE_FN_SCRIPT));
    let _ = complete_values(&mut completer, "mycmd ab:cd/pl");
    assert_eq!(
        session_value(&handle, "NIU918_CUR").as_deref(),
        Some("cd/pl"),
        "a non-drive colon word must keep the default split"
    );
    let breaks = session_value(&handle, "NIU918_BREAKS").unwrap();
    assert!(
        breaks.contains(':'),
        "no scoped repair may engage for a non-drive word, got {breaks:?}"
    );
}

/// Control: an ordinary path word observes the session wordbreaks
/// untouched (the colon stays in place for the whole attempt).
#[test]
fn issue918_plain_word_leaves_wordbreaks_alone() {
    let _lock = serialized();
    let (handle, mut completer) = completer_for(shell_with(PROBE_FN_SCRIPT));
    let _ = complete_values(&mut completer, "mycmd /tmp/issue918/pl");
    assert_eq!(
        session_value(&handle, "NIU918_CUR").as_deref(),
        Some("/tmp/issue918/pl")
    );
    let breaks = session_value(&handle, "NIU918_BREAKS").unwrap();
    assert!(
        breaks.contains(':'),
        "a plain word must not trigger the scoped repair, got {breaks:?}"
    );
}

/// The `--completion-probe` driver shares the repair: it is the
/// same completion machinery reached through the probe surface.
#[test]
fn issue918_completion_probe_sees_the_whole_drive_word() {
    let _lock = serialized();
    let mut shell = shell_with(PROBE_FN_SCRIPT);
    let line = "mycmd C:/tmp/issue918/pl";
    let values = shell.completion_probe(line, line.len());
    assert_eq!(
        values,
        vec!["hit".to_string()],
        "the probe must serve the function's candidate"
    );
    assert_eq!(
        shell.session().variable_value("NIU918_CUR").as_deref(),
        Some("C:/tmp/issue918/pl"),
        "the probe's attempt must hand the function the whole drive word"
    );
    let breaks = shell.session().variable_value("COMP_WORDBREAKS").unwrap();
    assert!(
        breaks.contains(':'),
        "the probe must restore the default wordbreaks, got {breaks:?}"
    );
}
