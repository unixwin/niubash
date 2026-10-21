//! unixwin/niubash#482: a stray `)` inside an OPEN multi-line group wedged
//! every gather loop — the interactive REPL sat in PS2 forever with no
//! diagnostic (GNU 5.3/5.2: `syntax error near unexpected token ')'`, the
//! whole group abandoned, rc 2, session recovers to PS1). The niubash#431
//! stray-close guard only audits COMPLETE groups; the grouped gather had
//! no terminal condition for a stray close while an opener was unresolved.
//!
//! Oracle (Git Bash GNU 5.2.37 + WSL GNU 5.3.0, script-file probes): every
//! form — `{ echo c\n)`, `f() {\n)`, `if t; then\n)`, `{ echo c\necho a)`,
//! `case x in\na) e\n)` — reports `syntax error near unexpected token ')'
//! AT THE STRAY LINE and the abandoned group runs nothing; interactive
//! `bash -i` prints the diagnostic without a line number and keeps reading.
//!
//! Fix: the engine gather treats an unclosable `)` (not a `(` close, not a
//! case-pattern terminator — Token::case_pattern_close) as a hard stop, so
//! the armed parse's mismatched-closer diagnostic reports the stray token;
//! the host REPL completeness scanner (is_repl_input_complete) mirrors the
//! verdict for the reedline/--noediting/stdin gathers.

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

fn run_c(command: &str) -> (String, String, i32) {
    let output = Command::new(niu_binary())
        .arg("-c")
        .arg(command)
        .output()
        .expect("spawn niu -c");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code().unwrap_or(-1),
    )
}

/// Feed `input` to niu on argv `args` (e.g. `-i` for the engine's
/// interactive stdin reader, `-s`, or nothing for the host gather).
fn run_with_args(args: &[&str], input: &str) -> (String, String, i32) {
    let dir = std::env::temp_dir().join(format!("niu-issue482-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    let input_path = dir.join("input.sh");
    std::fs::write(&input_path, input).expect("write stdin input");
    let stdin = std::fs::File::open(&input_path).expect("open stdin input");
    let output = Command::new(niu_binary())
        .args(args)
        .stdin(Stdio::from(stdin))
        .output()
        .expect("spawn niu");
    let result = (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code().unwrap_or(-1),
    );
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// The exact ticket shape: `{ echo c` opens the group, the `)` is stray.
/// GNU's non-interactive verdict, line for line.
#[test]
fn stdin_stray_close_inside_open_brace_group_rejects_at_stray_line() {
    let (stdout, stderr, code) = run_with_args(&[], "{ echo c\n)\necho after\n");
    assert_eq!(stdout, "", "the abandoned group and `after` never run");
    assert!(
        stderr.contains(": line 2: syntax error near unexpected token `)'\n")
            && stderr.ends_with(": line 2: `)'\n"),
        "GNU-verbatim diagnostic at the stray line, not an EOF report: {stderr:?}"
    );
    assert_eq!(code, 2);
}

/// `-s` (engine stdin driver): same verdict.
#[test]
fn s_flag_stray_close_inside_open_group_rejects() {
    let (stdout, stderr, code) = run_with_args(&["-s"], "{ echo c\n)\n");
    assert_eq!(stdout, "");
    assert!(
        stderr.contains(": line 2: syntax error near unexpected token `)'\n"),
        "{stderr:?}"
    );
    assert_eq!(code, 2);
}

/// The piped-`-i` reader (ticket methodology): GNU errors at the `)`,
/// abandons the group, and the session RECOVERS — the next command runs
/// and `rc` is 2. Before the fix the `)` and every later line were
/// swallowed into the open group and nothing ran.
#[test]
fn interactive_stdin_stray_close_errors_then_recovers_to_ps1() {
    let (stdout, stderr, code) =
        run_with_args(&["-i"], "{ echo c\n)\necho rc=$?-recovered\nexit\n");
    assert_eq!(code, 0);
    let transcript = format!("{stdout}{stderr}");
    assert!(
        transcript.contains("syntax error near unexpected token `)'"),
        "GNU's diagnostic must print, not wedge in PS2: {transcript:?}"
    );
    assert!(
        transcript.contains("rc=2-recovered"),
        "the session must recover to PS1 and run the next line: {transcript:?}"
    );
    assert!(
        !transcript.contains("unexpected end of file"),
        "the stray-close verdict, not the EOF diagnostic: {transcript:?}"
    );
}

/// `-c` on the ticket shape: GNU reports `line 2: unexpected token ')'' —
/// before the fix this was the EOF-from-`{` diagnostic (the ticket's
/// "adjacent observation").
#[test]
fn c_stray_close_inside_open_group_reports_stray_token() {
    let (stdout, stderr, code) = run_c("{ echo c\n)");
    assert_eq!(stdout, "");
    assert_eq!(
        stderr,
        concat!(
            "niu: -c: line 2: syntax error near unexpected token `)'\n",
            "niu: -c: line 2: `)'\n"
        ),
        "{stderr:?}"
    );
    assert_eq!(code, 2);
}

/// Mid-command stray close inside an open group (`{ echo c\necho a)`) is
/// the same GNU verdict, echoing the offending line verbatim.
#[test]
fn mid_command_stray_close_inside_group_rejects() {
    let (stdout, stderr, code) = run_with_args(&[], "{ echo c\necho a)\n}\n");
    assert_eq!(stdout, "", "GNU runs nothing from the aborted group");
    assert!(
        stderr.contains(": line 2: syntax error near unexpected token `)'\n")
            && stderr.ends_with(": line 2: `echo a)'\n"),
        "{stderr:?}"
    );
    assert_eq!(code, 2);
}

/// A `)` inside a `case` BODY (not pattern position) is stray in GNU too.
#[test]
fn stray_close_inside_case_body_rejects() {
    let (stdout, stderr, code) = run_with_args(&[], "case x in\na) echo hit\n)\nesac\n");
    assert_eq!(stdout, "");
    assert!(
        stderr.contains(": line 3: syntax error near unexpected token `)'\n"),
        "{stderr:?}"
    );
    assert_eq!(code, 2);
}

/// Normal groups must not regress: multi-line `{ }`, `( )`, `case`
/// patterns (`a)`, `(a|b)`), `f() {}`, `if/fi`, `for/done`, multi-line
/// compound assignments and command substitutions all still gather and run.
#[test]
fn normal_groups_still_gather_and_run() {
    for (input, want) in [
        ("{ echo a\necho b\n}\n", "a\nb\n"),
        ("(echo a\necho b)\n", "a\nb\n"),
        ("f() {\necho ok\n}\nf\n", "ok\n"),
        ("if true; then\necho ok\nfi\n", "ok\n"),
        ("for i in a b; do\nprintf $i\ndone\n", "ab"),
        ("case x in\na) echo hit;;\nesac\n", ""),
        ("case a in\n(a|b) echo hit;;\nesac\n", "hit\n"),
        ("a=(1 2\n)\necho done\n", "done\n"),
        ("x=$(echo hi\n)\necho $x\n", "hi\n"),
        ("{ echo c\ncase y in y) echo hit;; esac\n}\n", "c\nhit\n"),
    ] {
        let (stdout, stderr, code) = run_with_args(&[], input);
        assert_eq!(
            stdout, want,
            "{input:?} must still gather and run: {stderr:?}"
        );
        assert_eq!(code, 0, "{input:?}: {stderr:?}");
    }
}

/// A stray `)` at PS1 was already correct before the fix — pinned so the
/// gather termination keeps the same verdict.
#[test]
fn stray_close_at_ps1_still_rejects() {
    let (stdout, stderr, code) = run_c(")");
    assert_eq!(stdout, "");
    assert!(
        stderr.contains("syntax error near unexpected token `)'"),
        "{stderr:?}"
    );
    assert_eq!(code, 2);
}
