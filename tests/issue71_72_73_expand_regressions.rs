//! niubash#71 / #72 / #73 regressions: the three v1.0.1 expansion
//! regressions found by the same 246-case differential batch against
//! GNU bash. All expectations below are the GNU outputs recorded in the
//! issues.
//!
//! - #71: two command substitutions inside ONE double-quoted word — the
//!   first substitution's closing paren leaked into the output as a
//!   literal (`A)B` instead of `AB`).
//! - #72: `"${arr[*]}"` must join with the first character of IFS, not a
//!   hard-coded space.
//! - #73: `${arr[@]+"${arr[@]}"}` mis-parsed — at top level it swallowed
//!   the next line into the same command; inside a function it aborted
//!   the whole script with a syntax error blamed on the function header.

use std::path::PathBuf;
use std::process::Command;

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

fn run_c(script: &str) -> String {
    let output = Command::new(niu_binary())
        .arg("-c")
        .arg(script)
        .env("NIU_SKIP_WINUXCMD_ACTIVATION", "1")
        .output()
        .expect("spawn niu");
    assert!(
        output.status.success(),
        "niu -c failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn run_file(script: &str, tag: &str) -> String {
    let dir = std::env::temp_dir().join(format!("niu-w2-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("repro.sh");
    std::fs::write(&path, script).unwrap();
    let output = Command::new(niu_binary())
        .arg(&path)
        .env("NIU_SKIP_WINUXCMD_ACTIVATION", "1")
        .output()
        .expect("spawn niu");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        output.status.success(),
        "script failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn issue71_two_command_substitutions_in_one_quoted_word() {
    let out = run_c("echo \"$(echo A)$(echo B)\"\necho \"$(echo A)|$(echo B)\"");
    assert_eq!(out, "AB\nA|B\n");
}

#[test]
fn issue72_array_star_join_uses_first_ifs_char() {
    let out = run_c("a=(x y)\nIFS=-\necho \"${a[*]}\"");
    assert_eq!(out, "x-y\n");
}

#[test]
fn issue73_alternate_array_expansion_keeps_next_line() {
    let out = run_c("a=(x y)\necho ${a[@]+\"${a[@]}\"}\necho NEXTLINE");
    assert_eq!(out, "x y\nNEXTLINE\n");
}

#[test]
fn issue73_alternate_array_expansion_inside_function() {
    let script =
        "f() {\n  local -a a=()\n  echo ${a[@]+\"${a[@]}\"}\n  echo inner\n}\nf\necho after\n";
    let out = run_file(script, "i73");
    assert_eq!(out, "\ninner\nafter\n");
}
