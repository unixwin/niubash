//! niubash#727 — the FFI route is REMOVED (2026-11-01 ruling:
//! FFI does not revive in 1.5.0, and the residue comes out of the
//! product rather than sitting default-off). This pin holds the
//! post-removal contract on the real product path: with the whole
//! `NIU_WINUXCMD_FFI*` environment family set — master switch on,
//! a DLL path planted (fake bytes, plus a `tiers.tsv` claiming
//! tier-2 rows beside it, exactly the #727 activation recipe) or
//! missing — nothing reads those variables, nothing claims a
//! route, and every command runs through the PATH spawn route
//! with byte-correct results.
//!
//! The shapes asserted here are the #727 data-loss shapes, one
//! for one: F1 stdin redirection (`sort < f`, `wc -l < f`,
//! heredoc), F2 stdout redirection (`sort < f > out`), and F5
//! command-substitution capture (`$(tr a-z A-Z < f)`), plus the
//! pipeline shape that was always equivalent. On the pre-removal
//! tree these same assertions were green in the default-off state
//! and in the unix fallback arms, and red in the claimed-and-
//! served state on Windows (the #727 gate matrix: 56 of 204
//! cells damaged, all of them FFI-claimed cells). This pin keeps
//! them green in the only state that now exists.
//!
//! Platform scope: unix, mirroring the removed product-path
//! battery — the spawn route's byte behavior is the contract
//! being pinned, and the external commands resolve from the
//! platform `PATH` exactly as a session resolves them.
#![cfg(unix)]

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const WORDS: &str = "delta\nalpha\nbeta gamma\nbeta\n";
const SORTED: &str = "alpha\nbeta\nbeta gamma\ndelta\n";

/// The #727 activation recipe's tier table: sort/wc/tr claimed
/// at tier 2. Post-removal nothing reads this file; planting it
/// proves the table is as inert as the switch.
const TIERS: &str = "# winuxcmd-version: 9.9.9-issue727\n\
command\ttier\tfallback_shapes\tnote\n\
sort\ttier2\t-\tsynthetic battery row\n\
wc\ttier2\t-\tsynthetic battery row\n\
tr\ttier2\t-\tsynthetic battery row\n";

fn niu_binary() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_niu"));
    if p.exists() {
        return p;
    }
    let mut fallback = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fallback.push("target");
    fallback.push("debug");
    fallback.push("niu");
    fallback
}

struct Fixture {
    base: PathBuf,
    home: PathBuf,
    start: PathBuf,
    fake_dll: PathBuf,
    missing_dll: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!(
            "niu-issue727-removed-{label}-{}-{nanos}",
            std::process::id()
        ));
        let home = base.join("home");
        let start = base.join("start");
        let ffi_dir = base.join("ffi");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&start).unwrap();
        std::fs::create_dir_all(&ffi_dir).unwrap();
        std::fs::write(start.join("words.txt"), WORDS).unwrap();
        let fake_dll = ffi_dir.join("winuxcore.dll");
        std::fs::write(
            &fake_dll,
            b"issue727 dummy: present for discovery, not a library",
        )
        .unwrap();
        std::fs::write(ffi_dir.join("tiers.tsv"), TIERS).unwrap();
        let missing_dll = base.join("absent/winuxcore.dll");
        Self {
            base,
            home,
            start,
            fake_dll,
            missing_dll,
        }
    }

    /// Both DLL arms: a planted fake DLL (with its claiming tier
    /// table) and a path that does not exist at all.
    fn dll_arms(&self) -> [(&'static str, String); 2] {
        [
            ("fake-dll", self.fake_dll.to_string_lossy().into_owned()),
            (
                "missing-dll",
                self.missing_dll.to_string_lossy().into_owned(),
            ),
        ]
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

struct RunOut {
    code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    timed_out: bool,
}

fn run_niu(fx: &Fixture, script: &str, extra_env: &[(&str, String)]) -> RunOut {
    let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_string());
    let mut cmd = Command::new(niu_binary());
    cmd.args(["-c", script])
        .current_dir(&fx.start)
        .env_clear()
        .env("PATH", path)
        .env("HOME", &fx.home)
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .env("TMPDIR", "/tmp")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    let mut child = cmd.spawn().expect("spawn niu");
    let mut out_pipe = child.stdout.take().expect("stdout pipe");
    let mut err_pipe = child.stderr.take().expect("stderr pipe");
    let out_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out_pipe.read_to_end(&mut buf);
        buf
    });
    let err_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = err_pipe.read_to_end(&mut buf);
        buf
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    let (code, timed_out) = loop {
        match child.try_wait().expect("poll niu") {
            Some(status) => break (status.code(), false),
            None if Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break (None, true);
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    };
    RunOut {
        code,
        stdout: out_thread.join().unwrap_or_default(),
        stderr: err_thread.join().unwrap_or_default(),
        timed_out,
    }
}

fn ffi_env(dll: &str) -> Vec<(&'static str, String)> {
    vec![
        ("NIU_WINUXCMD_FFI", "1".to_string()),
        ("NIU_WINUXCMD_FFI_DLL", dll.to_string()),
    ]
}

fn assert_clean(out: &RunOut, what: &str, arm: &str) {
    assert!(!out.timed_out, "{what} on arm {arm}: niu hung");
    assert_eq!(out.code, Some(0), "{what} on arm {arm}: status");
    assert_eq!(
        out.stderr,
        b"",
        "{what} on arm {arm}: stderr must stay silent (no route log, no fallback noise); got: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn issue727_removed_stdin_and_redirect_shapes_are_byte_correct() {
    let fx = Fixture::new("shapes");
    for (arm, dll) in fx.dll_arms() {
        let env = ffi_env(&dll);

        // F2 + F1: `sort < f > out` — the stdin redirect reaches
        // the applet and the stdout redirect captures its output;
        // the #727 claimed state wrote an empty file here.
        let out = run_niu(&fx, "sort < words.txt > out.txt", &env);
        assert_clean(&out, "sort-redirect", arm);
        assert_eq!(out.stdout, b"", "sort-redirect on arm {arm}: stdout");
        let artifact = std::fs::read(fx.start.join("out.txt")).expect("artifact written");
        assert_eq!(artifact, SORTED.as_bytes(), "sort-redirect on arm {arm}");

        // F1: `wc -l < f` counts the redirected stdin; the #727
        // claimed state reported 0.
        let out = run_niu(&fx, "wc -l < words.txt", &env);
        assert_clean(&out, "wc-stdin", arm);
        assert_eq!(out.stdout, b"4\n", "wc-stdin on arm {arm}");

        // F1, heredoc form: the heredoc reaches the applet's
        // stdin (11 of 12 heredoc cells were damaged in #727).
        let out = run_niu(&fx, "sort <<EOF\ndelta\nalpha\nEOF", &env);
        assert_clean(&out, "sort-heredoc", arm);
        assert_eq!(out.stdout, b"alpha\ndelta\n", "sort-heredoc on arm {arm}");
    }
}

#[test]
fn issue727_removed_capture_and_pipeline_shapes_are_byte_correct() {
    let fx = Fixture::new("capture");
    for (arm, dll) in fx.dll_arms() {
        let env = ffi_env(&dll);

        // F5: `$(tr a-z A-Z < f)` — the capture holds the
        // transformed bytes; the #727 claimed state captured
        // empty and leaked the bytes to the terminal.
        let out = run_niu(&fx, "x=$(tr a-z A-Z < words.txt); echo \"cap=[$x]\"", &env);
        assert_clean(&out, "capture-tr", arm);
        assert_eq!(
            out.stdout, b"cap=[DELTA\nALPHA\nBETA GAMMA\nBETA]\n",
            "capture-tr on arm {arm}"
        );

        // The pipeline shape: always equivalent, must not move.
        let out = run_niu(&fx, "cat words.txt | grep beta | head -n 1", &env);
        assert_clean(&out, "pipeline", arm);
        assert_eq!(out.stdout, b"beta gamma\n", "pipeline on arm {arm}");
    }
}

#[test]
fn issue727_removed_env_family_is_inert() {
    let fx = Fixture::new("inert");
    // The full family, including the route-log switch: with the
    // route gone there is no logger left to switch on, so the
    // FFI=1 arm must be byte-identical to the unset arm —
    // stdout, stderr, and status alike.
    let script = "sort < words.txt";
    let mut reference: Option<RunOut> = None;
    let arms: [(&str, Vec<(&str, String)>); 3] = [
        ("unset", Vec::new()),
        ("on-with-log", {
            let mut env = ffi_env(&fx.fake_dll.to_string_lossy());
            env.push(("NIU_WINUXCMD_FFI_LOG", "1".to_string()));
            env
        }),
        (
            "explicit-off",
            vec![
                ("NIU_WINUXCMD_FFI", "0".to_string()),
                (
                    "NIU_WINUXCMD_FFI_DLL",
                    fx.fake_dll.to_string_lossy().into_owned(),
                ),
            ],
        ),
    ];
    for (arm, env) in arms {
        let out = run_niu(&fx, script, &env);
        assert_clean(&out, "inert", arm);
        assert_eq!(out.stdout, SORTED.as_bytes(), "inert on arm {arm}");
        match &reference {
            None => reference = Some(out),
            Some(reference) => {
                assert_eq!(out.code, reference.code, "inert: arm {arm} status drift");
                assert_eq!(
                    out.stdout, reference.stdout,
                    "inert: arm {arm} stdout drift"
                );
                assert_eq!(
                    out.stderr, reference.stderr,
                    "inert: arm {arm} stderr drift"
                );
            }
        }
    }
}
