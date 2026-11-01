//! niubash#727 — gate ① (product-path expansion) and gate ④
//! (invisible fallback) on the real product: the built `niu`
//! binary running scripts with the FFI switch in four states —
//! off, on with an unloadable DLL (claims, then silently falls
//! back to spawn), on with the DLL missing (never claims), and
//! the kill switch. For every script in the battery, all four
//! arms must produce byte-identical stdout, stderr, and exit
//! status, and the anchored scripts must match their literal
//! expectations, so a fallback defect (double execution,
//! swallowed output, wrong status, noise, a hang) cannot hide.
//!
//! The route-log runs prove the "on" arm really traverses the
//! product path (session builder registration → warm-up →
//! engine route seam → provider): with `NIU_WINUXCMD_FFI_LOG=1`
//! the claimed arm logs `route grep -> Ffi (Claimed)`, the
//! missing-DLL arm logs a spawn verdict, and without the log
//! switch the fallback is completely silent on stderr.
//!
//! Platform scope: unix. On this platform the production
//! backend is the unavailable backend, so a claimed call
//! exercises the load-failure → silent-fallback leg for real;
//! the FFI-served leg (a loadable `winuxcore.dll` executing
//! in-process) is Windows-only and remains a real-machine cell
//! (see the #727 cluster report). The provider-seam battery
//! (`crates/niubash-runtime/tests/issue727_ffi_gates.rs`)
//! covers the served leg's byte equivalence against a scripted
//! backend implementing the DLL side of the ABI contract.
#![cfg(unix)]

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const DATA: &str = "alpha\nbeta gamma\ndelta\nbeta\n";
const EXTRA: &str = "second file\nbeta tail\n";

const TIERS: &str = "# winuxcmd-version: 9.9.9-issue727\n\
command\ttier\tfallback_shapes\tnote\n\
cat\ttier2\t-\tsynthetic battery row\n\
grep\ttier2\t-\tsynthetic battery row\n\
head\ttier2\t-\tsynthetic battery row\n\
cp\ttier1\t-\ttier-1 control row\n";

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
    dll: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!(
            "niu-issue727-{label}-{}-{nanos}",
            std::process::id()
        ));
        let home = base.join("home");
        let start = base.join("start");
        let ffi_dir = base.join("ffi");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&start).unwrap();
        std::fs::create_dir_all(&ffi_dir).unwrap();
        std::fs::write(start.join("data.txt"), DATA).unwrap();
        std::fs::write(start.join("extra.txt"), EXTRA).unwrap();
        let dll = ffi_dir.join("winuxcore.dll");
        std::fs::write(
            &dll,
            b"issue727 dummy: present for discovery, not a library",
        )
        .unwrap();
        std::fs::write(ffi_dir.join("tiers.tsv"), TIERS).unwrap();
        Self {
            base,
            home,
            start,
            dll,
        }
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
                // Gate ④: a fallback that hangs is a failed gate.
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

/// The four switch arms of the battery.
fn arms(fx: &Fixture) -> [(&'static str, Vec<(&'static str, String)>); 4] {
    let dll = fx.dll.to_string_lossy().into_owned();
    let missing = fx
        .base
        .join("absent/winuxcore.dll")
        .to_string_lossy()
        .into_owned();
    [
        ("off", Vec::new()),
        (
            "on-fallback",
            vec![
                ("NIU_WINUXCMD_FFI", "1".to_string()),
                ("NIU_WINUXCMD_FFI_DLL", dll.clone()),
            ],
        ),
        (
            "on-missing-dll",
            vec![
                ("NIU_WINUXCMD_FFI", "1".to_string()),
                ("NIU_WINUXCMD_FFI_DLL", missing),
            ],
        ),
        (
            "kill-switch",
            vec![
                ("NIU_WINUXCMD_FFI", "0".to_string()),
                ("NIU_WINUXCMD_FFI_DLL", dll),
            ],
        ),
    ]
}

struct Script {
    name: &'static str,
    body: &'static str,
    expect: Option<(i32, &'static str, &'static str)>,
}

const SCRIPTS: &[Script] = &[
    Script {
        name: "grep-basic",
        body: "grep beta data.txt",
        expect: Some((0, "beta gamma\nbeta\n", "")),
    },
    Script {
        name: "grep-numbered",
        body: "grep -n beta data.txt",
        expect: Some((0, "2:beta gamma\n4:beta\n", "")),
    },
    Script {
        name: "grep-nomatch",
        body: "grep omega data.txt",
        expect: Some((1, "", "")),
    },
    Script {
        name: "grep-missing",
        body: "grep beta missing.txt",
        expect: Some((2, "", "grep: missing.txt: No such file or directory\n")),
    },
    Script {
        name: "grep-mixed-files",
        body: "grep beta data.txt missing.txt",
        expect: Some((
            2,
            "data.txt:beta gamma\ndata.txt:beta\n",
            "grep: missing.txt: No such file or directory\n",
        )),
    },
    Script {
        name: "cat-files",
        body: "cat data.txt extra.txt",
        expect: None,
    },
    Script {
        name: "cat-redirect-in",
        body: "cat < data.txt",
        expect: Some((0, DATA, "")),
    },
    Script {
        name: "pipeline",
        body: "cat data.txt | grep beta | head -n 1",
        expect: Some((0, "beta gamma\n", "")),
    },
    Script {
        name: "head-file",
        body: "head -n 2 data.txt",
        expect: Some((0, "alpha\nbeta gamma\n", "")),
    },
    Script {
        name: "status-after-nomatch",
        body: "grep omega data.txt; echo rc=$?",
        expect: Some((0, "rc=1\n", "")),
    },
    Script {
        name: "status-after-match",
        body: "grep beta data.txt > /dev/null; echo rc=$?",
        expect: Some((0, "rc=0\n", "")),
    },
    Script {
        name: "for-loop",
        body: "for i in 1 2 3; do grep -c beta data.txt; done",
        expect: Some((0, "2\n2\n2\n", "")),
    },
    Script {
        name: "prefix-assignment",
        body: "MYPROBE=zzz grep beta data.txt",
        expect: Some((0, "beta gamma\nbeta\n", "")),
    },
    Script {
        // Sixty claimed-then-fell-back calls inside one process:
        // the fallback must stay invisible and stable in a loop,
        // not just on the first call.
        name: "repeat-60",
        body: "i=0; while [ $i -lt 60 ]; do grep beta data.txt > /dev/null || exit 9; \
               i=$((i+1)); done; echo done-60",
        expect: Some((0, "done-60\n", "")),
    },
];

#[test]
fn issue727_product_path_four_arms_byte_identical() {
    let fx = Fixture::new("battery");
    for script in SCRIPTS {
        let mut reference: Option<(&str, RunOut)> = None;
        for (arm, env) in arms(&fx) {
            let out = run_niu(&fx, script.body, &env);
            assert!(
                !out.timed_out,
                "{} on arm {arm}: niu hung (gate ④ forbids a hanging fallback)",
                script.name
            );
            if let Some((code, stdout, stderr)) = script.expect {
                assert_eq!(out.code, Some(code), "{} on arm {arm}: status", script.name);
                assert_eq!(
                    out.stdout,
                    stdout.as_bytes(),
                    "{} on arm {arm}: stdout",
                    script.name
                );
                assert_eq!(
                    out.stderr,
                    stderr.as_bytes(),
                    "{} on arm {arm}: stderr",
                    script.name
                );
            }
            match &reference {
                None => reference = Some((arm, out)),
                Some((ref_arm, ref_out)) => {
                    assert_eq!(
                        out.code, ref_out.code,
                        "{}: arm {arm} status vs arm {ref_arm}",
                        script.name
                    );
                    assert_eq!(
                        out.stdout, ref_out.stdout,
                        "{}: arm {arm} stdout vs arm {ref_arm}",
                        script.name
                    );
                    assert_eq!(
                        out.stderr, ref_out.stderr,
                        "{}: arm {arm} stderr vs arm {ref_arm}",
                        script.name
                    );
                }
            }
        }
    }
}

#[test]
fn issue727_product_path_redirect_artifact_identical() {
    // A redirected claimed call writes its artifact through the
    // fallback exactly as the off arm does.
    let fx = Fixture::new("redirect");
    let mut reference: Option<Vec<u8>> = None;
    for (arm, env) in arms(&fx) {
        let out = run_niu(&fx, "grep beta data.txt > found.txt", &env);
        assert!(!out.timed_out, "arm {arm} hung");
        assert_eq!(out.code, Some(0), "arm {arm}");
        assert_eq!(out.stdout, b"", "arm {arm}: redirect keeps stdout empty");
        let artifact = std::fs::read(fx.start.join("found.txt")).expect("artifact written");
        assert_eq!(artifact, b"beta gamma\nbeta\n", "arm {arm} artifact");
        match &reference {
            None => reference = Some(artifact),
            Some(previous) => assert_eq!(&artifact, previous, "arm {arm} artifact drift"),
        }
    }
}

#[test]
fn issue727_product_path_route_log_proves_traversal_and_silence() {
    let fx = Fixture::new("routelog");
    let dll = fx.dll.to_string_lossy().into_owned();

    // The on arm claims through the product path: the provider's
    // own route log is the receipt.
    let claimed = run_niu(
        &fx,
        "grep beta data.txt",
        &[
            ("NIU_WINUXCMD_FFI", "1".to_string()),
            ("NIU_WINUXCMD_FFI_DLL", dll.clone()),
            ("NIU_WINUXCMD_FFI_LOG", "1".to_string()),
        ],
    );
    assert!(!claimed.timed_out);
    assert_eq!(claimed.code, Some(0));
    assert_eq!(claimed.stdout, b"beta gamma\nbeta\n");
    let stderr = String::from_utf8_lossy(&claimed.stderr);
    assert!(
        stderr.contains("route grep -> Ffi (Claimed)"),
        "the on arm must traverse the FFI claim path; stderr: {stderr}"
    );

    // The missing-DLL arm never claims (no tier table): the log
    // shows the spawn verdict instead.
    let missing = run_niu(
        &fx,
        "grep beta data.txt",
        &[
            ("NIU_WINUXCMD_FFI", "1".to_string()),
            (
                "NIU_WINUXCMD_FFI_DLL",
                fx.base
                    .join("absent/winuxcore.dll")
                    .to_string_lossy()
                    .into_owned(),
            ),
            ("NIU_WINUXCMD_FFI_LOG", "1".to_string()),
        ],
    );
    assert_eq!(missing.code, Some(0));
    let stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(
        stderr.contains("route grep -> Spawn (NoTierTable)"),
        "the missing-DLL arm must not claim; stderr: {stderr}"
    );

    // The kill switch declines by switch state.
    let killed = run_niu(
        &fx,
        "grep beta data.txt",
        &[
            ("NIU_WINUXCMD_FFI", "0".to_string()),
            ("NIU_WINUXCMD_FFI_DLL", dll),
            ("NIU_WINUXCMD_FFI_LOG", "1".to_string()),
        ],
    );
    assert_eq!(killed.code, Some(0));
    let stderr = String::from_utf8_lossy(&killed.stderr);
    assert!(
        stderr.contains("route grep -> Spawn (SwitchOff)"),
        "the kill-switch arm must decline by switch; stderr: {stderr}"
    );

    // Gate ④ silence: the same on arm without the log switch is
    // byte-silent about FFI on stderr — the fallback is invisible
    // to the user.
    let silent = run_niu(
        &fx,
        "grep beta data.txt",
        &[
            ("NIU_WINUXCMD_FFI", "1".to_string()),
            (
                "NIU_WINUXCMD_FFI_DLL",
                fx.dll.to_string_lossy().into_owned(),
            ),
        ],
    );
    assert_eq!(silent.code, Some(0));
    assert_eq!(silent.stdout, b"beta gamma\nbeta\n");
    assert!(
        !String::from_utf8_lossy(&silent.stderr).contains("ffi"),
        "fallback must be silent; stderr: {}",
        String::from_utf8_lossy(&silent.stderr)
    );
    assert_eq!(silent.stderr, b"");
}
