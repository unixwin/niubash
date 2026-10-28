//! niubash#76 / #93 regression: `find` as the FIRST stage of a pipeline
//! inside a command substitution lost its entire output, and the same
//! shape reached through process substitution + `eval` leaked find's
//! output onto the shell's stdout while the reader saw EOF. Other
//! producers (ls/seq/printf) in the same position worked, and find alone
//! or in a top-level pipeline worked. #93 is the v1.1.1 re-report of the
//! same two halves, so both are pinned here.
//!
//! Expectations are GNU's: the substitution captures every line, and
//! the process-substitution loop counts every directory with nothing
//! leaking to stdout. The tests use the `find`/`wc`/`cat`/`sort` the
//! shell resolves from PATH, and stand down — with the reason on
//! stderr — when a tool is not resolvable *as a GNU-semantics tool*:
//! on a stock Windows PATH, `C:\Windows\System32\find.exe` (Microsoft's
//! string-search FIND) shadows GNU find, "resolves" fine under
//! `command -v`, then rejects `-maxdepth`/`-type` and silently starves
//! the pipeline (`count=0`). Resolvability alone is not identity, so
//! the fixture probes each tool's actual GNU behavior before running
//! the pinned shapes (see root-D adjudication, D-REPORT).

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

fn run_c(script: &str) -> (bool, String) {
    let output = Command::new(niu_binary())
        .arg("-c")
        .arg(script)
        .env("NIU_SKIP_WINUXCMD_ACTIVATION", "1")
        .output()
        .expect("spawn niu");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

/// GNU-identity probe for one external tool: `tool` must resolve from
/// the shell's PATH *and* pass `probe` (a plain top-level pipeline — the
/// shapes that historically work against any GNU-semantics tool) with
/// exactly `expect` on stdout. On failure the reason, including the
/// resolved path, goes to stderr so a CI log shows *which* tool was
/// stood down and what it resolved to, instead of a bare skip or a
/// misleading assertion failure against a foreign implementation.
fn tool_identity_ok(tool: &str, probe: &str, expect: &str) -> bool {
    let (ok, out) = run_c(&format!("command -v {tool}"));
    if !ok || out.trim().is_empty() {
        eprintln!("issue76_93 stand-down: `{tool}` is not resolvable from the shell's PATH");
        return false;
    }
    let resolved = out.trim().to_string();
    let (pok, pout) = run_c(probe);
    // Normalize CRLF before comparing: a Windows-native GNU-semantics
    // tool may terminate its output lines with \r\n, and line endings
    // are not identity either.
    let got = pout.replace('\r', "");
    if !pok || got.trim() != expect {
        eprintln!(
            "issue76_93 stand-down: `{tool}` resolved to {resolved} but failed the \
             GNU identity probe `{probe}` (expected stdout {expect:?}, got \
             ok={pok}, stdout {pout:?}); a foreign namesake cannot pin GNU expectations"
        );
        return false;
    }
    true
}

/// Fixture directory with exactly two subdirectories (3 directories in
/// total including itself). Returns None — after printing the reason to
/// stderr — when any external tool the shapes need is missing or is a
/// foreign namesake rather than the GNU-semantics tool.
fn fixture() -> Option<PathBuf> {
    let dir = std::env::temp_dir().join(format!("niu-w2-find-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("d1")).unwrap();
    std::fs::create_dir_all(dir.join("d2")).unwrap();
    let d = dir.display().to_string();
    // `wc` is probed first: the `find` probe counts its output through
    // `wc -l` so the check does not depend on how the resolved find
    // echoes the starting path back (path spelling is not identity).
    let checks: [(&str, String, String); 4] = [
        (
            "wc",
            "printf 'x\\ny\\n' | wc -l".to_string(),
            "2".to_string(),
        ),
        ("cat", "printf 'ok' | cat".to_string(), "ok".to_string()),
        (
            "sort",
            "printf 'b\\na\\n' | sort".to_string(),
            "a\nb".to_string(),
        ),
        (
            "find",
            format!("find {d} -maxdepth 0 -type d | wc -l"),
            "1".to_string(),
        ),
    ];
    for (tool, probe, expect) in &checks {
        if !tool_identity_ok(tool, probe, expect) {
            let _ = std::fs::remove_dir_all(&dir);
            return None;
        }
    }
    Some(dir)
}

#[test]
fn issue76_find_first_stage_in_substitution_pipeline_is_captured() {
    let Some(dir) = fixture() else { return };
    let d = dir.display();
    let (ok, out) = run_c(&format!(
        "x=$(find {d} -maxdepth 1 -type d | wc -l); echo \"count=$x\""
    ));
    assert!(ok);
    assert_eq!(out, "count=3\n");
    let (ok, out) = run_c(&format!(
        "x=$(find {d} -maxdepth 1 -type d | cat); printf '%s\\n' \"$x\" | wc -l"
    ));
    assert!(ok);
    assert_eq!(out.trim(), "3");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn issue93_procsub_eval_find_output_does_not_leak() {
    let Some(dir) = fixture() else { return };
    let d = dir.display();
    let (ok, out) = run_c(&format!(
        "c=0; while IFS= read -r _; do c=$((c+1)); done \
         < <(eval \"find {d} -maxdepth 1 -type d\" | sort); echo \"c=$c\""
    ));
    assert!(ok);
    // Exactly the counter line: any leaked find output would appear here.
    assert_eq!(out, "c=3\n");
    let _ = std::fs::remove_dir_all(&dir);
}
