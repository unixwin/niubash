//! niubash#968 — wild (path-added) sources: extension-less entry files
//! with a shebang are assets.
//!
//! A source added by local path is a wild source; its asset discovery
//! used to enumerate `*.sh` / `*.bash` only, so the extension-less entry
//! file `complete_alias` was invisible to `plugin enable`:
//!
//! ```text
//! niu plugin add tests/fixtures/sources/complete-alias
//! niu plugin enable complete-alias/complete_alias
//! # niu: no source or asset named 'complete-alias/complete_alias'
//! ```
//!
//! The fix (captain ruling, option (a)): wild enumeration also admits a
//! visible regular file with no suffix whose first bytes are a shebang
//! (`#!`) — the shebang is the mechanism that marks the file as an
//! executable script entry rather than a data/README file. The Unix
//! executable bit is deliberately not consulted (meaningless on
//! Windows; the fixture itself is mode 644).
//!
//! Fail-first expectation: on the base tree, the enable cell (test 1)
//! and the list cell fail — `complete_alias` is not an asset at all —
//! while the bare-source refusal (test 2) and the no-shebang cells
//! (tests 3–4) already pass on base (they pin the boundary, not the
//! bug). Test 4 is the mutation self-check: the same file name with
//! the shebang stripped must stay invisible, so the pin is sensitive
//! to the mechanism, not to the name.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

fn niu_binary() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_niu"));
    if p.exists() {
        return p;
    }
    let mut fallback = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fallback.push("target");
    fallback.push("debug");
    fallback.push(if cfg!(windows) { "niu.exe" } else { "niubash" });
    fallback
}

fn run_niu_with_env(args: &[&str], envs: &[(&str, String)]) -> Output {
    let mut command = Command::new(niu_binary());
    command.args(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    command
        .output()
        .unwrap_or_else(|err| panic!("failed to run niu {args:?}: {err}"))
}

fn assert_success(output: &Output, context: &str) {
    assert!(
        output.status.success(),
        "{context} failed with {}:\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn stdout_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn temp_dir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("niubash-{name}-{}-{nanos}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn fixture(kind: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("sources")
        .join(kind)
}

/// HOME + USERPROFILE + NIU_PLUGIN_SOURCES_ROOT (+ explicit spec path)
/// pointed at one sandbox so add/enable write into the sandbox, never
/// the real user home.
struct Sandbox {
    home: PathBuf,
}

impl Sandbox {
    fn new(label: &str) -> (Self, Vec<(&'static str, String)>) {
        let temp = temp_dir(label);
        let home = temp.join("home");
        let sources_root = temp.join("sources");
        fs::create_dir_all(&home).unwrap();
        let envs = vec![
            ("HOME", home.to_string_lossy().into_owned()),
            ("USERPROFILE", home.to_string_lossy().into_owned()),
            (
                "NIU_PLUGIN_SOURCES_ROOT",
                sources_root.to_string_lossy().into_owned(),
            ),
            (
                "NIU_PLUGIN_SPEC",
                home.join(".niubash")
                    .join("plugins.json")
                    .to_string_lossy()
                    .into_owned(),
            ),
        ];
        (Sandbox { home }, envs)
    }
}

fn loader_of(sandbox: &Sandbox) -> String {
    fs::read_to_string(
        sandbox
            .home
            .join(".niubash")
            .join("generated")
            .join("plugins.niu"),
    )
    .unwrap_or_default()
}

/// Recursive copy (the mutation cells stage a tweaked copy of the
/// complete-alias fixture tree).
fn copy_dir_recursive(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap().filter_map(Result::ok) {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir_recursive(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// The issue's repro, end to end: add the path, enable the qualified
/// extension-less entry, and prove the wrapper is defined after load.
#[test]
fn wild_extensionless_shebang_entry_enables_by_qualified_name() {
    let (sandbox, envs) = Sandbox::new("968-entry");

    let add = run_niu_with_env(
        &[
            "plugin",
            "add",
            &fixture("complete-alias").to_string_lossy(),
        ],
        &envs,
    );
    assert_success(&add, "plugin add complete-alias");
    assert!(
        stdout_text(&add).contains("Installed source 'complete-alias'"),
        "{}",
        stdout_text(&add)
    );

    // The entry is listed as an asset, tagged as a script.
    let list = run_niu_with_env(&["plugin", "list"], &envs);
    assert_success(&list, "plugin list");
    let list_out = stdout_text(&list);
    assert!(
        list_out.contains("complete_alias"),
        "complete_alias must be listed as an asset: {list_out}"
    );
    assert!(
        list_out.contains("[script]"),
        "the shebang entry is tagged as a script: {list_out}"
    );

    // The qualified enable spelling from the issue now works.
    let enable = run_niu_with_env(
        &["plugin", "enable", "complete-alias/complete_alias"],
        &envs,
    );
    assert_success(&enable, "plugin enable complete-alias/complete_alias");
    let loader = loader_of(&sandbox);
    assert!(
        loader.contains("complete-alias/complete_alias"),
        "the loader must source the entry: {loader}"
    );

    // Engine proof: sourcing the loader defines the wrapper.
    let loader_path = sandbox
        .home
        .join(".niubash")
        .join("generated")
        .join("plugins.niu");
    let probe = run_niu_with_env(
        &[
            "-c",
            &format!(
                ". \"{}\"; printf '%s' \"$(type -t __complete_alias)\"",
                loader_path.to_string_lossy().replace('\\', "/")
            ),
        ],
        &envs,
    );
    assert_success(&probe, "engine wrapper probe");
    assert_eq!(
        stdout_text(&probe).trim_end(),
        "function",
        "the wrapper must be defined after load"
    );

    let _ = fs::remove_dir_all(&sandbox.home.parent().unwrap());
}

/// Bare-source enable still refuses to guess an entry (niubash#968
/// changes enumeration, not the no-guess rule): it lists the
/// candidates, now including the extension-less entry.
#[test]
fn wild_bare_source_enable_still_refuses_to_guess() {
    let (sandbox, envs) = Sandbox::new("968-noguess");

    let add = run_niu_with_env(
        &[
            "plugin",
            "add",
            &fixture("complete-alias").to_string_lossy(),
        ],
        &envs,
    );
    assert_success(&add, "plugin add complete-alias");

    let enable = run_niu_with_env(&["plugin", "enable", "complete-alias"], &envs);
    assert!(
        !enable.status.success(),
        "bare-source enable must still refuse: {}",
        stdout_text(&enable)
    );
    let combined = format!("{}{}", stdout_text(&enable), stderr_text(&enable));
    assert!(combined.contains("pick one"), "{combined}");
    assert!(combined.contains("complete_alias"), "{combined}");
    // The loader always carries its header block, so "nothing sourced"
    // means: no entry line names the asset.
    let loader = loader_of(&sandbox);
    assert!(
        !loader.contains("complete_alias"),
        "nothing sourced yet: {loader}"
    );

    let _ = fs::remove_dir_all(&sandbox.home.parent().unwrap());
}

/// Boundary: an extension-less file WITHOUT a shebang is not a
/// candidate, and neither is a README — the shebang is the gate.
#[test]
fn wild_extensionless_file_without_shebang_is_not_a_candidate() {
    let (sandbox, envs) = Sandbox::new("968-noshebang");
    // The staged tree must keep the tail `complete-alias`: the source
    // id derives from the origin tail.
    let staged = temp_dir("968-noshebang-tree").join("complete-alias");
    copy_dir_recursive(&fixture("complete-alias"), &staged);
    fs::write(staged.join("notes"), "plain notes, no shebang\n").unwrap();

    let add = run_niu_with_env(&["plugin", "add", &staged.to_string_lossy()], &envs);
    assert_success(&add, "plugin add staged tree");

    let list = run_niu_with_env(&["plugin", "list"], &envs);
    assert_success(&list, "plugin list");
    let list_out = stdout_text(&list);
    assert!(list_out.contains("complete_alias"), "{list_out}");
    assert!(
        !list_out.lines().any(|line| line.contains("notes")),
        "a shebang-less file must not be listed: {list_out}"
    );
    assert!(
        !list_out.contains("README"),
        "README must not be listed: {list_out}"
    );

    let enable = run_niu_with_env(&["plugin", "enable", "complete-alias/notes"], &envs);
    assert!(
        !enable.status.success(),
        "enabling a non-asset must fail: {}",
        stdout_text(&enable)
    );
    let combined = format!("{}{}", stdout_text(&enable), stderr_text(&enable));
    assert!(combined.contains("no source or asset"), "{combined}");

    let _ = fs::remove_dir_all(&sandbox.home.parent().unwrap());
    let _ = fs::remove_dir_all(&staged);
}

/// Mutation self-check: the SAME file name with the shebang stripped
/// must stay invisible — the pin keys on the mechanism, not the name.
#[test]
fn wild_extensionless_entry_without_shebang_loses_the_mechanism() {
    let (sandbox, envs) = Sandbox::new("968-mutation");
    // The staged tree must keep the tail `complete-alias`: the source
    // id derives from the origin tail.
    let staged = temp_dir("968-mutation-tree").join("complete-alias");
    copy_dir_recursive(&fixture("complete-alias"), &staged);
    let entry = staged.join("complete_alias");
    let text = fs::read_to_string(&entry).unwrap();
    let stripped = text.replacen("#!/usr/bin/env bash\n", "# no shebang here\n", 1);
    assert_ne!(text, stripped, "the fixture must carry a shebang to strip");
    fs::write(&entry, stripped).unwrap();

    let add = run_niu_with_env(&["plugin", "add", &staged.to_string_lossy()], &envs);
    assert_success(&add, "plugin add staged tree");

    let enable = run_niu_with_env(
        &["plugin", "enable", "complete-alias/complete_alias"],
        &envs,
    );
    assert!(
        !enable.status.success(),
        "without the shebang the entry must stay invisible: {}",
        stdout_text(&enable)
    );
    let combined = format!("{}{}", stdout_text(&enable), stderr_text(&enable));
    assert!(
        combined.contains("no source or asset named 'complete-alias/complete_alias'"),
        "{combined}"
    );

    let _ = fs::remove_dir_all(&sandbox.home.parent().unwrap());
    let _ = fs::remove_dir_all(&staged);
}
