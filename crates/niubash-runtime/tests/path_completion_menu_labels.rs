//! Regression pin: completing inside a subfolder (`sub/fi<TAB>`) must
//! list menu labels as the last path component (`file.txt`, `inner/`),
//! as GNU readline does — not `sub/file.txt`. The inserted `value`
//! keeps the full word so accepting a candidate still yields a
//! correct path.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use niubash_runtime::completion::{CompletionState, NiubashCompleter};
use niubash_runtime::shell::Shell;

#[test]
fn subfolder_completion_menu_shows_basenames_but_inserts_full_path() {
    let dir = std::env::temp_dir().join(format!("niubash-menu-labels-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub").join("inner")).unwrap();
    std::fs::write(dir.join("sub").join("file.txt"), "x").unwrap();

    let mut shell = Shell::new().unwrap();
    shell
        .execute_script(&format!(
            "cd '{}'",
            dir.display().to_string().replace('\\', "/")
        ))
        .unwrap();
    let handle = Rc::new(RefCell::new(shell));
    let state = Arc::new(Mutex::new(CompletionState::new(PathBuf::from(&dir))));
    let mut completer = NiubashCompleter::with_shell(state, handle);

    let line = "cat sub/";
    let suggestions = completer.complete(line, line.len());
    let _ = std::fs::remove_dir_all(&dir);

    let mut pairs: Vec<(String, String)> = suggestions
        .iter()
        .map(|s| (s.value.clone(), s.display_value().to_string()))
        .collect();
    pairs.sort();
    assert_eq!(
        pairs,
        vec![
            ("sub/file.txt".to_string(), "file.txt".to_string()),
            ("sub/inner/".to_string(), "inner/".to_string()),
        ]
    );
}
