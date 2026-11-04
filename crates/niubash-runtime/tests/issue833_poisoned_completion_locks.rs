//! niubash#833 regression pins: a poisoned completion lock must not
//! freeze the completion snapshot or zero out every candidate.
//!
//! Before the fix, every lock site on the completion path used
//! `if let Ok(...) = lock()` — one panic while a writer held the
//! `completion_state` lock poisoned it, after which
//! `update_completion_state` was skipped forever (stale cwd/env/
//! aliases/functions with zero signal) and the completer returned a
//! silent empty candidate list, indistinguishable from niubash#828.
//! The fix recovers the guard with the in-tree idiom
//! `lock().unwrap_or_else(|e| e.into_inner())`, the same graceful
//! degradation `DirCacheHandle::listing` already modeled.
//!
//! The mem-cache face (external.rs) and the command-cache face
//! (command.rs) are pinned by in-crate unit tests of the same name,
//! since those mutexes are not reachable from this API surface.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use niubash_runtime::completion::{CompletionState, NiubashCompleter, SpecificCompletion};

/// A completion state whose lock has been poisoned by a panicking
/// writer, with the standard plugins loaded.
fn poisoned_state() -> Arc<Mutex<CompletionState>> {
    let state = Arc::new(Mutex::new(CompletionState::new(PathBuf::from("."))));
    {
        let mut s = state.lock().unwrap();
        s.load_completion_dirs(&[]);
    }
    let poison = state.clone();
    let handle = std::thread::spawn(move || {
        let _guard = poison.lock().unwrap();
        panic!("issue833: deliberate completion_state poison");
    });
    assert!(handle.join().is_err());
    assert!(
        state.lock().is_err(),
        "the lock must actually be poisoned for these pins to mean anything"
    );
    state
}

#[test]
fn issue833_poisoned_state_still_completes_commands() {
    let state = poisoned_state();
    let mut completer = NiubashCompleter::new(state);
    let values: Vec<String> = completer
        .complete("ech", 3)
        .into_iter()
        .map(|s| s.value)
        .collect();
    assert!(
        values.iter().any(|v| v == "echo"),
        "a poisoned state lock must not zero out candidates, got {values:?}"
    );
}

#[test]
fn issue833_poisoned_state_still_completes_specific() {
    let state = poisoned_state();
    let mut completer = NiubashCompleter::new(state);
    let values: Vec<String> = completer
        .complete_specific(SpecificCompletion::Command, "ech", 3)
        .into_iter()
        .map(|s| s.value)
        .collect();
    assert!(
        values.iter().any(|v| v == "echo"),
        "complete_specific must survive a poisoned state lock, got {values:?}"
    );
}

#[test]
fn issue833_poisoned_state_update_state_still_applies() {
    let state = poisoned_state();
    let completer = NiubashCompleter::new(state.clone());
    let mut env = HashMap::new();
    env.insert("ISSUE833_MARKER".to_string(), "1".to_string());
    completer.update_state(PathBuf::from("/issue833/dir"), env);

    let s = state.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(s.current_dir, PathBuf::from("/issue833/dir"));
    assert_eq!(
        s.env_vars.get("ISSUE833_MARKER").map(String::as_str),
        Some("1"),
        "update_state must not be silently dropped on a poisoned lock"
    );
}
