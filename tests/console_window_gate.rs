//! niubash#451 regression gate: a console-less `niu -c` run must never show
//! a console window — not for niu.exe and not for any external child it
//! spawns. The bug class: a console-subsystem child created by a parent
//! with no console gets a *visible* console auto-allocated by Windows.
//!
//! Two spawn postures are covered:
//! - DETACHED_PROCESS niu (agents/CI/git hooks, the `bash.exe` forwarder):
//!   children take rubash's CREATE_NO_WINDOW path — a *hidden* console is
//!   fine, a visible window fails the gate.
//! - Plain-spawned niu (callers that forward no creation flags): Windows
//!   auto-allocates niu.exe a visible console before main(); the gate only
//!   demands that no such window survives the startup release — a brief
//!   (<poll) flash is the best a CUI binary can do in-process.

#![cfg(windows)]

use std::collections::HashMap;
use std::ffi::c_void;
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;

type Hwnd = *mut c_void;

#[link(name = "user32")]
extern "system" {
    fn EnumWindows(cb: extern "system" fn(Hwnd, isize) -> i32, lparam: isize) -> i32;
    fn GetClassNameW(hwnd: Hwnd, buf: *mut u16, len: i32) -> i32;
    fn IsWindowVisible(hwnd: Hwnd) -> i32;
    fn GetWindowThreadProcessId(hwnd: Hwnd, pid: *mut u32) -> u32;
    fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> *mut c_void;
    fn Process32FirstW(snap: *mut c_void, pe: *mut ProcessEntry) -> i32;
    fn Process32NextW(snap: *mut c_void, pe: *mut ProcessEntry) -> i32;
    fn CloseHandle(h: *mut c_void) -> i32;
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
    fn GetProcessTimes(
        h: *mut c_void,
        creation: *mut u64,
        exit: *mut u64,
        kernel: *mut u64,
        user: *mut u64,
    ) -> i32;
}

const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;

/// Process creation FILETIME (100 ns ticks since 1601), 0 if unreadable.
///
/// Root-D adjudication: the conhost counting in `run_and_observe_exe`
/// originally attributed every `conhost.exe` whose ppid chain reached
/// the root PID. A conhost *lingering* from an earlier, already-dead
/// niu whose PID was recycled as the new root satisfies that walk
/// while never having been allocated by the observed session — under
/// full-suite parallel load this produced phantom counts (one observed
/// "4 distinct conhost PIDs" failure that no instrumented rerun could
/// reproduce: 32/32 probed sessions, idle and under parallel load,
/// showed exactly one conhost per session, created 3–27 ms after the
/// root and alive for the whole session). A conhost allocated by/for
/// the observed session is necessarily created at or after the root
/// process itself; anything older is not this session's console.
fn creation_filetime(pid: u32) -> u64 {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return 0;
        }
        let (mut c, mut e, mut k, mut u) = (0u64, 0u64, 0u64, 0u64);
        let ok = GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u);
        CloseHandle(h);
        if ok != 0 {
            c
        } else {
            0
        }
    }
}

#[repr(C)]
struct ProcessEntry {
    size: u32,
    usage: u32,
    pid: u32,
    default_heap: usize,
    module_id: u32,
    threads: u32,
    ppid: u32,
    pri_class_base: i32,
    flags: u32,
    exe_file: [u16; 260],
}

/// PIDs owning at least one visible `ConsoleWindowClass` window.
fn visible_console_window_owners() -> Vec<u32> {
    use std::sync::Mutex;
    static OWNERS: Mutex<Vec<u32>> = Mutex::new(Vec::new());
    extern "system" fn collect(hwnd: Hwnd, _lparam: isize) -> i32 {
        let mut cls = [0u16; 64];
        unsafe {
            if GetClassNameW(hwnd, cls.as_mut_ptr(), cls.len() as i32) == 0 {
                return 1;
            }
            let name =
                String::from_utf16_lossy(&cls[..cls.iter().position(|&c| c == 0).unwrap_or(64)]);
            if name == "ConsoleWindowClass" && IsWindowVisible(hwnd) != 0 {
                let mut pid = 0u32;
                GetWindowThreadProcessId(hwnd, &mut pid);
                OWNERS.lock().unwrap().push(pid);
            }
        }
        1
    }
    OWNERS.lock().unwrap().clear();
    unsafe {
        EnumWindows(collect, 0);
    }
    std::mem::take(&mut *OWNERS.lock().unwrap())
}

/// pid → parent pid for every process in the snapshot.
fn process_tree() -> HashMap<u32, u32> {
    let mut map = HashMap::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap.is_null() || snap == -1isize as *mut c_void {
            return map;
        }
        let mut pe = ProcessEntry {
            size: std::mem::size_of::<ProcessEntry>() as u32,
            usage: 0,
            pid: 0,
            default_heap: 0,
            module_id: 0,
            threads: 0,
            ppid: 0,
            pri_class_base: 0,
            flags: 0,
            exe_file: [0; 260],
        };
        if Process32FirstW(snap, &mut pe) != 0 {
            loop {
                map.insert(pe.pid, pe.ppid);
                if Process32NextW(snap, &mut pe) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
    }
    map
}

/// Is `pid` `root` itself or a descendant of it?
fn in_tree(pid: u32, root: u32, tree: &HashMap<u32, u32>) -> bool {
    let mut cur = pid;
    for _ in 0..64 {
        if cur == root {
            return true;
        }
        match tree.get(&cur) {
            Some(&next) if next != cur && next != 0 => cur = next,
            _ => return false,
        }
    }
    false
}

/// Snapshot `(pid, ppid, exe_name_lowercase)` for every process on the host.
fn process_entries() -> Vec<(u32, u32, String)> {
    let mut out = Vec::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap.is_null() || snap == -1isize as *mut c_void {
            return out;
        }
        let mut pe = ProcessEntry {
            size: std::mem::size_of::<ProcessEntry>() as u32,
            usage: 0,
            pid: 0,
            default_heap: 0,
            module_id: 0,
            threads: 0,
            ppid: 0,
            pri_class_base: 0,
            flags: 0,
            exe_file: [0; 260],
        };
        if Process32FirstW(snap, &mut pe) != 0 {
            loop {
                let len = pe
                    .exe_file
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(pe.exe_file.len());
                let name = String::from_utf16_lossy(&pe.exe_file[..len]).to_ascii_lowercase();
                out.push((pe.pid, pe.ppid, name));
                if Process32NextW(snap, &mut pe) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
    }
    out
}

/// Poll while `child` runs; collect owner pids of visible console windows
/// belonging to `child`'s subtree. `transient_ok` ignores windows that die
/// before the next poll (the auto-allocated niu window released at
/// startup); `transient_ok=false` treats any sighting as a violation.
fn collect_offenders(
    child_pid: u32,
    mut alive: impl FnMut() -> bool,
    transient_ok: bool,
) -> Vec<u32> {
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut persistent: HashMap<u32, Instant> = HashMap::new();
    while alive() && Instant::now() < deadline {
        let owners = visible_console_window_owners();
        if !owners.is_empty() {
            let tree = process_tree();
            for owner in owners {
                if in_tree(owner, child_pid, &tree) {
                    persistent.entry(owner).or_insert_with(Instant::now);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    let grace = Duration::from_millis(if transient_ok { 500 } else { 0 });
    persistent
        .into_iter()
        .filter(|(_, first_seen)| !transient_ok || first_seen.elapsed() >= grace)
        .map(|(pid, _)| pid)
        .collect()
}

fn spawn_niu(args: &[&str], detached: bool) -> std::process::Child {
    let niu = env!("CARGO_BIN_EXE_niu");
    let mut command = Command::new(niu);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if detached {
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    command.spawn().expect("spawn niu")
}

#[test]
fn detached_niu_c_spawns_no_visible_console_window() {
    // ~2s of external-command work — long enough for a regression to flash.
    let mut child = spawn_niu(
        &["-c", "C:/Windows/System32/ping.exe -n 3 127.0.0.1 >NUL"],
        true,
    );
    let pid = child.id();
    let offenders = collect_offenders(pid, || child.try_wait().ok().flatten().is_none(), false);
    let status = child.wait().expect("wait niu");
    assert!(status.success(), "niu -c ping exited {status}");
    assert!(
        offenders.is_empty(),
        "visible console windows appeared in the niu tree (owner pids {offenders:?})"
    );
}

#[test]
fn plainly_spawned_niu_c_leaves_no_console_window_behind() {
    // A console-less host makes niu auto-allocate a visible console that the
    // startup release must kill; a host WITH a console shares it (no new
    // window at all). Either way nothing visible may persist.
    let mut child = spawn_niu(
        &["-c", "C:/Windows/System32/ping.exe -n 3 127.0.0.1 >NUL"],
        false,
    );
    let pid = child.id();
    let offenders = collect_offenders(pid, || child.try_wait().ok().flatten().is_none(), true);
    let status = child.wait().expect("wait niu");
    assert!(status.success(), "niu -c ping exited {status}");
    assert!(
        offenders.is_empty(),
        "console windows lingered past the startup release (owner pids {offenders:?})"
    );
}

#[test]
fn detached_multi_command_niu_c_allocates_at_most_one_hidden_console() {
    // niubash#462: a console-less `niu -c` session running N sequential
    // external commands must claim the first child's hidden console and let
    // children 2..N inherit it, allocating at most 1 conhost.exe for the
    // entire session while keeping piped stdout intact.
    let script = concat!(
        "C:/Windows/System32/hostname.exe; ",
        "C:/Windows/System32/ping.exe -n 2 127.0.0.1 >NUL; ",
        "C:/Windows/System32/hostname.exe; ",
        "C:/Windows/System32/ping.exe -n 2 127.0.0.1 >NUL; ",
        "C:/Windows/System32/hostname.exe"
    );
    let (output, offenders, conhosts) = run_and_observe_niu(&["-c", script], DETACHED_PROCESS);
    assert!(output.status.success(), "niu -c exited {}", output.status);
    assert!(
        offenders.is_empty(),
        "visible console windows appeared (owner pids {offenders:?})"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(
        lines.len(),
        3,
        "piped stdout must stay intact across hidden-console attach + subsequent children; got: {stdout:?}"
    );
    assert!(
        conhosts.len() <= 1,
        "expected at most 1 conhost.exe across 5 external spawns, saw {}: {conhosts:?}",
        conhosts.len()
    );
}

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Spawn `exe` with `args` and custom `flags` (+ `CREATE_NEW_PROCESS_GROUP`),
/// polling every 15 ms for visible `ConsoleWindowClass` windows and distinct
/// `conhost.exe` processes anywhere in `child`'s process subtree.
fn run_and_observe_exe(
    exe: &std::path::Path,
    args: &[&str],
    flags: u32,
    stdin_bytes: Option<&[u8]>,
) -> (
    std::process::Output,
    Vec<u32>,
    std::collections::HashSet<u32>,
) {
    let mut command = Command::new(exe);
    command
        .args(args)
        // Keep the console gates hermetic: on a host where winuxcmd's
        // command links are missing (CI runner), startup auto-activation
        // would spawn `winuxcmd.exe wpm links rebuild` and legitimately
        // allocate the session's one hidden console, breaking the
        // builtin-only 0-conhost assertion for environment reasons.
        .env("NIU_SKIP_WINUXCMD_ACTIVATION", "1")
        .creation_flags(flags | CREATE_NEW_PROCESS_GROUP)
        .stdin(if stdin_bytes.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn target");
    if let Some(bytes) = stdin_bytes {
        if let Some(mut sin) = child.stdin.take() {
            use std::io::Write;
            let _ = sin.write_all(bytes);
        }
    }
    let root_pid = child.id();
    let root_created_ft = creation_filetime(root_pid);
    let mut conhost_pids = std::collections::HashSet::new();
    // Creation FILETIME per sighted conhost pid, queried once. Only
    // conhosts created at/after the root count toward the session's
    // console budget (see `creation_filetime`); an unreadable timestamp
    // (0) keeps the sighting, failing open toward the old behavior
    // rather than hiding a real allocation.
    let mut conhost_created_ft: HashMap<u32, u64> = HashMap::new();
    let mut offenders = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(15);
    while child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
        let entries = process_entries();
        let tree: HashMap<u32, u32> = entries.iter().map(|&(p, pp, _)| (p, pp)).collect();
        for &(pid, _ppid, ref name) in &entries {
            if name == "conhost.exe" && in_tree(pid, root_pid, &tree) {
                let created = *conhost_created_ft
                    .entry(pid)
                    .or_insert_with(|| creation_filetime(pid));
                if created == 0 || root_created_ft == 0 || created >= root_created_ft {
                    conhost_pids.insert(pid);
                }
            }
        }
        for owner in visible_console_window_owners() {
            if in_tree(owner, root_pid, &tree) && !offenders.contains(&owner) {
                offenders.push(owner);
            }
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    let output = child.wait_with_output().expect("wait target");
    (output, offenders, conhost_pids)
}

fn run_and_observe_niu(
    args: &[&str],
    flags: u32,
) -> (
    std::process::Output,
    Vec<u32>,
    std::collections::HashSet<u32>,
) {
    let niu = std::path::Path::new(env!("CARGO_BIN_EXE_niu"));
    run_and_observe_exe(niu, args, flags, None)
}

fn non_empty_lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

#[test]
fn detached_builtin_only_session_allocates_zero_conhosts() {
    // Pure builtin script (echo, printf, arithmetic, loops, pwd, test) must
    // never allocate a hidden console at all (0 conhost.exe, 0 visible windows).
    let script = concat!(
        "acc=0; ",
        "for i in 1 2 3 4 5; do acc=$((acc + i)); done; ",
        "printf 'SUM=%d\\n' \"$acc\"; ",
        "test \"$acc\" -eq 15 && echo BUILTIN-OK; ",
        "pwd >/dev/null"
    );
    let (output, offenders, conhosts) = run_and_observe_niu(&["-c", script], DETACHED_PROCESS);
    assert!(output.status.success(), "niu -c exited {}", output.status);
    assert!(
        offenders.is_empty(),
        "visible windows appeared: {offenders:?}"
    );
    assert!(
        conhosts.is_empty(),
        "pure builtin session must allocate 0 conhost.exe, saw: {conhosts:?}"
    );
    assert_eq!(
        non_empty_lines(&output.stdout),
        vec!["SUM=15", "BUILTIN-OK"]
    );
}

#[test]
fn detached_command_substitutions_share_single_hidden_console() {
    // Exercises `command_substitution_values.rs` (`$(...)` and backticks).
    let script = concat!(
        "h1=$(C:/Windows/System32/hostname.exe); ",
        "h2=`C:/Windows/System32/hostname.exe`; ",
        "C:/Windows/System32/ping.exe -n 2 127.0.0.1 >NUL; ",
        "h3=$(C:/Windows/System32/hostname.exe); ",
        "echo \"$h1|$h2|$h3\""
    );
    let (output, offenders, conhosts) = run_and_observe_niu(&["-c", script], DETACHED_PROCESS);
    assert!(output.status.success(), "niu -c exited {}", output.status);
    assert!(
        offenders.is_empty(),
        "visible windows appeared: {offenders:?}"
    );
    assert!(
        conhosts.len() <= 1,
        "command substitutions must share <=1 conhost.exe, saw {}: {conhosts:?}",
        conhosts.len()
    );
    let lines = non_empty_lines(&output.stdout);
    assert_eq!(lines.len(), 1, "expected 1 combined line, got {lines:?}");
    let parts: Vec<&str> = lines[0].split('|').collect();
    assert_eq!(parts.len(), 3);
    assert!(!parts[0].is_empty() && parts[0] == parts[1] && parts[1] == parts[2]);
}

#[test]
fn detached_pipelines_share_single_hidden_console() {
    // Exercises multi-stage external pipelines (`pipeline_exec.rs` + `pipeline_stages.rs`).
    let script = concat!(
        "C:/Windows/System32/hostname.exe | C:/Windows/System32/findstr.exe . ; ",
        "C:/Windows/System32/ping.exe -n 2 127.0.0.1 | C:/Windows/System32/findstr.exe . >NUL ; ",
        "C:/Windows/System32/hostname.exe | C:/Windows/System32/findstr.exe ."
    );
    let (output, offenders, conhosts) = run_and_observe_niu(&["-c", script], DETACHED_PROCESS);
    assert!(output.status.success(), "niu -c exited {}", output.status);
    assert!(
        offenders.is_empty(),
        "visible windows appeared: {offenders:?}"
    );
    assert!(
        conhosts.len() <= 1,
        "multi-stage pipelines must share <=1 conhost.exe, saw {}: {conhosts:?}",
        conhosts.len()
    );
    assert_eq!(non_empty_lines(&output.stdout).len(), 2);
}

#[test]
fn detached_background_jobs_and_wait_share_single_hidden_console() {
    // Exercises BOTH direct async `&` background spawns (`compound_exec.rs:467`)
    // AND redirected/compound `&` wrapper spawns (`fd::spawn_whitelisted`,
    // `windows_impl.rs:1385`) when launched as the very first command of a
    // console-less session.
    let script = concat!(
        "C:/Windows/System32/ping.exe -n 2 127.0.0.1 >NUL & p1=$!; ",
        "( C:/Windows/System32/hostname.exe >/dev/null ) & p2=$!; ",
        "C:/Windows/System32/ping.exe -n 2 127.0.0.1 >/dev/null & p3=$!; ",
        "wait $p1 $p2 $p3; ",
        "C:/Windows/System32/hostname.exe"
    );
    let (output, offenders, conhosts) = run_and_observe_niu(&["-c", script], DETACHED_PROCESS);
    assert!(output.status.success(), "niu -c exited {}", output.status);
    assert!(
        offenders.is_empty(),
        "visible windows appeared in background jobs: {offenders:?}"
    );
    assert!(
        conhosts.len() <= 1,
        "background jobs + foreground commands must share <=1 conhost.exe, saw {}: {conhosts:?}",
        conhosts.len()
    );
    assert_eq!(non_empty_lines(&output.stdout).len(), 1);
}

#[test]
fn detached_subshells_functions_and_loops_share_single_hidden_console() {
    // Exercises shell functions, `( ... )` subshells, and `for` loops.
    let script = concat!(
        "emit_host() { C:/Windows/System32/hostname.exe; }; ",
        "( emit_host; C:/Windows/System32/ping.exe -n 2 127.0.0.1 >NUL ); ",
        "for n in 1 2 3; do emit_host; done"
    );
    let (output, offenders, conhosts) = run_and_observe_niu(&["-c", script], DETACHED_PROCESS);
    assert!(output.status.success(), "niu -c exited {}", output.status);
    assert!(
        offenders.is_empty(),
        "visible windows appeared: {offenders:?}"
    );
    assert!(
        conhosts.len() <= 1,
        "subshells/functions/loops must share <=1 conhost.exe, saw {}: {conhosts:?}",
        conhosts.len()
    );
    assert_eq!(non_empty_lines(&output.stdout).len(), 4);
}

#[test]
fn detached_nested_niu_and_exec_builtin_share_single_hidden_console() {
    // Deep nesting: `niu -c` spawning nested `niu -c` and `niu -c 'exec ...'`.
    // Once the outer niu claims the hidden console, nested niu processes
    // inherit it (GetConsoleCP()!=0, IsWindowVisible==0 so release_orphan_console
    // keeps it) and the whole multi-level tree allocates at most 1 conhost.exe.
    let script = concat!(
        "\"$NIU_SHELL\" -c \"C:/Windows/System32/hostname.exe; C:/Windows/System32/ping.exe -n 2 127.0.0.1 >NUL\"; ",
        "\"$NIU_SHELL\" -c \"exec C:/Windows/System32/hostname.exe\""
    );
    let (output, offenders, conhosts) = run_and_observe_niu(&["-c", script], DETACHED_PROCESS);
    assert!(
        output.status.success(),
        "nested niu -c exited {}",
        output.status
    );
    assert!(
        offenders.is_empty(),
        "visible windows appeared in nested niu tree: {offenders:?}"
    );
    assert!(
        conhosts.len() <= 1,
        "nested niu tree must share <=1 conhost.exe, saw {}: {conhosts:?}",
        conhosts.len()
    );
    assert_eq!(non_empty_lines(&output.stdout).len(), 2);
}

#[test]
fn detached_hardlink_faces_bash_and_sh_spawn_zero_windows_and_one_conhost() {
    // niubash#397 + #462: `bash.exe` and `sh.exe` hardlinked to `niu.exe` must
    // behave identically to `niu.exe` when invoked console-less.
    let niu = std::path::Path::new(env!("CARGO_BIN_EXE_niu"));
    let dir = std::env::temp_dir().join(format!("niu-gate-faces-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp face dir");

    let script = concat!(
        "echo FACE=$0; ",
        "C:/Windows/System32/hostname.exe; ",
        "C:/Windows/System32/ping.exe -n 2 127.0.0.1 >NUL; ",
        "C:/Windows/System32/hostname.exe"
    );
    for face_name in ["bash.exe", "sh.exe"] {
        let face_path = dir.join(face_name);
        if std::fs::hard_link(niu, &face_path).is_err() {
            std::fs::copy(niu, &face_path).expect("copy niu face");
        }
        let (output, offenders, conhosts) =
            run_and_observe_exe(&face_path, &["-c", script], DETACHED_PROCESS, None);
        assert!(
            output.status.success(),
            "{face_name} -c exited {}",
            output.status
        );
        assert!(
            offenders.is_empty(),
            "visible windows appeared under {face_name}: {offenders:?}"
        );
        assert!(
            conhosts.len() <= 1,
            "{face_name} must allocate <=1 conhost.exe, saw {}: {conhosts:?}",
            conhosts.len()
        );
        let lines = non_empty_lines(&output.stdout);
        assert_eq!(lines.len(), 3, "unexpected {face_name} stdout: {lines:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn detached_script_file_and_stdin_routes_spawn_zero_windows_and_one_conhost() {
    // Covers `niu <script.sh>` and `niu` with piped script on stdin (no `-c`).
    let niu = std::path::Path::new(env!("CARGO_BIN_EXE_niu"));
    let dir = std::env::temp_dir().join(format!("niu-gate-script-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp script dir");
    let script_path = dir.join("probe.sh");
    let body = concat!(
        "C:/Windows/System32/hostname.exe\n",
        "C:/Windows/System32/ping.exe -n 2 127.0.0.1 >NUL\n",
        "C:/Windows/System32/hostname.exe\n"
    );
    std::fs::write(&script_path, body).expect("write probe.sh");

    let script_arg = script_path.to_string_lossy().into_owned();
    let (out_file, offenders_file, conhosts_file) =
        run_and_observe_niu(&[&script_arg], DETACHED_PROCESS);
    assert!(out_file.status.success());
    assert!(
        offenders_file.is_empty(),
        "visible windows: {offenders_file:?}"
    );
    assert!(
        conhosts_file.len() <= 1,
        "script file route must allocate <=1 conhost.exe, saw {}: {conhosts_file:?}",
        conhosts_file.len()
    );
    assert_eq!(non_empty_lines(&out_file.stdout).len(), 2);

    let (out_stdin, offenders_stdin, conhosts_stdin) =
        run_and_observe_exe(niu, &[], DETACHED_PROCESS, Some(body.as_bytes()));
    assert!(out_stdin.status.success());
    assert!(
        offenders_stdin.is_empty(),
        "visible windows: {offenders_stdin:?}"
    );
    assert!(
        conhosts_stdin.len() <= 1,
        "stdin script route must allocate <=1 conhost.exe, saw {}: {conhosts_stdin:?}",
        conhosts_stdin.len()
    );
    assert_eq!(non_empty_lines(&out_stdin.stdout).len(), 2);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn detached_repl_command_and_launcher_verbs_spawn_zero_windows() {
    // Covers `-C` (`--repl-command`), `--version`, `--help`, and `plugin list`
    // under DETACHED_PROCESS.
    let repl_script = concat!(
        "C:/Windows/System32/hostname.exe; ",
        "C:/Windows/System32/ping.exe -n 2 127.0.0.1 >NUL; ",
        "C:/Windows/System32/hostname.exe"
    );
    let (out_c, offenders_c, conhosts_c) =
        run_and_observe_niu(&["-C", repl_script], DETACHED_PROCESS);
    assert!(out_c.status.success());
    assert!(
        offenders_c.is_empty(),
        "visible windows on -C: {offenders_c:?}"
    );
    assert!(
        conhosts_c.len() <= 1,
        "-C route must allocate <=1 conhost.exe, saw {}: {conhosts_c:?}",
        conhosts_c.len()
    );
    assert_eq!(non_empty_lines(&out_c.stdout).len(), 2);

    // `--version` probes `winuxcmd.exe --version` (<=1 hidden console);
    // `--help` and `plugin list` are pure in-process routes (0 conhosts).
    let (out_v, offenders_v, conhosts_v) = run_and_observe_niu(&["--version"], DETACHED_PROCESS);
    assert!(out_v.status.success());
    assert!(
        offenders_v.is_empty(),
        "visible windows on --version: {offenders_v:?}"
    );
    assert!(
        conhosts_v.len() <= 1,
        "--version must allocate <=1 conhost.exe, saw {}: {conhosts_v:?}",
        conhosts_v.len()
    );

    for args in [&["--help"][..], &["plugin", "list"][..]] {
        let (out, offenders, conhosts) = run_and_observe_niu(args, DETACHED_PROCESS);
        assert!(out.status.success(), "niu {args:?} failed: {}", out.status);
        assert!(
            offenders.is_empty(),
            "visible windows on {args:?}: {offenders:?}"
        );
        assert!(
            conhosts.is_empty(),
            "in-process launcher verb {args:?} must allocate 0 conhost.exe, saw: {conhosts:?}"
        );
    }
}

#[test]
fn create_no_window_parent_reuses_caller_hidden_console() {
    // When the caller itself spawned niu with CREATE_NO_WINDOW (e.g. a
    // runner/agent or the bash.exe shim), niu starts with a hidden console
    // already attached. `release_orphan_console` must keep it, and all
    // external children must inherit that single console without allocating a
    // second conhost.exe.
    let script = concat!(
        "C:/Windows/System32/hostname.exe; ",
        "C:/Windows/System32/ping.exe -n 2 127.0.0.1 >NUL; ",
        "C:/Windows/System32/hostname.exe; ",
        "C:/Windows/System32/ping.exe -n 2 127.0.0.1 >NUL; ",
        "C:/Windows/System32/hostname.exe"
    );
    let (output, offenders, conhosts) = run_and_observe_niu(&["-c", script], CREATE_NO_WINDOW);
    assert!(output.status.success());
    assert!(
        offenders.is_empty(),
        "visible windows under CREATE_NO_WINDOW parent: {offenders:?}"
    );
    assert!(
        conhosts.len() <= 1,
        "CREATE_NO_WINDOW parent must have <=1 conhost.exe in tree, saw {}: {conhosts:?}",
        conhosts.len()
    );
    assert_eq!(non_empty_lines(&output.stdout).len(), 3);
}

#[test]
fn detached_scaling_timing_gate_avoids_per_spawn_console_tax() {
    // Timing & zero-window gate: compare N=1 vs N=10 external commands in a
    // detached `niu -c` session. With 1 hidden console per session, the 9
    // additional `hostname.exe` spawns inherit the existing hidden console
    // instead of paying ~22-33 ms of fresh conhost.exe startup each.
    let niu = std::path::Path::new(env!("CARGO_BIN_EXE_niu"));
    let cmd = "C:/Windows/System32/hostname.exe >NUL";
    let script_1 = cmd.to_string();
    let script_10 = std::iter::repeat_n(cmd, 10).collect::<Vec<_>>().join("; ");

    // Warmup
    let _ = run_and_observe_exe(niu, &["-c", &script_1], DETACHED_PROCESS, None);

    let mut samples_1 = Vec::new();
    let mut samples_10 = Vec::new();
    for _ in 0..3 {
        let t0 = Instant::now();
        let (out1, off1, _) = run_and_observe_exe(niu, &["-c", &script_1], DETACHED_PROCESS, None);
        let d1 = t0.elapsed();
        assert!(out1.status.success());
        assert!(off1.is_empty(), "visible windows in N=1: {off1:?}");
        samples_1.push(d1);

        let t0 = Instant::now();
        let (out10, off10, con10) =
            run_and_observe_exe(niu, &["-c", &script_10], DETACHED_PROCESS, None);
        let d10 = t0.elapsed();
        assert!(out10.status.success());
        assert!(off10.is_empty(), "visible windows in N=10: {off10:?}");
        assert!(
            con10.len() <= 1,
            "N=10 must allocate <=1 conhost.exe, saw {}: {con10:?}",
            con10.len()
        );
        samples_10.push(d10);
    }
    samples_1.sort();
    samples_10.sort();
    let med_1 = samples_1[1];
    let med_10 = samples_10[1];
    let per_extra_ms = med_10.saturating_sub(med_1).as_secs_f64() * 1000.0 / 9.0;
    eprintln!(
        "[#462 timing gate] N=1 median={med_1:?}, N=10 median={med_10:?}, per_extra_spawn={per_extra_ms:.1} ms"
    );
    // A bare hostname.exe spawn is ~14-18 ms; per-spawn conhost allocation
    // pushed it to ~48-55 ms. Cap at 35 ms/spawn to catch a per-spawn conhost
    // regression while leaving generous headroom for loaded CI runners.
    assert!(
        per_extra_ms < 35.0,
        "per-spawn overhead too high ({per_extra_ms:.1} ms/spawn; N=1 {med_1:?}, N=10 {med_10:?}) — hidden console was not reused across session"
    );
}

#[test]
fn detached_coproc_and_process_substitution_share_single_hidden_console() {
    // Exercises `coproc` (`compound_exec.rs:2580`) and `<(...)` process substitution.
    let script = concat!(
        "coproc CP { C:/Windows/System32/hostname.exe; }; ",
        "read -r line <&\"${CP[0]}\"; ",
        "wait \"$CP_PID\" 2>/dev/null || true; ",
        "C:/Windows/System32/findstr.exe . <(C:/Windows/System32/hostname.exe); ",
        "C:/Windows/System32/ping.exe -n 2 127.0.0.1 >NUL; ",
        "echo \"COPROC=$line\""
    );
    let (output, offenders, conhosts) = run_and_observe_niu(&["-c", script], DETACHED_PROCESS);
    assert!(
        output.status.success(),
        "coproc/procsub exited {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        offenders.is_empty(),
        "visible windows in coproc/procsub: {offenders:?}"
    );
    assert!(
        conhosts.len() <= 1,
        "coproc/procsub must share <=1 conhost.exe, saw {}: {conhosts:?}",
        conhosts.len()
    );
    let lines = non_empty_lines(&output.stdout);
    assert_eq!(lines.len(), 2, "unexpected stdout: {lines:?}");
    assert!(lines[1].starts_with("COPROC=") && lines[1].len() > "COPROC=".len());
}

#[test]
fn detached_shebang_and_cmd_batch_scripts_share_single_hidden_console() {
    // Exercises extensionless `#!` scripts (`shell_wrapped_command` /
    // `external_inner.rs:1101`) and `.cmd` batch scripts (`cmd.exe /c`).
    let dir = std::env::temp_dir().join(format!("niu-gate-shebang-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp shebang dir");

    let shebang = dir.join("run_shebang");
    std::fs::write(
        &shebang,
        "#!/usr/bin/env sh\nC:/Windows/System32/hostname.exe\n",
    )
    .expect("write shebang");

    let batch = dir.join("run_batch.cmd");
    std::fs::write(
        &batch,
        "@echo off\r\nC:\\Windows\\System32\\hostname.exe\r\n",
    )
    .expect("write batch");

    let dir_posix = dir.to_string_lossy().replace('\\', "/");
    let script = format!(
        "\"{dir_posix}/run_shebang\"; \"{dir_posix}/run_batch.cmd\"; C:/Windows/System32/ping.exe -n 2 127.0.0.1 >NUL; \"{dir_posix}/run_shebang\""
    );
    let (output, offenders, conhosts) = run_and_observe_niu(&["-c", &script], DETACHED_PROCESS);
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        output.status.success(),
        "shebang/batch exited {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        offenders.is_empty(),
        "visible windows in shebang/batch: {offenders:?}"
    );
    assert!(
        conhosts.len() <= 1,
        "shebang/batch must share <=1 conhost.exe, saw {}: {conhosts:?}",
        conhosts.len()
    );
    assert_eq!(non_empty_lines(&output.stdout).len(), 3);
}

#[test]
fn detached_missing_first_command_and_encoded_command_stay_windowless() {
    // 1. If the first command fails/is missing, the guard must stay armed so
    //    subsequent CUI commands still claim 1 hidden console with 0 windows.
    let script = concat!(
        "definitely_missing_binary_462 2>/dev/null || true; ",
        "C:/Windows/System32/hostname.exe; ",
        "C:/Windows/System32/ping.exe -n 2 127.0.0.1 >NUL; ",
        "C:/Windows/System32/hostname.exe"
    );
    let (output, offenders, conhosts) = run_and_observe_niu(&["-c", script], DETACHED_PROCESS);
    assert!(output.status.success());
    assert!(offenders.is_empty(), "visible windows: {offenders:?}");
    assert!(
        conhosts.len() <= 1,
        "after missing cmd, must still allocate <=1 conhost.exe, saw {}: {conhosts:?}",
        conhosts.len()
    );
    assert_eq!(non_empty_lines(&output.stdout).len(), 2);

    // 2. `--encoded-command` (UTF-8 base64, used by automation runners).
    let raw = "C:/Windows/System32/hostname.exe; C:/Windows/System32/ping.exe -n 2 127.0.0.1 >NUL; C:/Windows/System32/hostname.exe";
    let b64 = encode_base64(raw.as_bytes());
    let (out_enc, off_enc, con_enc) =
        run_and_observe_niu(&["--encoded-command", &b64], DETACHED_PROCESS);
    assert!(out_enc.status.success());
    assert!(
        off_enc.is_empty(),
        "visible windows on --encoded-command: {off_enc:?}"
    );
    assert!(
        con_enc.len() <= 1,
        "--encoded-command must allocate <=1 conhost.exe, saw {}: {con_enc:?}",
        con_enc.len()
    );
    assert_eq!(non_empty_lines(&out_enc.stdout).len(), 2);
}

#[test]
fn detached_shim_forwarded_grandchild_spawns_zero_windows_and_one_conhost() {
    // niubash#466: a `layout: shim` command (`winuxcmd-shim.exe` ->
    // `opt/<pkg>/<tool>.exe` -> grandchild `hostname.exe`/`git.exe`) invoked
    // with redirected stdio (e.g. `python - <<'EOF'` calling `subprocess.run`)
    // must NOT be detached by the shim (`DETACHED_PROCESS`), which used to
    // strip the session's hidden console (`GetConsoleCP() == 0` in the payload)
    // and cause every grandchild CUI process to pop a visible console window
    // and allocate a fresh conhost.exe.
    //
    // Stage a synthetic `<root>/.wpm/shims.map` + `<root>/usr/bin/probe.exe`
    // (backed by the installed `winuxcmd-shim.exe` when present, or a copy of
    // `niu.exe` when running in a bare cargo checkout) that forwards to a
    // payload spawning 3 external grandchildren with redirected stdin.
    let niu = std::path::Path::new(env!("CARGO_BIN_EXE_niu"));
    let dir = std::env::temp_dir().join(format!("niu-gate-466-shim-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let wpm_dir = dir.join(".wpm");
    let usr_bin = dir.join("usr").join("bin");
    let opt_pkg = dir.join("opt").join("pkg");
    std::fs::create_dir_all(&wpm_dir).expect("mkdir .wpm");
    std::fs::create_dir_all(&usr_bin).expect("mkdir usr/bin");
    std::fs::create_dir_all(&opt_pkg).expect("mkdir opt/pkg");

    // Payload at `opt/pkg/probe.exe` is `niu.exe` itself; when invoked via
    // `usr/bin/probe.exe -c ... <<<heredoc`, it runs 3 external grandchildren.
    let payload_exe = opt_pkg.join("probe.exe");
    if std::fs::hard_link(niu, &payload_exe).is_err() {
        std::fs::copy(niu, &payload_exe).expect("copy payload");
    }
    std::fs::write(wpm_dir.join("shims.map"), "probe=opt\\pkg\\probe.exe\n")
        .expect("write shims.map");

    // Prefer the real `winuxcmd-shim.exe` from the host installation if
    // available so the test exercises the actual forwarder binary.
    let candidate_shims = [
        std::path::PathBuf::from(r"D:\repo\winuxcmd-main\winuxcmd-shim.exe"),
        dirs_shim_candidate(),
    ];
    let shim_src = candidate_shims
        .iter()
        .find(|p| p.is_file())
        .map(|p| p.as_path())
        .unwrap_or(niu);
    let shim_exe = usr_bin.join("probe.exe");
    std::fs::copy(shim_src, &shim_exe).expect("stage usr/bin/probe.exe");

    let probe_posix = shim_exe.to_string_lossy().replace('\\', "/");
    let script = format!(
        "\"{probe_posix}\" -c \"C:/Windows/System32/hostname.exe; C:/Windows/System32/ping.exe -n 2 127.0.0.1 >NUL; C:/Windows/System32/hostname.exe; C:/Windows/System32/ping.exe -n 2 127.0.0.1 >NUL; C:/Windows/System32/hostname.exe\" <<'EOF'\nheredoc-stdin\nEOF"
    );
    for flags in [DETACHED_PROCESS, CREATE_NO_WINDOW] {
        let (output, offenders, conhosts) = run_and_observe_niu(&["-c", &script], flags);
        assert!(
            output.status.success(),
            "#466 shim chain (flags={flags:#x}) exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            offenders.is_empty(),
            "#466 visible windows appeared in shim->payload->grandchild chain (flags={flags:#x}): {offenders:?}"
        );
        assert!(
            conhosts.len() <= 1,
            "#466 shim->payload->grandchild chain (flags={flags:#x}) must allocate <=1 conhost.exe, saw {}: {conhosts:?}",
            conhosts.len()
        );
        assert_eq!(non_empty_lines(&output.stdout).len(), 3);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn dirs_shim_candidate() -> std::path::PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
        .join("Programs")
        .join("Niubash")
        .join("winuxcmd")
        .join("usr")
        .join("bin")
        .join("winuxcmd-shim.exe")
}

fn encode_base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((triple >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(triple & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

// ---------------------------------------------------------------------------
// niubash#455: the orphan-console release must leave child stdio usable
// ---------------------------------------------------------------------------

const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;

#[repr(C)]
struct StartupInfoW {
    cb: u32,
    reserved: *mut u16,
    desktop: *mut u16,
    title: *mut u16,
    x: u32,
    y: u32,
    x_size: u32,
    y_size: u32,
    x_count_chars: u32,
    y_count_chars: u32,
    fill_attribute: u32,
    flags: u32,
    show_window: u16,
    reserved2: u16,
    reserved3: *mut u8,
    std_input: *mut c_void,
    std_output: *mut c_void,
    std_error: *mut c_void,
}

#[repr(C)]
struct ProcessInformation {
    process: *mut c_void,
    thread: *mut c_void,
    process_id: u32,
    thread_id: u32,
}

#[link(name = "kernel32")]
extern "system" {
    fn CreateProcessW(
        app: *const u16,
        cmdline: *mut u16,
        proc_attr: *mut c_void,
        thread_attr: *mut c_void,
        inherit: i32,
        flags: u32,
        env: *mut c_void,
        dir: *const u16,
        si: *const StartupInfoW,
        pi: *mut ProcessInformation,
    ) -> i32;
    fn WaitForSingleObject(h: *mut c_void, ms: u32) -> u32;
    fn GetExitCodeProcess(h: *mut c_void, code: *mut u32) -> i32;
}

/// Spawn `niu -c <script>` the way a console-less GUI/agent invoker does:
/// `CREATE_NEW_CONSOLE` with NO redirected std handles. Windows allocates a
/// fresh *visible* console AND points the child's three std slots at it —
/// the orphan-console release then frees the console under niu's feet. This
/// is the only spawn shape that reproduces the niubash#455 dead-stdio
/// handoff (std::process::Command always sets STARTF_USESTDHANDLES, so its
/// children get pipe/devnull handles that survive the release).
fn spawn_niu_fresh_console(script: &str) -> (u32, *mut c_void) {
    let niu = env!("CARGO_BIN_EXE_niu");
    let mut cmdline: Vec<u16> = format!("\"{niu}\" -c \"{script}\"")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let si = StartupInfoW {
        cb: std::mem::size_of::<StartupInfoW>() as u32,
        reserved: std::ptr::null_mut(),
        desktop: std::ptr::null_mut(),
        title: std::ptr::null_mut(),
        x: 0,
        y: 0,
        x_size: 0,
        y_size: 0,
        x_count_chars: 0,
        y_count_chars: 0,
        fill_attribute: 0,
        flags: 0,
        show_window: 0,
        reserved2: 0,
        reserved3: std::ptr::null_mut(),
        std_input: std::ptr::null_mut(),
        std_output: std::ptr::null_mut(),
        std_error: std::ptr::null_mut(),
    };
    let mut pi = ProcessInformation {
        process: std::ptr::null_mut(),
        thread: std::ptr::null_mut(),
        process_id: 0,
        thread_id: 0,
    };
    let ok = unsafe {
        CreateProcessW(
            std::ptr::null(),
            cmdline.as_mut_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
            CREATE_NEW_CONSOLE | CREATE_NEW_PROCESS_GROUP,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &si,
            &mut pi,
        )
    };
    assert!(
        ok != 0,
        "CreateProcessW(niu -c) failed: {}",
        windows_get_last_error()
    );
    unsafe { CloseHandle(pi.thread) };
    (pi.process_id, pi.process)
}

fn windows_get_last_error() -> u32 {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetLastError() -> u32;
    }
    unsafe { GetLastError() }
}

#[test]
fn plainly_spawned_niu_c_keeps_children_spawnable_after_console_release() {
    // niubash#455: release_orphan_console frees the auto-allocated console
    // that also owns the process's std handles; before the fix every later
    // external spawn inherited dead handles and died in the engine's stdio
    // plan ("Bad file descriptor", rc 126) — a console TUI child renders
    // nothing and reads nothing, the reported blank+dead-keys+hang. The
    // release must re-point the dead std slots (NUL) so children spawn.
    let dir = std::env::temp_dir().join(format!("niu-gate-455-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let out_file = dir.join("child-out.txt");
    let script = format!(
        "C:/Windows/System32/hostname.exe > \"{}\"; echo rc=$? >> \"{}\"",
        out_file.display(),
        out_file.display()
    );

    let (pid, process) = spawn_niu_fresh_console(&script);
    // ~2s of poll while niu runs: no persistent visible window may linger,
    // matching the release contract the rest of this file pins.
    let offenders = collect_offenders(
        pid,
        || (unsafe { WaitForSingleObject(process, 0) }) == 0x102, // WAIT_TIMEOUT
        true,
    );
    let mut code = u32::MAX;
    unsafe {
        WaitForSingleObject(process, 15_000);
        GetExitCodeProcess(process, &mut code);
        CloseHandle(process);
    }
    let body = std::fs::read_to_string(&out_file).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(code, 0, "niu -c exited {code}; child output: {body:?}");
    assert!(
        offenders.is_empty(),
        "visible console windows lingered (owner pids {offenders:?})"
    );
    let lines = non_empty_lines(body.as_bytes());
    assert_eq!(
        lines.len(),
        2,
        "expected hostname + rc lines after release, got: {body:?}"
    );
    assert_eq!(
        lines[1], "rc=0",
        "external child must spawn cleanly after the console release: {body:?}"
    );
}
