//! Compat test runner.
//!
//! Each `<name>.sh` in `tests/compat/fixtures/` is executed via the built
//! `niubash` binary, and its stdout is compared against `<name>.expected`.
//!
//! Tests are marked `#[ignore]` because they require winuxcmd command links
//! (for example `grep.exe` and `tr.exe`) to be discoverable in PATH. A bare
//! `winuxcmd.exe` is not enough because rubash resolves external pipeline
//! stages by command name. Run with:
//!
//!   PATH="C:/path/to/WinuxCmd/build-vs-release;$PATH" cargo test --test compat -- --ignored

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn niu_binary() -> PathBuf {
    // cargo test builds the bin to target/<profile>/niubash[.exe]
    let p = PathBuf::from(env!("CARGO_BIN_EXE_niu"));
    if !p.exists() {
        // fall back to the known target dir layout
        let mut fallback = repo_root();
        fallback.push("target");
        fallback.push("debug");
        fallback.push(if cfg!(windows) { "niu.exe" } else { "niubash" });
        if fallback.exists() {
            return fallback;
        }
    }
    p
}

fn fixtures_dir() -> PathBuf {
    let mut p = repo_root();
    p.push("tests");
    p.push("compat");
    p.push("fixtures");
    p
}

fn normalize(s: &str) -> String {
    // strip trailing whitespace per line; collapse CRLF -> LF; trim trailing newlines
    s.replace("\r\n", "\n")
        .lines()
        .map(|l| l.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_string()
}

fn run_case(name: &str) {
    ensure_winuxcmd_command_links();

    let dir = fixtures_dir();
    let script = dir.join(format!("{name}.sh"));
    let expected_path = dir.join(format!("{name}.expected"));

    let expected = fs::read_to_string(&expected_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", expected_path.display()));

    let script_content =
        fs::read_to_string(&script).unwrap_or_else(|e| panic!("read {}: {e}", script.display()));

    // Run via `niu -c <script_content>` so we exercise the same path as
    // interactive use without relying on the line-by-line script-file reader
    // (which still has heredoc/continuation gaps tracked as T-4).
    let bin = niu_binary();
    assert!(
        bin.exists(),
        "niubash binary not found at {}",
        bin.display()
    );

    let output = Command::new(&bin)
        .arg("-c")
        .arg(&script_content)
        .output()
        .unwrap_or_else(|e| panic!("spawn {}: {e}", bin.display()));

    if !output.stderr.is_empty() {
        eprintln!(
            "[{name}] stderr: {}",
            String::from_utf8_lossy(&output.stderr).trim_end()
        );
    }

    let actual = String::from_utf8_lossy(&output.stdout);
    let actual_norm = normalize(&actual);
    let expected_norm = normalize(&expected);
    assert_eq!(
        actual_norm, expected_norm,
        "[{name}] mismatch\n--- expected ---\n{expected_norm}\n--- actual ---\n{actual_norm}\n"
    );
}

fn ensure_winuxcmd_command_links() {
    let required = ["grep", "tr", "cat", "ls"];
    let missing = required
        .iter()
        .copied()
        .filter(|cmd| {
            Command::new(cmd)
                .arg("--help")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_err()
        })
        .collect::<Vec<_>>();

    assert!(
        missing.is_empty(),
        "compat tests require winuxcmd command links in the Windows process PATH, not just winuxcmd.exe; missing: {:?}. Run the bundled activation script, prepend the command-link directory with the Windows separator (`PATH=\"C:/path/to/WinuxCmd/build-vs-release;$PATH\"`), or run `winuxcmd.exe wpm links rebuild --root <winuxcmd-dir>` before `cargo test --test compat -- --ignored`.",
        missing
    );
}

macro_rules! compat_test {
    ($name:ident, $label:literal) => {
        #[test]
        #[ignore = "requires winuxcmd command links in PATH; run with --ignored"]
        fn $name() {
            run_case($label);
        }
    };
}

compat_test!(var_expansion, "var_expansion");
compat_test!(command_substitution, "command_substitution");
compat_test!(
    command_substitution_quoted_newline,
    "command_substitution_quoted_newline"
);
compat_test!(
    command_substitution_function_pipeline,
    "command_substitution_function_pipeline"
);
compat_test!(pipeline, "pipeline");
compat_test!(if_else, "if_else");
compat_test!(for_loop, "for_loop");
compat_test!(function, "function");
compat_test!(alias, "alias");
compat_test!(exit_code, "exit_code");
compat_test!(string_param, "string_param");
compat_test!(echo_flags, "echo_flags");
// Multi-line constructs require execute_script (whole-file tokenization).
compat_test!(heredoc, "heredoc");
compat_test!(continuation, "continuation");
compat_test!(multiline_if, "multiline_if");
compat_test!(multiline_for, "multiline_for");
compat_test!(and_or_status, "and_or_status");
compat_test!(bash_smoke, "bash_smoke");
