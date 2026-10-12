//! niubash#200: the parser used to strip a raw ESC byte (0x1B) that appeared
//! as the first character inside single quotes in a script file, while the
//! same byte mid-string survived. GNU bash keeps a raw ESC in every position
//! of every quoting context (verified against GNU bash 5.2.37, od -c).
//!
//! Root cause lives in the rubash lexer (word.rs): a word-initial data ESC is
//! byte-identical to the QUOTED_WORD_PREFIX marker the lexer itself emits, so
//! downstream strip_prefix consumers claimed it. Fixed in rubash by raw-byte
//! tagging a leading data ESC (unixwin/rubash#474); these tests pin the
//! product behavior byte-for-byte at the niu boundary.
//!
//! Boundary note (niubash#434/#451): those issues concern the *input bridge*
//! normalizing interactive typed input; this issue is the lexer's handling of
//! script-file bytes. Both entry paths (script-file argument and piped stdin)
//! are pinned here so neither normalization layer can eat the byte.

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

/// Run a script file through niu and return raw stdout bytes.
fn run_script_bytes(script: &[u8]) -> Vec<u8> {
    let dir = std::env::temp_dir().join(format!("niu-200-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("repro200.sh");
    std::fs::write(&path, script).unwrap();
    let output = Command::new(niu_binary())
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|err| panic!("spawn niu: {err}"));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        output.status.success(),
        "script failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

/// Run the same bytes through niu's piped-stdin entry (interactive-bridge
/// boundary counterpart of the script-file path).
fn run_stdin_bytes(script: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut child = Command::new(niu_binary())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|err| panic!("spawn niu: {err}"));
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(script)
        .unwrap_or_else(|err| panic!("write stdin: {err}"));
    let output = child.wait_with_output().unwrap();
    output.stdout
}

const ESC: u8 = 0x1B;

#[test]
fn raw_esc_first_char_inside_single_quotes_survives_script_file() {
    // The issue's exact reproducer: ESC as the first byte after the opening
    // `'` was eaten; the mid-string one survived. Both must come out.
    let mut script = b"printf '%s' '".to_vec();
    script.extend_from_slice(&[
        ESC, b'[', b'3', b'1', b'm', b'A', b'A', b'A', ESC, b'[', b'0', b'm', b'\'', b'\n',
    ]);
    let out = run_script_bytes(&script);
    let mut expected = vec![ESC, b'[', b'3', b'1', b'm', b'A', b'A', b'A'];
    expected.extend_from_slice(&[ESC, b'[', b'0', b'm']);
    assert_eq!(out, expected, "ESC bytes must survive in every position");
}

#[test]
fn raw_esc_word_initial_survives_every_quoting_context() {
    // Single-quoted, double-quoted, and unquoted word-initial ESC are all
    // data (GNU-identical), not a marker a consumer may claim.
    let body = [ESC, b'[', b'3', b'1', b'm', b'X'];
    let scripts = [
        {
            let mut s = b"printf '%s' '".to_vec();
            s.extend_from_slice(&body);
            s.extend_from_slice(b"'\n");
            s
        },
        {
            let mut s = b"printf '%s' \"".to_vec();
            s.extend_from_slice(&body);
            s.extend_from_slice(b"\"\n");
            s
        },
        {
            let mut s = b"printf '%s' ".to_vec();
            s.extend_from_slice(&body);
            s.push(b'\n');
            s
        },
    ];
    for script in &scripts {
        let out = run_script_bytes(script);
        // printf '%s' appends no newline, so the output is exactly the body.
        assert_eq!(out, body, "script: {:?}", String::from_utf8_lossy(script));
    }
}

#[test]
fn ansi_c_quoting_and_mid_word_esc_stay_gnu_identical() {
    // $'\033' textual escapes were never broken and must stay byte-identical;
    // a mid-word raw ESC was never claimed and must stay verbatim.
    let script = b"printf '%s' $'\\033[31mG'\nprintf '%s' $'X\\e[31mH'\n";
    let out = run_script_bytes(script);
    // Neither printf appends a newline: the two outputs concatenate.
    let mut expected = vec![ESC, b'[', b'3', b'1', b'm', b'G'];
    expected.extend_from_slice(&[b'X', ESC, b'[', b'3', b'1', b'm', b'H']);
    assert_eq!(out, expected);
}

#[test]
fn assignment_roundtrip_preserves_word_initial_esc() {
    // Stored-and-retrieved data takes the assignment storage path; the
    // leading ESC must survive the roundtrip in both quoting spellings.
    let mut script = b"v='".to_vec();
    script.extend_from_slice(&[ESC, b'[', b'3', b'1', b'm', b'V', b'\'', b'\n']);
    script.extend_from_slice(b"printf '%s\\n' \"$v\"\n");
    let out = run_script_bytes(&script);
    let expected = vec![ESC, b'[', b'3', b'1', b'm', b'V', b'\n'];
    assert_eq!(out, expected);
}

#[test]
fn piped_stdin_entry_preserves_word_initial_esc_too() {
    // The input-bridge boundary (niubash#434/#451 normalize interactive
    // input upstream of the lexer): the bridge must not pre-eat the byte
    // the lexer now preserves.
    let mut script = b"printf '%s' '".to_vec();
    script.extend_from_slice(&[
        ESC, b'[', b'3', b'1', b'm', b'S', b'\'', b'\n', b'e', b'x', b'i', b't', b'\n',
    ]);
    let out = run_stdin_bytes(&script);
    let expected = vec![ESC, b'[', b'3', b'1', b'm', b'S'];
    assert_eq!(out, expected);
}
