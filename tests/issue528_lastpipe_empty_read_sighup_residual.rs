//! niubash#528 (W5 residual / characterization pin): under
//! `shopt -s lastpipe`, `printf '' | read x` — GNU bash dies by SIGHUP
//! semantics: the read reports rc=129 and the shell prints a
//! `<pid> Hangup | read x` notice on stderr; niu originally returned
//! rc=1 silently.
//!
//! Recorded disposition: this niubash filing was a DUPLICATE of the
//! canonical engine issue unixwin/rubash#1217 (so recorded on the
//! issue itself), closed not_planned here in favor of the engine
//! tracker. rubash#1217 is closed/completed (engine fix 78f8ac45),
//! and the current engine delivers the GNU semantics on this host:
//! rc=129 plus the Hangup notice. This pin asserts exactly that
//! ruled surface — the 129 status and the notice naming the `read` —
//! through the niu binary in the issue's script-file repro form.
//!
//! Observed adjacent divergence (recorded, deliberately NOT asserted):
//! the notice's program-name prefix differs — GNU prefixes with the
//! script name as invoked (`r04.sh: line 2: ...`), the engine
//! prefixes with its face name (`bash: line 2: ...`). That is a
//! face-name question beyond #1217's ruling; it is noted in the
//! bf-w5 REPORT for the main session to adjudicate.

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

fn run_script(tag: &str, body: &str) -> (Output, PathBuf) {
    let dir = std::env::temp_dir().join(format!("niu-528-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let script = dir.join("probe.sh");
    std::fs::write(&script, body).expect("write script");
    let out = Command::new(niu_binary())
        .arg(&script)
        .current_dir(&dir)
        .env("HOME", &dir)
        .env_remove("NIU_ENV")
        .env_remove("BASH_ENV")
        .stdin(Stdio::null())
        .output()
        .expect("spawn niu");
    (out, dir)
}

#[test]
fn issue528_lastpipe_empty_pipe_read_reports_129_with_hangup_notice() {
    let (out, dir) = run_script(
        "empty",
        "shopt -s lastpipe\nprintf '' | read x; echo \"rc=$? x=[$x]\"\n",
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "rc=129 x=[]\n");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        stderr.contains("Hangup") && stderr.contains("read x"),
        "GNU-parity Hangup notice naming the read is required, got: {stderr:?}"
    );
}

#[test]
fn issue528_lastpipe_pipefail_true_pipe_read_reports_129() {
    let (out, dir) = run_script(
        "pipefail",
        "shopt -s lastpipe\nset -o pipefail; true | read x; echo \"rc=$? x=[$x]\"\n",
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "rc=129 x=[]\n");
}

#[test]
fn issue528_procsub_empty_read_stays_rc1_no_hangup() {
    // Scope pin from the issue: process substitution is NOT the
    // lastpipe pipeline path — rc=1 in both shells, no Hangup notice.
    let (out, dir) = run_script("procsub", "read x < <(printf ''); echo \"rc=$? x=[$x]\"\n");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "rc=1 x=[]\n");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        !stderr.contains("Hangup"),
        "procsub shape must not produce a Hangup notice, got: {stderr:?}"
    );
}
