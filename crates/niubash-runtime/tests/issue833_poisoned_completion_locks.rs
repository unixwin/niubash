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
fn issue833_poisoned_state_still_completes_filenames() {
    // GAP-4 (niubash#863): this pin used to assert that
    // `NiubashCompleter::update_state` — the seam that wrote a
    // caller-supplied env snapshot into the state — still applied
    // on a poisoned lock. That seam retired with the snapshot it
    // fed: the completer now captures the engine's live view
    // through the bridge, and there is no env snapshot left to
    // update. The surviving #833 contract on this surface is the
    // one the other two pins assert for commands, extended to the
    // host-carried faces: a poisoned state lock must not zero out
    // filename candidates built from the state's directory either.
    let dir =
        std::env::temp_dir().join(format!("niubash-issue833-filename-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("issue833file.txt"), "x").unwrap();

    let state = Arc::new(Mutex::new(CompletionState::new(dir.clone())));
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
    assert!(state.lock().is_err());

    let mut completer = NiubashCompleter::new(state);
    let values: Vec<String> = completer
        .complete_specific(
            SpecificCompletion::Filename,
            "cat issue833f",
            "cat issue833f".len(),
        )
        .into_iter()
        .map(|s| s.value)
        .collect();
    assert!(
        values.iter().any(|v| v == "issue833file.txt"),
        "filename completion must survive a poisoned state lock, got {values:?}"
    );

    let _ = std::fs::remove_dir_all(dir);
}
