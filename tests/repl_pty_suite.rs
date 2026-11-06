//! End-to-end PTY session chains for the interactive REPL (Refs
//! niubash#827 — the REPL counterpart of the issue827 PTY gate).
//!
//! Where tests/issue827_pty_gate.rs pins single scenarios and
//! tests/interactive.rs (Windows-only) pins single features, this
//! target pins *journeys*: multi-step sessions in which each step's
//! state feeds the next step's assertion (boot -> run -> exit code
//! observed by the parent shell; commands -> history file -> a second
//! session's recall; completion -> edit -> execute; a job stopped ->
//! backgrounded -> waited). Behavior expectations are written against
//! current master and must survive the GAP-9 host rewiring (repl /
//! completion / highlighting internals moving onto the engine token
//! view) unchanged — that is the acceptance-net role of this suite.
//!
//! Harness shape (stated plainly, the niubash#769 lesson): on unix
//! the driven niu is NOT spawned as a session leader. A GNU bash
//! running interactively on the pty is the session leader and job-
//! control parent; it launches niu as an ordinary foreground job, so
//! niu sits in its own process group inside the session with its
//! parent in another group of the same session — the group is NOT
//! orphaned, and POSIX signal delivery (SIGTSTP/SIGTTIN/SIGTTOU) to
//! niu and to the jobs niu spawns follows the real desktop shape.
//! (The repo driver's direct spawn leaves the driven shell alone in
//! an orphaned group, where the kernel discards those signals for
//! the leader itself; conclusions drawn from that shape about
//! suspension are invalid, which is why this suite carries its own
//! spawn.) On Windows there is no POSIX process-group model to
//! orphan; the suite drives the repo driver's direct spawn there and
//! the unix-only chains cfg-skip, exactly like the gate's S8.
//!
//! Parent shell: the first of $NIU_PTY_PARENT_BASH, /bin/bash, the
//! workspace GNU bash 5.3.20 oracle, or `bash` on PATH. Its only jobs
//! are launching niu and reporting niu's exit status back (`$?`), so
//! any GNU bash with job control carries the shape.
//!
//! Run: `NIU_INTERACTIVE_TESTS=1 cargo test --test repl_pty_suite --locked -- --nocapture`

// Each platform compiles a different subset of the harness surface.
#![allow(dead_code, unused_imports)]

#[path = "interactive/driver.rs"]
mod driver;

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use driver::{require_pty_or_skip, DEFAULT_TIMEOUT, HEAVY_TIMEOUT, TAB};

/// Short window for negative ("must not appear") assertions.
const ABSENT_WINDOW: Duration = Duration::from_millis(800);
const CTRL_C: &str = "\u{3}";
const CTRL_R: &str = "\u{12}";
const CTRL_Z: &str = "\u{1a}";
const UP: &str = "\u{1b}[A";
const DOWN: &str = "\u{1b}[B";
const LEFT: &str = "\u{1b}[D";
const ESC: &str = "\u{1b}";
const BACKSPACE: &str = "\u{7f}";

/// Parent shell prompt sentinel (the carrier bash's PS1).
const PARENT_PROMPT: &str = "PARENT> ";

/// Let the editor finish its prompt reset before sending more
/// keystrokes (the window documented in tests/issue827_pty_gate.rs:
/// keystrokes that land while the editor re-initializes after an
/// external execution are echoed by the line discipline and then
/// discarded). Only used where a keystroke must follow an execution;
/// expect-paced steps need no pause.
fn settle() {
    std::thread::sleep(Duration::from_millis(500));
}

/// PATH the sessions' children see (real coreutils for `ls`/`sh`).
fn session_path_dirs() -> Vec<PathBuf> {
    if cfg!(windows) {
        Vec::new()
    } else {
        vec![PathBuf::from("/usr/bin"), PathBuf::from("/bin")]
    }
}

fn unique_root(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "niu-repl-suite-{tag}-{}-{}",
        std::process::id(),
        nanos
    ))
}

/// Strip terminal escape sequences from a raw output chunk and drop
/// carriage returns, leaving the glyphs a user would read off the
/// screen, concatenated in emission order. Used for *frame*
/// assertions (what the current edit line reads) as opposed to the
/// expect stream (what has flowed past in order).
fn plain_screen(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            match chars.next() {
                Some('[') => {
                    // CSI: parameter/intermediate bytes until a final
                    // byte in @ (0x40) ..= ~ (0x7E).
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    // OSC: until BEL or ESC \.
                    let mut prev = '\0';
                    for c in chars.by_ref() {
                        if c == '\u{7}' || (prev == '\u{1b}' && c == '\\') {
                            break;
                        }
                        prev = c;
                    }
                }
                _ => {}
            }
        } else if c != '\r' && c != '\u{7}' {
            out.push(c);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Shared output plumbing (the driver's model: a reader thread feeding
// a search-and-consume buffer, plus a terminal responder answering
// the cursor-position / device-attribute queries so the editor never
// blocks waiting for a reply).
// ---------------------------------------------------------------------------

#[derive(Default)]
struct OutputBuffer {
    pending: Mutex<Vec<u8>>,
    eof: AtomicBool,
}

impl OutputBuffer {
    fn push(&self, bytes: &[u8]) {
        self.pending.lock().unwrap().extend_from_slice(bytes);
    }

    fn take_match(&self, needle: &str) -> Option<Vec<u8>> {
        let mut pending = self.pending.lock().unwrap();
        find_subslice(&pending, needle.as_bytes()).map(|end| pending.drain(..end).collect())
    }

    /// Consume through the first occurrence of `marker` that is
    /// immediately followed by an ASCII digit, plus the digit run
    /// itself; return the digits. Used to read the parent's
    /// `RCVAL=<digits>` report: the echoed command line also contains
    /// the marker (followed by `$?`), and bracketed-paste toggles sit
    /// between the echo line and the output line, so neither a plain
    /// first-match nor a line anchor can tell the two apart — the
    /// digit can.
    fn take_number_after(&self, marker: &str) -> Option<String> {
        let mut pending = self.pending.lock().unwrap();
        let needle = marker.as_bytes();
        let mut search_from = 0usize;
        loop {
            let rel = find_subslice(&pending[search_from..], needle)?;
            let start = search_from + rel;
            match pending.get(start) {
                Some(b) if b.is_ascii_digit() => {
                    let mut end = start;
                    while pending.get(end).is_some_and(|b| b.is_ascii_digit()) {
                        end += 1;
                    }
                    let window: Vec<u8> = pending.drain(..end).collect();
                    return Some(String::from_utf8_lossy(&window[start..]).into_owned());
                }
                Some(_) => search_from = start,
                None => return None,
            }
        }
    }

    fn take_pending(&self) -> Vec<u8> {
        std::mem::take(&mut self.pending.lock().unwrap())
    }

    fn is_eof(&self) -> bool {
        self.eof.load(Ordering::SeqCst)
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|pos| pos + needle.len())
}

/// Minimal terminal responder (same contract as the driver's: answer
/// `ESC[6n` with the tracked cursor position and `ESC[c` with a VT100
/// device-attribute reply, each query exactly once).
struct TerminalResponder {
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    row: u16,
    col: u16,
    carry: Vec<u8>,
}

impl TerminalResponder {
    fn new(writer: Arc<Mutex<Box<dyn Write + Send>>>) -> Self {
        Self {
            writer,
            row: 1,
            col: 1,
            carry: Vec::new(),
        }
    }

    fn observe(&mut self, bytes: &[u8]) {
        for &b in bytes {
            match b {
                b'\n' => {
                    self.row = self.row.saturating_add(1);
                    self.col = 1;
                }
                b'\r' => self.col = 1,
                0x08 => self.col = self.col.saturating_sub(1),
                _ => self.col = self.col.saturating_add(1),
            }
        }
        let mut stream = std::mem::take(&mut self.carry);
        stream.extend_from_slice(bytes);
        let mut rest = stream.as_slice();
        let mut replies: Vec<Vec<u8>> = Vec::new();
        loop {
            let cpr = find_subslice(rest, b"\x1b[6n");
            let da = find_subslice(rest, b"\x1b[c");
            let next = match (cpr, da) {
                (Some(c), Some(d)) if c <= d => {
                    Some((c, format!("\x1b[{};{}R", self.row, self.col).into_bytes()))
                }
                (_, Some(d)) => Some((d, b"\x1b[?1;2c".to_vec())),
                (Some(c), None) => {
                    Some((c, format!("\x1b[{};{}R", self.row, self.col).into_bytes()))
                }
                (None, None) => None,
            };
            match next {
                Some((start, reply)) => {
                    replies.push(reply);
                    rest = &rest[start..];
                }
                None => break,
            }
        }
        let keep = rest.len().saturating_sub(8);
        self.carry.extend_from_slice(&rest[keep..]);
        for reply in replies {
            let _ = self.writer.lock().unwrap().write_all(&reply);
        }
    }
}

// ---------------------------------------------------------------------------
// The unix carrier: a GNU bash (session leader, job control on)
// running on the pty; niu is its foreground job.
// ---------------------------------------------------------------------------

#[cfg(unix)]
mod carrier {
    use super::*;
    use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty, PtySize};

    pub struct Carrier {
        writer: Arc<Mutex<Box<dyn Write + Send>>>,
        output: Arc<OutputBuffer>,
        child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
        killer: Box<dyn ChildKiller + Send + Sync>,
        master: Box<dyn MasterPty + Send>,
        timeout: Duration,
        root: PathBuf,
        home: PathBuf,
        start: PathBuf,
        seen: Mutex<Vec<u8>>,
        owns_root: bool,
    }

    impl Carrier {
        /// The carrier shell's pid — also its session id (it leads a
        /// new session); everything in that session is ours to reap.
        pub fn session_id(&self) -> Option<u32> {
            self.child.lock().unwrap().process_id()
        }

        /// Non-consuming check: has `needle` appeared in the output
        /// buffered so far?
        pub fn buffered_contains(&self, needle: &str) -> bool {
            let pending = self.output.pending.lock().unwrap();
            find_subslice(&pending, needle.as_bytes()).is_some()
        }
    }

    impl Drop for Carrier {
        fn drop(&mut self) {
            let _ = self.killer.kill();
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if let Ok(Some(_)) = self.child.lock().unwrap().try_wait() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            if self.owns_root {
                let _ = std::fs::remove_dir_all(&self.root);
            }
        }
    }

    /// Locate the parent shell: env override, /bin/bash, the
    /// workspace GNU bash 5.3.20 oracle, then PATH.
    pub fn parent_bash() -> Option<PathBuf> {
        if let Some(p) = std::env::var_os("NIU_PTY_PARENT_BASH") {
            let p = PathBuf::from(p);
            if p.exists() {
                return Some(p);
            }
        }
        let system = PathBuf::from("/bin/bash");
        if system.exists() {
            return Some(system);
        }
        let oracle = PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
            .join("workspace/rubash-audit/gnu-bash-5.3.20/bin/bash");
        if oracle.exists() {
            return Some(oracle);
        }
        Some(PathBuf::from("bash"))
    }

    impl Carrier {
        /// Spawn the carrier bash over `root` (staged fresh by the
        /// caller or reused), with `niu_rc` written as the home's
        /// `.niubashrc`. Returns None when the pty cannot be opened.
        pub fn spawn(
            root: PathBuf,
            owns_root: bool,
            niu_rc: &str,
            size: (u16, u16),
            timeout: Duration,
        ) -> Option<Carrier> {
            let home = root.join("home");
            let start = root.join("start");
            std::fs::create_dir_all(home.join("tmp")).ok()?;
            std::fs::create_dir_all(&start).ok()?;
            std::fs::write(home.join(".niubashrc"), niu_rc).ok()?;
            // The parent's own rc: sentinel prompt, and its history
            // goes nowhere near the driven session's history file
            // (niu's default is $HOME/.niubash_history; the parent
            // would default to ~/.bash_history, but pin it away
            // entirely so the staged home stays single-purpose).
            let parent_rc = root.join("parent-rc");
            std::fs::write(
                &parent_rc,
                "PS1='PARENT> '\nPS2='PARENT2> '\nHISTFILE=/dev/null\n",
            )
            .ok()?;

            let bash = parent_bash()?;
            let pty_system = portable_pty::native_pty_system();
            let pair = pty_system
                .openpty(PtySize {
                    rows: size.1,
                    cols: size.0,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .ok()?;
            let master = pair.master;
            let slave = pair.slave;

            let mut command = CommandBuilder::new(&bash);
            command.arg("--rcfile");
            command.arg(&parent_rc);
            command.arg("-i");
            command.cwd(&start);
            command.env_clear();
            command.env("HOME", &home);
            command.env("PATH", std::env::join_paths(session_path_dirs()).unwrap());
            command.env("TEMP", home.join("tmp"));
            command.env("TMP", home.join("tmp"));
            command.env("TERM", "xterm");
            command.env("NIU_LANG", "en");
            command.env("RUST_LOG", "off");
            command.env("RUST_BACKTRACE", "0");

            let child = slave.spawn_command(command).ok()?;
            drop(slave);

            let writer = Arc::new(Mutex::new(master.take_writer().ok()?));
            let reader = master.try_clone_reader().ok()?;
            let output = Arc::new(OutputBuffer::default());
            let pump = {
                let output = Arc::clone(&output);
                let writer = Arc::clone(&writer);
                std::thread::spawn(move || {
                    let mut reader = reader;
                    let mut responder = TerminalResponder::new(writer);
                    let mut chunk = [0u8; 4096];
                    loop {
                        match reader.read(&mut chunk) {
                            Ok(0) => break,
                            Ok(n) => {
                                output.push(&chunk[..n]);
                                responder.observe(&chunk[..n]);
                            }
                            Err(_) => break,
                        }
                    }
                    output.eof.store(true, Ordering::SeqCst);
                })
            };
            std::mem::forget(pump);

            let killer = child.clone_killer();
            Some(Carrier {
                writer,
                output,
                child: Arc::new(Mutex::new(child)),
                killer,
                master,
                timeout,
                root,
                home,
                start,
                seen: Mutex::new(Vec::new()),
                owns_root,
            })
        }

        pub fn home(&self) -> &Path {
            &self.home
        }

        pub fn start(&self) -> &Path {
            &self.start
        }

        pub fn send(&mut self, keys: &str) {
            let result = {
                let mut w = self.writer.lock().unwrap();
                w.write_all(keys.as_bytes()).and_then(|()| w.flush())
            };
            result.unwrap_or_else(|e| panic!("send {keys:?} failed: {e}\n{}", self.transcript()));
        }

        pub fn send_line(&mut self, line: &str) {
            let mut bytes = line.as_bytes().to_vec();
            bytes.push(b'\r');
            let result = {
                let mut w = self.writer.lock().unwrap();
                w.write_all(&bytes).and_then(|()| w.flush())
            };
            result.unwrap_or_else(|e| {
                panic!("send_line {line:?} failed: {e}\n{}", self.transcript())
            });
        }

        pub fn expect(&mut self, needle: &str) -> String {
            let window = self
                .expect_inner(needle, self.timeout)
                .unwrap_or_else(|e| panic!("expected {needle:?}, {e}\n{}", self.transcript()));
            self.seen.lock().unwrap().extend_from_slice(&window);
            String::from_utf8_lossy(&window).into_owned()
        }

        pub fn expect_absent(&mut self, needle: &str, within: Duration) {
            match self.expect_inner(needle, within) {
                Ok(window) => panic!(
                    "did not expect {needle:?}; it appeared: {}\n{}",
                    String::from_utf8_lossy(&window),
                    self.transcript()
                ),
                Err(DriveError::Timeout) => {}
                Err(e) => panic!(
                    "expect_absent {needle:?} ended with {e}\n{}",
                    self.transcript()
                ),
            }
        }

        fn expect_inner(&mut self, needle: &str, timeout: Duration) -> Result<Vec<u8>, DriveError> {
            let deadline = Instant::now() + timeout;
            loop {
                if let Some(window) = self.output.take_match(needle) {
                    return Ok(window);
                }
                if self.output.is_eof() {
                    return Err(DriveError::Eof);
                }
                if Instant::now() >= deadline {
                    return Err(DriveError::Timeout);
                }
                std::thread::sleep(Duration::from_millis(15));
            }
        }

        /// Read the parent's `MARKER=<digits>` report (see
        /// [`OutputBuffer::take_number_after`] for why the digit is
        /// the discriminator).
        pub fn expect_value(&mut self, marker: &str) -> String {
            let deadline = Instant::now() + self.timeout;
            loop {
                if let Some(v) = self.output.take_number_after(marker) {
                    return v;
                }
                if Instant::now() >= deadline {
                    panic!(
                        "expected a {marker:?} line, timed out\n{}",
                        self.transcript()
                    );
                }
                std::thread::sleep(Duration::from_millis(15));
            }
        }

        /// Drain everything buffered right now (frame reads).
        pub fn drain(&mut self) -> String {
            let drained = self.output.take_pending();
            self.seen.lock().unwrap().extend_from_slice(&drained);
            String::from_utf8_lossy(&drained).into_owned()
        }

        /// Settle briefly, then read the current frame as plain text.
        pub fn frame(&mut self) -> String {
            std::thread::sleep(Duration::from_millis(300));
            plain_screen(&self.drain())
        }

        pub fn resize(&mut self, cols: u16, rows: u16) {
            self.master
                .resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .unwrap_or_else(|e| panic!("resize {cols}x{rows} failed: {e}"));
        }

        pub fn wait_exit(&mut self) -> u32 {
            let deadline = Instant::now() + self.timeout;
            loop {
                let exited = self
                    .child
                    .lock()
                    .unwrap()
                    .try_wait()
                    .unwrap_or_else(|e| panic!("wait for carrier exit failed: {e}"));
                if let Some(status) = exited {
                    return status.exit_code();
                }
                if Instant::now() >= deadline {
                    panic!("carrier did not exit\n{}", self.transcript());
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        pub fn transcript(&self) -> String {
            let mut text = String::from_utf8_lossy(&self.seen.lock().unwrap()).into_owned();
            text.push_str(&String::from_utf8_lossy(
                &self.output.pending.lock().unwrap(),
            ));
            text.replace('\u{1b}', "<ESC>").replace(['\r', '\u{7}'], "")
        }
    }

    #[derive(Debug)]
    pub enum DriveError {
        Timeout,
        Eof,
    }

    impl std::fmt::Display for DriveError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                DriveError::Timeout => write!(f, "timed out waiting for the expected output"),
                DriveError::Eof => write!(f, "the session reached EOF before the expected output"),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// ReplSession: one driven niu REPL, however it is carried. On unix it
// lives inside a Carrier (parent bash -> niu as foreground job); on
// Windows it is the repo driver's direct spawn.
// ---------------------------------------------------------------------------

pub struct ReplSession {
    #[cfg(unix)]
    carrier: carrier::Carrier,
    #[cfg(windows)]
    direct: driver::NiuSession,
    /// Whether niu is currently running (unix: launched in carrier).
    #[cfg(unix)]
    niu_running: bool,
}

impl ReplSession {
    /// Spawn over a fresh staged root (removed on drop).
    pub fn spawn(tag: &str, rc_extra: &str) -> ReplSession {
        Self::spawn_over(unique_root(tag), true, rc_extra, (100, 30))
    }

    /// Spawn over a caller-staged root (history chains reuse one home
    /// across sequential sessions; the caller removes the root).
    pub fn spawn_shared(tag: &str, root: &Path, rc_extra: &str) -> ReplSession {
        let _ = tag;
        Self::spawn_over(root.to_path_buf(), false, rc_extra, (100, 30))
    }

    fn spawn_over(root: PathBuf, owns_root: bool, rc_extra: &str, size: (u16, u16)) -> ReplSession {
        let rc = format!("{}{}", driver::default_rc(), rc_extra);
        #[cfg(unix)]
        {
            let mut c = carrier::Carrier::spawn(root, owns_root, &rc, size, HEAVY_TIMEOUT)
                .expect("carrier bash could not be spawned under a pseudo terminal");
            c.expect(PARENT_PROMPT);
            let mut s = ReplSession {
                carrier: c,
                niu_running: false,
            };
            s.launch_niu();
            s
        }
        #[cfg(windows)]
        {
            let _ = owns_root;
            let mut d = driver::NiuSession::spawn_shared_home(
                &root,
                &rc,
                &[],
                &session_path_dirs(),
                size,
                HEAVY_TIMEOUT,
            );
            d.wait_ready();
            ReplSession { direct: d }
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (root, owns_root, rc);
            panic!("repl_pty_suite: unsupported platform");
        }
    }

    /// (unix) Launch niu inside the carrier and wait for its first
    /// prompt. The niu binary path is typed at the parent prompt, so
    /// the parent treats it as an ordinary foreground job — the
    /// non-orphaned-group shape this suite exists to provide.
    #[cfg(unix)]
    fn launch_niu(&mut self) {
        let niu = driver::niu_binary();
        self.carrier.send_line(&niu.display().to_string());
        self.carrier.expect("Niubash");
        self.carrier.expect(driver::PROMPT1);
        self.niu_running = true;
    }

    pub fn home(&self) -> &Path {
        #[cfg(unix)]
        return self.carrier.home();
        #[cfg(windows)]
        return self.direct.home();
    }

    pub fn start(&self) -> &Path {
        #[cfg(unix)]
        return self.carrier.start();
        #[cfg(windows)]
        return self.direct.start();
    }

    pub fn send(&mut self, keys: &str) {
        #[cfg(unix)]
        self.carrier.send(keys);
        #[cfg(windows)]
        self.direct.send(keys);
    }

    pub fn send_line(&mut self, line: &str) {
        #[cfg(unix)]
        self.carrier.send_line(line);
        #[cfg(windows)]
        self.direct.send_line(line);
    }

    pub fn expect(&mut self, needle: &str) -> String {
        #[cfg(unix)]
        return self.carrier.expect(needle);
        #[cfg(windows)]
        return self.direct.expect(needle);
    }

    pub fn expect_prompt(&mut self) {
        self.expect(driver::PROMPT1);
    }

    pub fn expect_continuation(&mut self) {
        self.expect(driver::PROMPT2);
    }

    pub fn expect_absent(&mut self, needle: &str, within: Duration) {
        #[cfg(unix)]
        self.carrier.expect_absent(needle, within);
        #[cfg(windows)]
        self.direct.expect_absent(needle, within);
    }

    /// Settle, then the current frame as plain screen text.
    pub fn frame(&mut self) -> String {
        #[cfg(unix)]
        return self.carrier.frame();
        #[cfg(windows)]
        {
            std::thread::sleep(Duration::from_millis(300));
            plain_screen(&self.direct.drain_screen())
        }
    }

    /// Discard everything buffered so far (frame hygiene: the next
    /// frame read must reflect only what the next keypress paints).
    pub fn drain(&mut self) {
        #[cfg(unix)]
        {
            self.carrier.drain();
        }
        #[cfg(windows)]
        {
            self.direct.drain_screen();
        }
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        #[cfg(unix)]
        self.carrier.resize(cols, rows);
        #[cfg(windows)]
        self.direct.resize(cols, rows);
    }

    pub fn transcript(&self) -> String {
        #[cfg(unix)]
        return self.carrier.transcript();
        #[cfg(windows)]
        return self.direct.transcript();
    }

    /// End the niu session with `exit <code>` and return the exit
    /// code AS OBSERVED: on unix, by the parent shell's `$?` after
    /// niu returns to it (the chain's whole point — the code crossed
    /// a real job boundary); on Windows, the process exit code.
    pub fn exit_niu(&mut self, code: u32) -> u32 {
        #[cfg(unix)]
        {
            debug_assert!(self.niu_running);
            self.carrier.send_line(&format!("exit {code}"));
            self.carrier.expect(PARENT_PROMPT);
            self.niu_running = false;
            self.carrier.send_line("echo RCVAL=$?");
            let reported = self.carrier.expect_value("RCVAL=");
            reported.parse::<u32>().unwrap_or_else(|_| {
                panic!(
                    "parent reported a non-numeric $?: {reported:?}\n{}",
                    self.carrier.transcript()
                )
            })
        }
        #[cfg(windows)]
        {
            self.direct.send_line(&format!("exit {code}"));
            self.direct.wait_exit()
        }
    }

    /// (unix) End the carrier shell itself; returns its exit code.
    #[cfg(unix)]
    pub fn exit_carrier(&mut self) -> u32 {
        self.carrier.send_line("exit");
        self.carrier.wait_exit()
    }
}

/// A shell under test must never outlive its test: niu is the carrier
/// shell's child, so killing the carrier alone would orphan it (and a
/// shell whose terminal vanished can spin). On drop: ask niu to exit
/// if it is still up, then — on Linux — sweep the carrier's whole
/// session by pid before the carrier itself is killed.
#[cfg(unix)]
impl Drop for ReplSession {
    fn drop(&mut self) {
        if !self.niu_running {
            return;
        }
        self.niu_running = false;
        let c = &mut self.carrier;
        let sid = c.session_id();
        // Best effort: interrupt whatever is running and leave.
        c.send(CTRL_C);
        c.send_line("exit 0");
        let deadline = std::time::Instant::now() + Duration::from_millis(900);
        while std::time::Instant::now() < deadline {
            if c.buffered_contains(PARENT_PROMPT) {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        if let Some(sid) = sid {
            kill_session_members(sid);
        }
    }
}

/// SIGKILL every process in session `sid` except this test process
/// itself (Linux /proc sweep; other unixes rely on the graceful exit
/// above plus the carrier kill in `Carrier::drop`).
#[cfg(target_os = "linux")]
fn kill_session_members(sid: u32) {
    let me = std::process::id();
    let mut victims: Vec<u32> = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(pid) = name.to_str().and_then(|n| n.parse::<u32>().ok()) else {
                continue;
            };
            if pid == me {
                continue;
            }
            let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
                continue;
            };
            // Fields after the parenthesised comm: state ppid pgrp
            // session ... — session is index 3 of the remainder.
            let Some(rest) = stat.rsplit(") ").next() else {
                continue;
            };
            let fields: Vec<&str> = rest.split_whitespace().collect();
            if fields.get(3).and_then(|f| f.parse::<u32>().ok()) == Some(sid) {
                victims.push(pid);
            }
        }
    }
    for pid in victims {
        let _ = std::process::Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status();
    }
}

#[cfg(not(target_os = "linux"))]
fn kill_session_members(_sid: u32) {}

// ---------------------------------------------------------------------------
// Chain 1 — boot: the carrier launches niu, a command executes with
// rc-provided state, `$?` tracks a failing command, and `exit 7`
// carries the code back across the job boundary to the parent.
// ---------------------------------------------------------------------------

#[test]
fn repl_pty_boot_run_exit_code_chain() {
    if !require_pty_or_skip("repl_pty_boot_run_exit_code_chain") {
        return;
    }
    let mut s = ReplSession::spawn("boot", "BOOTV=bootval\n");
    // The typed line shows the literal `$BOOTV`; only execution can
    // produce the value (echo-proof).
    s.send_line("echo BOOTV=$BOOTV");
    s.expect("BOOTV=bootval");
    s.expect_prompt();
    // A failing command's status is the next line's input.
    s.send_line("false");
    s.expect_prompt();
    s.send_line("echo PREVST=$?");
    s.expect("PREVST=1");
    s.expect_prompt();
    // The exit code crosses back to the parent shell.
    let code = s.exit_niu(7);
    assert_eq!(
        code,
        7,
        "parent must observe niu's exit code 7\n{}",
        s.transcript()
    );
    #[cfg(unix)]
    assert_eq!(
        s.exit_carrier(),
        0,
        "carrier must exit cleanly\n{}",
        s.transcript()
    );
}

// ---------------------------------------------------------------------------
// Chain 3 — completion menu (default `select` interaction): TAB puts
// the first candidate in the line, further TABs cycle, Esc rolls
// back, and an accepted directory drills in to an executable line.
// ---------------------------------------------------------------------------

#[test]
fn repl_pty_completion_menu_cycle_edit_execute_chain() {
    if !require_pty_or_skip("repl_pty_completion_menu_cycle_edit_execute_chain") {
        return;
    }
    let mut s = ReplSession::spawn("menu", "");
    std::fs::create_dir(s.start().join("alpha")).unwrap();
    std::fs::write(s.start().join("alpha").join("aaa.txt"), "A-MENU\n").unwrap();
    std::fs::create_dir(s.start().join("alpine")).unwrap();
    std::fs::write(s.start().join("alpine").join("bbb.txt"), "B-MENU\n").unwrap();

    s.send("ls al");
    s.drain();
    s.send(TAB);
    let f = s.frame();
    assert!(
        f.contains("ls alpha/"),
        "TAB #1 must put the first candidate in the line (`ls alpha/`)\nframe:\n{f}"
    );
    assert!(
        f.contains("alpha") && f.contains("alpine"),
        "the candidate grid must list both directories\nframe:\n{f}"
    );
    s.send(TAB);
    let f = s.frame();
    assert!(
        f.contains("ls alpine/"),
        "TAB #2 must cycle the line to the next candidate (`ls alpine/`)\nframe:\n{f}"
    );
    s.send(TAB);
    let f = s.frame();
    assert!(
        f.contains("ls alpha/"),
        "TAB #3 must cycle back to the first candidate (`ls alpha/`)\nframe:\n{f}"
    );
    // Esc rolls the line back to the pre-completion snapshot.
    s.send(ESC);
    std::thread::sleep(Duration::from_millis(300));
    let f = s.frame();
    assert!(
        f.contains("ls al") && !f.contains("ls alpha/"),
        "Esc must roll the line back to `ls al`\nframe:\n{f}"
    );
    // Re-open, then ACCEPT the highlighted directory with Enter
    // (with the menu up, Enter accepts the candidate into the line —
    // it does not submit), and keep editing inside it: the completed
    // inner file makes the line executable.
    s.send(TAB);
    let f = s.frame();
    assert!(
        f.contains("ls alpha/"),
        "TAB after Esc must complete again\nframe:\n{f}"
    );
    s.send("\r");
    let f = s.frame();
    assert!(
        f.contains("ls alpha/") && !f.contains("alpha/aaa.txt"),
        "Enter with the menu up must accept `ls alpha/` unexecuted\nframe:\n{f}"
    );
    s.send("a");
    s.drain();
    s.send(TAB);
    let f = s.frame();
    assert!(
        f.contains("ls alpha/aaa.txt"),
        "TAB inside the accepted directory must complete the inner file\nframe:\n{f}"
    );
    s.send("\r");
    s.expect("alpha/aaa.txt");
    s.expect_prompt();
}

// ---------------------------------------------------------------------------
// Chain 5 — continuation: unclosed quote and heredoc hold PS2 across
// lines, an edit on a continuation line lands in the executed text,
// and PS1 returns exactly when the construct closes.
// ---------------------------------------------------------------------------

#[test]
fn repl_pty_continuation_quote_and_heredoc_chain() {
    if !require_pty_or_skip("repl_pty_continuation_quote_and_heredoc_chain") {
        return;
    }
    let mut s = ReplSession::spawn("cont", "");
    // Unclosed double quote -> PS2; the closing line completes it.
    s.send_line("printf 'Q1=%s\\n' \"multi");
    s.expect_continuation();
    s.send_line("line\"");
    s.expect("Q1=multi");
    s.expect("line");
    s.expect_prompt();
    // Heredoc: PS2 for the body and the delimiter line, output only
    // once the delimiter lands.
    s.send_line("cat <<HEOF");
    s.expect_continuation();
    s.send_line("body $((3*3))");
    s.expect_continuation();
    s.send_line("HEOF");
    s.expect("body 9");
    s.expect_prompt();
    // Edit on a continuation line: type a typo (`secnd` for
    // `second`), erase its wrong tail, retype — the executed string
    // must be the corrected one.
    s.send_line("echo \"AB");
    s.expect_continuation();
    s.send("secnd");
    s.send(BACKSPACE);
    s.send(BACKSPACE);
    s.send_line("ond\"");
    s.expect("AB");
    s.expect("second");
    s.expect_prompt();
}

// ---------------------------------------------------------------------------
// Chain 6 — continuation into a function definition: the body lines
// hold PS2, the closing brace defines without executing, the call
// runs the body, and a one-line redefinition replaces it.
// ---------------------------------------------------------------------------

#[test]
fn repl_pty_continuation_function_definition_chain() {
    if !require_pty_or_skip("repl_pty_continuation_function_definition_chain") {
        return;
    }
    let mut s = ReplSession::spawn("funcdef", "");
    s.send_line("greet() {");
    s.expect_continuation();
    s.send_line("echo GREET_$((6*7))");
    s.expect_continuation();
    s.send_line("}");
    s.expect_prompt();
    // Defining must not have executed the body: the buffer showed the
    // literal `$((6*7))`, so only a call can produce GREET_42.
    s.expect_absent("GREET_42", ABSENT_WINDOW);
    s.send_line("greet");
    s.expect("GREET_42");
    s.expect_prompt();
    // Redefinition replaces the body for the next call.
    s.send_line("greet() { echo GREET2_$((1+1)); }");
    s.expect_prompt();
    s.send_line("greet");
    s.expect("GREET2_2");
    s.expect_prompt();
}

// ---------------------------------------------------------------------------
// Chain 7 — Ctrl-C in its three seats: mid-typed (the line dies, the
// session is unharmed), mid-continuation (the construct is
// abandoned), and mid-execution (the job dies, `$?` is 130).
// ---------------------------------------------------------------------------

#[test]
fn repl_pty_ctrl_c_discard_interrupt_abandon_chain() {
    if !require_pty_or_skip("repl_pty_ctrl_c_discard_interrupt_abandon_chain") {
        return;
    }
    let mut s = ReplSession::spawn("ctrlc", "");
    // Mid-typed: the typed text carries the literal `$((6*7))`, so
    // DEAD_42 can only appear if the discarded line somehow ran.
    s.send("echo DEAD_$((6*7))");
    s.send(CTRL_C);
    s.expect_prompt();
    s.expect_absent("DEAD_42", ABSENT_WINDOW);
    settle();
    s.send_line("echo LIVE_$((6*7))");
    s.expect("LIVE_42");
    s.expect_prompt();
    // Mid-continuation: abandon the open quote, land back on PS1.
    s.send_line("echo \"unclosed");
    s.expect_continuation();
    s.send(CTRL_C);
    s.expect_prompt();
    settle();
    s.send_line("echo CLEAN_$((2+3))");
    s.expect("CLEAN_5");
    s.expect_prompt();
    // Mid-execution: Ctrl-C kills the foreground job, status 130.
    s.send_line("sleep 30");
    std::thread::sleep(Duration::from_millis(600));
    s.send(CTRL_C);
    s.expect_prompt();
    s.send_line("echo STC=$?");
    s.expect("STC=130");
    s.expect_prompt();
}

// ---------------------------------------------------------------------------
// Chain 8 — Ctrl-Z / jobs / bg / wait (unix): a foreground job stops,
// `jobs` reports it Stopped, `bg` resumes it in the background, and
// `wait` collects its completion marker. Under the carrier shape
// every process group here is non-orphaned, so the stop is a real
// POSIX stop, not the orphaned-group discard.
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn repl_pty_ctrl_z_stop_bg_wait_chain() {
    if !require_pty_or_skip("repl_pty_ctrl_z_stop_bg_wait_chain") {
        return;
    }
    let mut s = ReplSession::spawn("ctrlz", "");
    // One job (inside `sh -c`) whose marker prints only when the job
    // truly runs to completion; the typed text carries `$((6*7))`,
    // so ZDONE_42 is execution-proof.
    s.send_line("sh -c 'sleep 4; echo ZDONE_$((6*7))'");
    std::thread::sleep(Duration::from_millis(600));
    s.send(CTRL_Z);
    s.expect_prompt();
    s.expect_absent("ZDONE_42", ABSENT_WINDOW);
    s.send_line("jobs");
    s.expect("Stopped");
    s.expect_prompt();
    settle();
    s.send_line("bg");
    s.expect_prompt();
    s.send_line("wait");
    s.expect("ZDONE_42");
    s.expect_prompt();
    // The shell is fully healthy after the job dance.
    s.send_line("echo AFTERZ_$((8+1))");
    s.expect("AFTERZ_9");
    s.expect_prompt();
}

// ---------------------------------------------------------------------------
// Chain 9 — resize mid-edit: a wrapped line sits in the buffer, the
// terminal shrinks, the redrawn buffer keeps every character, the
// line executes as typed, and $COLUMNS follows both resizes.
// ---------------------------------------------------------------------------

#[test]
fn repl_pty_resize_mid_edit_redraw_chain() {
    if !require_pty_or_skip("repl_pty_resize_mid_edit_redraw_chain") {
        return;
    }
    let mut s = ReplSession::spawn("resize", "");
    s.send_line("echo COLR=$COLUMNS");
    s.expect("COLR=100");
    s.expect_prompt();
    // A long line whose only execution witness is a short marker;
    // the 60-char padding rides in a comment so the buffer is long
    // enough to wrap at the smaller width.
    let padding = "x".repeat(60);
    let line = format!("printf 'RSZ42\\n' # {padding}");
    s.send(&line);
    let f = s.frame().replace('\n', "");
    assert!(
        f.contains(&padding),
        "the typed line must be fully buffered before the resize\nframe:\n{f}"
    );
    s.resize(40, 30);
    let f = s.frame().replace('\n', "");
    assert!(
        f.contains(&padding),
        "the redraw after shrinking to 40 cols must keep the whole buffer\nframe:\n{f}"
    );
    s.send("\r");
    s.expect("RSZ42");
    s.expect_prompt();
    s.send_line("echo COLR=$COLUMNS");
    s.expect("COLR=40");
    s.expect_prompt();
    s.resize(120, 30);
    s.send_line("echo COLR=$COLUMNS");
    s.expect("COLR=120");
    s.expect_prompt();
}

// ---------------------------------------------------------------------------
// Chain 10 — bracketed paste: a two-line chunk lands in the buffer
// unexecuted (both lines visible, neither output present), and one
// Enter runs the chunk as one multiline unit, in order.
// ---------------------------------------------------------------------------

#[test]
fn repl_pty_bracketed_paste_multiline_chain() {
    if cfg!(windows) {
        // Platform limitation, not a regression: ConPTY consumes the
        // ESC[200~/ESC[201~ markers before they reach the shell (the
        // niubash#202 case, same treatment as the gate's S7).
        return;
    }
    if !require_pty_or_skip("repl_pty_bracketed_paste_multiline_chain") {
        return;
    }
    let mut s = ReplSession::spawn("paste", "PV1=paste-one\nPV2=paste-two\n");
    s.send("\u{1b}[200~echo $PV1\r\necho $PV2 tail\u{1b}[201~");
    // Buffered, not executed: the values are the execution witnesses
    // (the buffer shows the literal `$PV1`/`$PV2`).
    s.expect_absent("paste-one", ABSENT_WINDOW);
    s.expect_absent("paste-two", ABSENT_WINDOW);
    let f = s.frame();
    assert!(
        f.contains("echo $PV1") && f.contains("echo $PV2 tail"),
        "both pasted lines must sit in the edit buffer\nframe:\n{f}"
    );
    s.send("\r");
    s.expect("paste-one");
    s.expect("paste-two tail");
    s.expect_prompt();
}

// ---------------------------------------------------------------------------
// Chain 11 — wide characters (CJK): insertion, whole-char backspace
// (never a split char), mid-line insertion by column, and byte-exact
// execution output throughout.
// ---------------------------------------------------------------------------

#[test]
fn repl_pty_cjk_edit_and_execute_chain() {
    if !require_pty_or_skip("repl_pty_cjk_edit_and_execute_chain") {
        return;
    }
    let mut s = ReplSession::spawn("cjk", "");
    // Store wide text, read it back through expansion: the typed
    // assignment shows the glyphs, but only execution pairs them
    // with the GOT= prefix.
    s.send_line("XW=你好世界");
    s.expect_prompt();
    s.send_line("echo GOT=$XW");
    s.expect("GOT=你好世界");
    s.expect_prompt();
    // Whole-char backspace: drop `X`, drop `世` (one key, one whole
    // char), retype `界` — execution must print the exact result.
    s.send("echo E=你好世X");
    s.drain();
    s.send(BACKSPACE);
    s.send(BACKSPACE);
    s.send("界");
    let f = s.frame();
    assert!(
        f.contains("E=你好界"),
        "after two backspaces and a retype the line must read `echo E=你好界`\nframe:\n{f}"
    );
    s.send("\r");
    s.expect("E=你好界");
    s.expect_prompt();
    // Mid-line insertion across a wide char: cursor left over `好`,
    // insert ASCII — the stored value must keep every char in order.
    s.send("Y=你好");
    s.send(LEFT);
    s.send("AB");
    let f = s.frame();
    assert!(
        f.contains("Y=你AB好"),
        "insertion before `好` must produce `Y=你AB好`\nframe:\n{f}"
    );
    s.send("\r");
    s.expect_prompt();
    s.send_line("echo YV=$Y");
    s.expect("YV=你AB好");
    s.expect_prompt();
    // No split char anywhere in the whole session.
    assert!(
        !s.transcript().contains('\u{fffd}'),
        "a replacement char appeared — a wide char was split\n{}",
        s.transcript()
    );
}
// ---------------------------------------------------------------------------
// Chain 2 — history: session 1's commands land in $HISTFILE line by
// line at exit; session 2 reads the file once at startup (Up recalls
// its last line, Ctrl-R re-executes an entry), stays isolated from a
// mid-session external append (the GNU visibility rule), and its own
// commands append after everything that came before.
// ---------------------------------------------------------------------------

#[test]
fn repl_pty_history_persist_recall_isolation_chain() {
    if !require_pty_or_skip("repl_pty_history_persist_recall_isolation_chain") {
        return;
    }
    let root = unique_root("hist");
    let hist_path = root.join("home").join(".niubash_history");

    // --- Session 1: two commands, then exit. ---
    {
        let mut s = ReplSession::spawn_shared("hist", &root, "");
        s.send_line("echo HISTA_$((20+22))");
        s.expect("HISTA_42");
        s.expect_prompt();
        s.send_line("echo hist-second-line");
        s.expect("hist-second-line");
        s.expect_prompt();
        assert_eq!(s.exit_niu(0), 0);
        #[cfg(unix)]
        s.exit_carrier();
    }
    // The history file, line by line: exactly the session's entered
    // lines, in order, nothing from the carrier shell.
    let lines: Vec<String> = std::fs::read_to_string(&hist_path)
        .unwrap_or_else(|e| panic!("history file unreadable after session 1: {e}"))
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(
        lines,
        ["echo HISTA_$((20+22))", "echo hist-second-line", "exit 0"],
        "session 1 must persist exactly its entered lines"
    );

    // --- Session 2 over the same home. ---
    {
        let mut s = ReplSession::spawn_shared("hist", &root, "");
        // Startup read: the first Up recalls the file's last line.
        s.send(UP);
        let f = s.frame();
        assert!(
            f.contains("exit 0"),
            "first Up in session 2 must recall the file's last line (`exit 0`)\nframe:\n{f}"
        );
        s.send(CTRL_C);
        s.expect_prompt();
        settle();
        // Ctrl-R finds session 1's command and re-executes it.
        s.send(CTRL_R);
        std::thread::sleep(Duration::from_millis(150));
        s.send("HISTA");
        std::thread::sleep(Duration::from_millis(150));
        s.send("\r");
        s.send("\r");
        s.expect("HISTA_42");
        s.expect_prompt();
        // Isolation: another writer appends to the file mid-session;
        // this session's recall must never surface that line (it may
        // only arrive via `history -n`/`-r` or a fresh session — the
        // GNU visibility semantics niu adopted).
        {
            use std::io::Write as _;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&hist_path)
                .unwrap();
            writeln!(f, "echo EXTERNAL_INJECTED_LINE").unwrap();
        }
        let mut frames = String::new();
        let mut last_frame = String::new();
        for _ in 0..4 {
            s.send(UP);
            last_frame = s.frame();
            frames.push_str(&last_frame);
            frames.push('\n');
        }
        assert!(
            !frames.contains("EXTERNAL_INJECTED_LINE"),
            "mid-session external append leaked into this session's recall\nframes:\n{frames}"
        );
        assert!(
            last_frame.contains("echo HISTA_$((20+22))"),
            "walking Up must bottom out at the file's oldest entry\nframe:\n{last_frame}"
        );
        s.send(CTRL_C);
        s.expect_prompt();
        settle();
        s.send_line("echo HISTB_marker");
        s.expect("HISTB_marker");
        s.expect_prompt();
        assert_eq!(s.exit_niu(0), 0);
        #[cfg(unix)]
        s.exit_carrier();
    }
    // After session 2: the session-owned lines are all present in
    // order (the re-executed HISTA command was recorded again), and
    // the total line count is 7 — the externally appended line among
    // them. (The count is pinned deliberately: any change to the
    // history write face — e.g. a move to GNU's blind-overwrite
    // semantics, which would drop the external line — must surface
    // here for adjudication rather than pass silently. See
    // REPORT.md for the GNU contrast.)
    let lines: Vec<String> = std::fs::read_to_string(&hist_path)
        .unwrap_or_else(|e| panic!("history file unreadable after session 2: {e}"))
        .lines()
        .map(str::to_string)
        .collect();
    let owned: Vec<&str> = lines
        .iter()
        .map(String::as_str)
        .filter(|l| *l != "echo EXTERNAL_INJECTED_LINE")
        .collect();
    assert_eq!(
        owned,
        [
            "echo HISTA_$((20+22))",
            "echo hist-second-line",
            "exit 0",
            "echo HISTA_$((20+22))",
            "echo HISTB_marker",
            "exit 0",
        ],
        "session-owned history lines must persist in order across both sessions"
    );
    assert_eq!(lines.len(), 7, "history file line count drifted: {lines:?}");
    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// Chain 4 — completion compspec fallback (the fzf shape, niubash#828's
// engine root in rubash#2570): a `complete -D` handler function that
// yields NO candidates. Without `-o default` there is no fallback
// (GNU agrees — probed against GNU bash 5.3.20); with `-o default`
// the engine must fall back to filename completion.
// ---------------------------------------------------------------------------

#[test]
fn repl_pty_completion_compspec_default_fallback_chain() {
    if !require_pty_or_skip("repl_pty_completion_compspec_default_fallback_chain") {
        return;
    }
    let mut s = ReplSession::spawn("compspec", "");
    std::fs::write(s.start().join("zebra.txt"), "ZEBRA\n").unwrap();
    // The fzf-shaped registration: a default handler whose function
    // produces an empty COMPREPLY.
    s.send_line("__fb() { COMPREPLY=(); }");
    s.expect_prompt();
    s.send_line("complete -D -F __fb");
    s.expect_prompt();
    // No `-o default`: TAB offers nothing (the line is untouched).
    s.send("ls z");
    s.drain();
    s.send(TAB);
    let f = s.frame();
    assert!(
        f.contains("ls z") && !f.contains("zebra"),
        "without `-o default` an empty -D result must offer no candidates\nframe:\n{f}"
    );
    s.send(CTRL_C);
    s.expect_prompt();
    settle();
    // With `-o default`: the same empty result must fall back to
    // filename completion, and the committed line executes.
    s.send_line("complete -D -o default -F __fb");
    s.expect_prompt();
    s.send("ls z");
    s.drain();
    s.send(TAB);
    let f = s.frame();
    assert!(
        f.contains("ls zebra.txt "),
        "with `-o default` an empty -D result must fall back to filename completion\nframe:\n{f}"
    );
    s.send("\r");
    s.expect("zebra.txt");
    s.expect_prompt();
}

// ---------------------------------------------------------------------------
// Chain 12 — bind assembly (niubash#863 GAP-13): the inputrc read, a
// runtime `bind`, and a `bind -r` removal all land in the mirrored
// editor keymap — each step's keypress proves the previous step's
// registration, and the inputrc rewrite proves the once-only
// initialize is really once-only. The links, in order:
//
// 1. inputrc (`"\C-t": "INPUTRC"`) — read by the host's
//    `Session::initialize_readline` call in `load_engine_bindings`,
//    after the rc, before the first snapshot.
// 2. runtime `bind '"\C-y": "RCBIND"'` — the next prompt's editor
//    has it (generation moves, mirror rebuilds).
// 3. inputrc rewritten mid-session, then a second runtime `bind`
//    moves the generation (which re-runs the load): the second
//    initialize is a no-op, so Ctrl-T still inserts the FIRST
//    file's text.
// 4. `bind -r '\C-u'` — the tombstone rebuilds the mirror and the
//    key stops inserting.
//
// The rc deliberately carries NO `bind` line: the engine
// self-initializes on its first `bind` invocation
// (bind_builtins.rs), so an rc `bind` would read the inputrc
// during rc sourcing and mask the host call site — link 1 would
// pass even with the host call deleted, and the mutation
// self-check below would be vacuous. With a bind-free rc, the
// host's `load_engine_bindings` call is the ONLY trigger for the
// first inputrc read.
//
// The plugin-macro write face (`Session::register_binding`, the
// `rl_macro_bind` route) is not driven here: it needs a loadable
// plugin, which this suite cannot build without a C toolchain. It is
// covered by the `loadable-builtin-niu` probe in
// scripts/readline-plugin-probes (pty_probes.py: `rl_probe macro` ->
// Ctrl-T inserts MAGIC) and by the lib-level
// `issue863_gap13_register_macro_binding_*` pins.
// ---------------------------------------------------------------------------

#[test]
fn repl_pty_bind_inputrc_rc_runtime_removal_chain() {
    if !require_pty_or_skip("repl_pty_bind_inputrc_rc_runtime_removal_chain") {
        return;
    }
    const CTRL_T: &str = "\u{14}";
    const CTRL_U: &str = "\u{15}";
    const CTRL_Y: &str = "\u{19}";

    let root = unique_root("bind");
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join(".inputrc"), "\"\\\\C-t\": \"INPUTRC\"\n").unwrap();
    // Bind-free rc (see the header comment): the host's initialize
    // call must be the trigger for the first inputrc read.
    let mut s = ReplSession::spawn_shared("bind", &root, "");

    // Link 1 — inputrc macro: Ctrl-T inserts the file's text.
    s.send("echo ");
    s.send(CTRL_T);
    let f = s.frame();
    assert!(
        f.contains("echo INPUTRC"),
        "Ctrl-T must insert the inputrc macro text (`echo INPUTRC`)\nframe:\n{f}"
    );
    s.send("\r");
    s.expect("INPUTRC");
    s.expect_prompt();

    // Link 2 — runtime bind macro: Ctrl-Y inserts the runtime text.
    s.send_line("bind '\"\\\\C-y\": \"RCBIND\"'");
    s.expect_prompt();
    s.send("echo ");
    s.send(CTRL_Y);
    let f = s.frame();
    assert!(
        f.contains("echo RCBIND"),
        "Ctrl-Y must insert the runtime bind macro text (`echo RCBIND`)\nframe:\n{f}"
    );
    s.send("\r");
    s.expect("RCBIND");
    s.expect_prompt();

    // Link 3 — rewrite the inputrc, then move the generation with a
    // second runtime bind: the second initialize is a no-op, so
    // Ctrl-T must still insert the FIRST file's text, never the
    // rewritten one — while the new runtime macro lands.
    std::fs::write(home.join(".inputrc"), "\"\\\\C-t\": \"INPUTRC-V2\"\n").unwrap();
    s.send_line("bind '\"\\\\C-u\": \"RUNTIME\"'");
    s.expect_prompt();

    s.send("echo ");
    s.send(CTRL_T);
    let f = s.frame();
    assert!(
        f.contains("echo INPUTRC") && !f.contains("INPUTRC-V2"),
        "after the inputrc rewrite Ctrl-T must still insert the first file's text\nframe:\n{f}"
    );
    s.send("\r");
    s.expect("INPUTRC");
    s.expect_prompt();

    s.send("echo ");
    s.send(CTRL_U);
    let f = s.frame();
    assert!(
        f.contains("echo RUNTIME"),
        "Ctrl-U must insert the runtime bind macro text (`echo RUNTIME`)\nframe:\n{f}"
    );
    s.send("\r");
    s.expect("RUNTIME");
    s.expect_prompt();

    // Link 4 — removal: `bind -r` clears the macro; Ctrl-U must no
    // longer insert it (and must not resurrect the earlier text).
    s.send_line("bind -r '\\C-u'");
    s.expect_prompt();
    s.drain();
    s.send("echo ");
    s.send(CTRL_U);
    let f = s.frame();
    assert!(
        !f.contains("RUNTIME"),
        "after `bind -r` Ctrl-U must no longer insert the removed macro\nframe:\n{f}"
    );
    s.send(CTRL_C);
    s.expect_prompt();
    settle();

    assert_eq!(s.exit_niu(0), 0);
    #[cfg(unix)]
    assert_eq!(
        s.exit_carrier(),
        0,
        "carrier must exit cleanly\n{}",
        s.transcript()
    );
    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// Chain 13 — `complete -E` / `-I` host gate (niubash#863 B5-11):
// the empty line and the command word reach the engine's compspecs
// through `Session::has_compspec_at`, and the governing answer
// suppresses the cwd/alias/function overlays (ruling Q-C (i): the
// compspec answer replaces default completion). Each link registers
// exactly the spec it proves, then removes it, so a leftover
// registration cannot carry the next link:
//
// 1. empty line + only `-E` -> `eone`/`etwo`, no cwd overlay
//    (`./esub/`, `./eprog`, `./dprog` would all match the empty
//    word if the overlays ran — the overlay the gate must suppress
//    is visible, not vacuous).
// 2. whitespace-only line, cursor at EOL + only `-I` -> `ione`/
//    `itwo` (W1: `-E` does not trigger here; `-I` does).
// 3. column 0 ahead of the command name (`"   ls"`, cursor 0) +
//    only `-E` -> `eone`/`etwo` (W2: the empty-line arm, not `-I`).
// 4. command word + only `-D` -> host command completion
//    (`./dprog`), never the `-D` wordlist (`dalpha`) — the Q-D
//    guard shape: `-D` never governs the command word.
//
// The rc carries no `bind` line and no compspec registration, so
// nothing masks the host gate: the gate is the only trigger for the
// `-E`/`-I` answers. (The `-o default` / `-o bashdefault`
// pass-through is the engine's own outcome — rubash complete.rs
// runs the fallback inside `complete_detailed` — and is pinned
// engine-side plus by the existing default-fallback chain above,
// not re-pinned here.)
//
// Trigger: all four links drive completion with TAB (niubash#927:
// a bare TAB on an empty word reaches the completer exactly like a
// non-empty word — TAB #1 splices the first candidate into the line
// and opens the grid, so the wordlist + no-overlay assertions still
// discriminate the gate exactly: with the old name-form gate the
// completer returns the cwd overlay instead of the wordlist, and TAB
// splices that. Before #927 links 1–3 drove through M-*
// (`insert-completions`) because a bare TAB inserted a literal tab
// and never reached the completer; the M-* shape is no longer
// needed here now that TAB reaches the completer on empty words.
// ---------------------------------------------------------------------------

#[test]
fn repl_pty_completion_empty_initial_compspec_chain() {
    if !require_pty_or_skip("repl_pty_completion_empty_initial_compspec_chain") {
        return;
    }
    let mut s = ReplSession::spawn("b511", "");
    // Cwd fixtures the overlays would offer if not suppressed: a
    // directory and two executables (plain data files never qualify
    // at command position, niubash#671, so they would prove nothing).
    std::fs::create_dir(s.start().join("esub")).unwrap();
    for name in ["eprog", "dprog"] {
        let path = s.start().join(name);
        std::fs::write(&path, "#!/bin/sh\necho hi\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    // Link 1 — empty line + only `-E`, driven by TAB (niubash#927).
    s.send_line("complete -E -W 'eone etwo'");
    s.expect_prompt();
    settle();
    s.drain();
    s.send(TAB);
    std::thread::sleep(Duration::from_millis(500));
    let f = s.frame();
    assert!(
        f.contains("eone")
            && f.contains("etwo")
            && !f.contains("./esub/")
            && !f.contains("./eprog")
            && !f.contains("./dprog"),
        "empty line + -E must offer the -E wordlist with no cwd overlay\nframe:\n{f}"
    );
    s.send(CTRL_C);
    s.expect_prompt();
    settle();

    // Link 2 — whitespace-only line, cursor at EOL + only `-I`.
    s.send_line("complete -r -E");
    s.expect_prompt();
    s.send_line("complete -I -W 'ione itwo'");
    s.expect_prompt();
    settle();
    s.send("   ");
    s.drain();
    s.send(TAB);
    std::thread::sleep(Duration::from_millis(500));
    let f = s.frame();
    assert!(
        f.contains("ione")
            && f.contains("itwo")
            && !f.contains("./esub/")
            && !f.contains("./eprog"),
        "whitespace-only line at EOL + -I must offer the -I wordlist with no cwd overlay\nframe:\n{f}"
    );
    s.send(CTRL_C);
    s.expect_prompt();
    settle();

    // Link 3 — column 0 ahead of the command name + only `-E`.
    s.send_line("complete -r -I");
    s.expect_prompt();
    s.send_line("complete -E -W 'eone etwo'");
    s.expect_prompt();
    settle();
    s.send("   ls");
    s.drain();
    for _ in 0..5 {
        s.send(LEFT);
    }
    s.drain();
    s.send(TAB);
    std::thread::sleep(Duration::from_millis(500));
    let f = s.frame();
    assert!(
        f.contains("eone")
            && f.contains("etwo")
            && !f.contains("./esub/")
            && !f.contains("./eprog"),
        "column 0 ahead of the command name + -E must offer the -E wordlist with no cwd overlay\nframe:\n{f}"
    );
    s.send(CTRL_C);
    s.expect_prompt();
    settle();

    // Link 4 — command word + only `-D`: host command completion,
    // never the `-D` wordlist.
    s.send_line("complete -r -E");
    s.expect_prompt();
    s.send_line("complete -D -W 'dalpha dbeta'");
    s.expect_prompt();
    settle();
    s.send("d");
    s.drain();
    s.send(TAB);
    let f = s.frame();
    assert!(
        !f.contains("dalpha") && f.contains("dprog"),
        "command word + only -D must take host command completion, never the -D wordlist\nframe:\n{f}"
    );
    s.send(CTRL_C);
    s.expect_prompt();
    settle();

    assert_eq!(s.exit_niu(0), 0);
    #[cfg(unix)]
    assert_eq!(
        s.exit_carrier(),
        0,
        "carrier must exit cleanly\n{}",
        s.transcript()
    );
}
