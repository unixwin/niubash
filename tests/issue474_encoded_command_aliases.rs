//! niubash#474: the `--encoded-command` verbatim handoff accepts two more
//! spellings — `-EncodedCommand`, the PowerShell spelling matched
//! case-insensitively like every PowerShell parameter name (so tooling that
//! already emits `pwsh -EncodedCommand <b64>` command lines plugs in
//! unchanged), and the short `--ec`. Every spelling feeds the same decode +
//! `-c` re-entry, so the same payload must produce byte-identical results,
//! the `v1:` prefix guard must behave identically, and the missing-argument
//! diagnostic must be the same text under all of them. The single-dash
//! cluster space is untouched: `-ec` keeps its GNU `-e -c` meaning.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

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

fn run(args: &[&str]) -> Output {
    Command::new(niu_binary())
        .args(args)
        .env_remove("NIU_ENV")
        .env_remove("BASH_ENV")
        .stdin(Stdio::null())
        .output()
        .expect("spawn niu")
}

/// Standard RFC 4648 base64, so the tests need no `base64` utility in PATH.
fn b64(input: &str) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = input.as_bytes();
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((n >> 6) & 63) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(n & 63) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Every accepted spelling of the verbatim handoff.
const SPELLINGS: &[&str] = &["--encoded-command", "-EncodedCommand", "--ec"];

fn assert_same_output(alias: &Output, reference: &Output, spelling: &str) {
    assert_eq!(
        alias.status.code(),
        reference.status.code(),
        "{spelling}: exit code differs from --encoded-command (stderr {})",
        String::from_utf8_lossy(&alias.stderr)
    );
    assert_eq!(
        alias.stdout, reference.stdout,
        "{spelling}: stdout differs from --encoded-command"
    );
    assert_eq!(
        alias.stderr, reference.stderr,
        "{spelling}: stderr differs from --encoded-command"
    );
}

#[test]
fn all_spellings_execute_the_same_payload_byte_for_byte() {
    let command = "echo hello; printf 'done\\n'";
    let payload = b64(command);
    let reference = run(&["--encoded-command", &payload]);
    assert_eq!(reference.status.code(), Some(0));
    assert_eq!(reference.stdout, b"hello\ndone\n");
    for spelling in [
        "-EncodedCommand",
        "--ec",
        "-encodedcommand",
        "-ENCODEDCOMMAND",
    ] {
        assert_same_output(&run(&[spelling, &payload]), &reference, spelling);
    }
}

#[test]
fn v1_prefix_guard_holds_under_every_spelling() {
    // The `v1:` prefix (MSYS path-rewrite guard) is stripped identically on
    // the alias routes — it must not leak into the decoded command.
    let payload = format!("v1:{}", b64("echo guarded"));
    for spelling in SPELLINGS {
        let out = run(&[spelling, &payload]);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{spelling}: v1 payload failed (stderr {})",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(out.stdout, b"guarded\n", "{spelling}: v1 output");
    }
}

#[test]
fn missing_argument_reports_identically_under_every_spelling() {
    let reference = run(&["--encoded-command"]);
    let stderr = String::from_utf8_lossy(&reference.stderr);
    assert!(
        stderr.contains("--encoded-command requires an argument"),
        "canonical missing-arg diagnostic changed: {stderr}"
    );
    for spelling in ["-EncodedCommand", "--ec"] {
        assert_same_output(&run(&[spelling]), &reference, spelling);
    }
}

#[test]
fn aliases_keep_leading_invocation_flags() {
    // The niubash#390 contract holds under the aliases too: `-e` before the
    // alias must still reach the shell (errexit aborts before `echo AFTER`).
    let command = "false; echo AFTER";
    let plain = run(&["-e", "-c", command]);
    for spelling in SPELLINGS {
        let out = run(&["-e", spelling, &b64(command)]);
        assert!(
            !out.status.success(),
            "`-e {spelling}` did not fail-fast: rc {:?}, stdout {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout)
        );
        assert!(
            out.stdout.is_empty(),
            "`-e {spelling}` ran past the failure"
        );
        assert_eq!(out.status.code(), plain.status.code(), "{spelling}");
    }
}

#[test]
fn single_dash_ec_remains_the_gnu_cluster() {
    // The alias is double-dash `--ec`; single-dash `-ec` keeps GNU meaning
    // (`-e -c` cluster): the next word is the command string, not a payload.
    let out = run(&["-ec", "echo cluster"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(out.stdout, b"cluster\n");
}

#[test]
fn help_groups_the_verbatim_handoff_spellings() {
    let out = run(&["--help"]);
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    for needle in [
        "Verbatim handoff",
        "--encoded-command",
        "-EncodedCommand",
        "--ec",
    ] {
        assert!(stdout.contains(needle), "niu --help is missing `{needle}`");
    }
}
