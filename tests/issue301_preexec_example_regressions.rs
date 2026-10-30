//! niubash#301 — help's example repo rcrowley/bash-preexec is 404
//! (moved to rcaloras/bash-preexec).
//!
//! The `niu plugin add` help example is copy-paste surface: the repo
//! it names must exist. The upstream project moved from
//! rcrowley/bash-preexec to rcaloras/bash-preexec; the old name 404s.
//! These guards pin the live name in both copy-paste surfaces (the
//! CLI help text and the bundled plugin recipe) and keep the dead
//! name out of all shipped source. (CHANGELOG.md keeps the old name
//! as a historical record and is deliberately out of scope.)

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn collect_rs_toml_and_json(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_toml_and_json(&path, out);
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("rs" | "toml" | "json")
        ) {
            out.push(path);
        }
    }
}

#[test]
fn help_example_names_the_live_preexec_repo() {
    let main_rs = read(&repo_root().join("src/main.rs"));
    assert!(
        main_rs.contains("rcaloras/bash-preexec"),
        "the plugin help example must name rcaloras/bash-preexec"
    );
    let recipes = read(&repo_root().join("crates/niubash-runtime/assets/plugins/recipes.json"));
    assert!(
        recipes.contains("rcaloras/bash-preexec"),
        "the bundled bash-preexec recipe must point at rcaloras/bash-preexec"
    );
}

#[test]
fn dead_preexec_repo_name_absent_from_shipped_source() {
    let mut files = Vec::new();
    collect_rs_toml_and_json(&repo_root().join("src"), &mut files);
    collect_rs_toml_and_json(&repo_root().join("crates"), &mut files);
    assert!(!files.is_empty(), "source scan must find files");
    for file in files {
        let text = read(&file);
        assert!(
            !text.contains("rcrowley"),
            "{} still references the 404 rcrowley/bash-preexec",
            file.display()
        );
    }
}
