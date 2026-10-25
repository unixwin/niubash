//! niubash#102 regression: `export -p` must not emit environment entries
//! whose names are not valid shell identifiers (inherited on Windows
//! with names containing parentheses, e.g. `CommonProgramFiles(x86)`).
//! Tools snapshot the environment by saving `export -p` output and
//! sourcing it back; an unquoted parenthesised name made every such
//! snapshot a syntax error. GNU keeps those entries out of the variable
//! table entirely, so the listing never shows them — while they still
//! reach child processes through the environment.
//!
//! The child runs with a cleared environment so the assertion cannot be
//! confused by whatever the test harness itself inherited.

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

#[test]
fn issue102_export_p_omits_invalid_identifier_names() {
    let output = Command::new(niu_binary())
        .arg("-c")
        .arg("export -p")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("NIU_SKIP_WINUXCMD_ACTIVATION", "1")
        .env("TEST(BRACKET)", "x")
        .env("NIU_PIN_OK", "1")
        .output()
        .expect("spawn niu");
    assert!(
        output.status.success(),
        "niu -c failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("TEST(BRACKET)"),
        "export -p leaked an invalid identifier name:\n{stdout}"
    );
    // Sanity: the listing itself works — a normal exported name shows.
    assert!(
        stdout.contains("NIU_PIN_OK"),
        "export -p did not list a normal exported variable:\n{stdout}"
    );
}
