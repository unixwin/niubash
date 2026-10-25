//! niubash#19 regression: IFS-based word splitting of an unquoted parameter
//! expansion into `set -- $var` must follow bash — a custom IFS splits the
//! value on its characters instead of leaving the value as one field.
//!
//! GNU bash expectation (the issue's own repro): with `IFS='"'`,
//! `line='a"b"c'` splits into three fields a / b / c. The broken build
//! produced a single field containing the whole value.

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

#[test]
fn issue19_custom_ifs_splits_unquoted_expansion_in_set_args() {
    let out = run_c(
        "line='a\"b\"c'\n\
         old=$IFS\n\
         IFS='\"'\n\
         set -- $line\n\
         IFS=$old\n\
         printf 'n=%s one=%s two=%s three=%s\n' \"$#\" \"$1\" \"$2\" \"$3\"",
    );
    assert_eq!(out, "n=3 one=a two=b three=c\n");
}

#[test]
fn issue19_default_ifs_still_splits_on_whitespace() {
    let out = run_c("v='a  b\tc'\nset -- $v\nprintf 'n=%s\n' \"$#\"");
    assert_eq!(out, "n=3\n");
}
