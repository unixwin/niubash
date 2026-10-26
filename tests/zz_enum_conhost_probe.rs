//! ENUM-ONLY probe (root-D adjudication, never merge): instrumented
//! conhost observation for the CREATE_NO_WINDOW vs DETACHED_PROCESS
//! postures behind `create_no_window_parent_reuses_caller_hidden_console`.
//!
//! The gate test accumulates *distinct* conhost PIDs over the whole run,
//! which cannot distinguish (a) several conhosts alive at once, (b)
//! sequential allocate/release churn, (c) one stable console. This probe
//! samples every 5 ms and prints, per run: cumulative distinct PIDs, the
//! max simultaneous count, and each conhost PID's parent + observed
//! lifespan. Always passes; the printed record is the evidence.

#![cfg(windows)]

use std::collections::HashMap;
use std::ffi::c_void;
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;

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

#[link(name = "kernel32")]
extern "system" {
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

/// Process creation FILETIME (100 ns ticks since 1601) or 0 if unreadable.
/// This is the discriminator between a conhost the observed session
/// actually allocated (created at/after the root niu) and a lingering
/// conhost from a dead process whose PID was reused by the root (created
/// strictly before the root).
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

struct Sight {
    ppid: u32,
    created_ft: u64,
    first_ms: u128,
    last_ms: u128,
    samples: u32,
}

fn observe(script: &str, flags: u32, label: &str, run: usize) {
    let niu = env!("CARGO_BIN_EXE_niu");
    let mut child = Command::new(niu)
        .args(["-c", script])
        .env("NIU_SKIP_WINUXCMD_ACTIVATION", "1")
        .creation_flags(flags | CREATE_NEW_PROCESS_GROUP)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn niu");
    let root = child.id();
    let root_created_ft = creation_filetime(root);
    let t0 = Instant::now();
    let mut sights: HashMap<u32, Sight> = HashMap::new();
    let mut max_simultaneous = 0usize;
    let deadline = t0 + Duration::from_secs(15);
    while child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
        let entries = process_entries();
        let tree: HashMap<u32, u32> = entries.iter().map(|&(p, pp, _)| (p, pp)).collect();
        let mut alive_now = 0usize;
        for &(pid, ppid, ref name) in &entries {
            if name == "conhost.exe" && in_tree(pid, root, &tree) {
                alive_now += 1;
                let now = t0.elapsed().as_millis();
                let s = sights.entry(pid).or_insert(Sight {
                    ppid,
                    created_ft: creation_filetime(pid),
                    first_ms: now,
                    last_ms: now,
                    samples: 0,
                });
                s.last_ms = now;
                s.samples += 1;
            }
        }
        max_simultaneous = max_simultaneous.max(alive_now);
        std::thread::sleep(Duration::from_millis(5));
    }
    let status = child.wait().expect("wait niu");
    let mut pids: Vec<u32> = sights.keys().copied().collect();
    pids.sort_unstable();
    println!(
        "PROBE {label} run{run}: root_niu_pid={root} root_created_ft={root_created_ft} status={status} cumulative_distinct={} max_simultaneous={}",
        sights.len(),
        max_simultaneous
    );
    for pid in pids {
        let s = &sights[&pid];
        // Signed delta in ms between the conhost's creation and the root
        // niu's creation. Negative => conhost predates the root: it cannot
        // have been allocated by this session (PID-reuse zombie).
        let delta_ms = (s.created_ft as i128 - root_created_ft as i128) / 10_000;
        println!(
            "PROBE {label} run{run}:   conhost pid={pid} ppid={} created_ft={} delta_vs_root={}ms first={}ms last={}ms samples={}",
            s.ppid, s.created_ft, delta_ms, s.first_ms, s.last_ms, s.samples
        );
    }
}

#[test]
fn enum_conhost_probe() {
    let script = concat!(
        "C:/Windows/System32/hostname.exe; ",
        "C:/Windows/System32/ping.exe -n 2 127.0.0.1 >NUL; ",
        "C:/Windows/System32/hostname.exe; ",
        "C:/Windows/System32/ping.exe -n 2 127.0.0.1 >NUL; ",
        "C:/Windows/System32/hostname.exe"
    );
    for run in 1..=4 {
        observe(script, CREATE_NO_WINDOW, "CREATE_NO_WINDOW", run);
    }
    for run in 1..=4 {
        observe(script, DETACHED_PROCESS, "DETACHED", run);
    }
}
