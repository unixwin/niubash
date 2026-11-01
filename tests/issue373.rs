//! niubash#373 regression harness.
//!
//! Sub-item 1: a DSR-silent terminal must not kill the shell.
//!
//! GNU bash/readline never asks the terminal where the cursor is (it counts
//! prompt width from escape heuristics), so a terminal that does not answer
//! `ESC[6n` — dumb terminals, `script(1)` recordings, some serial/tmux
//! configurations — still gets its first prompt instantly. niu's line editor
//! anchors the prompt with a cursor-position query before the first paint;
//! on a terminal that never answers, the editor used to stall ~2.6 s, print
//! "niubash: line editor error: The cursor position could not be read within
//! a normal duration", and exit before painting any prompt (the niubash#355
//! finding this follow-up closes). After the fix the query is advisory: the
//! prompt paints without a reply and every later cycle stays interactive.
//!
//! Platform split (measured on this harness, Win10 19045):
//!
//! * Unix: crossterm parses `ESC[6n` answers off the input stream itself,
//!   so a silent terminal is fully exercisable — the hard regression
//!   assertions live here (unix-only leg). NOTE the repo's interactive CI
//!   runs on Windows desktops only, so CI reaches the control leg; the
//!   silent leg's assertions need a unix host (any Linux/macOS desktop or
//!   Termux check-out: `cargo test --test issue373_dsr`).
//! * Windows/ConPTY: conhost forwards the cursor-position request to the
//!   connected terminal at session attach and BLOCKS THE CLIENT before its
//!   first instruction — the transcript of a silent session is exactly
//!   `<ESC>[6n`, no banner, no startup ticks. That wedge lives below niu
//!   (the terminal-side contract: Windows Terminal, pywinpty's agent and
//!   the shared interactive driver all answer), so on Windows this suite
//!   pins the contract's other half: with a DSR-answering terminal, normal
//!   interactive startup stays fast and clean.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty, PtySize};

/// Sentinel PS1 pinned in the sandbox rc (same convention as
/// tests/interactive.rs).
const PROMPT1: &str = "P1> ";
/// Startup banner head.
const BANNER: &str = "Niubash";
/// The line-editor error the pre-fix editor printed when the
/// cursor-position query timed out (crossterm's wording, unix path).
const LINE_EDITOR_ERROR: &str = "line editor error";

/// Per-assert timeout for the control (answering) leg.
const CONTROL_TIMEOUT: Duration = Duration::from_secs(15);

/// Budget for the first prompt on a DSR-silent unix terminal: normal
/// startup (~0.3 s here) plus the editor's bounded cursor query (crossterm
/// gives it a fixed 2 s) plus transport slack.
const SILENT_FIRST_PROMPT_BUDGET: Duration = Duration::from_secs(10);

/// Budget for one full prompt cycle on a DSR-silent unix terminal: each
/// cycle re-anchors the prompt with one bounded query (2 s on timeout) and
/// the repaint's drift check re-measures the stale row once more. The
/// degraded path must stay interactive — a #355-shaped abort (error text,
/// exit, no prompt) cannot pass, and neither can an unbounded stall.
const SILENT_CYCLE_BUDGET: Duration = Duration::from_secs(12);

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

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "niu-373-dsr-{prefix}-{}-{nanos}",
        std::process::id()
    ))
}

/// Whether interactive tests were explicitly forced on (CI setting).
fn tests_forced() -> bool {
    std::env::var_os("NIU_INTERACTIVE_TESTS").is_some_and(|v| v != "0")
}

/// Same gate as the shared driver: skip when this host cannot open a pseudo
/// terminal (hard failure under NIU_INTERACTIVE_TESTS=1).
fn require_pty_or_skip(context: &str) -> bool {
    if std::env::var_os("CI").is_some() && !tests_forced() {
        eprintln!("SKIP {context}: hosted-CI ConPTY is not representative");
        return false;
    }
    static PTY_AVAILABLE: OnceLock<bool> = OnceLock::new();
    let available = *PTY_AVAILABLE
        .get_or_init(|| SilentSession::spawn("probe-373", "# probe\n", true).is_some());
    if available {
        return true;
    }
    if tests_forced() {
        panic!("NIU_INTERACTIVE_TESTS=1 but no pseudo terminal is available ({context})");
    }
    eprintln!("SKIP {context}: no pseudo terminal available on this host");
    false
}

struct OutputBuffer {
    pending: Mutex<Vec<u8>>,
    eof: AtomicBool,
}

impl OutputBuffer {
    fn push(&self, bytes: &[u8]) {
        self.pending.lock().unwrap().extend_from_slice(bytes);
    }

    /// True once `needle` has arrived (non-consuming peek).
    fn contains(&self, needle: &str) -> bool {
        let pending = self.pending.lock().unwrap();
        find_subslice(&pending, needle.as_bytes()).is_some()
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

/// A niu process under a pseudo terminal whose terminal side either answers
/// the `ESC[6n`/`ESC[c` queries a real terminal answers (`answer_queries`)
/// or stays silent (the DSR-silent regression condition).
struct SilentSession {
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    output: Arc<OutputBuffer>,
    child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    #[allow(dead_code)]
    master: Box<dyn MasterPty + Send>,
    root: PathBuf,
}

impl Drop for SilentSession {
    fn drop(&mut self) {
        let _ = self.killer.kill();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.lock().unwrap().try_wait() {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl SilentSession {
    /// Spawn with the deterministic rc; `answer_queries` selects the
    /// answering control leg or the DSR-silent condition. `None` means this
    /// host cannot open a pseudo terminal.
    fn spawn(prefix: &str, rc: &str, answer_queries: bool) -> Option<SilentSession> {
        let root = unique_temp_dir(prefix);
        let home = root.join("home");
        let start = root.join("start");
        std::fs::create_dir_all(home.join("tmp")).ok()?;
        std::fs::create_dir_all(&start).ok()?;
        std::fs::write(home.join(".niubashrc"), rc).ok()?;

        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 30,
                cols: 120,
                pixel_width: 0,
                pixel_height: 0,
            })
            .ok()?;
        let master = pair.master;
        let slave = pair.slave;

        let system_root = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                if cfg!(windows) {
                    PathBuf::from(r"C:\Windows")
                } else {
                    // Non-Windows hosts have no System32. The `C:\Windows`
                    // fallback contains the Unix PATH separator ('C:' before
                    // the ':' split would make join_paths fail), so use a
                    // colon-free placeholder: these tests drive builtins and
                    // niu itself, never PATH tools.
                    PathBuf::from("/tmp")
                }
            });
        let system32 = system_root.join("System32");
        let mut command = CommandBuilder::new(niu_binary());
        command.cwd(&start);
        command.env_clear();
        command.env("SystemRoot", system_root.clone());
        command.env(
            "PATH",
            std::env::join_paths([&system32, &system_root]).unwrap(),
        );
        command.env("COMSPEC", system32.join("cmd.exe"));
        command.env("HOME", &home);
        command.env("USERPROFILE", &home);
        command.env("LOCALAPPDATA", home.join("localappdata"));
        command.env("APPDATA", home.join("appdata"));
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
        let output = Arc::new(OutputBuffer {
            pending: Mutex::new(Vec::new()),
            eof: AtomicBool::new(false),
        });
        let pump_writer = Arc::clone(&writer);
        let pump_output = Arc::clone(&output);
        std::thread::spawn(move || {
            let mut reader = reader;
            let mut chunk = [0u8; 4096];
            let mut carry: Vec<u8> = Vec::new();
            let (mut row, mut col) = (1u16, 1u16);
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        let bytes = &chunk[..n];
                        pump_output.push(bytes);
                        if answer_queries {
                            // Track the cursor through control moves and
                            // answer each query exactly once (tail carry so
                            // queries split across chunks still match).
                            for &b in bytes {
                                match b {
                                    b'\n' => {
                                        row = row.saturating_add(1);
                                        col = 1;
                                    }
                                    b'\r' => col = 1,
                                    0x08 => col = col.saturating_sub(1),
                                    _ => col = col.saturating_add(1),
                                }
                            }
                            carry.extend_from_slice(bytes);
                            let mut replies: Vec<Vec<u8>> = Vec::new();
                            loop {
                                let cpr = find_subslice(&carry, b"\x1b[6n");
                                let da = find_subslice(&carry, b"\x1b[c");
                                let (pos, reply) = match (cpr, da) {
                                    (Some(c), Some(d)) if c <= d => {
                                        (c, format!("\x1b[{row};{col}R").into_bytes())
                                    }
                                    (_, Some(d)) => (d, b"\x1b[?1;2c".to_vec()),
                                    (Some(c), None) => {
                                        (c, format!("\x1b[{row};{col}R").into_bytes())
                                    }
                                    (None, None) => break,
                                };
                                replies.push(reply);
                                carry.drain(..pos);
                            }
                            for reply in replies {
                                let _ = pump_writer.lock().unwrap().write_all(&reply);
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
            pump_output.eof.store(true, Ordering::SeqCst);
        });

        let killer = child.clone_killer();
        Some(SilentSession {
            writer,
            output,
            child: Arc::new(Mutex::new(child)),
            killer,
            master,
            root,
        })
    }

    fn send(&self, keys: &str) {
        let mut w = self.writer.lock().unwrap();
        w.write_all(keys.as_bytes())
            .and_then(|()| w.flush())
            .unwrap();
    }

    fn send_line(&self, line: &str) {
        let mut bytes = line.as_bytes().to_vec();
        bytes.push(b'\r');
        let mut w = self.writer.lock().unwrap();
        w.write_all(&bytes).and_then(|()| w.flush()).unwrap();
    }

    /// Wait until `needle` arrives; returns the wait in milliseconds. The
    /// needle stays buffered (peek), so later assertions can still see it.
    fn wait_ms(&self, needle: &str, budget: Duration) -> Result<u128, String> {
        let start = Instant::now();
        loop {
            if self.output.contains(needle) {
                return Ok(start.elapsed().as_millis());
            }
            if self.output.is_eof() {
                return Err(format!(
                    "EOF before {needle:?} after {} ms",
                    start.elapsed().as_millis()
                ));
            }
            if start.elapsed() >= budget {
                return Err(format!(
                    "timed out after {} ms waiting for {needle:?}",
                    budget.as_millis()
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Consume-and-wait: true once `needle` arrived; the stream up to its
    /// end is drained so a later identical needle needs a fresh occurrence.
    fn expect_take(&self, needle: &str, budget: Duration) -> Result<(), String> {
        let start = Instant::now();
        loop {
            {
                let mut pending = self.output.pending.lock().unwrap();
                if let Some(end) = find_subslice(&pending, needle.as_bytes()) {
                    pending.drain(..end);
                    return Ok(());
                }
            }
            if self.output.is_eof() {
                return Err(format!("EOF before {needle:?}"));
            }
            if start.elapsed() >= budget {
                return Err(format!("timed out waiting for {needle:?}"));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Everything the terminal side has received so far, escapes visible.
    fn transcript(&self) -> String {
        String::from_utf8_lossy(&self.output.pending.lock().unwrap()).replace('\u{1b}', "<ESC>")
    }
}

/// The deterministic rc: ASCII prompt sentinels, no default plugin packs.
fn default_rc() -> String {
    "PS1='P1> '\nPS2='P2> '\nNIU_DISABLE_DEFAULT_PLUGINS=1\n".to_string()
}

/// One startup + prompt-cycle round trip, timed per stage. Every budget is
/// execution-level: banner sentinel, prompt sentinel, `echo` marker output,
/// and no line-editor error anywhere in the stream.
fn startup_and_cycle(
    session: &SilentSession,
    first_prompt_budget: Duration,
    cycle_budget: Duration,
) -> Result<(), String> {
    let banner_ms = session.wait_ms(BANNER, CONTROL_TIMEOUT)?;
    let prompt_ms = session.wait_ms(PROMPT1, first_prompt_budget)?;
    eprintln!("niu373 timing: banner={banner_ms}ms first_prompt=+{prompt_ms}ms");

    let cycle_start = Instant::now();
    session.send_line("echo dsr373-out");
    session.wait_ms("dsr373-out", cycle_budget)?;
    eprintln!(
        "niu373 timing: echo_cycle={}ms",
        cycle_start.elapsed().as_millis()
    );

    if session.output.contains(LINE_EDITOR_ERROR) {
        return Err("line editor error reached the terminal".to_string());
    }
    Ok(())
}

/// Control leg, every platform: the terminal answers `ESC[6n`/`ESC[c` like
/// a real terminal (and like the shared interactive driver does). Startup
/// and one prompt cycle must complete cleanly — this pins the harness
/// itself and guards sub-item 3's "no regression in normal interactive
/// startup".
#[test]
fn dsr_answering_terminal_startup_and_round_trip() {
    if !require_pty_or_skip("dsr_answering_terminal_startup_and_round_trip") {
        return;
    }
    let session = SilentSession::spawn("dsr373-control", &default_rc(), true)
        .expect("session after a successful probe");
    if let Err(e) = startup_and_cycle(&session, CONTROL_TIMEOUT, CONTROL_TIMEOUT) {
        panic!(
            "control leg failed: {e}\ntranscript tail:\n{}",
            session.transcript()
        );
    }
}

/// The regression leg, unix: a terminal that never answers `ESC[6n` must
/// still get the banner, a first prompt, and an interactive prompt cycle —
/// with no line-editor error (the pre-fix behavior aborted `read_line` and
/// killed the session). Windows/ConPTY cannot exercise this: conhost blocks
/// the client before its first instruction when the DSR goes unanswered
/// (see the module docs), so there the contract is pinned by the control
/// leg instead.
#[test]
fn dsr_silent_terminal_still_paints_prompt_and_stays_interactive() {
    if !require_pty_or_skip("dsr_silent_terminal_still_paints_prompt_and_stays_interactive") {
        return;
    }
    if cfg!(windows) {
        eprintln!(
            "SKIP silent assertions on Windows/ConPTY: conhost withholds the client \
             until the terminal answers ESC[6n (session-attach contract, below niu); \
             the unix leg carries the regression assertions"
        );
        return;
    }
    let session = SilentSession::spawn("dsr373-silent", &default_rc(), false)
        .expect("session after a successful probe");
    if let Err(e) = startup_and_cycle(&session, SILENT_FIRST_PROMPT_BUDGET, SILENT_CYCLE_BUDGET) {
        panic!(
            "silent leg failed: {e}\ntranscript tail:\n{}",
            session.transcript()
        );
    }
}

/// CR-round regression (F1): the full Ctrl+R flow through a real session —
/// seed a history entry, reverse-search it, accept with Enter (the match
/// must LOAD into the line), submit, and observe the executed output.
///
/// The first CR round of this PR wrapped every bare Enter in a
/// `Multiple([Esc, Enter])`; reedline 0.52 routes whole `Multiple` batches
/// to the history-search handler while a search is active and that handler
/// drops `Multiple(_)` as `Inapplicable` — the accepting Enter became a
/// dead key and no query could ever load. With the TAB-window gate the
/// search's Enter stays the bare event its handler implements.
///
/// Assertion shape: `printf` keeps the executed output (`cr373-R42`)
/// distinct from the history line (`printf 'cr373-%s\n' R42`), and the
/// first execution's output is consumed before the search begins, so only
/// a real load+submit can produce the second occurrence.
#[test]
fn ctrl_r_history_search_enter_loads_match_and_executes() {
    if !require_pty_or_skip("ctrl_r_history_search_enter_loads_match_and_executes") {
        return;
    }
    let session = SilentSession::spawn("cr373-flow", &default_rc(), true)
        .expect("session after a successful probe");
    session.wait_ms(BANNER, CONTROL_TIMEOUT).unwrap();
    session.wait_ms(PROMPT1, CONTROL_TIMEOUT).unwrap();
    session.send_line("printf 'cr373-%s\\n' R42");
    session.expect_take("cr373-R42", CONTROL_TIMEOUT).unwrap();
    // Reverse search, narrow to the seed entry, accept with Enter (the
    // load under test), then submit the loaded line.
    session.send("\u{12}");
    std::thread::sleep(Duration::from_millis(150));
    session.send("R42");
    std::thread::sleep(Duration::from_millis(150));
    session.send("\r");
    std::thread::sleep(Duration::from_millis(150));
    session.send("\r");
    session.expect_take("cr373-R42", CONTROL_TIMEOUT).unwrap();
}
