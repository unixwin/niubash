//! niubash#204 — RETURN trap must fire both when a sourced script finishes
//! AND (for functions that inherit it) when a shell function returns.
//!
//! GNU oracle: `D:\Git\usr\bin\bash.exe` (GNU bash 5.2.37(1)-release,
//! x86_64-pc-msys), script-file probes 2026-10-09, byte-compared against the
//! niu built from this tree. Semantics (GNU execute_cmd.c:5291-5295): shell
//! functions inherit the RETURN trap only when function tracing is on
//! globally (`set -T` / functrace, which `shopt -s extdebug` implies) or the
//! individual function carries the trace attribute (`declare -ft f`); a
//! sourced script's completion fires the trap unconditionally
//! (evalfile.c:395 source_file).
//!
//! Oracle-verified matrix locked below:
//!
//! | scenario                                        | GNU 5.2.37 | niu |
//! |-------------------------------------------------|------------|-----|
//! | plain `trap … RETURN; f`                        | no fire    | same |
//! | `declare -ft f` (trace attribute)               | fires      | same |
//! | `declare -f -t f`                               | fires      | same |
//! | `declare -t f` (no -f: variable-path, no attr)  | no fire    | same |
//! | `shopt -s extdebug`                             | fires      | same |
//! | `set -T`, nested calls                          | once/level | same |
//! | source completion                               | fires      | same |
//! | trap set inside function                        | fires at that function's return, persists after | same |
//!
//! Root-cause note: the function-return dispatch itself lives in the
//! embedded rubash engine (`run_function_return_trap`,
//! src/executor/trap_exec.rs, rubash#343/#345 family). niu 1.3.3 shipped a
//! rubash revision predating that work, which is what the issue observed;
//! the tests here pin the GNU-aligned contract on the niubash side so the
//! class cannot regress silently again.

use std::path::PathBuf;
use std::process::{Command, Stdio};

fn niu_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_niu"))
}

/// Runs a script body through the built niu binary and returns trimmed
/// stdout, mirroring the oracle probe style (script file, LF endings).
fn run_script(body: &str) -> String {
    let dir = std::env::temp_dir().join("niubash-issue204");
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("probe.sh");
    std::fs::write(&script, format!("{body}\n")).unwrap();
    let output = Command::new(niu_binary())
        .arg(&script)
        .stdin(Stdio::null())
        .output()
        .expect("spawn niu");
    assert!(
        output.status.success(),
        "niu failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string()
}

/// The issue's headline half: a sourced script's completion fires the
/// RETURN trap (no regression on the pre-existing path).
#[test]
fn return_trap_fires_when_source_completes() {
    let dir = std::env::temp_dir().join("niubash-issue204");
    std::fs::create_dir_all(&dir).unwrap();
    let sourced = dir.join("sourced_204.sh");
    std::fs::write(&sourced, "true\n").unwrap();
    let sourced = sourced.to_string_lossy().replace('\\', "/");
    let out = run_script(&format!(
        "trap 'echo T-SRC' RETURN\n. '{sourced}'\necho done"
    ));
    assert_eq!(out, "T-SRC\ndone");
}

/// The issue's other half, GNU-exact: a function WITHOUT the trace
/// attribute and without functrace does NOT inherit the RETURN trap (GNU
/// prints only `done`).
#[test]
fn plain_function_call_does_not_fire_return_trap() {
    let out = run_script("f() { :; }\ntrap 'echo T-RETURN' RETURN\nf\necho done");
    assert_eq!(out, "done");
}

/// `declare -ft f` gives f the trace attribute, so it inherits the RETURN
/// trap and fires on return even with the global functrace option off.
#[test]
fn trace_attribute_function_fires_return_trap_on_return() {
    let out = run_script("f() { :; }\ndeclare -ft f\ntrap 'echo T-RETURN' RETURN\nf\necho done");
    assert_eq!(out, "T-RETURN\ndone");
}

/// The separated cluster form (`declare -f -t f`, rubash#217) is the same
/// attribute and fires identically.
#[test]
fn declare_f_then_t_form_fires_return_trap() {
    let out = run_script("f() { :; }\ndeclare -f -t f\ntrap 'echo T-RETURN' RETURN\nf\necho done");
    assert_eq!(out, "T-RETURN\ndone");
}

/// `declare -t f` without `-f` goes down the variable-attribute path; GNU
/// does not give the function the trace attribute, so nothing fires.
#[test]
fn declare_t_without_f_does_not_fire_return_trap() {
    let out = run_script("f() { :; }\ndeclare -t f\ntrap 'echo T-RETURN' RETURN\nf\necho done");
    assert_eq!(out, "done");
}

/// extdebug implies functrace (shopt.def:621), so the function inherits the
/// RETURN trap and fires on return.
#[test]
fn extdebug_enables_return_trap_on_function_return() {
    let out =
        run_script("shopt -s extdebug\nf() { :; }\ntrap 'echo T-RETURN' RETURN\nf\necho done");
    assert_eq!(out, "T-RETURN\ndone");
}

/// With functrace on, every level of a nested call chain fires the RETURN
/// trap exactly once per return, unwinding inner-first.
#[test]
fn functrace_fires_return_trap_once_per_nested_level() {
    let out = run_script(
        "set -T\ntrap 'echo R' RETURN\ninner() { echo in; }\nouter() { inner; echo mid; }\nouter\necho done",
    );
    assert_eq!(out, "in\nR\nmid\nR\ndone");
}

/// A RETURN trap armed INSIDE a function fires at that function's return and
/// (GNU scope behavior) stays armed afterwards, replacing any outer trap.
#[test]
fn trap_set_inside_function_fires_at_its_return_and_persists() {
    let out = run_script(
        "trap 'echo OUTER' RETURN\nw() { trap 'echo INNER' RETURN; echo in-w; }\nw\necho after\ntrap -p RETURN",
    );
    assert_eq!(out, "in-w\nINNER\nafter\ntrap -- 'echo INNER' RETURN");
}

/// Oracle guard for the same scope rule: clearing the trap first means the
/// inner arming is the only fire.
#[test]
fn cleared_outer_trap_then_inner_arming_fires_once() {
    let out =
        run_script("trap - RETURN\nw() { trap 'echo INNER' RETURN; echo in-w; }\nw\necho done");
    assert_eq!(out, "in-w\nINNER\ndone");
}
