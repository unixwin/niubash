//! niubash#791 — product-face pin for the rubash#2467 heredoc fd fix
//! (engine fix: rubash PR #2468, merged at rubash fa34dbff; this file
//! pins the behavior at the niu boundary, through the same face a
//! `niu -c` invocation drives: `Shell::new_one_shot` + `execute_script`).
//!
//! The defect: a standalone all-digit WORD separated by whitespace
//! from a following `<<` was fused into the heredoc's io_number and
//! swallowed from the argument list — `head -n 3 <<EOF` lost the `3`
//! (head failed with "option requires an argument -- 'n'") and
//! `echo 3 <<X` printed an empty line. GNU parse.y read_token_word()
//! (parse.y:5725-5741) makes an all-digit token a NUMBER only when
//! the character that ended the token is `<` or `>` (or the previous
//! token was `<`/`&`), so the grammar's `NUMBER LESS_LESS WORD` form
//! (parse.y:629) can only engage for the fused spelling `3<<X`.
//!
//! Every expectation below was measured against the GNU bash 5.3.20
//! oracle. Command output is captured through an in-script `> file`
//! redirect placed AFTER the heredoc delimiter word, which leaves
//! the token shape under test (digit word, whitespace, `<<`)
//! untouched; the oracle was re-run on these exact capture variants
//! and produced byte-identical outcomes to the plain spellings.

use std::path::Path;

use niubash_runtime::Shell;

/// Run `script` through the one-shot product face; returns its
/// exit code.
fn run_script(script: &str) -> i32 {
    let mut shell = Shell::new_one_shot().expect("one-shot shell constructs");
    shell.session_mut().inherit_process_stdin();
    shell
        .execute_script(script)
        .expect("script executes through the one-shot face")
}

fn shell_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Execute `command_line` (which must end with the heredoc operator
/// and delimiter word) plus `body` lines and the delimiter, with the
/// command's stdout redirected to a fresh capture file. Returns
/// (exit code, capture contents). The redirect is appended after the
/// delimiter word so the digit-word / `<<` adjacency being pinned is
/// exactly the one from the issue.
fn heredoc_capture(command_line: &str, body: &str, delimiter: &str) -> (i32, String) {
    let dir = tempfile::tempdir().expect("capture tempdir");
    let cap = dir.path().join("cap.txt");
    let script = format!("{command_line} > {}\n{body}{delimiter}\n", shell_path(&cap));
    let code = run_script(&script);
    let captured = std::fs::read_to_string(&cap).unwrap_or_default();
    (code, captured)
}

/// Premise probe for the `head` case: some environments resolve no
/// `head` at all through the product face (the pin's subject is the
/// parser, not command availability). Mirrors the premise-guard
/// idiom of tests/issue207_platform_smoke_contract.rs: probe through
/// niu itself, skip only when the premise is missing.
fn head_resolvable() -> bool {
    let dir = tempfile::tempdir().expect("probe tempdir");
    let cap = dir.path().join("probe.txt");
    let script = format!("head --version > {}\n", shell_path(&cap));
    let code = run_script(&script);
    code != 127 && cap.exists()
}

#[test]
fn issue791_head_argument_survives_spaced_heredoc() {
    if !head_resolvable() {
        eprintln!(
            "skip issue791_head_argument_survives_spaced_heredoc: premise missing — \
             no `head` resolves through the niubash product face in this environment"
        );
        return;
    }
    let (code, out) = heredoc_capture("head -n 3 <<EOF", "a\nb\nc\nd\n", "EOF");
    assert_eq!(out, "a\nb\nc\n", "the `3` must reach head as -n's argument");
    assert_eq!(code, 0);
}

#[test]
fn issue791_echo_number_word_survives_spaced_heredoc() {
    let (code, out) = heredoc_capture("echo 3 <<X", "", "X");
    assert_eq!(out, "3\n", "the `3` must stay an echo argument");
    assert_eq!(code, 0);
}

#[test]
fn issue791_echo_number_word_survives_spaced_heredoc_dash() {
    let (code, out) = heredoc_capture("echo 3 <<-X", "", "X");
    assert_eq!(out, "3\n", "the `3` must stay an echo argument (<<- form)");
    assert_eq!(code, 0);
}

#[test]
fn issue791_fused_io_number_still_redirects() {
    // Negative control: the fused spelling is a true io_number — the
    // heredoc feeds fd 3, echo's stdout (fd 1) stays empty of args,
    // so the capture holds just the newline of an argument-less echo.
    let (code, out) = heredoc_capture("echo 3<<Y", "", "Y");
    assert_eq!(out, "\n", "fused `3<<Y` must keep io_number semantics");
    assert_eq!(code, 0);
}
