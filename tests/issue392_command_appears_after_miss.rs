//! niubash#392: transient `command not found` (127) for an existing, unrelated
//! command, coinciding with a concurrent wpm/winuxcmd update.
//!
//! Reporter shape: inside ONE `niu -c` invocation a loop calling `awk` failed
//! 5/5 with `niu: line 1: awk: command not found`, healed in the next process,
//! while `.wpm` mtimes showed an update swapping the command layer in the same
//! window. `awk` is a WPM-managed link; wpm replaces links by unlink+create
//! (WinuxCmd#1188/#1206), so a lookup that lands in the absent window decides
//! whether the process keeps answering 127.
//!
//! Mechanism (root-caused in rubash#769, fixing this issue): the engine's
//! command-lookup cache memoized MISSES and per-directory listings for the
//! life of an env fingerprint, so one unlucky lookup during the absent window
//! poisoned the rest of the process. The fix stops memoizing misses and
//! validates a cached listing against the directory mtime before concluding a
//! miss. These tests run the deterministic no-race shape of that mechanism
//! through the real `niu` binary, so a niubash build wired to a stale
//! (pre-#769) rubash fails here even though the engine repo is fixed.
//!
//! Contract:
//!   - a name missed once, then created in an already-consulted PATH dir, must
//!     resolve on the next call in the SAME process (GNU bash behavior);
//!   - a genuinely absent name must still fail 127 twice in a row (negative
//!     answers stay honest - the fix is not "stop saying no");
//!   - a listing primed by one hit must not hide a second name added later.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Unique probe name - must not exist anywhere on the machine's PATH.
const PROBE_A: &str = "niu392probe_a";
const PROBE_B: &str = "niu392probe_b";

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

/// A tiny standalone executable to copy around as the "newly appeared"
/// command. Windows uses whoami.exe; Unix uses a generated `#!/bin/sh`
/// script because `/bin/true` is a toybox/coreutils multicall on some hosts
/// (Termux): copying that binary under the probe name makes it reject the
/// unknown applet instead of exiting 0.
fn donor_exe() -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(r"C:\Windows\System32\whoami.exe")
    }
    #[cfg(unix)]
    {
        use std::sync::OnceLock;
        static DONOR: OnceLock<PathBuf> = OnceLock::new();
        return DONOR
            .get_or_init(|| {
                let path = std::env::temp_dir().join(format!(
                    "niu392-donor-{}-{}.sh",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                ));
                std::fs::write(&path, "#!/bin/sh\nexit 0\n").expect("write donor script");
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                    .expect("chmod donor script");
                path
            })
            .clone();
    }
    #[cfg(not(any(windows, unix)))]
    {
        panic!("no donor executable for the issue392 probe");
    }
}

fn unique_scratch_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "niu-issue392-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Place `name` as an executable inside `dir`, platform-correct.
fn install_executable(dir: &Path, donor: &Path, name: &str) -> PathBuf {
    let target = dir.join(if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    });
    std::fs::copy(donor, &target).expect("copy donor exe");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))
            .expect("chmod +x");
    }
    target
}

fn run_niu_with_probe_dir(
    niu: &Path,
    probe_dir: &Path,
    donor: &Path,
    script: &str,
) -> (String, String, i32) {
    let mut path = probe_dir.to_string_lossy().into_owned();
    if let Ok(existing) = std::env::var("PATH") {
        let sep = if cfg!(windows) { ';' } else { ':' };
        path.push(sep);
        path.push_str(&existing);
    }
    let output = Command::new(niu)
        .arg("-c")
        .arg(script)
        .env("PATH", path)
        // The in-script creation copies the donor into the probe dir; both
        // must be visible to the child or the create silently fails and the
        // subsequent 127 would be a correct answer, not the poison.
        .env("NIU392_DONOR", donor)
        .env("NIU392_DIR", probe_dir)
        .output()
        .expect("spawn niu -c");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code().unwrap_or(-1),
    )
}

fn probe_call(name: &str) -> String {
    // stdout silenced so return codes stay greppable; stderr stays attached so
    // the command-not-found diagnostics remain assertable.
    format!("{name} >/dev/null; echo $?\n")
}

/// The deterministic #392 shape, no race needed: miss a name, create the
/// executable in the already-consulted PATH directory, call again - all in one
/// `niu -c` process. GNU bash finds it immediately; the pre-#769 engine kept
/// answering 127 for the life of the process (the reported 5/5 loop).
#[test]
fn command_appears_after_miss_in_one_process() {
    let niu = niu_binary();
    let dir = unique_scratch_dir("appear");
    let donor = donor_exe();

    let script = format!(
        "{}{}",
        probe_call(PROBE_A),
        // Creation happens from INSIDE the same process, after the miss.
        format_args!(
            "cp \"$NIU392_DONOR\" \"$NIU392_DIR/{}\"\n{}",
            if cfg!(windows) {
                format!("{PROBE_A}.exe")
            } else {
                PROBE_A.to_string()
            },
            probe_call(PROBE_A)
        )
    );

    let (stdout, stderr, _code) = run_niu_with_probe_dir(&niu, &dir, &donor, &script);
    let _ = std::fs::remove_dir_all(&dir);

    let rcs: Vec<&str> = stdout.lines().collect();
    assert_eq!(rcs.len(), 2, "two rc lines expected, got {stdout:?}");
    assert_eq!(
        rcs[0], "127",
        "first call: name absent, must be 127 ({stderr})"
    );
    assert_eq!(
        rcs[1], "0",
        "second call after creation, SAME process: must resolve (rc 0), got 127 - \
         the #392 miss-memoization is live in the wired rubash"
    );
    assert!(
        stderr.contains("command not found"),
        "the first miss must diagnose as command-not-found: {stderr:?}"
    );
}

/// Negative answers stay honest: a name that never appears must fail 127
/// twice in a row (guards the fix from regressing into "resolution never
/// fails", and pins the reporter's ability to trust a 127).
#[test]
fn genuinely_absent_name_still_fails_127_twice() {
    let niu = niu_binary();
    let dir = unique_scratch_dir("absent");

    let script = format!("{}{}", probe_call(PROBE_A), probe_call(PROBE_A));
    let (stdout, stderr, _code) = run_niu_with_probe_dir(&niu, &dir, &donor_exe(), &script);
    let _ = std::fs::remove_dir_all(&dir);

    let rcs: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        rcs,
        vec!["127", "127"],
        "absent name stays absent ({stderr})"
    );
    assert!(stderr.contains("command not found"), "{stderr:?}");
}

/// Listing-primed variant: resolving one name primes the per-directory
/// listing; a SECOND name created afterwards must still resolve (the cache
/// must not hold the primed "this is the whole directory" answer).
#[test]
fn second_name_added_after_primed_listing_resolves() {
    let niu = niu_binary();
    let dir = unique_scratch_dir("primed");
    let donor = donor_exe();

    install_executable(&dir, &donor, PROBE_A);

    let script = format!(
        "{}{}",
        probe_call(PROBE_A),
        format_args!(
            "cp \"$NIU392_DONOR\" \"$NIU392_DIR/{}\"\n{}",
            if cfg!(windows) {
                format!("{PROBE_B}.exe")
            } else {
                PROBE_B.to_string()
            },
            probe_call(PROBE_B)
        )
    );

    let (stdout, stderr, _code) = run_niu_with_probe_dir(&niu, &dir, &donor, &script);
    let _ = std::fs::remove_dir_all(&dir);

    let rcs: Vec<&str> = stdout.lines().collect();
    assert_eq!(rcs.len(), 2, "two rc lines expected, got {stdout:?}");
    assert_eq!(rcs[0], "0", "primed name resolves ({stderr})");
    assert_eq!(
        rcs[1], "0",
        "second name created after the listing was primed must resolve too"
    );
}

/// Hit stability: a name that exists from the start keeps resolving across
/// repeated calls - the miss-path hardening must not disturb the hit path.
#[test]
fn existing_command_keeps_resolving_repeatedly() {
    let niu = niu_binary();
    let dir = unique_scratch_dir("stable");
    let donor = donor_exe();

    install_executable(&dir, &donor, PROBE_A);

    let script = format!(
        "n=0; while [ $n -lt 20 ]; do\n{}n=$((n+1)); done",
        probe_call(PROBE_A)
    );
    let (stdout, stderr, _code) = run_niu_with_probe_dir(&niu, &dir, &donor, &script);
    let _ = std::fs::remove_dir_all(&dir);

    let rcs: Vec<&str> = stdout.lines().collect();
    assert_eq!(rcs.len(), 20, "20 calls expected, got {}", rcs.len());
    for (i, rc) in rcs.iter().enumerate() {
        assert_eq!(*rc, "0", "call {i} must resolve ({stderr})");
    }
}
