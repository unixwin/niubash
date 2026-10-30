//! niubash#654: niu refused non-UTF-8 scripts with rc 126 "cannot
//! execute binary file" that rubash and GNU execute fine.
//!
//! Root cause: `read_script_operand` merged GNU's two separate gates
//! — binary determination (check_binary_file) and byte decoding —
//! into one refusal (`check_binary_file || from_utf8().is_err()`).
//! GNU has no UTF-8 validity gate on script files: after the binary
//! check, raw bytes decode with invalid sequences carried as raw-byte
//! markers (rubash#132, `bytes_to_script_text`), the same two-step
//! rubash's own main performs. The fix separates the gates again.

use std::path::PathBuf;
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
    fallback.push(if cfg!(windows) { "niu.exe" } else { "niu" });
    fallback
}

fn unique_temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("niu-issue654-{tag}-{nanos}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run_niu(dir: &PathBuf, args: &[&str]) -> Output {
    Command::new(niu_binary())
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .output()
        .expect("spawn niu")
}

/// A script carrying a Latin-1 byte (0xE9) executes, and the byte
/// reaches the output untouched — rubash and GNU both run this fine.
#[test]
fn latin1_script_executes() {
    let dir = unique_temp_dir("latin1");
    std::fs::write(dir.join("latin.sh"), b"echo caf\xe9 latin1\n").unwrap();
    let output = run_niu(&dir, &["latin.sh"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"caf\xe9 latin1\n".as_slice());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A true binary (ELF header + NUL bytes — check_binary_file's own
/// criteria) is still refused with the GNU shape: rc 126, the
/// "cannot execute binary file" diagnostic, and no execution.
#[test]
fn true_binary_still_refused() {
    let dir = unique_temp_dir("binary");
    let mut bytes = b"\x7fELF\x02\x01\x01\x00".to_vec();
    bytes.extend_from_slice(b"echo should-not-run\n");
    std::fs::write(dir.join("bin.sh"), &bytes).unwrap();
    let output = run_niu(&dir, &["bin.sh"]);
    assert_eq!(output.status.code(), Some(126));
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        stderr.contains("cannot execute binary file"),
        "stderr was: {stderr:?}"
    );
    assert!(
        output.stdout.is_empty(),
        "a refused binary must not execute, stdout was: {:?}",
        output.stdout
    );
    let _ = std::fs::remove_dir_all(&dir);
}
