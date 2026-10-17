//! niubash#390: `--encoded-command` must honour the leading invocation flags.
//!
//! The quoting-free channel is documented as the agent/CI path with the same
//! one-shot contract as `-c`, but its launcher arm built the shell directly
//! and never applied the option words that stood before `--encoded-command`.
//! `-e`, `-u`, `-o pipefail`, `-eu` and clustered forms were therefore
//! dropped: a failing command kept running and the process exited 0 exactly
//! where the docs point callers at fail-fast. These tests pin the encoded
//! route to the unencoded `-c` route flag for flag, and keep the flagless
//! path unchanged.

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

fn assert_matches_plain(encoded: Output, plain: Output, what: &str) {
    assert_eq!(
        encoded.status.code(),
        plain.status.code(),
        "{what}: encoded and -c exit codes differ; enc stderr {}",
        String::from_utf8_lossy(&encoded.stderr)
    );
    assert_eq!(
        encoded.stdout, plain.stdout,
        "{what}: encoded and -c stdout differ"
    );
}

#[test]
fn encoded_errexit_matches_the_unencoded_c_form() {
    // `-e` must abort before `echo AFTER` runs, on both routes.
    let command = "false; echo AFTER";
    let encoded = run(&["-e", "--encoded-command", &b64(command)]);
    let plain = run(&["-e", "-c", command]);
    assert!(
        !encoded.status.success(),
        "`-e --encoded-command` did not fail-fast: rc {:?}, stdout {}",
        encoded.status.code(),
        String::from_utf8_lossy(&encoded.stdout)
    );
    assert!(
        encoded.stdout.is_empty(),
        "errexit ran the following command: {}",
        String::from_utf8_lossy(&encoded.stdout)
    );
    assert_matches_plain(encoded, plain, "-e");
}

#[test]
fn encoded_pipefail_matches_the_unencoded_c_form() {
    // `false | true` succeeds without pipefail; with `-o pipefail` the
    // pipeline must exit nonzero, on both the separated and clustered forms.
    let command = "false | true";
    let plain = run(&["-o", "pipefail", "-c", command]);
    assert!(
        !plain.status.success(),
        "control `-o pipefail -c` did not fail"
    );

    let separated = run(&["-o", "pipefail", "--encoded-command", &b64(command)]);
    assert_matches_plain(separated, plain.clone(), "-o pipefail");

    let clustered = run(&["-eo", "pipefail", "--encoded-command", &b64(command)]);
    assert!(
        !clustered.status.success(),
        "`-eo pipefail --encoded-command` did not fail: rc {:?}",
        clustered.status.code()
    );
    assert_matches_plain(clustered, plain, "-eo pipefail");
}

#[test]
fn encoded_nounset_matches_the_unencoded_c_form() {
    let command = "echo $NIU390_UNSET_VARIABLE";
    let encoded = run(&["-eu", "--encoded-command", &b64(command)]);
    let plain = run(&["-eu", "-c", command]);
    assert!(
        !encoded.status.success(),
        "`-eu --encoded-command` ignored nounset: rc {:?}, stdout {}",
        encoded.status.code(),
        String::from_utf8_lossy(&encoded.stdout)
    );
    assert_matches_plain(encoded, plain, "-eu");
}

#[test]
fn encoded_clustered_euo_pipefail_keeps_every_flag() {
    // The common CI idiom, clustered: `-euo pipefail` must reach the encoded
    // route both as flags (errexit) and as a value option (pipefail).
    let command = "false | true; echo AFTER";
    let encoded = run(&["-euo", "pipefail", "--encoded-command", &b64(command)]);
    let plain = run(&["-euo", "pipefail", "-c", command]);
    assert!(
        !encoded.status.success(),
        "`-euo pipefail --encoded-command` did not fail: rc {:?}, stdout {}",
        encoded.status.code(),
        String::from_utf8_lossy(&encoded.stdout)
    );
    assert!(
        encoded.stdout.is_empty(),
        "errexit ran the following command: {}",
        String::from_utf8_lossy(&encoded.stdout)
    );
    assert_matches_plain(encoded, plain, "-euo pipefail");
}

#[test]
fn encoded_with_trailing_c_operands_matches_the_unencoded_form() {
    // A word after the payload keeps GNU `-c` operand meaning ($0 and
    // positional parameters) instead of being silently ignored.
    let command = "false; echo AFTER";
    let encoded = run(&["-e", "--encoded-command", &b64(command), "-c", "operand"]);
    let plain = run(&["-e", "-c", command, "-c", "operand"]);
    assert!(!encoded.status.success());
    assert_matches_plain(encoded, plain, "trailing -c operand");
}

#[test]
fn encoded_invalid_option_is_a_usage_error() {
    let out = run(&["-Z", "--encoded-command", &b64("echo hi")]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "invalid option must be EX_BADUSAGE"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.starts_with("bash: -Z: invalid option"),
        "expected the engine usage surface, got: {stderr}"
    );
    assert!(out.stdout.is_empty());
}

#[test]
fn flagless_encoded_command_is_unchanged() {
    // No-flag guard: the documented quoting-free form still runs the payload
    // and propagates its status exactly like the unencoded `-c` form.
    let failing = "false; echo AFTER";
    let encoded = run(&["--encoded-command", &b64(failing)]);
    let plain = run(&["-c", failing]);
    assert!(
        encoded.status.success(),
        "flagless encoded route changed: rc {:?}",
        encoded.status.code()
    );
    assert_eq!(encoded.stdout, b"AFTER\n");
    assert_matches_plain(encoded, plain, "flagless");

    let echo = run(&["--encoded-command", &b64("echo hello")]);
    assert_eq!(echo.status.code(), Some(0));
    assert_eq!(echo.stdout, b"hello\n");
}
