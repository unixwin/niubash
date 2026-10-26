//! unixwin/niubash#577: the bare-stdin fallback reader
//! (`read_unbuffered_line` in src/main.rs) widened every input BYTE into a
//! char (`byte as char`, Latin-1), so a UTF-8 script fed on standard
//! input was mojibake'd before the engine ever saw it — every multibyte
//! character doubled in size. The fallback is the common path, not an
//! edge: while fd 0 is the inherited process stdin, the engine's
//! `script_fd0_line` returns None and the host always reads through it.
//! The same reader also feeds `$THIS_SH` child input lines.
//!
//! Fix: accumulate the raw bytes and decode them with the engine's own
//! `rubash::executor::bytes_to_shell_text` — the same decode the
//! engine's fd-0 line path uses — so bare stdin, `-s`, and script-file
//! routes all hand the engine identical text, byte-exact for UTF-8 and
//! round-tripping raw non-UTF-8 bytes instead of widening them.
//!
//! Oracle: GNU bash 5.3.20 passes script bytes through untouched
//! (verified 2026-10-26, cluster gnu-expectations).

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

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

fn run_niu(args: &[&str], input: &[u8]) -> (Vec<u8>, Vec<u8>, i32) {
    let dir = std::env::temp_dir().join(format!(
        "niu-issue577-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    let mut child = Command::new(niu_binary())
        .args(args)
        .current_dir(&dir)
        .env("HOME", &dir)
        .env("USERPROFILE", &dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn niu");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(input)
        .expect("feed stdin script");
    let output = child.wait_with_output().expect("wait niu");
    let _ = std::fs::remove_dir_all(&dir);
    (
        output.stdout,
        output.stderr,
        output.status.code().unwrap_or(-1),
    )
}

/// `echo 中文 café\n` as raw UTF-8 bytes, and the bytes GNU prints back.
const UTF8_SCRIPT: &[u8] = b"echo \xe4\xb8\xad\xe6\x96\x87 caf\xc3\xa9\n";
const UTF8_EXPECTED: &[u8] = b"\xe4\xb8\xad\xe6\x96\x87 caf\xc3\xa9\n";

#[test]
fn issue577_utf8_script_via_bare_stdin_is_byte_exact() {
    let (out, err, rc) = run_niu(&[], UTF8_SCRIPT);
    assert_eq!(rc, 0, "stderr: {}", String::from_utf8_lossy(&err));
    assert_eq!(
        out, UTF8_EXPECTED,
        "bare stdin must not Latin-1-widen UTF-8 script bytes; got {out:02x?}"
    );
}

#[test]
fn issue577_bare_stdin_matches_dash_s_route() {
    let (bare, _, bare_rc) = run_niu(&[], UTF8_SCRIPT);
    let (dash_s, _, s_rc) = run_niu(&["-s"], UTF8_SCRIPT);
    assert_eq!(bare_rc, 0);
    assert_eq!(s_rc, 0);
    assert_eq!(
        bare, dash_s,
        "the two stdin routes must agree byte-for-byte"
    );
    assert_eq!(bare, UTF8_EXPECTED);
}

#[test]
fn issue577_raw_non_utf8_byte_round_trips() {
    // `echo "<0xff>"`: GNU prints the raw byte back (3c ff 3e 0a). The
    // pre-fix reader widened it to U+00FF (c3 bf on the wire).
    let (out, err, rc) = run_niu(&[], b"echo \"<\xff>\"\n");
    assert_eq!(rc, 0, "stderr: {}", String::from_utf8_lossy(&err));
    assert_eq!(out, b"<\xff>\n", "raw byte must round-trip; got {out:02x?}");
}
