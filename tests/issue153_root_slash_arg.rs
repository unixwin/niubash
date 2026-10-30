//! niubash#153: a single-character `/` argument must reach external children
//! verbatim. GNU hands argv to execve unchanged (bash execute_cmd.c:6126
//! shell_execve) — `/` is `expr`'s division operator and `tr`'s SET1 member,
//! not a path only the shell can decide. The engine used to translate a bare
//! `/` to the configured shell root (install tree) whenever one was set,
//! which broke `expr 10 / 3` (rc=2, root path reported as the unexpected
//! argument) and silently corrupted `tr / X`. Fixed in rubash
//! (windows_external_absolute_argument_needs_translation); these tests pin
//! the product behavior with NIU_ROOT configured, simulating the installed
//! layout where the regression fired.
//!
//! The child used for the argv probe is niu itself, so the suite needs no
//! external Unix tools on PATH.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

fn niu_binary() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_niu"));
    if p.exists() {
        return p;
    }
    let mut fallback = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fallback.push("target");
    fallback.push("debug");
    fallback.push(if cfg!(windows) { "niu.exe" } else { "niu" });
    fallback
}

/// Run `niu` with a configured NIU_ROOT (installed-layout shell root) so
/// the root under test is exactly `root`.
fn run_with_root(script: &str, root: &Path) -> Output {
    let mut command = Command::new(niu_binary());
    command
        .arg("-c")
        .arg(script)
        .env("NIU_ROOT", root)
        .stdin(Stdio::null());
    command
        .output()
        .unwrap_or_else(|err| panic!("spawn niu: {err}"))
}

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("{}-{}-{}", prefix, std::process::id(), nanos))
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_string()
}

/// A bare `/` stays a data character in every argument position — quoted,
/// unquoted, or carried by a variable — instead of becoming the install
/// root path.
#[test]
fn bare_slash_argument_reaches_children_verbatim() {
    let root = unique_temp_dir("niu-153-bare");
    std::fs::create_dir_all(&root).unwrap();

    // Child niu echoes its positional parameters back; the word `/` must
    // survive the parent's spawn translation unchanged.
    let output = run_with_root("niu -c 'printf \"[%s]\\n\" \"$1\"' probe /", &root);
    assert!(
        output.status.success(),
        "single-slash probe failed: {}",
        stderr_of(&output)
    );
    assert_eq!(stdout_of(&output), "[/]", "stderr: {}", stderr_of(&output));

    // Word positions around a bare `/` keep their identity (an `expr`-style
    // operand row: `expr 10 / 3` must see three words, 10, /, 3).
    let output = run_with_root(
        "niu -c 'printf \"[%s][%s][%s]\\n\" \"$1\" \"$2\" \"$3\"' probe 10 / 3",
        &root,
    );
    assert!(
        output.status.success(),
        "operand-row probe failed: {}",
        stderr_of(&output)
    );
    assert_eq!(stdout_of(&output), "[10][/][3]");

    // Variable-carried and quoted spellings must behave the same.
    let output = run_with_root(
        "d=/; niu -c 'printf \"[%s][%s]\\n\" \"$1\" \"$2\"' probe \"$d\" \"/\"",
        &root,
    );
    assert!(
        output.status.success(),
        "variable-carried probe failed: {}",
        stderr_of(&output)
    );
    assert_eq!(stdout_of(&output), "[/][/]");

    let _ = std::fs::remove_dir_all(&root);
}

/// The fix must not un-map the logical install tree: root-relative redirect
/// targets and `[[ -f ]]` operands still resolve under NIU_ROOT (that is the
/// shell_path_to_windows layer, distinct from the external-argument gate).
#[test]
fn root_relative_paths_still_map_to_the_install_root() {
    let root = unique_temp_dir("niu-153-redirect");
    std::fs::create_dir_all(&root).unwrap();

    let output = run_with_root(
        "printf 'hi\\n' > /probe153.txt\n\
         if [[ ! -f /probe153.txt ]]; then echo MISSING; exit 9; fi\n\
         read -r line < /probe153.txt\n\
         printf '%s\\n' \"$line\"",
        &root,
    );
    assert!(
        output.status.success(),
        "root-relative redirect roundtrip failed: {}",
        stderr_of(&output)
    );
    assert_eq!(stdout_of(&output), "hi");
    assert!(
        root.join("probe153.txt").is_file(),
        "redirect target must land under the configured root"
    );

    let _ = std::fs::remove_dir_all(&root);
}
