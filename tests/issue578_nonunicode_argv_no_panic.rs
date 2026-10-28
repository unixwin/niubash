//! unixwin/niubash#578: the niu binary collected argv with
//! `std::env::args()`, which PANICS on the first non-Unicode word — a
//! `-c` string or script name carrying raw bytes outside UTF-8 (on
//! Windows: an unpaired UTF-16 surrogate or legacy-code-page bytes from
//! the invoking host) killed the shell at startup with a Rust panic
//! banner and rc 101, before any shell error surface could engage. The
//! bundled `bash` face forwards with `args_os` losslessly; the host
//! itself now collects with `args_os` too and converts lossily only at
//! the String consumption points, like argv[0] was already treated.
//!
//! The byte-argv cases are Unix-only (Windows argv is UTF-16 at the OS
//! boundary); the no-panic contract is platform-wide.

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

fn assert_no_panic(stderr: &[u8], rc: i32) {
    let err = String::from_utf8_lossy(stderr);
    assert_ne!(rc, 101, "rc 101 is the Rust panic exit; stderr: {err:?}");
    assert!(
        !err.contains("panicked"),
        "panic banner must never reach the user; stderr: {err:?}"
    );
}

#[test]
fn issue578_valid_utf8_c_string_still_works() {
    let output = Command::new(niu_binary())
        .args(["-c", "echo argv-ok"])
        .stdin(Stdio::null())
        .output()
        .expect("spawn niu");
    assert_no_panic(&output.stderr, output.status.code().unwrap_or(-1));
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "argv-ok");
}

#[cfg(unix)]
#[test]
fn issue578_dash_c_with_invalid_utf8_byte_does_not_panic() {
    use std::os::unix::ffi::OsStrExt;
    let bad = std::ffi::OsStr::from_bytes(b"echo \xff");
    let output = Command::new(niu_binary())
        .arg("-c")
        .arg(bad)
        .stdin(Stdio::null())
        .output()
        .expect("spawn niu");
    assert_no_panic(&output.stderr, output.status.code().unwrap_or(-1));
}

#[cfg(unix)]
#[test]
fn issue578_invalid_utf8_script_name_does_not_panic() {
    use std::os::unix::ffi::OsStrExt;
    let bad = std::ffi::OsStr::from_bytes(b"\xff.sh");
    let output = Command::new(niu_binary())
        .arg(bad)
        .stdin(Stdio::null())
        .output()
        .expect("spawn niu");
    let rc = output.status.code().unwrap_or(-1);
    assert_no_panic(&output.stderr, rc);
    assert_ne!(rc, 0, "a nonexistent script name must be a shell error");
}

#[cfg(unix)]
#[test]
fn issue578_invalid_utf8_argv0_face_does_not_panic() {
    // Invoke through a byte-named symlink so argv[0] itself is
    // non-Unicode: startup reads argv[0] for the shell face/name.
    use std::os::unix::ffi::OsStrExt;
    let dir = std::env::temp_dir().join(format!("niu-issue578-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    let link = dir.join(std::ffi::OsStr::from_bytes(b"\xffniu"));
    std::os::unix::fs::symlink(niu_binary(), &link).expect("symlink niu");
    let output = Command::new(&link)
        .args(["-c", "echo face-ok"])
        .stdin(Stdio::null())
        .output()
        .expect("spawn niu via byte-named link");
    let _ = std::fs::remove_dir_all(&dir);
    assert_no_panic(&output.stderr, output.status.code().unwrap_or(-1));
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "face-ok");
}
