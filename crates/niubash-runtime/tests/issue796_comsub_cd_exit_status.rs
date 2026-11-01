//! niubash#796 — comsub assignment + `cd` made every later `$?` read the
//! command substitution's own exit status instead of the last command's.
//!
//! Root cause (host layer): the cd-bearing all-simple-sequence fast path
//! (`execute_host_synced_simple_ast`, removed by the fix this file pins)
//! drove the engine per command via `Executor::execute_command`, bypassing
//! the engine list walker's per-command bookkeeping — most visibly the
//! one-command-scoped `$?` comsub overlay reset (rubash ast_exec.rs:541,
//! clearing `word_expansion_comsub_exit`, the word-expansion-local comsub
//! status of rubash#485). After `X=$(exit 5)` the overlay stayed armed, so
//! `false; echo $?` printed 5 (the substitution's status), `command -v
//! nosuchcmd; echo $?` printed 0, and a child exiting 3 vanished. GNU has
//! no such overlay: subst.c command_substitute writes the child's wait
//! status into last_command_exit_value and the NEXT command's own
//! completion overwrites it (execute_cmd.c:624 execute_command_internal
//! commits per command), so after `false` a `$?` read is always 1.
//!
//! Every expectation below was measured against the WSL GNU bash 5.3.20
//! oracle (script files, `MSYS_NO_PATHCONV=1 wsl bash <file>`), plus the
//! standalone rubash.exe of the same engine commit, which was green
//! before this fix — the defect was exclusively in the niu host layer's
//! driving face.
//!
//! The one-shot face here is the same one `niu -c` drives
//! (`Shell::new_one_shot` + `execute_script`); each probe cds into a
//! tempdir so the sequence shape (comsub assignment, `cd`, failing
//! command, `$?` reader) matches the issue's `DIR=$(...); cd "$DIR"`
//! family without depending on any fixed path.

use std::path::Path;

use niubash_runtime::Shell;

/// Run `script` through the one-shot product face; returns its exit
/// code. The script must write its `$?` observations to `cap`
/// itself — the driver returns only the run status.
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

/// Run `body` (the commands after the cd) inside the #796 trigger
/// prefix — a comsub assignment followed by `cd` into a fresh tempdir —
/// with `$?` readers redirected to a capture file. Returns the capture
/// contents. The process cwd is restored afterwards so later tests and
/// the tempdir drop are unaffected.
fn probe_after_comsub_cd(body: &str) -> String {
    let dir = tempfile::tempdir().expect("trigger tempdir");
    let cap = dir.path().join("cap.txt");
    let script = format!(
        "X=$(echo hi); cd {}\n{}\n",
        shell_path(dir.path()),
        body.replace("{cap}", &shell_path(&cap))
    );
    let home = std::env::current_dir().expect("test cwd readable");
    let code = run_script(&script);
    std::env::set_current_dir(&home).expect("restore test cwd");
    // The reader lines echo into the capture; a nonzero run status here
    // means the script itself failed before writing its observation.
    assert_eq!(
        code,
        0,
        "probe script must succeed; body was:\n{body}\ncapture:\n{}",
        std::fs::read_to_string(&cap).unwrap_or_default()
    );
    std::fs::read_to_string(&cap).unwrap_or_default()
}

/// Shape 1: `false` after comsub+cd. Every reader spelling that consults
/// the expansion overlay (bare `$?`, `$?` mid-word, `$?` inside quotes
/// with more word text) must read the failing command's own status.
#[test]
fn comsub_cd_false_status_survives_all_reader_shapes() {
    // GNU: `false` -> $? = 1.
    assert_eq!(
        probe_after_comsub_cd(r#"false; echo $? > {cap}"#),
        "1\n",
        "bare $? read after comsub+cd+false"
    );
    assert_eq!(
        probe_after_comsub_cd(r#"false; echo "missing-rc=$?" > {cap}"#),
        "missing-rc=1\n",
        "quoted reader with word text before $?"
    );
    assert_eq!(
        probe_after_comsub_cd(r#"false; echo pre$?post > {cap}"#),
        "pre1post\n",
        "$? embedded mid-word"
    );
    // The failing command keeps its status even when a successful
    // command precedes it in the same sequence (issue: "失败前夹成功
    // 命令不救").
    assert_eq!(
        probe_after_comsub_cd(r#"true; false; echo $? > {cap}"#),
        "1\n",
        "success before the failure does not mask it"
    );
}

/// Shape 1 mechanism pin: the leaked value was the substitution's OWN
/// exit status (`X=$(exit 5)` -> readers printed 5), proving the read
/// side was shadowed by the comsub overlay rather than the failure not
/// being recorded. With the fix, the substitution's status is visible
/// only until the next command commits its own.
#[test]
fn comsub_status_does_not_shadow_next_command() {
    // GNU: the assignment command completes with the substitution's
    // status while it is still the last command...
    assert_eq!(
        probe_after_comsub_cd(r#"X=$(sh -c 'exit 5' 2>/dev/null); echo $? > {cap}"#),
        "5\n",
        "$? right after the comsub assignment IS the substitution's status"
    );
    // ...and the next command overwrites it (execute_cmd.c:624).
    assert_eq!(
        probe_after_comsub_cd(r#"X=$(sh -c 'exit 5' 2>/dev/null); false; echo $? > {cap}"#),
        "1\n",
        "the failure after the comsub must overwrite the substitution status"
    );
    // Same through the bare engine shape: an in-shell `exit` inside the
    // substitution needs no external child, pinning the same rule
    // without PATH dependence.
    assert_eq!(
        probe_after_comsub_cd(r#"X=$(exit 5); false; echo $? > {cap}"#),
        "1\n",
        "subshell-exit comsub status must not survive `false`"
    );
}

/// Shape 2: `command -v nosuchcmd-xyz` (builtin miss) after comsub+cd —
/// the configure-script probe idiom whose silent 0 broke every
/// dependency check.
#[test]
fn comsub_cd_command_v_miss_reports_one() {
    assert_eq!(
        probe_after_comsub_cd(r#"command -v nosuchcmd-xyz; echo $? > {cap}"#),
        "1\n",
        "command -v miss must report 1, not the leaked comsub 0"
    );
    // The miss keeps its status after a hit, which pre-fix restored the
    // leaked overlay value instead (bisectU1: hit, hit, miss -> 0).
    assert_eq!(
        probe_after_comsub_cd(
            r#"command -v echo; command -v cd; command -v nosuchcmd-xyz; echo $? > {cap}"#
        ),
        "1\n",
        "hits before the miss must not resurrect the comsub status"
    );
}

/// Shape 3: a child process exiting 3 after comsub+cd — the last
/// command's exit code was swallowed wholesale (issue: `bash -c 'exit
/// 3'; echo $?` -> 0). Uses the engine's own `( exit 3 )` subshell so
/// the pin holds on any PATH; an external-child variant runs only when
/// a shell child is available.
#[test]
fn comsub_cd_subshell_exit_code_not_swallowed() {
    assert_eq!(
        probe_after_comsub_cd(r#"( exit 3 ); echo $? > {cap}"#),
        "3\n",
        "subshell exit 3 must reach $?"
    );
    assert_eq!(
        probe_after_comsub_cd(r#"false; ( exit 3 ); echo $? > {cap}"#),
        "3\n",
        "the LAST command's status wins"
    );
}

/// External-child variant of shape 3, premise-guarded the
/// tests/issue207_platform_smoke_contract.rs way: probe the premise
/// (a runnable `sh -c` child) through the shell itself and skip only
/// when the environment provides none.
#[test]
fn comsub_cd_external_child_exit_code_not_swallowed() {
    let premise = probe_after_comsub_cd(r#"sh -c 'exit 0' >/dev/null 2>&1; echo $? > {cap}"#);
    if premise.trim() != "0" {
        eprintln!("skipped: no runnable `sh -c` child in this environment");
        return;
    }
    assert_eq!(
        probe_after_comsub_cd(r#"sh -c 'exit 3'; echo $? > {cap}"#),
        "3\n",
        "child exit 3 must reach $?"
    );
    assert_eq!(
        probe_after_comsub_cd(r#"bash -c 'exit 3' 2>/dev/null; echo $? > {cap}"#),
        "3\n",
        "same for a bash child (issue's exact probe)"
    );
}

/// Boundary pins from the issue: comsub alone or cd alone never
/// misreported; only the stacked shape did. These guard against an
/// over-broad "fix" that would change the single-ingredient behavior.
#[test]
fn single_ingredient_sequences_still_track_status() {
    let dir = tempfile::tempdir().expect("boundary tempdir");
    let cap = dir.path().join("cap.txt");
    let cap_path = shell_path(&cap);

    // comsub without cd
    let mut shell = Shell::new_one_shot().expect("one-shot shell constructs");
    shell.session_mut().inherit_process_stdin();
    let code = shell
        .execute_script(&format!("X=$(echo hi)\nfalse\necho $? > {cap_path}\n"))
        .expect("comsub-only script runs");
    assert_eq!(code, 0);
    assert_eq!(
        std::fs::read_to_string(&cap).unwrap_or_default(),
        "1\n",
        "comsub without cd: $? must be the failure's"
    );

    // cd without comsub
    let home = std::env::current_dir().expect("test cwd readable");
    let mut shell = Shell::new_one_shot().expect("one-shot shell constructs");
    shell.session_mut().inherit_process_stdin();
    let code = shell
        .execute_script(&format!(
            "cd {}\nfalse\necho $? > {cap_path}\n",
            shell_path(dir.path())
        ))
        .expect("cd-only script runs");
    std::env::set_current_dir(&home).expect("restore test cwd");
    assert_eq!(code, 0);
    assert_eq!(
        std::fs::read_to_string(&cap).unwrap_or_default(),
        "1\n",
        "cd without comsub: $? must be the failure's"
    );
}
