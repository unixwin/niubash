//! unixwin/niubash#431: `niu -c` and stdin scripts accepted `(A;B);)` — the
//! subshell ran and the stray `)` was silently dropped (rc 0) — while GNU
//! bash 5.3.0 rejects the shape in every mode with `syntax error near
//! unexpected token ')'`, rc 2, and the subshell unexecuted.
//!
//! Oracle note: the earliest "GNU 5.3 (msys) tolerates the tail" record was
//! produced against `winuxcmd/usr/bin/bash.exe`, which is a launcher shim
//! that spawns niu itself — the tolerant verdict was niu's own behavior
//! echoed back (rubash#852). The authority is WSL GNU bash 5.3.0 (re-run
//! 2026-10-19, bash copied to argv[0] `niu` for name-identical capture):
//! every form — `-c` single-line, `-c` with the newline after the `;`,
//! stdin script single/multi-line, script file — rejects, rc 2; a list
//! terminated by `;`+newline before the stray token still runs first
//! (parse.y:1274-1286 vs simple_list_terminator parse.y:1310-1314), and the
//! offending line echoes verbatim (y.error).
//!
//! Root cause: `Shell::execute_script`'s fast path called the engine's
//! `parse()` without the stray-close guard the engine's own readers arm
//! (`ParseLoopOptions::stray_close_is_error`, script_driver run_source_impl),
//! so the guard's `__RUBASH_PARSE_ERROR__` node never existed and the host
//! executed the parsed prefix. The fix arms the same guard for the
//! non-interactive fast path (with the source text, for GNU's verbatim line
//! echo) and hands the input to the engine when it trips, which reproduces
//! GNU's read-parse-execute split byte-for-byte. Since rubash#2860 the
//! hand-off target is the Session contract's `run` — the arm was the host's
//! last direct `script_driver` call; the migration's own pins live in
//! `tests/issue2860_stray_close_session_run.rs`.

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

/// Feed `input` to niu as a non-interactive stdin script (the no-flag `niu`
/// route: GNU shell.c:780-786 read_from_stdin).
fn run_stdin_script(input: &str) -> (String, String, i32) {
    let dir = std::env::temp_dir().join(format!("niu-issue431-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    let input_path = dir.join("input.sh");
    std::fs::write(&input_path, input).expect("write stdin input");
    let stdin = std::fs::File::open(&input_path).expect("open stdin input");
    let output = Command::new(niu_binary())
        .stdin(Stdio::from(stdin))
        .output()
        .expect("spawn niu stdin script");
    let result = (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code().unwrap_or(-1),
    );
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// The exact issue shape on `-c`: the subshell must not run and the stray
/// `)` is a syntax error with GNU's diagnostic and rc.
#[test]
fn c_stray_close_after_subshell_list_rejects() {
    let (stdout, stderr, code) = run_c("(printf a; printf b);)");
    assert_eq!(stdout, "", "the subshell must not execute");
    assert_eq!(
        stderr,
        concat!(
            "niu: -c: line 1: syntax error near unexpected token `)'\n",
            "niu: -c: line 1: `(printf a; printf b);)'\n"
        ),
        "GNU-verbatim diagnostic (y.error echoes the input line)"
    );
    assert_eq!(code, 2);
}

/// The newline-after-`;` variant: the whole list still fails to reduce
/// (simple_list_terminator is newline/EOF only — parse.y:1310-1314), so the
/// diagnostic blames line 2 and nothing runs.
#[test]
fn c_stray_close_newline_form_rejects() {
    let (stdout, stderr, code) = run_c("(printf a;\nprintf b);)");
    assert_eq!(stdout, "");
    assert_eq!(
        stderr,
        concat!(
            "niu: -c: line 2: syntax error near unexpected token `)'\n",
            "niu: -c: line 2: `printf b);)'\n"
        )
    );
    assert_eq!(code, 2);
}

/// A `)` that only closes a subshell is fine — the guard must not over-fire.
#[test]
fn c_balanced_subshell_still_runs() {
    let (stdout, _, code) = run_c("(printf a; printf b)");
    assert_eq!(stdout, "ab");
    assert_eq!(code, 0);
    let (stdout, _, code) = run_c("(printf a;\nprintf b)");
    assert_eq!(stdout, "ab");
    assert_eq!(code, 0);
}

/// Stdin script, single-line form: reject with the absolute physical line.
/// The diagnostic names argv[0] verbatim (GNU never rebinds $0 for a stdin
/// script — shell.c:1613 is the script-FILE rebind), same as `bash -s`.
#[test]
fn stdin_stray_close_single_line_rejects() {
    let (stdout, stderr, code) = run_stdin_script("(printf a; printf b);)\n");
    assert_eq!(stdout, "");
    assert!(
        stderr.contains(": line 1: syntax error near unexpected token `)'\n")
            && stderr.ends_with("`(printf a; printf b);)'\n"),
        "argv0-named diagnostic with the verbatim line echo: {stderr:?}"
    );
    assert_eq!(code, 2);
}

/// Stdin script, multi-line form: the `;`+newline-terminated list runs first
/// (GNU read-parse-execute split), then the stray `)` on physical line 3
/// aborts with rc 2 — later input never runs.
#[test]
fn stdin_stray_close_multi_line_runs_prefix_then_rejects() {
    let (stdout, stderr, code) = run_stdin_script("(printf a;\nprintf b);\n)\necho after\n");
    assert_eq!(stdout, "ab", "the complete list runs; `after` does not");
    assert!(
        stderr.contains(": line 3: syntax error near unexpected token `)'\n")
            && stderr.ends_with(": line 3: `)'\n"),
        "absolute physical line across gathered batches: {stderr:?}"
    );
    assert_eq!(code, 2);
}

/// The `-s` engine driver route (already engine-driven before the fix) stays
/// pinned at the same verdict.
#[test]
fn s_flag_stray_close_rejects() {
    let dir = std::env::temp_dir().join(format!("niu-issue431-s-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    let input_path = dir.join("input.sh");
    std::fs::write(&input_path, "(printf a; printf b);)\n").expect("write stdin input");
    let stdin = std::fs::File::open(&input_path).expect("open stdin input");
    let output = Command::new(niu_binary())
        .arg("-s")
        .stdin(Stdio::from(stdin))
        .output()
        .expect("spawn niu -s");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(": line 1: syntax error near unexpected token `)'\n")
            && stderr.ends_with("`(printf a; printf b);)'\n"),
        "{stderr:?}"
    );
    assert_eq!(output.status.code(), Some(2));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The script-file route: prefix runs, diagnostic names the script at the
/// absolute line, rc 2.
#[test]
fn script_file_stray_close_runs_prefix_then_rejects() {
    let dir = std::env::temp_dir().join(format!("niu-issue431-f-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    let script = dir.join("stray.sh");
    std::fs::write(&script, b"(printf a;\nprintf b);\n)\necho after\n").expect("write script");

    let output = Command::new(niu_binary())
        .arg(&script)
        .output()
        .expect("spawn niu script");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "ab");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(": line 3: syntax error near unexpected token `)'\n")
            && stderr.ends_with(": line 3: `)'\n"),
        "{stderr:?}"
    );
    assert_eq!(output.status.code(), Some(2));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Mid-command stray close (`echo a)`) is the same GNU verdict, and the
/// offending line echoes VERBATIM — the token-reconstruction echo used to
/// insert a space (`echo a ;;` for input `echo a;;`), which no GNU oracle
/// produces (y.error prints the line as read).
#[test]
fn mid_command_stray_close_and_verbatim_echo() {
    let (stdout, stderr, code) = run_c("echo a)");
    assert_eq!(stdout, "");
    assert_eq!(
        stderr,
        concat!(
            "niu: -c: line 1: syntax error near unexpected token `)'\n",
            "niu: -c: line 1: `echo a)'\n"
        )
    );
    assert_eq!(code, 2);

    let (stdout, stderr, code) = run_c("echo a;;");
    assert_eq!(stdout, "");
    assert_eq!(
        stderr,
        concat!(
            "niu: -c: line 1: syntax error near unexpected token `;;'\n",
            "niu: -c: line 1: `echo a;;'\n"
        ),
        "the echo must be the input verbatim, not token-reconstructed"
    );
    assert_eq!(code, 2);
}
